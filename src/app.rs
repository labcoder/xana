//! Application-edge command orchestration.
//!
//! This module may inspect terminal capabilities, process environment, and the
//! current directory. It validates those inputs and passes owned configuration
//! inward; it does not put frontend or process-global concerns into `Agent`.

use crate::{
    agent::Agent,
    artifact::ArtifactStore,
    cli::{
        self, AuthCommand, Cli, Command, ConfigCommand, ConnectionCommand, ModelCommand,
        OperationCommand, RouteCommand, SessionCommand,
    },
    config::{CredentialReference, NewConnection, ProviderKind, XanaConfig},
    context::{ContextBudget, ContextPlanReport},
    credential::{CredentialResolver, SecretString, delete_secret, store_secret},
    init::{self, InitPlan, WriteOutcome},
    managed::codex::{AccountStatus, CodexAppServer, CodexLaunchConfig, LoginMode},
    managed_terminal::{ManagedChatConfig, run_codex_chat},
    model::{ExecutionKind, ModelManager},
    operation::{RecoveryAction, execute_recovery, plan_recovery},
    orchestration::{
        ChildSupervisor, ExecutionOwner, NativeChildFactory, OrchestrationBudget, ParentExecution,
        ResolvedAgentConfig, RouteResolver,
    },
    paths::XanaPaths,
    permission::{PermissionBroker, PermissionPolicy},
    presentation::{self, BannerMode},
    prompt::{ProductDocumentationHint, PromptAssembler, PromptEnvironment, PromptSurface},
    provider::{anthropic::AnthropicClient, openai_compat::OpenAiCompatClient},
    reset::ResetPlan,
    runtime::{RuntimeCommand, RuntimeHandle},
    session::{DurableSession, RestoredOperation},
    shell::Shell,
    terminal::{self, ChatHeader},
    tool::ToolRegistry,
    vision::MediaResolver,
};
use anyhow::{Context, Result};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::sync::Arc;

const PROMPT_TOTAL_TOKENS: usize = 32_768;
const PROMPT_CONVERSATION_RESERVE_TOKENS: usize = 8_192;
const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

pub(crate) async fn run(cli: Cli, paths: XanaPaths) -> Result<()> {
    let no_banner = cli.no_banner;

    if cli.resume.is_some() && cli.command.is_some() {
        anyhow::bail!("--resume starts chat and cannot be combined with a subcommand");
    }

    match cli.command {
        None => {
            let mode = banner_mode(
                true,
                io::stdin().is_terminal(),
                io::stdout().is_terminal(),
                no_banner,
            );
            run_default(&paths, mode, cli.resume).await
        }
        Some(Command::Init(args)) => run_init_command(&args, &paths, no_banner),
        Some(Command::Reset(args)) => run_reset_command(&args, &paths),
        Some(Command::Config(args)) => {
            let stdout = io::stdout();
            run_config_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Session(args)) => {
            let stdout = io::stdout();
            run_session_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Operation(args)) => {
            let stdout = io::stdout();
            run_operation_command(args.command, &paths, &mut stdout.lock()).await
        }
        Some(Command::Connection(args)) => {
            let stdout = io::stdout();
            run_connection_command(args.command, &paths, &mut stdout.lock()).await
        }
        Some(Command::Model(args)) => {
            let stdout = io::stdout();
            run_model_command(args.command, &paths, &mut stdout.lock()).await
        }
        Some(Command::Route(args)) => {
            let stdout = io::stdout();
            run_route_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Auth(args)) => {
            let stdout = io::stdout();
            run_auth_command(args.command, &paths, &mut stdout.lock()).await
        }
    }
}

fn run_route_command<W: Write>(
    command: RouteCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load route registry")?;
    let manager = ModelManager::new(
        registry.clone(),
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    );
    let resolver = RouteResolver::new(&registry, &manager);
    match command {
        RouteCommand::List => {
            if registry.routes.is_empty() {
                writeln!(output, "no child task routes configured")?;
                return Ok(());
            }
            for route in registry.routes.keys() {
                let marker = if registry.default_child_route.as_deref() == Some(route.as_str()) {
                    "*"
                } else {
                    " "
                };
                match resolver.resolve(Some(route)) {
                    Ok(resolved) => writeln!(
                        output,
                        "{marker} {}\t{}\t{}/{}\tprofile {}",
                        route,
                        resolved.owner.as_str(),
                        resolved.connection,
                        resolved.model.id,
                        resolved.profile
                    )?,
                    Err(error) => writeln!(output, "{marker} {route}\tunavailable\t{error}")?,
                }
            }
            Ok(())
        }
        RouteCommand::Check { route } => {
            let resolved = resolver.resolve(Some(&route))?;
            write_resolved_route(output, &resolved)
        }
    }
}

fn write_resolved_route<W: Write>(output: &mut W, route: &ResolvedAgentConfig) -> Result<()> {
    writeln!(output, "route: {}", route.route)?;
    writeln!(output, "profile: {}", route.profile)?;
    writeln!(output, "execution: {}", route.owner.as_str())?;
    writeln!(output, "connection: {}", route.connection)?;
    writeln!(output, "kind: {}", route.provider_kind.as_str())?;
    writeln!(output, "model: {}", route.model.id)?;
    if route.owner == ExecutionOwner::Codex {
        writeln!(
            output,
            "reasoning effort: {}",
            route.reasoning_effort.as_deref().unwrap_or("model default")
        )?;
        writeln!(
            output,
            "reasoning summary: {}",
            route
                .reasoning_summary
                .map_or_else(|| "provider default".to_owned(), |value| value.to_string())
        )?;
    }
    let capabilities = route
        .capabilities
        .capabilities()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    writeln!(
        output,
        "capabilities: {}",
        if capabilities.is_empty() {
            "none".to_owned()
        } else {
            capabilities.join(",")
        }
    )?;
    writeln!(
        output,
        "permission ceiling: {}",
        route.permission_mode.as_str()
    )?;
    writeln!(output, "maximum tool rounds: {}", route.max_tool_rounds)?;
    writeln!(
        output,
        "orchestration: fan-out {}, descendants {}, concurrency {}, deadline {}s, context {} tokens, report {} bytes, artifacts {} bytes",
        route.orchestration.max_fan_out,
        route.orchestration.max_descendants,
        route.orchestration.max_concurrency,
        route.orchestration.deadline_seconds,
        route.orchestration.max_context_tokens,
        route.orchestration.max_report_bytes,
        route.orchestration.max_artifact_bytes
    )?;
    Ok(())
}

fn run_reset_with_io<R: BufRead, W: Write>(
    args: &cli::ResetArgs,
    paths: &XanaPaths,
    is_terminal: bool,
    input: &mut R,
    output: &mut W,
) -> Result<()> {
    let plan = ResetPlan::inspect(paths).context("could not inspect Xana setup state")?;
    if plan.targets().is_empty() {
        writeln!(output, "Xana setup state is already clear.")?;
        writeln!(output, "Next: xana init")?;
        writeln!(output, "Source checkout: cargo run -- init")?;
        return Ok(());
    }

    if !args.yes {
        writeln!(output, "Reset will remove:")?;
        for target in plan.targets() {
            writeln!(output, "  {}: {}", target.label, target.path.display())?;
        }
        writeln!(output, "Reset preserves:")?;
        writeln!(output, "  native sessions and artifacts")?;
        writeln!(output, "  stored API keys")?;
        writeln!(
            output,
            "  Codex authentication and Codex-owned conversations"
        )?;
        if !is_terminal {
            anyhow::bail!("reset requires --yes when stdin is not a terminal")
        }
        write!(output, "Reset Xana setup state? [y/N]: ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0
            || !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        {
            writeln!(output, "No changes made.")?;
            return Ok(());
        }
    }

    let removed = plan.execute().context("could not reset Xana setup state")?;
    writeln!(output, "Xana setup state reset.")?;
    for target in removed {
        writeln!(
            output,
            "  removed {}: {}",
            target.label,
            target.path.display()
        )?;
    }
    writeln!(output)?;
    writeln!(
        output,
        "Preserved native sessions, artifacts, stored API keys, and external Codex state."
    )?;
    writeln!(output, "Next: xana init")?;
    writeln!(output, "Source checkout: cargo run -- init")?;
    Ok(())
}

fn run_reset_command(args: &cli::ResetArgs, paths: &XanaPaths) -> Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    run_reset_with_io(args, paths, stdin.is_terminal(), &mut input, &mut output)
}

async fn run_auth_command<W: Write>(
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

fn model_manager(paths: &XanaPaths) -> Result<ModelManager> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load connection registry")?;
    Ok(ModelManager::new(
        registry,
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    ))
}

fn codex_launch(connection: &crate::config::ConnectionConfig) -> CodexLaunchConfig {
    CodexLaunchConfig {
        program: connection
            .codex_program
            .clone()
            .unwrap_or_else(|| "codex".into()),
        home: connection.codex_home.clone(),
    }
}

async fn run_connection_command<W: Write>(
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

async fn run_model_command<W: Write>(
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
                .map(|value| value.parse::<crate::model::ReasoningSummary>())
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

async fn run_operation_command<W: Write>(
    command: OperationCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let config = load_config(paths)?;
    let shell =
        Shell::resolve(config.shell.clone()).context("could not resolve configured shell")?;
    let tools = ToolRegistry::builtins(shell).context("could not build tool registry")?;

    match command {
        OperationCommand::Plan {
            session,
            operation_id,
        } => {
            let (_, restored) = DurableSession::inspect_restored(paths.data_dir(), session)?;
            let operation = restored
                .operation_details
                .get(&operation_id)
                .with_context(|| format!("operation {operation_id} is not in session {session}"))?;
            let actions = plan_recovery(operation, &tools)?;
            write_recovery_plan(output, session, operation, &actions)
        }
        OperationCommand::Resume {
            session,
            operation_id,
        } => {
            execute_recovery_command(
                RuntimeCommand::ResumeOperation {
                    session_id: session,
                    operation_id,
                },
                paths,
                &config,
                &tools,
                output,
            )
            .await
        }
    }
}

async fn execute_recovery_command<W: Write>(
    command: RuntimeCommand,
    paths: &XanaPaths,
    config: &XanaConfig,
    tools: &ToolRegistry,
    output: &mut W,
) -> Result<()> {
    match command {
        RuntimeCommand::ResumeOperation {
            session_id,
            operation_id,
        } => {
            let (mut durable, _) = DurableSession::resume(paths.data_dir(), session_id)?;
            let operation = durable.restored_operation(operation_id).with_context(|| {
                format!("operation {operation_id} is not in session {session_id}")
            })?;
            let policy = PermissionPolicy::new(
                config.permission_mode.into(),
                config.permission_rules.clone(),
                durable.workspace_root(),
            )
            .context("could not resolve current recovery permission policy")?;
            let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
            let (permissions, broker) =
                PermissionBroker::spawn_for_durable_runtime(policy, true, events);
            let actions = execute_recovery(
                &mut durable,
                operation_id,
                tools,
                &permissions,
                &mut event_receiver,
                |request| terminal::prompt_permission_decision(request).map_err(Into::into),
            )
            .await?;
            permissions.shutdown();
            let _ = broker.await;
            write_recovery_plan(output, session_id, &operation, &actions)
        }
        RuntimeCommand::SubmitTurn { .. }
        | RuntimeCommand::SubmitTurnWithImages { .. }
        | RuntimeCommand::ClearConversation
        | RuntimeCommand::DecidePermission { .. }
        | RuntimeCommand::DecideChildPermission { .. }
        | RuntimeCommand::ListChildren
        | RuntimeCommand::InspectChild { .. }
        | RuntimeCommand::CancelChild { .. }
        | RuntimeCommand::Shutdown => {
            anyhow::bail!("the explicit recovery controller accepts only ResumeOperation")
        }
    }
}

fn write_recovery_plan<W: Write>(
    output: &mut W,
    session_id: crate::identity::SessionId,
    operation: &RestoredOperation,
    actions: &[RecoveryAction],
) -> Result<()> {
    writeln!(output, "session: {session_id}")?;
    writeln!(output, "operation: {}", operation.operation_id)?;
    writeln!(output, "thread: {}", operation.thread_id)?;
    writeln!(output, "input entry: {}", operation.input_entry_id)?;
    if operation.step_order.is_empty() {
        writeln!(output, "steps: none committed")?;
    } else {
        let steps = operation
            .step_order
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "steps: {steps}")?;
    }
    for action in actions {
        match action {
            RecoveryAction::AlreadyCompleted { result_id } => {
                writeln!(output, "already completed: result {result_id}")?
            }
            RecoveryAction::ReplayExactInvocation { invocation_id } => {
                writeln!(output, "replay after current permission: {invocation_id}")?
            }
            RecoveryAction::RecordInterruption {
                invocation_id,
                result_id,
                reason,
            } => writeln!(
                output,
                "record interruption: invocation {invocation_id}, result {result_id}, reason {reason:?}"
            )?,
            RecoveryAction::ContinueWithNextInvocation => {
                writeln!(output, "continue in original call order")?
            }
            RecoveryAction::FinishOperation => writeln!(output, "finish operation")?,
        }
    }
    Ok(())
}

fn load_config(paths: &XanaPaths) -> Result<XanaConfig> {
    XanaConfig::load_from(paths.config_file()).with_context(|| {
        format!(
            "failed to load config from {}",
            paths.config_file().display()
        )
    })
}

async fn run_default(
    paths: &XanaPaths,
    banner_mode: BannerMode,
    resume: Option<crate::identity::SessionId>,
) -> Result<()> {
    {
        let mut output = anstream::stdout().lock();
        presentation::write_banner(&mut output, banner_mode)
            .context("could not write Xana banner")?;
        writeln!(
            output,
            "loading Xana config from {}",
            paths.config_file().display()
        )?;
    }

    let config = match XanaConfig::load_from(paths.config_file()) {
        Ok(config) => config,
        Err(error) if error.is_missing_config() => {
            anyhow::bail!(
                "Xana is not initialized at {}\nrun `xana init` to create it",
                paths.config_file().display()
            );
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to load config from {}",
                    paths.config_file().display()
                )
            });
        }
    };

    let manager = model_manager(paths)?;
    let child_registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load child route registry")?;
    let selected = manager.selected()?;
    let selected_connection = manager.connection(&selected.connection)?.clone();

    let XanaConfig {
        permission_mode,
        permission_rules,
        shell,
        max_tool_rounds,
        ..
    } = config;
    let provider_name = selected.connection;
    let provider_kind = selected_connection.kind;
    let model = selected.model;
    let shell = Shell::resolve(shell).context("could not resolve configured shell")?;
    let configured_shell = shell.prompt_description();
    let workspace_root = std::env::current_dir()
        .context("could not resolve Xana workspace root")?
        .canonicalize()
        .context("could not canonicalize Xana workspace root")?;
    let artifact_store = ArtifactStore::new(paths.data_dir().join("artifacts"));

    if provider_kind == ProviderKind::Codex {
        if resume.is_some() {
            anyhow::bail!(
                "Xana durable --resume applies to native conversations; Codex owns managed thread resume"
            )
        }
        let server = CodexAppServer::spawn(&codex_launch(&selected_connection)).await?;
        return run_codex_chat(
            server,
            manager,
            ManagedChatConfig {
                connection: provider_name,
                model,
                workspace: workspace_root,
                data_root: paths.data_dir().to_owned(),
                artifact_store,
                owner: crate::identity::PrincipalId::new(),
                developer_instructions: crate::prompt::xana_identity(),
                identity_version: crate::prompt::XANA_IDENTITY_VERSION,
            },
        )
        .await;
    }

    let base_url = selected_connection
        .base_url
        .clone()
        .context("selected native connection has no endpoint")?;

    let media = MediaResolver::new(artifact_store.clone(), crate::artifact::MAX_ARTIFACT_BYTES);
    let credentials = CredentialResolver::default();
    let (provider, endpoint): (Box<dyn crate::provider::ConversationalProvider>, String) =
        match provider_kind {
            ProviderKind::OpenAiCompat | ProviderKind::Ollama => {
                let client = match selected_connection.credential.as_ref() {
                    Some(reference) => {
                        let secret = credentials.resolve(reference)?;
                        OpenAiCompatClient::with_bearer_and_attribution(
                            base_url,
                            model.clone(),
                            secret,
                            None,
                            None,
                        )
                    }
                    None => OpenAiCompatClient::new(base_url, model.clone()),
                }
                .with_media_resolver(media);
                let endpoint = client.endpoint().to_owned();
                (Box::new(client), endpoint)
            }
            ProviderKind::OpenAi | ProviderKind::OpenRouter => {
                let reference = selected_connection
                    .credential
                    .as_ref()
                    .context("selected API connection has no credential reference")?;
                let secret = credentials.resolve(reference)?;
                let client = OpenAiCompatClient::with_bearer_and_attribution(
                    base_url,
                    model.clone(),
                    secret,
                    None,
                    (provider_kind == ProviderKind::OpenRouter).then(|| "Xana".to_owned()),
                )
                .with_media_resolver(media);
                let endpoint = client.endpoint().to_owned();
                (Box::new(client), endpoint)
            }
            ProviderKind::Anthropic => {
                let reference = selected_connection
                    .credential
                    .as_ref()
                    .context("selected Anthropic connection has no credential reference")?;
                let secret = credentials.resolve(reference)?;
                let client = AnthropicClient::new(base_url, secret, model.clone())
                    .with_media_resolver(media);
                let endpoint = client.endpoint().to_owned();
                (Box::new(client), endpoint)
            }
            ProviderKind::Codex => unreachable!("managed Codex was composed above"),
        };
    let mut tools =
        ToolRegistry::builtins(shell.clone()).context("could not build tool registry")?;
    let (session, permission_policy, resumed, repair_truncate_to, unfinished, restored_children) =
        match resume {
            Some(session_id) => {
                let (session, summary) = DurableSession::resume(paths.data_dir(), session_id)?;
                if session.workspace_root() != workspace_root {
                    writeln!(
                        anstream::stdout().lock(),
                        "resuming session workspace {} (current directory is {})",
                        session.workspace_root().display(),
                        workspace_root.display()
                    )?;
                }
                let unfinished = summary.unfinished.clone();
                let permission_policy = PermissionPolicy::new(
                    permission_mode.into(),
                    permission_rules.clone(),
                    session.workspace_root(),
                )
                .context("could not resolve permission policy for the session workspace")?;
                (
                    session,
                    permission_policy,
                    true,
                    summary.repair_truncate_to,
                    unfinished,
                    summary.children,
                )
            }
            None => {
                let permission_policy = PermissionPolicy::new(
                    permission_mode.into(),
                    permission_rules.clone(),
                    &workspace_root,
                )
                .context("could not resolve permission policy for the launch workspace")?;
                (
                    DurableSession::create(paths.data_dir(), workspace_root.clone())?,
                    permission_policy,
                    false,
                    None,
                    Vec::new(),
                    Vec::new(),
                )
            }
        };
    let workspace_root = session.workspace_root().to_owned();
    let artifact_owner = session.artifact_owner();
    let restored_plans = session.started_orchestration_plans();
    let child_supervisor = if child_registry.routes.is_empty() {
        None
    } else {
        let root_profile = child_registry
            .profiles
            .get(&child_registry.default_profile)
            .context("validated configuration lost its default profile")?;
        let budget = OrchestrationBudget::new(
            root_profile.orchestration.clone(),
            root_profile.max_tool_rounds,
        );
        let factory = NativeChildFactory::new(
            child_registry,
            model_manager(paths)?,
            shell,
            workspace_root.clone(),
            artifact_store.clone(),
            permission_rules,
        );
        let (handle, supervisor) = ChildSupervisor::with_restored(
            ParentExecution {
                agent_id: session.agent_id(),
                thread_id: session.thread_id(),
            },
            Arc::new(factory),
            restored_children.clone(),
            restored_plans,
            budget,
            artifact_store.clone(),
            artifact_owner,
        );
        tools
            .enable_child_delegation(handle.clone())
            .context("could not register child delegation tool")?;
        Some((handle, supervisor))
    };
    let environment = PromptEnvironment {
        operating_system: std::env::consts::OS.to_owned(),
        working_directory: workspace_root.clone(),
        configured_shell,
        surface: PromptSurface::Cli,
    };
    let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
    let prompt_assembler = PromptAssembler::new(
        definitions,
        environment,
        Some(ProductDocumentationHint {
            capability: "xana_docs".to_owned(),
            references: crate::self_docs::default_catalog()
                .list(None)
                .into_iter()
                .map(|entry| entry.id.to_owned())
                .collect(),
        }),
        ContextBudget {
            total_tokens: PROMPT_TOTAL_TOKENS,
            conversation_reserve_tokens: PROMPT_CONVERSATION_RESERVE_TOKENS,
        },
    );
    let prompt = prompt_assembler
        .assemble(&[])
        .context("could not assemble Xana base prompt")?;
    let context_report = ContextPlanReport::render(&prompt.context_plan)
        .as_str()
        .to_owned();
    let agent = Agent::new(
        provider,
        tools,
        workspace_root.clone(),
        prompt,
        max_tool_rounds,
    );
    let session_id = session.session_id();
    let session_path = session.path().to_owned();
    let runtime = match child_supervisor {
        Some((handle, supervisor)) => RuntimeHandle::spawn_persistent_with_supervisor(
            agent,
            permission_policy,
            true,
            session,
            prompt_assembler,
            handle,
            supervisor,
        )?,
        None => RuntimeHandle::spawn_persistent(
            agent,
            permission_policy,
            true,
            session,
            prompt_assembler,
        )?,
    };
    let header = ChatHeader {
        provider_name,
        model,
        endpoint,
        context_report,
        session_id,
        session_path,
        resumed,
        repair_truncate_to,
        unfinished,
        children: restored_children,
        workspace_root: workspace_root.clone(),
        artifact_store,
        owner: artifact_owner,
        models: manager,
    };

    match terminal::run_chat(runtime, header).await? {
        terminal::ChatExit::Quit => Ok(()),
        terminal::ChatExit::Restart => Box::pin(run_default(paths, BannerMode::Hidden, None)).await,
    }
}

fn run_session_command<W: Write>(
    command: SessionCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    match command {
        SessionCommand::Inspect { session_id } => {
            let summary = DurableSession::inspect(paths.data_dir(), session_id)?;
            writeln!(output, "session: {}", summary.session_id)?;
            writeln!(output, "path: {}", summary.path.display())?;
            writeln!(output, "records: {}", summary.record_count)?;
            writeln!(
                output,
                "unfinished operations: {}",
                summary.unfinished.len()
            )?;
            for (operation_id, state) in summary.unfinished {
                writeln!(output, "  {operation_id}: {state:?}")?;
            }
            writeln!(output, "artifacts: {}", summary.artifact_count)?;
            writeln!(output, "artifact bytes: {}", summary.artifact_bytes)?;
            writeln!(
                output,
                "context versions: {}",
                summary.context_versions.len()
            )?;
            for (context_id, version) in summary.context_versions {
                writeln!(output, "  {context_id} v{version}")?;
            }
            writeln!(output, "children: {}", summary.children.len())?;
            for child in summary.children {
                let attribution = &child.handle.admission.attribution;
                writeln!(
                    output,
                    "  {} parent={} route={} owner={} connection={} model={} state={:?} usage={:?} report={:?}{}",
                    attribution.agent_id,
                    attribution.parent_agent_id,
                    attribution.route,
                    attribution.owner.as_str(),
                    attribution.connection,
                    attribution.model,
                    child.handle.lifecycle,
                    child.handle.usage,
                    child.handle.report,
                    if child.projected_interruption {
                        " (projected after restart)"
                    } else {
                        ""
                    }
                )?;
            }
            match summary.repair_truncate_to {
                Some(offset) => {
                    writeln!(output, "torn tail: repair would truncate to byte {offset}")?
                }
                None => writeln!(output, "torn tail: none")?,
            }
            Ok(())
        }
    }
}

fn run_config_command<W: Write>(
    command: ConfigCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            writeln!(output, "{}", paths.config_file().display())?;
            Ok(())
        }
        ConfigCommand::Check => {
            load_config(paths)?;
            writeln!(
                output,
                "configuration is valid: {}",
                paths.config_file().display()
            )?;
            Ok(())
        }
    }
}

fn run_init_with_io<R: BufRead, W: Write>(
    args: &cli::InitArgs,
    paths: &XanaPaths,
    is_terminal: bool,
    input: &mut R,
    output: &mut W,
    banner_mode: BannerMode,
) -> Result<()> {
    match XanaConfig::load_from(paths.config_file()) {
        Ok(_) => {
            writeln!(output, "Xana is already initialized.")?;
            writeln!(output, "  Config: {}", paths.config_file().display())?;
            return Ok(());
        }
        Err(error) if error.is_missing_config() => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "cannot initialize over the configuration at {}",
                    paths.config_file().display()
                )
            });
        }
    }

    presentation::write_banner(output, banner_mode).context("could not write Xana banner")?;

    let initial = match init::plan(args, is_terminal, input, output)
        .context("could not plan Xana initialization")?
    {
        InitPlan::Create(initial) => initial,
        InitPlan::Cancelled => {
            writeln!(output, "No changes made.")?;
            return Ok(());
        }
    };

    let rendered = XanaConfig::render_initial(initial.clone())
        .context("could not render initial configuration")?;

    if args.dry_run {
        writeln!(
            output,
            "Configuration preview (not written): {}",
            paths.config_file().display()
        )?;
        writeln!(output)?;
        write!(output, "{rendered}")?;
        return Ok(());
    }

    let outcome = init::write_new_config(paths.config_file(), &rendered)
        .context("could not create Xana configuration")?;

    match outcome {
        WriteOutcome::Created { path } if args.non_interactive => {
            writeln!(output, "configuration created: {}", path.display())?;
        }
        WriteOutcome::Created { path } => {
            writeln!(output, "Configuration created.")?;
            writeln!(output, "  Config:   {}", path.display())?;
            writeln!(output, "  Connection: {}", initial.connection.name())?;
            writeln!(output, "  Model:    {}", initial.model)?;
            writeln!(output)?;
            if initial.connection.is_codex() {
                writeln!(output, "Next:")?;
                writeln!(
                    output,
                    "  1. xana connection status {}",
                    initial.connection.name()
                )?;
                writeln!(
                    output,
                    "  2. xana connection login {}",
                    initial.connection.name()
                )?;
                writeln!(
                    output,
                    "  3. xana connection refresh {}",
                    initial.connection.name()
                )?;
                writeln!(
                    output,
                    "  4. xana model list --connection {}",
                    initial.connection.name()
                )?;
                writeln!(
                    output,
                    "  5. xana model use {}/MODEL",
                    initial.connection.name()
                )?;
                writeln!(output, "  6. xana")?;
                writeln!(output)?;
                writeln!(
                    output,
                    "Running from this source checkout? Replace `xana` with `cargo run --`,"
                )?;
                writeln!(
                    output,
                    "for example: cargo run -- connection status {}",
                    initial.connection.name()
                )?;
            } else {
                writeln!(output, "Next: xana")?;
            }
        }
        WriteOutcome::AlreadyInitialized { path } => {
            writeln!(output, "Xana is already initialized.")?;
            writeln!(output, "  Config: {}", path.display())?;
        }
    }

    Ok(())
}

fn run_init_command(args: &cli::InitArgs, paths: &XanaPaths, no_banner: bool) -> Result<()> {
    let stdin = io::stdin();
    let input_is_terminal = stdin.is_terminal();
    let output_is_terminal = io::stdout().is_terminal();
    let banner_mode = banner_mode(
        !args.non_interactive && !args.dry_run,
        input_is_terminal,
        output_is_terminal,
        no_banner,
    );
    let mut input = stdin.lock();
    let mut output = anstream::stdout().lock();

    run_init_with_io(
        args,
        paths,
        input_is_terminal,
        &mut input,
        &mut output,
        banner_mode,
    )
}

fn banner_mode(
    selected_surface: bool,
    input_is_terminal: bool,
    output_is_terminal: bool,
    suppressed: bool,
) -> BannerMode {
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let dumb_terminal = std::env::var("TERM").is_ok_and(|term| term.eq_ignore_ascii_case("dumb"));

    presentation::choose_banner_mode(
        selected_surface,
        input_is_terminal,
        output_is_terminal,
        suppressed,
        no_color,
        dumb_terminal,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InitialConfig, InitialConnection};
    use std::{fs, io::Cursor};
    use tempfile::tempdir;

    #[test]
    fn config_commands_use_the_injected_paths_and_writer() {
        let directory = tempdir().expect("temporary Xana home");
        let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned()))
            .expect("absolute Xana home");
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "ollama".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "model".to_owned(),
            max_tool_rounds: 8,
            shell: crate::shell::ShellConfig::default(),
            permission_mode: crate::config::PermissionMode::Ask,
        })
        .expect("render config");
        fs::write(paths.config_file(), rendered).expect("write config");

        let mut path_output = Vec::new();
        run_config_command(ConfigCommand::Path, &paths, &mut path_output)
            .expect("print config path");
        let mut check_output = Vec::new();
        run_config_command(ConfigCommand::Check, &paths, &mut check_output).expect("check config");

        assert_eq!(
            String::from_utf8(path_output).expect("path output"),
            format!("{}\n", paths.config_file().display())
        );
        assert!(
            String::from_utf8(check_output)
                .expect("check output")
                .starts_with("configuration is valid:")
        );
    }

    #[test]
    fn route_commands_report_exact_local_resolution_without_starting_a_provider() {
        let directory = tempdir().expect("temporary Xana home");
        let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned()))
            .expect("absolute Xana home");
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "ollama".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen".to_owned(),
            max_tool_rounds: 8,
            shell: crate::shell::ShellConfig::default(),
            permission_mode: crate::config::PermissionMode::Ask,
        })
        .expect("render config");
        fs::write(paths.config_file(), rendered).expect("write config");

        let mut listed = Vec::new();
        run_route_command(RouteCommand::List, &paths, &mut listed).expect("list routes");
        let mut checked = Vec::new();
        run_route_command(
            RouteCommand::Check {
                route: "default".into(),
            },
            &paths,
            &mut checked,
        )
        .expect("check route");

        assert_eq!(
            String::from_utf8(listed).expect("utf8 list"),
            "* default\tnative\tollama/qwen\tprofile default\n"
        );
        let checked = String::from_utf8(checked).expect("utf8 check");
        assert!(checked.contains("route: default\n"));
        assert!(checked.contains("execution: native\n"));
        assert!(checked.contains("connection: ollama\n"));
        assert!(checked.contains("model: qwen\n"));
        assert!(!checked.to_ascii_lowercase().contains("secret"));
    }

    #[test]
    fn reset_confirmation_can_decline_without_removing_configuration() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
        fs::create_dir_all(paths.config_file().parent().expect("config parent"))
            .expect("create config parent");
        fs::write(paths.config_file(), b"existing").expect("existing config");
        let mut input = Cursor::new(b"n\n");
        let mut output = Vec::new();

        run_reset_with_io(
            &cli::ResetArgs { yes: false },
            &paths,
            true,
            &mut input,
            &mut output,
        )
        .expect("decline reset");

        assert!(paths.config_file().is_file());
        assert!(
            String::from_utf8(output)
                .expect("reset output")
                .contains("No changes made.")
        );
    }

    #[test]
    fn confirmed_reset_does_not_read_input_and_allows_initialization_again() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
        fs::create_dir_all(paths.config_file().parent().expect("config parent"))
            .expect("create config parent");
        fs::write(paths.config_file(), b"existing").expect("existing config");
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        run_reset_with_io(
            &cli::ResetArgs { yes: true },
            &paths,
            false,
            &mut input,
            &mut output,
        )
        .expect("confirmed reset");

        assert_eq!(input.position(), 0);
        assert!(!paths.config_file().exists());
        assert!(
            String::from_utf8(output)
                .expect("reset output")
                .contains("cargo run -- init")
        );
    }

    #[test]
    fn noninteractive_init_routes_without_process_environment_or_terminal() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
        let args = cli::InitArgs {
            non_interactive: true,
            kind: Some(crate::cli::InitConnectionKindChoice::Ollama),
            provider_name: Some("ollama".to_owned()),
            base_url: Some("http://localhost:11434/v1".to_owned()),
            codex_program: None,
            codex_home: None,
            model: Some("model".to_owned()),
            max_tool_rounds: Some(8),
            shell: None,
            shell_program: None,
            permission_mode: Some(crate::cli::PermissionChoice::Ask),
            dry_run: false,
        };
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        run_init_with_io(
            &args,
            &paths,
            false,
            &mut input,
            &mut output,
            BannerMode::Hidden,
        )
        .expect("noninteractive init");

        assert!(paths.config_file().is_file());
        assert_eq!(input.position(), 0);
        assert!(
            String::from_utf8(output)
                .expect("init output")
                .starts_with("configuration created:")
        );
    }

    #[test]
    fn interactive_codex_init_writes_managed_runtime_and_next_steps() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
        let args = cli::InitArgs {
            non_interactive: false,
            kind: None,
            provider_name: None,
            base_url: None,
            codex_program: None,
            codex_home: None,
            model: None,
            max_tool_rounds: None,
            shell: None,
            shell_program: None,
            permission_mode: None,
            dry_run: false,
        };
        let mut input = Cursor::new(b"2\n\n\ngpt-5.6-sol\n\n\n\n\ny\n");
        let mut output = Vec::new();

        run_init_with_io(
            &args,
            &paths,
            true,
            &mut input,
            &mut output,
            BannerMode::Hidden,
        )
        .expect("interactive Codex init");

        let rendered = fs::read_to_string(paths.config_file()).expect("created configuration");
        assert!(rendered.contains("kind = \"codex\""));
        assert!(rendered.contains("codex_program = \"codex\""));
        assert!(!rendered.contains("base_url"));
        let transcript = String::from_utf8(output).expect("init output");
        assert!(transcript.contains("xana connection status codex"));
        assert!(transcript.contains("xana connection login codex"));
        assert!(transcript.contains("xana model list --connection codex"));
        assert!(transcript.contains("xana model use codex/MODEL"));
        assert!(transcript.contains("cargo run -- connection status codex"));
    }

    #[test]
    fn dry_run_routes_without_creating_the_home() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths =
            XanaPaths::resolve(Some(root.clone().into_os_string())).expect("absolute Xana home");
        let args = cli::InitArgs {
            dry_run: true,
            ..cli::InitArgs {
                non_interactive: true,
                kind: Some(crate::cli::InitConnectionKindChoice::Ollama),
                provider_name: Some("ollama".to_owned()),
                base_url: Some("http://localhost:11434/v1".to_owned()),
                codex_program: None,
                codex_home: None,
                model: Some("model".to_owned()),
                max_tool_rounds: None,
                shell: None,
                shell_program: None,
                permission_mode: Some(crate::cli::PermissionChoice::Ask),
                dry_run: false,
            }
        };
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        run_init_with_io(
            &args,
            &paths,
            false,
            &mut input,
            &mut output,
            BannerMode::Hidden,
        )
        .expect("dry-run init");

        assert!(!root.exists());
        assert!(
            String::from_utf8(output)
                .expect("dry-run output")
                .contains("permission_mode = \"ask\"")
        );
    }
}
