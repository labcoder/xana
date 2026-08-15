//! Application-edge command orchestration.
//!
//! This module may inspect terminal capabilities, process environment, and the
//! current directory. It validates those inputs and passes owned configuration
//! inward; it does not put frontend or process-global concerns into `Agent`.

mod chat;
mod connections;
mod external_agents;
mod hosting;
mod image_commands;
mod mcp_commands;
mod one_shot;
mod operations;
mod plugins;
mod profiles;
mod projects;
mod recovery;
mod sessions;
mod skills;

use connections::{
    codex_launch, model_manager, run_auth_command, run_connection_command, run_model_command,
};
#[cfg(test)]
use recovery::run_reset_with_io;
use recovery::{run_config_command, run_doctor_command, run_reset_command};

#[cfg(test)]
use crate::{cli::ConfigCommand, config::CredentialReference};
use crate::{
    cli::{self, Cli, Command, OutputChoice, SessionCommand},
    config::XanaConfig,
    init::{self, InitPlan, WriteOutcome},
    oneshot::OneShotOutput,
    paths::XanaPaths,
    presentation::{self, BannerMode},
    tui,
};
use anyhow::{Context, Result};
use std::io::{self, BufRead, IsTerminal, Write};

pub(crate) async fn run(cli: Cli, paths: XanaPaths) -> Result<()> {
    let no_banner = cli.no_banner;

    if (cli.resume.is_some() || cli.continue_chat || cli.plain || cli.tui || cli.print.is_some())
        && cli.command.is_some()
    {
        anyhow::bail!(
            "chat surface, continuation, and one-shot options cannot be combined with a subcommand"
        );
    }
    match cli.command {
        None => {
            if let Some(argument) = cli.print {
                let output = if cli.json || cli.output == Some(OutputChoice::Json) {
                    OneShotOutput::Json
                } else {
                    OneShotOutput::Text
                };
                return one_shot::run_and_render(
                    &paths,
                    cli.resume,
                    cli.continue_chat,
                    argument,
                    output,
                )
                .await;
            }
            ensure_setup(&paths).await?;
            let surface = prepare_default_chat_surface(&paths, cli.plain, cli.tui, no_banner)?;
            chat::run(&paths, surface, cli.resume, cli.continue_chat, false, None)
                .await
                .map(|_| ())
        }
        Some(Command::Init(args)) => run_init_command(&args, &paths, no_banner),
        Some(Command::Serve(args)) => hosting::run_serve(&args, &paths).await,
        Some(Command::Attach(args)) => hosting::run_attach(&args, &paths).await,
        Some(Command::Setup(args)) => {
            if args.if_needed {
                if *args
                    != (cli::SetupArgs {
                        if_needed: true,
                        ..cli::SetupArgs::default()
                    })
                {
                    anyhow::bail!("--if-needed cannot be combined with other setup options");
                }
                return run_setup_if_needed_command(&paths).await;
            }
            let outcome = run_setup_command(&args, &paths).await?;
            if outcome.starts_new_conversation(args.start_new) {
                let surface = prepare_default_chat_surface(&paths, false, false, no_banner)?;
                chat::run(&paths, surface, None, false, true, None)
                    .await
                    .map(|_| ())
            } else {
                Ok(())
            }
        }
        Some(Command::Doctor(args)) => run_doctor_command(&args, &paths).await,
        Some(Command::Reset(args)) => run_reset_command(&args, &paths),
        Some(Command::Config(args)) => {
            let stdout = io::stdout();
            run_config_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Session(args)) => {
            if args.command == SessionCommand::New {
                ensure_setup(&paths).await?;
                let surface = prepare_default_chat_surface(&paths, false, false, no_banner)?;
                chat::run(&paths, surface, None, false, true, None)
                    .await
                    .map(|_| ())
            } else {
                let stdout = io::stdout();
                sessions::run_command(args.command, &paths, &mut stdout.lock())
            }
        }
        Some(Command::Project(args)) => {
            let stdout = io::stdout();
            projects::run_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Profile(args)) => {
            let stdout = io::stdout();
            profiles::run_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Skill(args)) => {
            let stdout = io::stdout();
            skills::run_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Plugin(args)) => {
            let stdout = io::stdout();
            plugins::run_command(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Mcp(args)) => match args.command {
            cli::McpCommand::Serve {
                workspace,
                profile,
                allow,
            } => mcp_commands::serve(&paths, workspace, profile, allow).await,
            command => {
                let stdout = io::stdout();
                mcp_commands::run(command, &paths, &mut stdout.lock()).await
            }
        },
        Some(Command::ExternalAgent(args)) => {
            let stdout = io::stdout();
            external_agents::run(args.command, &paths, &mut stdout.lock()).await
        }
        Some(Command::Image(args)) => {
            let stdout = io::stdout();
            let stdin = io::stdin();
            image_commands::run(args.command, &paths, &mut stdin.lock(), &mut stdout.lock()).await
        }
        Some(Command::Connect(args)) => match args.integration {
            Some(cli::ConnectIntegration::Provider) => {
                let setup = cli::SetupArgs {
                    section: Some(cli::SetupSectionChoice::Connection),
                    ..cli::SetupArgs::default()
                };
                run_setup_command(&setup, &paths).await.map(|_| ())
            }
            Some(cli::ConnectIntegration::Profile) => {
                let setup = cli::SetupArgs {
                    section: Some(cli::SetupSectionChoice::ProfilesRoutes),
                    ..cli::SetupArgs::default()
                };
                run_setup_command(&setup, &paths).await.map(|_| ())
            }
            integration => {
                let mut output = io::stdout().lock();
                writeln!(
                    output,
                    "Xana integration hub (read-only until you choose an exact command)"
                )?;
                writeln!(output, "  provider        xana connect provider")?;
                writeln!(output, "  profile         xana connect profile")?;
                writeln!(output, "  plugin          xana plugin list")?;
                writeln!(output, "  MCP             xana mcp list")?;
                writeln!(output, "  external agent  xana external-agent list")?;
                writeln!(output, "  image routes    xana image list")?;
                if let Some(integration) = integration {
                    writeln!(
                        output,
                        "Selected {integration:?}; use the exact command shown above so review, cancellation, and failures remain explicit."
                    )?;
                }
                Ok(())
            }
        },
        Some(Command::Operation(args)) => {
            let stdout = io::stdout();
            operations::run_operation(args.command, &paths, &mut stdout.lock()).await
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
            operations::run_route(args.command, &paths, &mut stdout.lock())
        }
        Some(Command::Auth(args)) => {
            let stdout = io::stdout();
            run_auth_command(args.command, &paths, &mut stdout.lock()).await
        }
    }
}

async fn ensure_setup(paths: &XanaPaths) -> Result<()> {
    match XanaConfig::load_from(paths.config_file()) {
        Ok(_) => Ok(()),
        Err(error) => {
            if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
                anyhow::bail!(
                    "Xana configuration is absent or invalid at {}: {error}\nrun `xana setup --non-interactive --kind KIND --connection NAME --model MODEL --permission-mode MODE --yes`",
                    paths.config_file().display()
                );
            }
            eprintln!("xana: configuration is absent or invalid: {error}");
            eprintln!("xana: starting provider-neutral guided setup");
            let _ = run_setup_command(&cli::SetupArgs::default(), paths).await?;
            XanaConfig::load_from(paths.config_file())
                .context("setup ended without installing a valid configuration")?;
            Ok(())
        }
    }
}

fn prepare_default_chat_surface(
    paths: &XanaPaths,
    plain: bool,
    require_tui: bool,
    no_banner: bool,
) -> Result<chat::ChatSurface> {
    let input_is_terminal = io::stdin().is_terminal();
    let output_is_terminal = io::stdout().is_terminal();
    let mode = banner_mode(
        paths,
        true,
        input_is_terminal,
        output_is_terminal,
        no_banner,
    );
    if require_tui && !(input_is_terminal && output_is_terminal) {
        anyhow::bail!("--tui requires interactive stdin and stdout; use `xana --plain`");
    }
    if plain || !(input_is_terminal && output_is_terminal) {
        return Ok(chat::ChatSurface::Plain(mode));
    }
    let preferences =
        presentation::PresentationPreferences::load(&paths.presentation_file()).preferences;
    match tui::prepare(mode.profile(), preferences, paths.presentation_file()) {
        Ok(prepared) => Ok(chat::ChatSurface::Tui {
            prepared,
            required: require_tui,
        }),
        Err(error) if !require_tui => {
            eprintln!(
                "xana: could not initialize the full-screen terminal ({error}); falling back to --plain"
            );
            Ok(chat::ChatSurface::Plain(mode))
        }
        Err(error) => Err(error).context("could not initialize the required Xana TUI"),
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

fn run_init_with_io<R: BufRead, W: Write>(
    args: &cli::InitArgs,
    paths: &XanaPaths,
    is_terminal: bool,
    input: &mut R,
    output: &mut W,
    banner_mode: BannerMode,
) -> Result<()> {
    writeln!(
        output,
        "`xana init` is deprecated through v0.5.0; use provider-neutral `xana setup`."
    )?;
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
    crate::private_state::ensure_interoperable_records(paths)
        .context("configuration created, but private interoperable records need recovery; run `xana config migrate --apply`")?;

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
                    "  3. xana model refresh {}",
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
        paths,
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

async fn run_setup_command(
    args: &cli::SetupArgs,
    paths: &XanaPaths,
) -> Result<crate::setup::SetupOutcome> {
    let stdin = io::stdin();
    let input_is_terminal = stdin.is_terminal();
    let output_is_terminal = io::stdout().is_terminal();
    let profile = banner_mode(paths, true, input_is_terminal, output_is_terminal, false).profile();
    let mut input = stdin.lock();
    let mut output = anstream::stdout().lock();
    crate::setup::run(
        args,
        paths,
        input_is_terminal,
        output_is_terminal,
        &mut input,
        &mut output,
        profile,
    )
    .await
}

async fn run_setup_if_needed_command(paths: &XanaPaths) -> Result<()> {
    let stdin = io::stdin();
    let input_is_terminal = stdin.is_terminal();
    let output_is_terminal = io::stdout().is_terminal();
    let mut input = stdin.lock();
    let mut output = anstream::stdout().lock();
    crate::setup::run_if_needed(
        paths,
        input_is_terminal,
        output_is_terminal,
        &mut input,
        &mut output,
    )
    .await
}

fn banner_mode(
    paths: &XanaPaths,
    selected_surface: bool,
    input_is_terminal: bool,
    output_is_terminal: bool,
    suppressed: bool,
) -> BannerMode {
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let dumb_terminal = std::env::var("TERM").is_ok_and(|term| term.eq_ignore_ascii_case("dumb"));
    let color_depth = if no_color || !output_is_terminal || dumb_terminal {
        presentation::ColorDepth::None
    } else if std::env::var("COLORTERM")
        .is_ok_and(|value| value.to_ascii_lowercase().contains("truecolor"))
    {
        presentation::ColorDepth::TrueColor
    } else if std::env::var("TERM")
        .is_ok_and(|value| value.to_ascii_lowercase().contains("256color"))
    {
        presentation::ColorDepth::Ansi256
    } else {
        presentation::ColorDepth::Ansi16
    };
    let background = std::env::var("COLORFGBG").ok().and_then(|value| {
        let background = value.rsplit(';').next()?.parse::<u8>().ok()?;
        Some(if background <= 6 {
            presentation::Background::Dark
        } else {
            presentation::Background::Light
        })
    });
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|width| (20..=500).contains(width))
        .unwrap_or(80);
    let unicode = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .is_ok_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
        || cfg!(windows);
    let reduced_motion = std::env::var("XANA_REDUCED_MOTION")
        .is_ok_and(|value| !matches!(value.as_str(), "0" | "false" | "no"));
    let loaded = presentation::PresentationPreferences::load(&paths.presentation_file());
    if let Some(warning) = loaded.warning {
        eprintln!("xana: {warning}");
    }
    let profile = presentation::resolve(
        presentation::TerminalFacts {
            input_is_terminal,
            output_is_terminal,
            color_depth,
            background,
            unicode,
            width,
            reduced_motion,
            dumb: dumb_terminal,
        },
        &loaded.preferences,
    );
    presentation::choose_banner_mode(
        selected_surface && input_is_terminal && output_is_terminal && !dumb_terminal,
        suppressed,
        profile,
    )
}

#[cfg(test)]
mod tests;
