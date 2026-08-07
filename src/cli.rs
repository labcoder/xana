//! Typed command-line syntax.
//!
//! Clap turns process arguments into command data only; command execution and
//! terminal/filesystem effects remain at the application edge.

use crate::shell::ShellKind;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ShellChoice {
    Platform,
    Posix,
    #[value(name = "git_bash")]
    GitBash,
    #[value(name = "powershell")]
    PowerShell,
    Cmd,
}

impl From<ShellChoice> for ShellKind {
    fn from(value: ShellChoice) -> Self {
        match value {
            ShellChoice::Platform => Self::Platform,
            ShellChoice::Posix => Self::Posix,
            ShellChoice::GitBash => Self::GitBash,
            ShellChoice::PowerShell => Self::PowerShell,
            ShellChoice::Cmd => Self::Cmd,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "xana", version, about = "A small personal AI agent harness")]
pub(crate) struct Cli {
    /// Suppress Xana's terminal banner.
    #[arg(long, global = true)]
    pub(crate) no_banner: bool,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum Command {
    /// Create Xana's first configuration.
    Init(InitArgs),
    /// Inspect the active configuration.
    Config(ConfigArgs),
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct InitArgs {
    /// Require all setup values from flags and never read stdin.
    #[arg(long)]
    pub(crate) non_interactive: bool,

    /// Name the OpenAI-compatible provider connection.
    #[arg(long, value_name = "NAME")]
    pub(crate) provider_name: Option<String>,

    /// Set the provider's absolute HTTP(S) base URL.
    #[arg(long, value_name = "URL")]
    pub(crate) base_url: Option<String>,

    /// Select the model used by the default profile.
    #[arg(long, value_name = "MODEL")]
    pub(crate) model: Option<String>,

    /// Set the bounded tool-call round limit.
    #[arg(long, value_name = "COUNT")]
    pub(crate) max_tool_rounds: Option<usize>,

    /// Select the shell used by run_command (defaults to platform).
    #[arg(long, value_enum, value_name = "KIND")]
    pub(crate) shell: Option<ShellChoice>,

    /// Override the executable used for the selected shell.
    #[arg(long, value_name = "PATH", requires = "non_interactive")]
    pub(crate) shell_program: Option<PathBuf>,

    /// Explicitly accept automatic file-tool execution with host access.
    #[arg(long)]
    pub(crate) accept_automatic_tools: bool,

    /// Render and validate the proposed TOML without writing it.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    pub(crate) command: ConfigCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum ConfigCommand {
    /// Print the active config.toml path.
    Path,
    /// Load and validate the active config.toml.
    Check,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn no_subcommand_means_normal_chat() {
        let cli = Cli::try_parse_from(["xana"]).expect("bare invocation");

        assert!(!cli.no_banner);
        assert_eq!(cli.command, None);
    }

    #[test]
    fn parses_interactive_init() {
        let cli =
            Cli::try_parse_from(["xana", "init", "--dry-run"]).expect("interactive initialization");

        assert_eq!(
            cli.command,
            Some(Command::Init(InitArgs {
                non_interactive: false,
                provider_name: None,
                base_url: None,
                model: None,
                max_tool_rounds: None,
                shell: None,
                shell_program: None,
                accept_automatic_tools: false,
                dry_run: true,
            }))
        );
    }

    #[test]
    fn parses_complete_noninteractive_init() {
        let cli = Cli::try_parse_from([
            "xana",
            "init",
            "--non-interactive",
            "--provider-name",
            "ollama",
            "--base-url",
            "http://localhost:11434/v1",
            "--model",
            "qwen3:1.7b",
            "--max-tool-rounds",
            "12",
            "--shell",
            "powershell",
            "--shell-program",
            "pwsh.exe",
            "--accept-automatic-tools",
        ])
        .expect("complete noninteractive initialization");

        assert_eq!(
            cli.command,
            Some(Command::Init(InitArgs {
                non_interactive: true,
                provider_name: Some("ollama".to_owned()),
                base_url: Some("http://localhost:11434/v1".to_owned()),
                model: Some("qwen3:1.7b".to_owned()),
                max_tool_rounds: Some(12),
                shell: Some(ShellChoice::PowerShell),
                shell_program: Some(PathBuf::from("pwsh.exe")),
                accept_automatic_tools: true,
                dry_run: false,
            }))
        );
    }

    #[test]
    fn parses_config_path_and_check() {
        let path = Cli::try_parse_from(["xana", "config", "path"]).expect("config path command");
        let check = Cli::try_parse_from(["xana", "config", "check"]).expect("config check command");

        assert_eq!(
            path.command,
            Some(Command::Config(ConfigArgs {
                command: ConfigCommand::Path,
            }))
        );
        assert_eq!(
            check.command,
            Some(Command::Config(ConfigArgs {
                command: ConfigCommand::Check,
            }))
        );
    }

    #[test]
    fn rejects_unknown_commands_and_invalid_round_counts() {
        let unknown =
            Cli::try_parse_from(["xana", "unknown"]).expect_err("unknown command should fail");
        let invalid_rounds =
            Cli::try_parse_from(["xana", "init", "--max-tool-rounds", "not-a-number"])
                .expect_err("invalid count should fail");

        assert_eq!(unknown.kind(), ErrorKind::InvalidSubcommand);
        assert_eq!(invalid_rounds.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn no_banner_is_global() {
        let bare = Cli::try_parse_from(["xana", "--no-banner"]).expect("bare no-banner");
        let init = Cli::try_parse_from(["xana", "init", "--no-banner"]).expect("init no-banner");

        assert!(bare.no_banner);
        assert!(init.no_banner);
    }
}
