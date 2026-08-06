mod agent;
mod cli;
mod config;
mod init;
mod message;
mod paths;
mod presentation;
mod provider;
mod tool;

use agent::Agent;
use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Command, ConfigCommand};
use config::{PermissionMode, ProviderKind, XanaConfig};
use init::{InitPlan, WriteOutcome};
use message::{ContentBlock, Message, Role};
use paths::XanaPaths;
use presentation::BannerMode;
use provider::openai_compat::OpenAiCompatClient;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::PathBuf,
};
use tool::ToolRegistry;

#[derive(Debug, PartialEq, Eq)]
enum InputAction<'a> {
    Quit,
    Clear,
    Ignore,
    Send(&'a str),
}

fn classify_input(line: &str) -> InputAction<'_> {
    match line.trim() {
        "/quit" => InputAction::Quit,
        "/clear" => InputAction::Clear,
        "" => InputAction::Ignore,
        input => InputAction::Send(input),
    }
}

fn print_assistant(message: &Message) {
    print!("xana> ");

    for block in &message.content {
        match block {
            ContentBlock::Text(text) => print!("{text}"),
            ContentBlock::ToolCall(tool_call) => {
                print!("[tool call requested: {}]", tool_call.name);
            }
            ContentBlock::ToolResult(_) => {}
        }
    }

    println!();
}

fn run_chat(config: XanaConfig, workspace_root: PathBuf) -> Result<()> {
    let XanaConfig {
        provider_name,
        provider_kind,
        base_url,
        model,
        permission_mode,
        max_tool_rounds,
    } = config;

    match permission_mode {
        PermissionMode::Allow => {}
    }

    println!("provider connection: {provider_name}");
    println!("model: {model}");

    let provider = match provider_kind {
        ProviderKind::OpenAiCompat => OpenAiCompatClient::new(base_url, model),
    };

    println!("chat endpoint: {}", provider.endpoint());

    let tools = ToolRegistry::builtins().context("could not build tool registry")?;
    let agent = Agent::new(provider, tools, workspace_root, max_tool_rounds);
    let mut editor = DefaultEditor::new().context("could not initialize line editor")?;
    let mut messages = Vec::new();

    loop {
        match editor.readline("you> ") {
            Ok(line) => match classify_input(&line) {
                InputAction::Quit => {
                    break;
                }
                InputAction::Clear => {
                    messages.clear();
                    println!("xana> conversation cleared");
                }
                InputAction::Ignore => {}
                InputAction::Send(input) => {
                    editor
                        .add_history_entry(input)
                        .context("could not add input to editor history")?;

                    messages.push(Message::text(Role::User, input));
                    let assistant = agent.run_turn(&mut messages)?;
                    print_assistant(&assistant);
                    messages.push(assistant);
                }
            },
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                break;
            }
            Err(error) => {
                return Err(error.into());
            }
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

    let workspace_root =
        std::env::current_dir().context("could not resolve Xana workspace root")?;
    run_chat(config, workspace_root)
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    let paths = XanaPaths::resolve(std::env::var_os("XANA_HOME"))
        .context("could not resolve Xana paths")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Cursor};
    use tempfile::tempdir;

    #[test]
    fn classifies_commands_blanks_and_messages() {
        assert_eq!(classify_input("/quit"), InputAction::Quit);
        assert_eq!(classify_input("  /clear  "), InputAction::Clear);
        assert_eq!(classify_input("   "), InputAction::Ignore);
        assert_eq!(
            classify_input("  hello Xana  "),
            InputAction::Send("hello Xana")
        );
        assert_eq!(classify_input("clear"), InputAction::Send("clear"));
    }

    #[test]
    fn config_commands_use_the_injected_paths_and_writer() {
        let directory = tempdir().expect("temporary Xana home");
        let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned()))
            .expect("absolute Xana home");
        let rendered = XanaConfig::render_initial(config::InitialConfig {
            provider_name: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "model".to_owned(),
            max_tool_rounds: 8,
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
