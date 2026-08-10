//! Typed command-line syntax.
//!
//! Clap turns process arguments into command data only; command execution and
//! terminal/filesystem effects remain at the application edge.

use crate::{
    config::PermissionMode,
    identity::{ArtifactId, SessionId},
    shell::ShellKind,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{net::IpAddr, path::PathBuf};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum PermissionChoice {
    Deny,
    Ask,
    Allow,
}

impl From<PermissionChoice> for PermissionMode {
    fn from(value: PermissionChoice) -> Self {
        match value {
            PermissionChoice::Deny => Self::Deny,
            PermissionChoice::Ask => Self::Ask,
            PermissionChoice::Allow => Self::Allow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum InitConnectionKindChoice {
    Ollama,
    #[value(name = "openai_compat")]
    OpenAiCompat,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub(crate) enum OutputChoice {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SetupSectionChoice {
    Connection,
    #[value(name = "permissions-shell")]
    PermissionsShell,
    #[value(name = "profiles-routes")]
    ProfilesRoutes,
    Appearance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ThemeChoice {
    Auto,
    Dark,
    Light,
    Monochrome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum GlyphChoice {
    Auto,
    Unicode,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum MotionChoice {
    Auto,
    Full,
    Reduced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum DensityChoice {
    Auto,
    Comfortable,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ComposerChoice {
    Submit,
    Newline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ActivityChoice {
    Auto,
    Open,
    Hidden,
}

#[derive(Debug, Parser)]
#[command(name = "xana", version, about = "A small personal AI agent harness")]
pub(crate) struct Cli {
    /// Suppress Xana's terminal banner.
    #[arg(long, global = true)]
    pub(crate) no_banner: bool,

    /// Use the permanent append-only terminal surface.
    #[arg(long, conflicts_with = "tui")]
    pub(crate) plain: bool,

    /// Require the full-screen terminal surface.
    #[arg(long, conflicts_with = "plain")]
    pub(crate) tui: bool,

    /// Perform exactly one turn. Pass PROMPT or omit it to read stdin.
    #[arg(short = 'p', long = "print", num_args = 0..=1, value_name = "PROMPT")]
    pub(crate) print: Option<Option<String>>,

    /// Select the latest compatible conversation for this workspace and owner.
    #[arg(
        long = "continue",
        visible_alias = "continue-chat",
        conflicts_with = "resume"
    )]
    pub(crate) continue_chat: bool,

    /// Select one-shot output encoding.
    #[arg(long, value_enum, requires = "print")]
    pub(crate) output: Option<OutputChoice>,

    /// Alias for `--output json`.
    #[arg(long, requires = "print", conflicts_with = "output")]
    pub(crate) json: bool,

    /// Resume exactly one durable session instead of creating a new one.
    #[arg(long, value_name = "SESSION_ID")]
    pub(crate) resume: Option<SessionId>,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum Command {
    /// Host Xana explicitly for authenticated local frontend attachment.
    Serve(ServeArgs),
    /// Attach to this workspace's foreground host; observation is the default.
    Attach(AttachArgs),
    /// Establish a provider connection and atomically install a quick configuration.
    Setup(Box<SetupArgs>),
    /// Diagnose this Xana installation without mutating it by default.
    Doctor(DoctorArgs),
    /// Deprecated through v0.5.0; use `xana setup`.
    #[command(hide = true)]
    Init(InitArgs),
    /// Clear setup state so Xana can be initialized again.
    #[command(visible_alias = "clean")]
    Reset(ResetArgs),
    /// Inspect the active configuration.
    Config(ConfigArgs),
    /// List, inspect, or select conversations without entering chat.
    Session(SessionArgs),
    /// Inspect or explicitly reconcile interrupted operations.
    Operation(OperationArgs),
    /// Inspect or manage model-provider connections.
    Connection(ConnectionArgs),
    /// List, refresh, and select connection-owned models.
    Model(ModelArgs),
    /// Inspect exact child task routes without starting work.
    Route(RouteArgs),
    /// Deprecated compatibility alias for connection login/status/logout.
    #[command(hide = true)]
    Auth(AuthArgs),
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ServeArgs {
    /// Loopback address to bind. Non-loopback addresses are rejected.
    #[arg(long, default_value = "127.0.0.1")]
    pub(crate) bind: IpAddr,

    /// Local port, or zero to let the operating system select one.
    #[arg(long, default_value_t = 0)]
    pub(crate) port: u16,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct AttachArgs {
    /// Explicitly acquire controller authority for the hosted conversation.
    #[arg(long)]
    pub(crate) control: bool,

    /// Explicitly replace the current controller.
    #[arg(long, requires = "control")]
    pub(crate) takeover: bool,

    /// Submit one prompt after acquiring control.
    #[arg(long, requires = "control", value_name = "PROMPT")]
    pub(crate) prompt: Option<String>,

    /// Retrieve one authorized bounded artifact preview by immutable id.
    #[arg(long, value_name = "ARTIFACT_ID")]
    pub(crate) artifact: Option<ArtifactId>,
}

#[derive(Debug, Args, PartialEq, Eq, Default)]
pub(crate) struct SetupArgs {
    /// Never prompt; require durable choices as flags.
    #[arg(long)]
    pub(crate) non_interactive: bool,

    /// Select a provider or managed-runtime kind.
    #[arg(long, value_enum, value_name = "KIND")]
    pub(crate) kind: Option<ConnectionKindChoice>,

    /// Name the connection.
    #[arg(long, value_name = "NAME")]
    pub(crate) connection: Option<String>,

    /// Override the provider's absolute HTTP(S) base URL.
    #[arg(long, value_name = "URL")]
    pub(crate) base_url: Option<String>,

    /// Override the managed Codex executable.
    #[arg(long, value_name = "PROGRAM")]
    pub(crate) codex_program: Option<String>,

    /// Isolate managed Codex in an absolute CODEX_HOME.
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_home: Option<PathBuf>,

    /// Resolve an API key from this environment variable.
    #[arg(long, value_name = "VARIABLE", conflicts_with = "key_from_stdin")]
    pub(crate) credential_env: Option<String>,

    /// Read one API key from stdin; never place secrets in argv.
    #[arg(long, conflicts_with = "credential_env")]
    pub(crate) key_from_stdin: bool,

    /// Select an exact model from the established live catalog.
    #[arg(long, value_name = "MODEL")]
    pub(crate) model: Option<String>,

    /// Select a managed model's advertised reasoning effort.
    #[arg(long, value_name = "EFFORT")]
    pub(crate) reasoning_effort: Option<String>,

    /// Select the default permission policy.
    #[arg(long, value_enum, value_name = "MODE")]
    pub(crate) permission_mode: Option<PermissionChoice>,

    /// Confirm installation without a final terminal prompt.
    #[arg(long)]
    pub(crate) yes: bool,

    /// Establish and validate the connection but do not mutate durable state.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Run every advanced setup section in one staged review.
    #[arg(long, conflicts_with = "section")]
    pub(crate) full: bool,

    /// Rerun one focused setup section.
    #[arg(long, value_enum)]
    pub(crate) section: Option<SetupSectionChoice>,

    /// Configure the native command shell.
    #[arg(long, value_enum)]
    pub(crate) shell: Option<ShellChoice>,

    /// Override the selected shell executable.
    #[arg(long, requires = "shell")]
    pub(crate) shell_program: Option<PathBuf>,

    /// Set a comma-separated logical capability set on the edited profile.
    #[arg(long)]
    pub(crate) capabilities: Option<String>,

    /// Name the profile edited by profiles/routes setup.
    #[arg(long)]
    pub(crate) profile: Option<String>,

    /// Select the exact connection for the edited profile.
    #[arg(long)]
    pub(crate) profile_connection: Option<String>,

    /// Select the exact model for the edited profile.
    #[arg(long)]
    pub(crate) profile_model: Option<String>,

    /// Name the exact task route edited by profiles/routes setup.
    #[arg(long)]
    pub(crate) route: Option<String>,

    /// Select the exact profile for the edited route.
    #[arg(long)]
    pub(crate) route_profile: Option<String>,

    #[arg(long)]
    pub(crate) max_fan_out: Option<usize>,
    #[arg(long)]
    pub(crate) max_descendants: Option<usize>,
    #[arg(long)]
    pub(crate) max_concurrency: Option<usize>,
    #[arg(long)]
    pub(crate) deadline_seconds: Option<u64>,
    #[arg(long)]
    pub(crate) max_context_tokens: Option<usize>,
    #[arg(long)]
    pub(crate) max_report_bytes: Option<usize>,
    #[arg(long)]
    pub(crate) max_artifact_bytes: Option<usize>,

    /// Add a validated permission rule as ID:DECISION:EFFECT[:WORKSPACE].
    #[arg(long, value_name = "RULE")]
    pub(crate) permission_rule: Vec<String>,

    #[arg(long, value_enum)]
    pub(crate) theme: Option<ThemeChoice>,
    #[arg(long, value_enum)]
    pub(crate) glyphs: Option<GlyphChoice>,
    #[arg(long, value_enum)]
    pub(crate) motion: Option<MotionChoice>,
    #[arg(long, value_enum)]
    pub(crate) density: Option<DensityChoice>,
    #[arg(long, value_enum)]
    pub(crate) composer: Option<ComposerChoice>,
    #[arg(long, value_enum)]
    pub(crate) activity: Option<ActivityChoice>,

    /// Start a new conversation immediately after a new-session change.
    #[arg(long)]
    pub(crate) start_new: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ResetScopeChoice {
    Setup,
    Sessions,
    Caches,
    Credentials,
    All,
}

#[derive(Debug, Args, PartialEq, Eq, Default)]
pub(crate) struct ResetArgs {
    /// Select one or more exact reset scopes; the compatibility default is setup.
    #[arg(long, value_enum, action = clap::ArgAction::Append)]
    pub(crate) scope: Vec<ResetScopeChoice>,

    /// Confirm removal without reading from the terminal.
    #[arg(long)]
    pub(crate) yes: bool,

    /// Separately confirm deletion of referenced OS-stored API credentials.
    #[arg(long)]
    pub(crate) credentials_yes: bool,

    /// Print exact targets and preservation promises without changing state.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args, PartialEq, Eq, Default)]
pub(crate) struct DoctorArgs {
    /// Apply only the deterministic repairs listed in the diagnostic preview.
    #[arg(long)]
    pub(crate) fix: bool,

    /// Confirm deterministic repairs without reading from the terminal.
    #[arg(long, requires = "fix")]
    pub(crate) yes: bool,

    /// Select human-readable or versioned JSON output.
    #[arg(long, value_enum, default_value_t = OutputChoice::Text)]
    pub(crate) output: OutputChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ConnectionKindChoice {
    Ollama,
    #[value(name = "openai_compat")]
    OpenAiCompat,
    OpenAi,
    OpenRouter,
    Anthropic,
    Codex,
}

impl From<ConnectionKindChoice> for crate::config::ProviderKind {
    fn from(value: ConnectionKindChoice) -> Self {
        match value {
            ConnectionKindChoice::Ollama => Self::Ollama,
            ConnectionKindChoice::OpenAiCompat => Self::OpenAiCompat,
            ConnectionKindChoice::OpenAi => Self::OpenAi,
            ConnectionKindChoice::OpenRouter => Self::OpenRouter,
            ConnectionKindChoice::Anthropic => Self::Anthropic,
            ConnectionKindChoice::Codex => Self::Codex,
        }
    }
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ConnectionArgs {
    #[command(subcommand)]
    pub(crate) command: ConnectionCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum ConnectionCommand {
    /// List configured native providers and managed runtimes.
    List,
    /// Add a connection declaration without storing plaintext credentials.
    Add {
        id: String,
        #[arg(long, value_enum)]
        kind: ConnectionKindChoice,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long, conflicts_with = "credential_id")]
        env: Option<String>,
        #[arg(long, conflicts_with = "env")]
        credential_id: Option<String>,
        #[arg(long)]
        model: String,
        #[arg(long)]
        codex_program: Option<String>,
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// Show credential/account and runtime status for one connection.
    Status { id: String },
    /// Store an API key in the operating-system credential store.
    SetKey {
        id: String,
        /// Read the key from stdin instead of a hidden terminal prompt.
        #[arg(long)]
        from_stdin: bool,
    },
    /// Start an explicit managed-runtime login.
    Login {
        id: String,
        /// Use a headless-friendly device-code flow.
        #[arg(long)]
        device_code: bool,
    },
    /// Log out of a managed account. This may affect other Codex clients using the same CODEX_HOME.
    Logout {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Delete an API key from the operating-system credential store.
    DeleteKey { id: String },
    /// Refresh and cache non-secret model metadata.
    Refresh { id: String },
    /// Remove an unreferenced connection declaration.
    Remove {
        id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ModelArgs {
    #[command(subcommand)]
    pub(crate) command: Option<ModelCommand>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum ModelCommand {
    /// List models, optionally for only one connection.
    List {
        #[arg(long)]
        connection: Option<String>,
    },
    /// Persist the next-conversation selection as CONNECTION/MODEL.
    Use {
        selection: String,
        /// Managed Codex reasoning effort advertised by the selected model.
        #[arg(long)]
        effort: Option<String>,
        /// Managed Codex reasoning summary: auto, concise, detailed, or off.
        #[arg(long)]
        summary: Option<String>,
    },
    /// Explicitly refresh one connection's catalog.
    Refresh { connection: String },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct RouteArgs {
    #[command(subcommand)]
    pub(crate) command: RouteCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum RouteCommand {
    /// List configured routes and local availability.
    List,
    /// Resolve one route into its immutable child configuration.
    Check { route: String },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct AuthArgs {
    #[command(subcommand)]
    pub(crate) command: AuthCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum AuthCommand {
    /// Start device authorization for a named provider.
    Login { provider: String },
    /// Show bounded credential status without printing secrets.
    Status { provider: String },
    /// Remove a stored credential.
    Logout { provider: String },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct InitArgs {
    /// Require all setup values from flags and never read stdin.
    #[arg(long)]
    pub(crate) non_interactive: bool,

    /// Select the first connection kind (defaults to openai_compat for compatibility).
    #[arg(long, value_enum, value_name = "KIND")]
    pub(crate) kind: Option<InitConnectionKindChoice>,

    /// Name the first provider or managed-runtime connection.
    #[arg(long, value_name = "NAME")]
    pub(crate) provider_name: Option<String>,

    /// Set the provider's absolute HTTP(S) base URL.
    #[arg(long, value_name = "URL")]
    pub(crate) base_url: Option<String>,

    /// Override the Codex executable for a managed Codex connection.
    #[arg(long, value_name = "PROGRAM")]
    pub(crate) codex_program: Option<String>,

    /// Isolate a managed Codex connection in an absolute CODEX_HOME.
    #[arg(long, value_name = "PATH")]
    pub(crate) codex_home: Option<PathBuf>,

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

    /// Select the default permission policy for all tools.
    #[arg(long, value_enum, value_name = "MODE")]
    pub(crate) permission_mode: Option<PermissionChoice>,

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
    /// Edit a bounded temporary copy and install it only after validation.
    Edit {
        /// Exact editor executable; defaults to XANA_EDITOR, VISUAL, EDITOR, or a platform editor.
        #[arg(long, value_name = "PROGRAM")]
        editor: Option<PathBuf>,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct SessionArgs {
    #[command(subcommand)]
    pub(crate) command: SessionCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum SessionCommand {
    /// List bounded conversations for the canonical current workspace.
    List,
    /// Print bounded metadata without conversation content.
    Inspect { session_id: SessionId },
    /// Select one retained managed conversation for its next resume.
    SelectManaged {
        connection: String,
        thread_id: String,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct OperationArgs {
    #[command(subcommand)]
    pub(crate) command: OperationCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum OperationCommand {
    /// Render a read-only recovery plan without arguments.
    Plan {
        #[arg(long)]
        session: SessionId,
        operation_id: crate::identity::OperationId,
    },
    /// Explicitly reconcile one unfinished operation.
    Resume {
        #[arg(long)]
        session: SessionId,
        operation_id: crate::identity::OperationId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn no_subcommand_means_normal_chat() {
        let cli = Cli::try_parse_from(["xana"]).expect("bare invocation");

        assert!(!cli.no_banner);
        assert_eq!(cli.resume, None);
        assert_eq!(cli.command, None);
    }

    #[test]
    fn parses_plain_tui_and_one_shot_surface_contracts() {
        let plain = Cli::try_parse_from(["xana", "--plain"]).expect("plain surface");
        assert!(plain.plain);
        assert!(!plain.tui);

        let tui = Cli::try_parse_from(["xana", "--tui"]).expect("TUI surface");
        assert!(tui.tui);
        assert!(!tui.plain);

        let argument =
            Cli::try_parse_from(["xana", "-p", "hello"]).expect("one-shot argument surface");
        assert_eq!(argument.print, Some(Some("hello".to_owned())));

        let stdin = Cli::try_parse_from(["xana", "--print"]).expect("one-shot stdin surface");
        assert_eq!(stdin.print, Some(None));

        let json = Cli::try_parse_from(["xana", "--json", "-p", "hello"]).expect("JSON alias");
        assert!(json.json);
        assert_eq!(json.output, None);

        let continued = Cli::try_parse_from(["xana", "--continue"]).expect("continuation");
        assert!(continued.continue_chat);
        let compatibility =
            Cli::try_parse_from(["xana", "--continue-chat"]).expect("continuation alias");
        assert!(compatibility.continue_chat);

        assert!(Cli::try_parse_from(["xana", "--plain", "--tui"]).is_err());
        let conflict = Cli::try_parse_from([
            "xana",
            "--resume",
            "9eb8cfe0-2b3a-4c7b-9dc9-0a34f6490bf3",
            "--continue",
        ])
        .expect_err("resume and continue conflict");
        assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parses_auth_lifecycle_commands() {
        assert_eq!(
            Cli::try_parse_from(["xana", "auth", "status", "codex"])
                .expect("auth status")
                .command,
            Some(Command::Auth(AuthArgs {
                command: AuthCommand::Status {
                    provider: "codex".to_owned(),
                },
            }))
        );
    }

    #[test]
    fn parses_connection_and_model_control_plane() {
        assert_eq!(
            Cli::try_parse_from([
                "xana",
                "connection",
                "add",
                "codex",
                "--kind",
                "codex",
                "--model",
                "gpt-5.3-codex",
            ])
            .unwrap()
            .command,
            Some(Command::Connection(ConnectionArgs {
                command: ConnectionCommand::Add {
                    id: "codex".into(),
                    kind: ConnectionKindChoice::Codex,
                    base_url: None,
                    env: None,
                    credential_id: None,
                    model: "gpt-5.3-codex".into(),
                    codex_program: None,
                    codex_home: None,
                }
            }))
        );
        assert_eq!(
            Cli::try_parse_from(["xana", "model", "use", "openrouter/openai/gpt-4.1"])
                .unwrap()
                .command,
            Some(Command::Model(ModelArgs {
                command: Some(ModelCommand::Use {
                    selection: "openrouter/openai/gpt-4.1".into(),
                    effort: None,
                    summary: None,
                })
            }))
        );
        assert_eq!(
            Cli::try_parse_from([
                "xana",
                "model",
                "use",
                "codex/gpt-5.6-sol",
                "--effort",
                "xhigh",
                "--summary",
                "detailed",
            ])
            .unwrap()
            .command,
            Some(Command::Model(ModelArgs {
                command: Some(ModelCommand::Use {
                    selection: "codex/gpt-5.6-sol".into(),
                    effort: Some("xhigh".into()),
                    summary: Some("detailed".into()),
                })
            }))
        );
    }

    #[test]
    fn parses_read_only_route_diagnostics() {
        assert_eq!(
            Cli::try_parse_from(["xana", "route", "list"])
                .expect("route list")
                .command,
            Some(Command::Route(RouteArgs {
                command: RouteCommand::List,
            }))
        );
        assert_eq!(
            Cli::try_parse_from(["xana", "route", "check", "worker"])
                .expect("route check")
                .command,
            Some(Command::Route(RouteArgs {
                command: RouteCommand::Check {
                    route: "worker".into(),
                },
            }))
        );
    }

    #[test]
    fn parses_interactive_init() {
        let cli =
            Cli::try_parse_from(["xana", "init", "--dry-run"]).expect("interactive initialization");

        assert_eq!(
            cli.command,
            Some(Command::Init(InitArgs {
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
                dry_run: true,
            }))
        );
    }

    #[test]
    fn parses_guarded_reset_and_clean_alias() {
        let reset = Cli::try_parse_from(["xana", "reset", "--yes"]).expect("reset command");
        let clean = Cli::try_parse_from(["xana", "clean"]).expect("clean alias");

        assert_eq!(
            reset.command,
            Some(Command::Reset(ResetArgs {
                yes: true,
                ..ResetArgs::default()
            }))
        );
        assert_eq!(
            clean.command,
            Some(Command::Reset(ResetArgs {
                yes: false,
                ..ResetArgs::default()
            }))
        );

        let scoped = Cli::try_parse_from([
            "xana",
            "reset",
            "--scope",
            "sessions",
            "--scope",
            "credentials",
            "--yes",
            "--credentials-yes",
        ])
        .expect("scoped reset");
        assert_eq!(
            scoped.command,
            Some(Command::Reset(ResetArgs {
                scope: vec![ResetScopeChoice::Sessions, ResetScopeChoice::Credentials],
                yes: true,
                credentials_yes: true,
                dry_run: false,
            }))
        );

        let doctor =
            Cli::try_parse_from(["xana", "doctor", "--output", "json"]).expect("doctor command");
        assert_eq!(
            doctor.command,
            Some(Command::Doctor(DoctorArgs {
                output: OutputChoice::Json,
                ..DoctorArgs::default()
            }))
        );
    }

    #[test]
    fn parses_loopback_serve_and_workspace_attach() {
        let serve = Cli::try_parse_from(["xana", "serve", "--bind", "::1", "--port", "43123"])
            .expect("serve command");
        let attach = Cli::try_parse_from(["xana", "attach"]).expect("attach command");

        assert_eq!(
            serve.command,
            Some(Command::Serve(ServeArgs {
                bind: "::1".parse().unwrap(),
                port: 43123,
            }))
        );

        let artifact_id = ArtifactId::new();
        let artifact =
            Cli::try_parse_from(["xana", "attach", "--artifact", &artifact_id.to_string()])
                .expect("artifact attachment");
        assert!(matches!(
            artifact.command,
            Some(Command::Attach(AttachArgs {
                artifact: Some(actual),
                ..
            })) if actual == artifact_id
        ));
        assert_eq!(
            attach.command,
            Some(Command::Attach(AttachArgs {
                control: false,
                takeover: false,
                prompt: None,
                artifact: None,
            }))
        );

        let controller = Cli::try_parse_from([
            "xana",
            "attach",
            "--control",
            "--takeover",
            "--prompt",
            "continue",
        ])
        .expect("controller attach");
        assert_eq!(
            controller.command,
            Some(Command::Attach(AttachArgs {
                control: true,
                takeover: true,
                prompt: Some("continue".into()),
                artifact: None,
            }))
        );
    }

    #[test]
    fn parses_complete_noninteractive_init() {
        let cli = Cli::try_parse_from([
            "xana",
            "init",
            "--non-interactive",
            "--kind",
            "ollama",
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
            "--permission-mode",
            "ask",
        ])
        .expect("complete noninteractive initialization");

        assert_eq!(
            cli.command,
            Some(Command::Init(InitArgs {
                non_interactive: true,
                kind: Some(InitConnectionKindChoice::Ollama),
                provider_name: Some("ollama".to_owned()),
                base_url: Some("http://localhost:11434/v1".to_owned()),
                codex_program: None,
                codex_home: None,
                model: Some("qwen3:1.7b".to_owned()),
                max_tool_rounds: Some(12),
                shell: Some(ShellChoice::PowerShell),
                shell_program: Some(PathBuf::from("pwsh.exe")),
                permission_mode: Some(PermissionChoice::Ask),
                dry_run: false,
            }))
        );
    }

    #[test]
    fn parses_provider_neutral_noninteractive_setup_without_secret_argv() {
        let cli = Cli::try_parse_from([
            "xana",
            "setup",
            "--non-interactive",
            "--kind",
            "open-router",
            "--connection",
            "openrouter",
            "--credential-env",
            "OPENROUTER_API_KEY",
            "--model",
            "openai/gpt-4.1",
            "--permission-mode",
            "ask",
            "--yes",
        ])
        .expect("complete setup");

        assert_eq!(
            cli.command,
            Some(Command::Setup(Box::new(SetupArgs {
                non_interactive: true,
                kind: Some(ConnectionKindChoice::OpenRouter),
                connection: Some("openrouter".into()),
                base_url: None,
                codex_program: None,
                codex_home: None,
                credential_env: Some("OPENROUTER_API_KEY".into()),
                key_from_stdin: false,
                model: Some("openai/gpt-4.1".into()),
                reasoning_effort: None,
                permission_mode: Some(PermissionChoice::Ask),
                yes: true,
                dry_run: false,
                ..SetupArgs::default()
            })))
        );
    }

    #[test]
    fn parses_exact_sectional_setup_operations() {
        let appearance = Cli::try_parse_from([
            "xana",
            "setup",
            "--non-interactive",
            "--section",
            "appearance",
            "--theme",
            "monochrome",
            "--motion",
            "reduced",
            "--yes",
        ])
        .unwrap();
        assert!(matches!(
            appearance.command,
            Some(Command::Setup(args)) if matches!(*args, SetupArgs {
                section: Some(SetupSectionChoice::Appearance),
                theme: Some(ThemeChoice::Monochrome),
                motion: Some(MotionChoice::Reduced),
                ..
            })
        ));

        let routes = Cli::try_parse_from([
            "xana",
            "setup",
            "--non-interactive",
            "--section",
            "profiles-routes",
            "--profile",
            "reviewer",
            "--profile-connection",
            "ollama",
            "--profile-model",
            "qwen",
            "--route",
            "review",
            "--route-profile",
            "reviewer",
            "--max-concurrency",
            "2",
            "--yes",
        ])
        .unwrap();
        assert!(matches!(
            routes.command,
            Some(Command::Setup(args)) if matches!(*args, SetupArgs {
                section: Some(SetupSectionChoice::ProfilesRoutes),
                profile: Some(_),
                route: Some(_),
                max_concurrency: Some(2),
                ..
            })
        ));
    }

    #[test]
    fn parses_managed_codex_init() {
        let cli = Cli::try_parse_from([
            "xana",
            "init",
            "--non-interactive",
            "--kind",
            "codex",
            "--provider-name",
            "codex",
            "--codex-program",
            "codex-preview",
            "--model",
            "gpt-5.6-sol",
            "--permission-mode",
            "ask",
        ])
        .expect("managed Codex initialization");

        assert!(matches!(
            cli.command,
            Some(Command::Init(InitArgs {
                kind: Some(InitConnectionKindChoice::Codex),
                codex_program: Some(program),
                base_url: None,
                ..
            })) if program == "codex-preview"
        ));
    }

    #[test]
    fn parses_config_path_check_and_edit() {
        let path = Cli::try_parse_from(["xana", "config", "path"]).expect("config path command");
        let check = Cli::try_parse_from(["xana", "config", "check"]).expect("config check command");
        let edit = Cli::try_parse_from(["xana", "config", "edit", "--editor", "code"])
            .expect("config edit command");

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
        assert_eq!(
            edit.command,
            Some(Command::Config(ConfigArgs {
                command: ConfigCommand::Edit {
                    editor: Some(PathBuf::from("code")),
                },
            }))
        );
    }

    #[test]
    fn parses_explicit_resume_and_session_inspection() {
        let id = SessionId::new();
        let resume =
            Cli::try_parse_from(["xana", "--resume", &id.to_string()]).expect("resume argument");
        let inspect = Cli::try_parse_from(["xana", "session", "inspect", &id.to_string()])
            .expect("session inspect command");

        assert_eq!(resume.resume, Some(id));
        assert_eq!(resume.command, None);
        assert_eq!(
            inspect.command,
            Some(Command::Session(SessionArgs {
                command: SessionCommand::Inspect { session_id: id },
            }))
        );
        assert!(matches!(
            Cli::try_parse_from(["xana", "session", "list"])
                .unwrap()
                .command,
            Some(Command::Session(SessionArgs {
                command: SessionCommand::List
            }))
        ));
        assert!(matches!(
            Cli::try_parse_from(["xana", "session", "select-managed", "codex", "thread-1"])
                .unwrap()
                .command,
            Some(Command::Session(SessionArgs {
                command: SessionCommand::SelectManaged { .. }
            }))
        ));
    }

    #[test]
    fn parses_operation_plan_and_resume() {
        let session_id = SessionId::new();
        let operation_id = crate::identity::OperationId::new();
        for action in ["plan", "resume"] {
            let parsed = Cli::try_parse_from([
                "xana",
                "operation",
                action,
                "--session",
                &session_id.to_string(),
                &operation_id.to_string(),
            ])
            .expect("operation command");
            assert!(matches!(parsed.command, Some(Command::Operation(_))));
        }
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
