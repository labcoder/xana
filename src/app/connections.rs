//! Connection, credential, catalog, and model command orchestration.

use crate::{
    cli::{AuthCommand, ConnectionCommand, ModelCommand, PermissionChoice, SetupArgs},
    config::{CredentialReference, ProviderKind, XanaConfig},
    connection_management::{
        ConnectionEffect, ConnectionManagement, ConnectionProgress, ConnectionProgressStage,
        ConnectionReceipt, ConnectionSummaryView, ConnectionTestReceipt, CredentialState,
        ManagedAccountState, ReachabilityState, RecoveryAction,
    },
    credential::{SecretString, delete_secret, store_secret},
    managed::codex::{
        AccountStatus, CodexAppServer, CodexLaunchConfig, LoginCancellation, LoginMode,
    },
    model_catalog::{ExecutionKind, ModelManager},
    paths::XanaPaths,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::io::{self, IsTerminal, Read, Write};

const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

pub(super) async fn run_auth_command<W: Write>(
    command: AuthCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let provider = match &command {
        AuthCommand::Login { provider }
        | AuthCommand::Status { provider }
        | AuthCommand::Logout { provider } => provider.clone(),
    };
    writeln!(output, "`xana auth` is deprecated; use `xana connection`.")?;
    let translated = match command {
        AuthCommand::Login { .. } => ConnectionCommand::Login {
            id: provider.clone(),
            device_code: false,
        },
        AuthCommand::Status { .. } => ConnectionCommand::Status {
            id: provider.clone(),
        },
        AuthCommand::Logout { .. } => ConnectionCommand::Logout {
            id: provider.clone(),
            yes: false,
        },
    };
    run_connection_command(translated, paths, output, false).await
}

pub(crate) fn model_manager(paths: &XanaPaths) -> Result<ModelManager> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load connection registry")?;
    Ok(ModelManager::new(
        registry,
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    ))
}

pub(crate) fn codex_launch(connection: &crate::config::ConnectionConfig) -> CodexLaunchConfig {
    CodexLaunchConfig {
        program: connection
            .codex_program
            .clone()
            .unwrap_or_else(|| "codex".into()),
        home: connection.codex_home.clone(),
    }
}

pub(super) async fn run_connection_command<W: Write>(
    command: ConnectionCommand,
    paths: &XanaPaths,
    output: &mut W,
    json: bool,
) -> Result<()> {
    match command {
        ConnectionCommand::Add {
            id,
            kind,
            base_url,
            env,
            credential_id,
            key_from_stdin,
            model,
            codex_program,
            codex_home,
            yes,
            dry_run,
        } => {
            let permission_mode = XanaConfig::load_registry_from(paths.config_file())
                .map(|registry| permission_choice(registry.permission_mode))
                .unwrap_or(PermissionChoice::Ask);
            let setup_args = SetupArgs {
                non_interactive: true,
                quick: true,
                kind: Some(kind),
                connection: Some(id.clone()),
                base_url,
                codex_program,
                codex_home,
                credential_env: env,
                credential_id,
                key_from_stdin,
                model: Some(model),
                permission_mode: Some(permission_mode),
                yes,
                dry_run,
                ..SetupArgs::default()
            };
            let mut input = io::stdin().lock();
            let mut transcript = Vec::new();
            let outcome = crate::setup::run(
                &setup_args,
                paths,
                false,
                false,
                &mut input,
                &mut transcript,
                crate::presentation::ResolvedPresentation::plain(),
            )
            .await?;
            let effect = if dry_run {
                ConnectionEffect::Validated
            } else {
                ConnectionEffect::Added
            };
            let receipt = action_receipt(
                &id,
                if dry_run {
                    "connection.add.validated.v1"
                } else {
                    "connection.add.completed.v1"
                },
                effect,
            );
            if json {
                write_json(output, &receipt)?;
            } else {
                output.write_all(&transcript)?;
                writeln!(output, "receipt: {}", receipt.semantic_code)?;
            }
            debug_assert!(matches!(
                outcome,
                crate::setup::SetupOutcome::Committed { .. }
                    | crate::setup::SetupOutcome::Unchanged
            ));
            Ok(())
        }
        ConnectionCommand::Test { id } => {
            let (receipt, _) = test_connection(paths, &id).await?;
            write_connection_test(output, &receipt, json)?;
            if !receipt.usable {
                anyhow::bail!(
                    "connection {id:?} is not usable; recovery={}",
                    wire_name(&receipt.recovery)
                );
            }
            Ok(())
        }
        ConnectionCommand::Repair { id } => {
            let (test, models) = test_connection(paths, &id).await?;
            if !test.usable {
                write_connection_test(output, &test, json)?;
                anyhow::bail!(
                    "connection {id:?} could not be repaired; recovery={}",
                    wire_name(&test.recovery)
                );
            }
            let manager = model_manager(paths)?;
            manager.write_discovered_cache(&id, &models)?;
            let receipt = action_receipt(
                &id,
                "connection.repair.completed.v1",
                ConnectionEffect::Repaired,
            );
            if json {
                write_json(output, &receipt)?;
            } else {
                writeln!(output, "connection repaired: {id}")?;
                writeln!(output, "cached models: {}", models.len())?;
                writeln!(output, "receipt: {}", receipt.semantic_code)?;
            }
            Ok(())
        }
        ConnectionCommand::List => {
            let snapshot = ConnectionManagement::open(paths)?.snapshot()?;
            if json {
                write_json(output, &snapshot)?;
            } else {
                for summary in &snapshot.connections {
                    write_connection_row(output, summary)?;
                }
            }
            Ok(())
        }
        ConnectionCommand::Status { id } => {
            let management = ConnectionManagement::open(paths)?;
            let mut detail = management
                .snapshot()?
                .connections
                .into_iter()
                .find(|connection| connection.id == id)
                .with_context(|| format!("unknown connection {id:?}"))?;
            let manager = model_manager(paths)?;
            let connection = manager.connection(&id)?;
            if connection.kind == ProviderKind::Codex {
                let mut server = CodexAppServer::spawn(&codex_launch(connection)).await?;
                let account = server.account_status().await?;
                detail.observe_managed_account(managed_account_state(&account));
                let rate_limits = if matches!(account, AccountStatus::ChatGpt { .. }) {
                    let limits = server.rate_limits().await?;
                    [
                        ("primary", "/rateLimits/primary/usedPercent"),
                        ("secondary", "/rateLimits/secondary/usedPercent"),
                    ]
                    .into_iter()
                    .filter_map(|(name, pointer)| {
                        limits
                            .pointer(pointer)
                            .and_then(serde_json::Value::as_f64)
                            .map(|value| (name, value))
                    })
                    .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let version = server.version.clone();
                let codex_home = server.codex_home.clone();
                server.shutdown().await?;
                if json {
                    write_json(output, &detail)?;
                } else {
                    write_connection_detail(output, &detail)?;
                    writeln!(output, "runtime: {version}")?;
                    writeln!(output, "managed account home: {}", codex_home.display())?;
                    for (name, percent) in rate_limits {
                        writeln!(output, "{name} usage: {percent:.0}%")?;
                    }
                }
            } else {
                if json {
                    write_json(output, &detail)?;
                } else {
                    write_connection_detail(output, &detail)?;
                }
            }
            Ok(())
        }
        ConnectionCommand::SetKey { id, from_stdin } => {
            let manager = model_manager(paths)?;
            let connection = manager.connection(&id)?;
            let CredentialReference::Stored { id: credential_id } = connection
                .credential
                .as_ref()
                .context("connection does not declare a stored credential")?
            else {
                anyhow::bail!(
                    "connection {id:?} uses an environment credential; set that variable instead"
                )
            };
            let credential_id = credential_id.clone();
            let secret = if from_stdin {
                let mut input = String::new();
                io::stdin()
                    .take((MAX_CREDENTIAL_BYTES as u64).saturating_add(1))
                    .read_to_string(&mut input)?;
                if input.len() > MAX_CREDENTIAL_BYTES {
                    anyhow::bail!("credential exceeds the {MAX_CREDENTIAL_BYTES}-byte limit")
                }
                while input.ends_with(['\r', '\n']) {
                    input.pop();
                }
                SecretString::new(input)?
            } else {
                if !io::stdin().is_terminal() {
                    anyhow::bail!("hidden key entry requires a terminal; use --from-stdin")
                }
                SecretString::new(rpassword::prompt_password(format!("API key for {id}: "))?)?
            };
            let models = manager.probe_native(&id, Some(&secret)).await?;
            if models.is_empty() {
                anyhow::bail!(
                    "connection {id:?} returned an empty model catalog; the prior credential was preserved"
                )
            }
            store_secret(&credential_id, &secret)?;
            let cache_warning = manager
                .write_discovered_cache(&id, &models)
                .err()
                .map(|error| {
                    format!(
                        "credential was stored, but the catalog cache was not updated: {error}; run `xana model refresh {id}`"
                    )
                });
            let mut receipt = action_receipt(
                &id,
                "connection.credential_replace.completed.v1",
                ConnectionEffect::CredentialReplaced,
            );
            receipt.warnings.extend(cache_warning);
            if json {
                write_json(output, &receipt)?;
            } else {
                writeln!(
                    output,
                    "credential validated against {} model(s) and stored in the operating-system credential store for {id}",
                    models.len()
                )?;
                for warning in &receipt.warnings {
                    writeln!(output, "warning: {warning}")?;
                }
            }
            Ok(())
        }
        ConnectionCommand::DeleteKey { id, yes } => {
            if !yes {
                anyhow::bail!(
                    "credential deletion is separate from connection removal and requires --yes"
                )
            }
            let manager = model_manager(paths)?;
            let connection = manager.connection(&id)?;
            let CredentialReference::Stored { id: credential_id } = connection
                .credential
                .as_ref()
                .context("connection does not declare a stored credential")?
            else {
                anyhow::bail!("connection {id:?} uses an environment credential")
            };
            let deleted = delete_secret(credential_id)?;
            let receipt = action_receipt(
                &id,
                "connection.credential_delete.completed.v1",
                ConnectionEffect::CredentialDeleted,
            );
            if json {
                write_json(output, &receipt)?;
            } else {
                writeln!(
                    output,
                    "credential {} for {id}",
                    if deleted {
                        "deleted"
                    } else {
                        "was already absent"
                    }
                )?;
            }
            Ok(())
        }
        ConnectionCommand::Login { id, device_code } => {
            let manager = model_manager(paths)?;
            let connection = manager.connection(&id)?;
            if connection.kind != ProviderKind::Codex {
                anyhow::bail!("connection {id:?} uses an API key, not managed login")
            }
            let mut server = CodexAppServer::spawn(&codex_launch(connection)).await?;
            if !matches!(server.account_status().await?, AccountStatus::LoggedOut) {
                if json {
                    write_json(
                        output,
                        &action_receipt(
                            &id,
                            "connection.managed_login.already_complete.v1",
                            ConnectionEffect::ManagedLoginCompleted,
                        ),
                    )?;
                } else {
                    writeln!(output, "Codex is already logged in.")?;
                }
                server.shutdown().await?;
                return Ok(());
            }
            let instructions = server
                .begin_login(if device_code {
                    LoginMode::DeviceCode
                } else {
                    LoginMode::Browser
                })
                .await?;
            writeln!(output, "Open this URL to authorize Codex:")?;
            writeln!(output, "{}", instructions.url)?;
            if let Some(code) = &instructions.user_code {
                writeln!(output, "Code: {code}")?;
            }
            writeln!(output, "Waiting for authorization...")?;
            let status = tokio::select! {
                result = server.wait_for_login(&instructions.login_id) => Some(result?),
                signal = tokio::signal::ctrl_c() => {
                    signal.context("could not listen for managed-login cancellation")?;
                    None
                }
            };
            let Some(status) = status else {
                let cancellation = server.cancel_login(&instructions.login_id).await?;
                let receipt = action_receipt(
                    &id,
                    "connection.managed_login.cancelled.v1",
                    ConnectionEffect::ManagedLoginCancelled,
                );
                if json {
                    write_json(output, &receipt)?;
                } else {
                    writeln!(
                        output,
                        "Managed login {}. No Xana configuration changed.",
                        match cancellation {
                            LoginCancellation::Cancelled => "cancelled",
                            LoginCancellation::NotFound => "was already complete or absent",
                        }
                    )?;
                    writeln!(output, "receipt: {}", receipt.semantic_code)?;
                }
                server.shutdown().await?;
                return Ok(());
            };
            if json {
                write_json(
                    output,
                    &action_receipt(
                        &id,
                        "connection.managed_login.completed.v1",
                        ConnectionEffect::ManagedLoginCompleted,
                    ),
                )?;
            } else {
                write_account_status(output, &status)?;
            }
            server.shutdown().await?;
            Ok(())
        }
        ConnectionCommand::Logout { id, yes } => {
            if !yes {
                anyhow::bail!(
                    "logout changes the shared Codex account for this CODEX_HOME and may affect other Codex clients; rerun with --yes"
                )
            }
            let manager = model_manager(paths)?;
            let connection = manager.connection(&id)?;
            if connection.kind != ProviderKind::Codex {
                anyhow::bail!("use `xana connection delete-key {id}` for API-key connections")
            }
            let mut server = CodexAppServer::spawn(&codex_launch(connection)).await?;
            let already_logged_out =
                matches!(server.account_status().await?, AccountStatus::LoggedOut);
            if already_logged_out {
                if !json {
                    writeln!(output, "Codex was already logged out.")?;
                }
            } else {
                server.logout().await?;
                if !json {
                    writeln!(output, "Codex account logged out for this CODEX_HOME.")?;
                }
            }
            server.shutdown().await?;
            if json {
                write_json(
                    output,
                    &action_receipt(
                        &id,
                        "connection.managed_logout.completed.v1",
                        ConnectionEffect::ManagedLogoutCompleted,
                    ),
                )?;
            }
            Ok(())
        }
        ConnectionCommand::Refresh { id } => refresh_models(paths, &id, output, json).await,
        ConnectionCommand::Remove { id, yes } => {
            if !yes {
                anyhow::bail!("connection removal requires --yes")
            }
            let management = ConnectionManagement::open(paths)?;
            let plan = management.removal_plan(&id, std::iter::empty())?;
            let receipt = management.remove(&plan)?;
            if json {
                write_json(output, &receipt)?;
            } else {
                writeln!(output, "connection removed: {id}")?;
                writeln!(
                    output,
                    "backup: {}",
                    receipt
                        .backup
                        .as_ref()
                        .expect("remove receipt backup")
                        .display()
                )?;
                if !receipt.retained_authority.is_empty() {
                    writeln!(
                        output,
                        "retained authority: {}",
                        receipt.retained_authority.join(", ")
                    )?;
                }
            }
            Ok(())
        }
    }
}

fn permission_choice(mode: crate::config::PermissionMode) -> PermissionChoice {
    match mode {
        crate::config::PermissionMode::Deny => PermissionChoice::Deny,
        crate::config::PermissionMode::Ask => PermissionChoice::Ask,
        crate::config::PermissionMode::Allow => PermissionChoice::Allow,
    }
}

pub(crate) async fn test_connection(
    paths: &XanaPaths,
    id: &str,
) -> Result<(
    ConnectionTestReceipt,
    Vec<crate::model_catalog::ModelDescriptor>,
)> {
    let management = ConnectionManagement::open(paths)?;
    let summary = management
        .snapshot()?
        .connections
        .into_iter()
        .find(|connection| connection.id == id)
        .with_context(|| format!("unknown connection {id:?}"))?;
    let manager = model_manager(paths)?;
    let connection = manager.connection(id)?.clone();
    let mut progress = vec![
        ConnectionProgress {
            semantic_code: "connection.test.validating.v1",
            stage: ConnectionProgressStage::Validating,
        },
        ConnectionProgress {
            semantic_code: "connection.test.authority.v1",
            stage: ConnectionProgressStage::CheckingAuthority,
        },
    ];
    let mut receipt = ConnectionTestReceipt {
        version: crate::connection_management::CONNECTION_STATE_VERSION,
        semantic_code: "connection.test.completed.v1",
        effect: ConnectionEffect::Tested,
        connection: id.to_owned(),
        execution: summary.execution,
        reachability: ReachabilityState::NotTested,
        credential: summary.facets.credential,
        account: summary.facets.account,
        discovered_model_count: 0,
        usable: false,
        recovery: summary.recovery,
        failure: None,
        progress: Vec::new(),
    };
    if matches!(
        receipt.credential,
        CredentialState::Missing | CredentialState::Inaccessible
    ) {
        progress.push(ConnectionProgress {
            semantic_code: "connection.test.completed.v1",
            stage: ConnectionProgressStage::Complete,
        });
        receipt.progress = progress;
        return Ok((receipt, Vec::new()));
    }
    progress.push(ConnectionProgress {
        semantic_code: "connection.test.catalog.v1",
        stage: ConnectionProgressStage::DiscoveringCatalog,
    });
    let models = if connection.kind == ProviderKind::Codex {
        match CodexAppServer::spawn(&codex_launch(&connection)).await {
            Err(error) => {
                receipt.reachability = ReachabilityState::Unreachable;
                receipt.recovery = RecoveryAction::Inspect;
                receipt.failure = Some(bounded_failure(error.to_string()));
                Vec::new()
            }
            Ok(mut server) => {
                receipt.reachability = ReachabilityState::Reachable;
                let models = match server.account_status().await {
                    Err(error) => {
                        receipt.recovery = RecoveryAction::Inspect;
                        receipt.failure = Some(bounded_failure(error.to_string()));
                        Vec::new()
                    }
                    Ok(account) => {
                        receipt.account = managed_account_state(&account);
                        if matches!(account, AccountStatus::LoggedOut) {
                            receipt.recovery = RecoveryAction::Login;
                            Vec::new()
                        } else {
                            match server.models().await {
                                Ok(models) => models,
                                Err(error) => {
                                    receipt.recovery = RecoveryAction::RefreshCatalog;
                                    receipt.failure = Some(bounded_failure(error.to_string()));
                                    Vec::new()
                                }
                            }
                        }
                    }
                };
                server.shutdown().await?;
                models
            }
        }
    } else {
        match manager.probe_native(id, None).await {
            Ok(models) => {
                receipt.reachability = ReachabilityState::Reachable;
                models
            }
            Err(error) => {
                receipt.reachability = ReachabilityState::Unreachable;
                receipt.recovery = RecoveryAction::Inspect;
                receipt.failure = Some(bounded_failure(error.to_string()));
                Vec::new()
            }
        }
    };
    receipt.discovered_model_count = models.len();
    receipt.usable = receipt.reachability == ReachabilityState::Reachable
        && !models.is_empty()
        && !matches!(receipt.account, ManagedAccountState::LoggedOut);
    if receipt.usable {
        receipt.recovery = RecoveryAction::None;
    } else if receipt.reachability == ReachabilityState::Reachable
        && receipt.recovery != RecoveryAction::Login
    {
        receipt.recovery = RecoveryAction::RefreshCatalog;
    }
    progress.push(ConnectionProgress {
        semantic_code: "connection.test.completed.v1",
        stage: ConnectionProgressStage::Complete,
    });
    receipt.progress = progress;
    Ok((receipt, models))
}

fn write_connection_test<W: Write>(
    output: &mut W,
    receipt: &ConnectionTestReceipt,
    json: bool,
) -> Result<()> {
    if json {
        return write_json(output, receipt);
    }
    writeln!(output, "connection: {}", receipt.connection)?;
    writeln!(output, "execution: {}", wire_name(&receipt.execution))?;
    writeln!(output, "reachability: {}", wire_name(&receipt.reachability))?;
    writeln!(output, "credential: {}", wire_name(&receipt.credential))?;
    writeln!(output, "account: {}", wire_name(&receipt.account))?;
    writeln!(
        output,
        "discovered models: {}",
        receipt.discovered_model_count
    )?;
    writeln!(output, "usable: {}", receipt.usable)?;
    writeln!(output, "recovery: {}", wire_name(&receipt.recovery))?;
    if let Some(failure) = &receipt.failure {
        writeln!(output, "failure: {failure}")?;
    }
    writeln!(output, "receipt: {}", receipt.semantic_code)?;
    Ok(())
}

fn bounded_failure(mut message: String) -> String {
    const MAX_CHARS: usize = 1_024;
    if message.chars().count() <= MAX_CHARS {
        return message;
    }
    message = message.chars().take(MAX_CHARS).collect();
    message.push('…');
    message
}

fn write_account_status<W: Write>(output: &mut W, status: &AccountStatus) -> Result<()> {
    match status {
        AccountStatus::LoggedOut => writeln!(output, "account: logged out")?,
        AccountStatus::ApiKey => writeln!(output, "account: Codex-managed API key")?,
        AccountStatus::ChatGpt { plan } => writeln!(output, "account: ChatGPT ({plan})")?,
        AccountStatus::Other { kind } => writeln!(output, "account: {kind}")?,
    }
    Ok(())
}

fn managed_account_state(status: &AccountStatus) -> ManagedAccountState {
    match status {
        AccountStatus::LoggedOut => ManagedAccountState::LoggedOut,
        AccountStatus::ApiKey => ManagedAccountState::LoggedIn {
            kind: "Codex-managed API key".to_owned(),
        },
        AccountStatus::ChatGpt { plan } => ManagedAccountState::LoggedIn {
            kind: format!("ChatGPT ({plan})"),
        },
        AccountStatus::Other { kind } => ManagedAccountState::LoggedIn { kind: kind.clone() },
    }
}

fn action_receipt(
    id: &str,
    semantic_code: &'static str,
    effect: ConnectionEffect,
) -> ConnectionReceipt {
    ConnectionReceipt {
        version: crate::connection_management::CONNECTION_STATE_VERSION,
        semantic_code,
        connection: id.to_owned(),
        effect,
        backup: None,
        retained_authority: Vec::new(),
        warnings: Vec::new(),
    }
}

fn write_json<W: Write, T: Serialize>(output: &mut W, value: &T) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    writeln!(output)?;
    Ok(())
}

fn write_connection_row<W: Write>(
    output: &mut W,
    connection: &ConnectionSummaryView,
) -> Result<()> {
    let marker = if connection.selected_for_new_conversations {
        "*"
    } else {
        " "
    };
    writeln!(
        output,
        "{marker} {}\t{}\t{}\t{} model(s)\trecovery={}",
        connection.id,
        connection.provider.as_str(),
        wire_name(&connection.health),
        connection.models.len(),
        wire_name(&connection.recovery)
    )?;
    Ok(())
}

fn write_connection_detail<W: Write>(
    output: &mut W,
    connection: &ConnectionSummaryView,
) -> Result<()> {
    writeln!(output, "connection: {}", connection.id)?;
    writeln!(output, "provider: {}", connection.provider.as_str())?;
    writeln!(output, "execution: {}", wire_name(&connection.execution))?;
    writeln!(output, "health: {}", wire_name(&connection.health))?;
    writeln!(
        output,
        "selected for new conversations: {}",
        connection.selected_for_new_conversations
    )?;
    writeln!(
        output,
        "selected model: {} ({})",
        connection
            .selected_model
            .as_deref()
            .unwrap_or("not selected"),
        wire_name(&connection.facets.selected_model)
    )?;
    writeln!(
        output,
        "credential: {} ({})",
        wire_name(&connection.credential_source),
        wire_name(&connection.facets.credential)
    )?;
    writeln!(output, "account: {}", wire_name(&connection.facets.account))?;
    writeln!(
        output,
        "reachability: {}",
        wire_name(&connection.facets.reachability)
    )?;
    writeln!(
        output,
        "catalog: {} ({} cached, {} available)",
        wire_name(&connection.facets.catalog.freshness),
        connection.facets.catalog.cached_model_count,
        connection.facets.catalog.available_model_count
    )?;
    if let Some(fetched_at) = connection.facets.catalog.fetched_at_unix_seconds {
        writeln!(output, "catalog fetched at: {fetched_at} unix seconds")?;
    }
    writeln!(
        output,
        "profiles: {}",
        if connection.profile_references.is_empty() {
            "none".to_owned()
        } else {
            connection.profile_references.join(", ")
        }
    )?;
    writeln!(output, "recovery: {}", wire_name(&connection.recovery))?;
    Ok(())
}

fn wire_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .map(|value| match value {
            serde_json::Value::String(value) => value,
            serde_json::Value::Object(mut value) => value
                .remove("state")
                .or_else(|| value.remove("source"))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "structured".to_owned()),
            _ => "unknown".to_owned(),
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(crate) async fn refresh_connection(
    paths: &XanaPaths,
    id: &str,
) -> Result<(ConnectionReceipt, usize)> {
    let manager = model_manager(paths)?;
    let connection = manager.connection(id)?;
    let models = if connection.kind == ProviderKind::Codex {
        let mut server = CodexAppServer::spawn(&codex_launch(connection)).await?;
        let models = server.models().await?;
        manager.write_managed_cache(id, &models)?;
        server.shutdown().await?;
        models
    } else {
        manager.refresh_native(id).await?
    };
    let count = models.len();
    Ok((
        action_receipt(
            id,
            "connection.catalog_refresh.completed.v1",
            ConnectionEffect::CatalogRefreshed,
        ),
        count,
    ))
}

async fn refresh_models<W: Write>(
    paths: &XanaPaths,
    id: &str,
    output: &mut W,
    json: bool,
) -> Result<()> {
    let (receipt, model_count) = refresh_connection(paths, id).await?;
    if json {
        write_json(output, &receipt)?;
    } else {
        writeln!(output, "cached {model_count} model(s) for {id}")?;
    }
    Ok(())
}

pub(super) async fn run_model_command<W: Write>(
    command: Option<ModelCommand>,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    match command {
        Some(ModelCommand::Use {
            selection,
            effort,
            summary,
        }) => {
            let (connection, model) = selection
                .split_once('/')
                .context("model selection must be CONNECTION/MODEL")?;
            let effort = effort.and_then(|value| (value != "auto").then_some(value));
            let summary = summary
                .map(|value| value.parse::<crate::model_catalog::ReasoningSummary>())
                .transpose()?;
            let (selected, receipt) =
                ConnectionManagement::open(paths)?.select(connection, model, effort, summary)?;
            writeln!(
                output,
                "selected {}/{} for the next conversation",
                selected.connection, selected.model
            )?;
            if selected.reasoning_effort.is_some() || selected.reasoning_summary.is_some() {
                writeln!(
                    output,
                    "reasoning effort: {}; summary: {}",
                    selected
                        .reasoning_effort
                        .as_deref()
                        .unwrap_or("model default"),
                    selected
                        .reasoning_summary
                        .map_or_else(|| "provider default".into(), |value| value.to_string())
                )?;
            }
            writeln!(output, "receipt: {}", receipt.semantic_code)?;
            Ok(())
        }
        Some(ModelCommand::Refresh { connection }) => {
            refresh_models(paths, &connection, output, false).await
        }
        Some(ModelCommand::List { connection }) => {
            list_models(paths, connection.as_deref(), output)
        }
        None => {
            list_models(paths, None, output)?;
            writeln!(output, "select with: xana model use CONNECTION/MODEL")?;
            Ok(())
        }
    }
}

fn list_models<W: Write>(paths: &XanaPaths, only: Option<&str>, output: &mut W) -> Result<()> {
    let manager = model_manager(paths)?;
    let selected = manager.selected()?;
    let configured = manager.configured_default()?;
    writeln!(
        output,
        "effective selection: {}/{} (saved override when present; otherwise the configured default)",
        selected.connection, selected.model
    )?;
    writeln!(
        output,
        "configured default: {}/{} (default profile in config.toml)",
        configured.connection, configured.model
    )?;
    for summary in manager.summaries() {
        if only.is_some_and(|only| only != summary.id) {
            continue;
        }
        let execution = match summary.execution {
            ExecutionKind::Native => "native",
            ExecutionKind::Managed => "managed",
        };
        writeln!(
            output,
            "{} ({execution}, {})",
            summary.id,
            summary.kind.as_str()
        )?;
        for model in summary.models {
            let marker = if summary.id == selected.connection && model.id == selected.model {
                "*"
            } else {
                " "
            };
            let modalities = model
                .input_modalities
                .into_iter()
                .collect::<Vec<_>>()
                .join(",");
            let efforts = model
                .reasoning_efforts
                .iter()
                .map(|effort| effort.id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let reasoning = if efforts.is_empty() {
                match model.reasoning {
                    Some(true) => "yes (provider-controlled)".to_owned(),
                    Some(false) => "no".to_owned(),
                    None => "unknown".to_owned(),
                }
            } else {
                format!(
                    "{} (default {})",
                    efforts,
                    model
                        .default_reasoning_effort
                        .as_deref()
                        .unwrap_or("unspecified")
                )
            };
            let context = model
                .context_tokens
                .map_or_else(|| "unknown".to_owned(), |tokens| tokens.to_string());
            writeln!(
                output,
                "  {marker} {}\tinput={}\ttools={}\treasoning={}\tcontext={}\t{}",
                model.id,
                modalities,
                match model.tools {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                },
                reasoning,
                context,
                model.display_name
            )?;
        }
    }
    Ok(())
}
