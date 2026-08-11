//! Connection, credential, catalog, and model command orchestration.

use crate::{
    cli::{AuthCommand, ConnectionCommand, ModelCommand},
    config::{CredentialReference, NewConnection, ProviderKind, XanaConfig},
    credential::{SecretString, delete_secret, store_secret},
    managed::codex::{AccountStatus, CodexAppServer, CodexLaunchConfig, LoginMode},
    model_catalog::{ExecutionKind, ModelManager},
    paths::XanaPaths,
};
use anyhow::{Context, Result};
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
    run_connection_command(translated, paths, output).await
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
            XanaConfig::add_connection(
                paths.config_file(),
                NewConnection {
                    id: id.clone(),
                    kind,
                    base_url,
                    credential,
                    model: model.clone(),
                    codex_program,
                    codex_home,
                },
            )?;
            writeln!(output, "connection added: {id} ({})", kind.as_str())?;
            writeln!(output, "model declared: {id}/{model}")?;
            if matches!(
                kind,
                ProviderKind::OpenAi | ProviderKind::OpenRouter | ProviderKind::Anthropic
            ) {
                writeln!(output, "next: xana connection set-key {id}")?;
            } else if kind == ProviderKind::Codex {
                writeln!(output, "next: xana connection status {id}")?;
            }
            Ok(())
        }
        ConnectionCommand::List => {
            let manager = model_manager(paths)?;
            let selected = manager.selected()?;
            for summary in manager.summaries() {
                let marker = if summary.id == selected.connection {
                    "*"
                } else {
                    " "
                };
                writeln!(
                    output,
                    "{marker} {}\t{}\t{}\t{} model(s)",
                    summary.id,
                    summary.kind.as_str(),
                    summary.credential,
                    summary.models.len()
                )?;
            }
            Ok(())
        }
        ConnectionCommand::Status { id } => {
            let manager = model_manager(paths)?;
            let connection = manager.connection(&id)?;
            if connection.kind == ProviderKind::Codex {
                let mut server = CodexAppServer::spawn(&codex_launch(connection)).await?;
                writeln!(output, "connection: {id}")?;
                writeln!(output, "kind: managed codex app-server")?;
                writeln!(output, "runtime: {}", server.version)?;
                writeln!(output, "codex home: {}", server.codex_home.display())?;
                let account = server.account_status().await?;
                write_account_status(output, &account)?;
                if matches!(account, AccountStatus::ChatGpt { .. }) {
                    let limits = server.rate_limits().await?;
                    for (name, pointer) in [
                        ("primary", "/rateLimits/primary/usedPercent"),
                        ("secondary", "/rateLimits/secondary/usedPercent"),
                    ] {
                        if let Some(percent) =
                            limits.pointer(pointer).and_then(serde_json::Value::as_f64)
                        {
                            writeln!(output, "{name} usage: {percent:.0}%")?;
                        }
                    }
                }
                server.shutdown().await?;
            } else {
                let summary = manager
                    .summaries()
                    .into_iter()
                    .find(|summary| summary.id == id)
                    .expect("connection was resolved");
                writeln!(output, "connection: {id}")?;
                writeln!(output, "kind: native {}", summary.kind.as_str())?;
                writeln!(output, "credential: {}", summary.credential)?;
                writeln!(output, "cached/configured models: {}", summary.models.len())?;
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
            store_secret(credential_id, &secret)?;
            writeln!(
                output,
                "credential stored in the operating-system credential store for {id}"
            )?;
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
            writeln!(
                output,
                "credential {} for {id}",
                if deleted {
                    "deleted"
                } else {
                    "was already absent"
                }
            )?;
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
                writeln!(output, "Codex is already logged in.")?;
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
            write_account_status(output, &status)?;
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
            if matches!(server.account_status().await?, AccountStatus::LoggedOut) {
                writeln!(output, "Codex was already logged out.")?;
            } else {
                server.logout().await?;
                writeln!(output, "Codex account logged out for this CODEX_HOME.")?;
            }
            server.shutdown().await?;
            Ok(())
        }
        ConnectionCommand::Refresh { id } => refresh_models(paths, &id, output).await,
        ConnectionCommand::Remove { id, yes } => {
            if !yes {
                anyhow::bail!("connection removal requires --yes")
            }
            let selected = model_manager(paths)?.selected()?;
            if selected.connection == id {
                anyhow::bail!(
                    "connection {id:?} is selected; select another model before removing it"
                )
            }
            XanaConfig::remove_connection(paths.config_file(), &id)?;
            writeln!(output, "connection removed: {id}")?;
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

async fn refresh_models<W: Write>(paths: &XanaPaths, id: &str, output: &mut W) -> Result<()> {
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
    writeln!(output, "cached {} model(s) for {id}", models.len())?;
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
            let manager = model_manager(paths)?;
            let effort = effort.and_then(|value| (value != "auto").then_some(value));
            let summary = summary
                .map(|value| value.parse::<crate::model_catalog::ReasoningSummary>())
                .transpose()?;
            let selected = manager.select_with_options(connection, model, effort, summary)?;
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
            Ok(())
        }
        Some(ModelCommand::Refresh { connection }) => {
            refresh_models(paths, &connection, output).await
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
                "-".to_owned()
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
            writeln!(
                output,
                "  {marker} {}\t{}\t{}\t{}",
                model.id, modalities, reasoning, model.display_name
            )?;
        }
    }
    Ok(())
}
