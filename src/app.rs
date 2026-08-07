//! Application-edge command orchestration.
//!
//! This module may inspect terminal capabilities, process environment, and the
//! current directory. It validates those inputs and passes owned configuration
//! inward; it does not put frontend or process-global concerns into `Agent`.

use crate::{
    agent::Agent,
    cli::{self, Cli, Command, ConfigCommand},
    config::{PermissionMode, ProviderKind, XanaConfig},
    context::{ContextBudget, ContextPlanReport, load_project_sources},
    init::{self, InitPlan, WriteOutcome},
    paths::XanaPaths,
    presentation::{self, BannerMode},
    prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
    provider::openai_compat::OpenAiCompatClient,
    shell::Shell,
    terminal::{self, ChatHeader},
    tool::ToolRegistry,
};
use anyhow::{Context, Result};
use std::io::{self, BufRead, IsTerminal, Write};

const PROMPT_TOTAL_TOKENS: usize = 32_768;
const PROMPT_CONVERSATION_RESERVE_TOKENS: usize = 8_192;

pub(crate) fn run(cli: Cli, paths: XanaPaths) -> Result<()> {
    let no_banner = cli.no_banner;

    match cli.command {
        None => {
            let mode = banner_mode(
                true,
                io::stdin().is_terminal(),
                io::stdout().is_terminal(),
                no_banner,
            );
            run_default(&paths, mode)
        }
        Some(Command::Init(args)) => run_init_command(&args, &paths, no_banner),
        Some(Command::Config(args)) => {
            let stdout = io::stdout();
            run_config_command(args.command, &paths, &mut stdout.lock())
        }
    }
}

fn load_config(paths: &XanaPaths) -> Result<XanaConfig> {
    XanaConfig::load_from(paths.config_file()).with_context(|| {
        format!(
            "failed to load config from {}",
            paths.config_file().display()
        )
    })
}

fn run_default(paths: &XanaPaths, banner_mode: BannerMode) -> Result<()> {
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
        shell,
        max_tool_rounds,
    } = config;

    match permission_mode {
        PermissionMode::Allow => {}
    }

    let provider = match provider_kind {
        ProviderKind::OpenAiCompat => OpenAiCompatClient::new(base_url, model.clone()),
    };
    let endpoint = provider.endpoint().to_owned();
    let shell = Shell::resolve(shell).context("could not resolve configured shell")?;
    let configured_shell = shell.prompt_description();
    let tools = ToolRegistry::builtins(shell, terminal::terminal_approver())
        .context("could not build tool registry")?;
    let workspace_root = std::env::current_dir()
        .context("could not resolve Xana workspace root")?
        .canonicalize()
        .context("could not canonicalize Xana workspace root")?;
    let project_sources = load_project_sources(&workspace_root)
        .context("could not load project prompt instructions")?;
    let environment = PromptEnvironment {
        operating_system: std::env::consts::OS.to_owned(),
        working_directory: workspace_root.clone(),
        configured_shell,
        surface: PromptSurface::Cli,
    };
    let definitions = tools.definitions();
    let prompt = assemble_snapshot(PromptInputs {
        tool_definitions: &definitions,
        environment: &environment,
        product_documentation: None,
        project_sources: &project_sources,
        budget: ContextBudget {
            total_tokens: PROMPT_TOTAL_TOKENS,
            conversation_reserve_tokens: PROMPT_CONVERSATION_RESERVE_TOKENS,
        },
    })
    .context("could not assemble Xana prompt")?;
    let context_report = ContextPlanReport::render(&prompt.context_plan)
        .as_str()
        .to_owned();
    let agent = Agent::new(provider, tools, workspace_root, prompt, max_tool_rounds);
    let header = ChatHeader {
        provider_name,
        model,
        endpoint,
        context_report,
    };

    terminal::run_chat(agent, header)
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
            accept_automatic_tools: true,
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
                accept_automatic_tools: true,
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
                .contains("permission_mode = \"allow\"")
        );
    }
}
