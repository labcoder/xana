//! Application-edge command orchestration.
//!
//! This module may inspect terminal capabilities, process environment, and the
//! current directory. It validates those inputs and passes owned configuration
//! inward; it does not put frontend or process-global concerns into `Agent`.

use crate::{
    agent::Agent,
    cli::{self, Cli, Command, ConfigCommand, OperationCommand, SessionCommand},
    config::{ProviderKind, XanaConfig},
    context::{ContextBudget, ContextPlanReport},
    init::{self, InitPlan, WriteOutcome},
    operation::{RecoveryAction, execute_recovery, plan_recovery},
    paths::XanaPaths,
    permission::{PermissionBroker, PermissionPolicy},
    presentation::{self, BannerMode},
    prompt::{PromptAssembler, PromptEnvironment, PromptSurface},
    provider::openai_compat::OpenAiCompatClient,
    runtime::{RuntimeCommand, RuntimeHandle},
    session::{DurableSession, RestoredOperation},
    shell::Shell,
    terminal::{self, ChatHeader},
    tool::ToolRegistry,
};
use anyhow::{Context, Result};
use std::io::{self, BufRead, IsTerminal, Write};

const PROMPT_TOTAL_TOKENS: usize = 32_768;
const PROMPT_CONVERSATION_RESERVE_TOKENS: usize = 8_192;

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
    }
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
        | RuntimeCommand::ClearConversation
        | RuntimeCommand::DecidePermission { .. }
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

    let XanaConfig {
        provider_name,
        provider_kind,
        base_url,
        model,
        permission_mode,
        permission_rules,
        shell,
        max_tool_rounds,
    } = config;

    let provider = match provider_kind {
        ProviderKind::OpenAiCompat => OpenAiCompatClient::new(base_url, model.clone()),
    };
    let endpoint = provider.endpoint().to_owned();
    let shell = Shell::resolve(shell).context("could not resolve configured shell")?;
    let configured_shell = shell.prompt_description();
    let tools = ToolRegistry::builtins(shell).context("could not build tool registry")?;
    let workspace_root = std::env::current_dir()
        .context("could not resolve Xana workspace root")?
        .canonicalize()
        .context("could not canonicalize Xana workspace root")?;
    let (session, permission_policy, resumed, repair_truncate_to, unfinished) = match resume {
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
                permission_rules,
                session.workspace_root(),
            )
            .context("could not resolve permission policy for the session workspace")?;
            (
                session,
                permission_policy,
                true,
                summary.repair_truncate_to,
                unfinished,
            )
        }
        None => {
            let permission_policy =
                PermissionPolicy::new(permission_mode.into(), permission_rules, &workspace_root)
                    .context("could not resolve permission policy for the launch workspace")?;
            (
                DurableSession::create(paths.data_dir(), workspace_root.clone())?,
                permission_policy,
                false,
                None,
                Vec::new(),
            )
        }
    };
    let workspace_root = session.workspace_root().to_owned();
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
        None,
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
        Box::new(provider),
        tools,
        workspace_root,
        prompt,
        max_tool_rounds,
    );
    let session_id = session.session_id();
    let session_path = session.path().to_owned();
    let runtime =
        RuntimeHandle::spawn_persistent(agent, permission_policy, true, session, prompt_assembler)?;
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
    };

    terminal::run_chat(runtime, header).await
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
            writeln!(output, "  Provider: {}", initial.provider_name)?;
            writeln!(output, "  Model:    {}", initial.model)?;
            writeln!(output)?;
            writeln!(output, "Next: xana")?;
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
    use crate::config::InitialConfig;
    use std::{fs, io::Cursor};
    use tempfile::tempdir;

    #[test]
    fn config_commands_use_the_injected_paths_and_writer() {
        let directory = tempdir().expect("temporary Xana home");
        let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned()))
            .expect("absolute Xana home");
        let rendered = XanaConfig::render_initial(InitialConfig {
            provider_name: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
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
    fn noninteractive_init_routes_without_process_environment_or_terminal() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
        let args = cli::InitArgs {
            non_interactive: true,
            provider_name: Some("ollama".to_owned()),
            base_url: Some("http://localhost:11434/v1".to_owned()),
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
    fn dry_run_routes_without_creating_the_home() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths =
            XanaPaths::resolve(Some(root.clone().into_os_string())).expect("absolute Xana home");
        let args = cli::InitArgs {
            dry_run: true,
            ..cli::InitArgs {
                non_interactive: true,
                provider_name: Some("ollama".to_owned()),
                base_url: Some("http://localhost:11434/v1".to_owned()),
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
