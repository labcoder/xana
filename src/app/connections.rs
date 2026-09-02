//! Connection, credential, catalog, and model command orchestration.

use crate::{
    cli::{AuthCommand, ConnectionCommand, ModelCommand},
    config::{CredentialReference, ProviderKind, XanaConfig},
    connection_management::{
        ConnectionDraft, ConnectionEffect, ConnectionManagement, ConnectionReceipt,
        ConnectionSummaryView, ManagedAccountState,
    },
    credential::{SecretString, delete_secret, store_secret},
    managed::codex::{AccountStatus, CodexAppServer, CodexLaunchConfig, LoginMode},
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

pub(super) fn model_manager(paths: &XanaPaths) -> Result<ModelManager> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load connection registry")?;
    Ok(ModelManager::new(
        registry,
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    ))
}

pub(super) fn codex_launch(connection: &crate::config::ConnectionConfig) -> CodexLaunchConfig {
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
            model,
            codex_program,
            codex_home,
        } => {
            let kind = kind.into();
            let credential = match (env, credential_id) {
                (Some(variable), None) => Some(CredentialReference::Environment { variable }),
                (None, Some(id)) => Some(CredentialReference::Stored { id }),
                (None, None)
                    if matches!(
                        kind,
                        ProviderKind::OpenAi | ProviderKind::OpenRouter | ProviderKind::Anthropic
                    ) =>
                {
                    Some(CredentialReference::Stored { id: id.clone() })
                }
                (None, None) => None,
                (Some(_), Some(_)) => unreachable!("clap rejects conflicting flags"),
            };
            let receipt = ConnectionManagement::open(paths)?.add(ConnectionDraft {
                id: id.clone(),
                kind,
                base_url,
                credential,
                model: model.clone(),
                codex_program,
                codex_home,
            })?;
            if json {
                write_json(output, &receipt)?;
            } else {
                writeln!(output, "connection added: {id} ({})", kind.as_str())?;
                writeln!(output, "model declared: {id}/{model}")?;
                writeln!(
                    output,
                    "backup: {}",
                    receipt
                        .backup
                        .as_ref()
                        .expect("add receipt backup")
                        .display()
                )?;
                if matches!(
                    kind,
                    ProviderKind::OpenAi | ProviderKind::OpenRouter | ProviderKind::Anthropic
                ) {
                    writeln!(output, "next: xana connection set-key {id}")?;
                } else {
                    writeln!(output, "next: xana connection status {id}")?;
                }
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
        ConnectionCommand::DeleteKey { id } => {
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
            let status = server.wait_for_login(&instructions.login_id).await?;
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

async fn refresh_models<W: Write>(
    paths: &XanaPaths,
    id: &str,
    output: &mut W,
    json: bool,
) -> Result<()> {
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
    if json {
        write_json(
            output,
            &action_receipt(
                id,
                "connection.catalog_refresh.completed.v1",
                ConnectionEffect::CatalogRefreshed,
            ),
        )?;
    } else {
        writeln!(output, "cached {} model(s) for {id}", models.len())?;
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
