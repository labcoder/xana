//! Typed command-line syntax.
//!
//! Clap turns process arguments into command data only; command execution and
//! terminal/filesystem effects remain at the application edge.

use crate::{
    config::PermissionMode,
    identity::{ArtifactId, ProjectId, SessionId},
    shell::ShellKind,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{net::IpAddr, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ShellChoice {
    Platform,
    Posix,
    #[value(name = "git-bash", alias = "git_bash")]
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
    #[value(name = "openai-compatible", alias = "openai_compat")]
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
    #[value(name = "show", alias = "open")]
    Open,
    #[value(name = "hide", alias = "hidden")]
    Hidden,
}

#[derive(Debug, Parser)]
#[command(
    name = "xana",
    version,
    about = "A personal AI agent harness for interactive and automated work",
    after_help = "Run `xana` to start an interactive conversation, `xana -p PROMPT` for one automated turn, or `xana setup` to configure connections."
)]
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
    /// Run guided, quick, full, or focused atomic configuration setup.
    #[command(display_order = 1)]
    Setup(Box<SetupArgs>),
    /// Diagnose this Xana installation without mutating it by default.
    #[command(display_order = 2)]
    Doctor(DoctorArgs),
    /// Inspect the active configuration.
    #[command(display_order = 3)]
    Config(ConfigArgs),
    /// Clear setup state so Xana can be initialized again.
    #[command(visible_alias = "clean", display_order = 4)]
    Reset(ResetArgs),
    /// Inspect or manage model-provider connections.
    #[command(display_order = 5)]
    Connection(ConnectionArgs),
    /// List, refresh, and select connection-owned models.
    #[command(display_order = 6)]
    Model(ModelArgs),
    /// List, create, inspect, select, or archive conversations.
    #[command(display_order = 7)]
    Session(SessionArgs),
    /// Organize conversations with optional local Xana projects.
    #[command(display_order = 8)]
    Project(ProjectArgs),
    /// Create, inspect, resolve, and freeze named agent profiles.
    #[command(display_order = 9)]
    Profile(ProfileArgs),
    /// Discover, validate, and explicitly activate Agent Skills.
    #[command(display_order = 10)]
    Skill(SkillArgs),
    /// Inspect and manage declarative Agent Plugin bundles.
    #[command(display_order = 11)]
    Plugin(PluginArgs),
    /// Host Xana explicitly for authenticated local frontend attachment.
    #[command(display_order = 20)]
    Serve(ServeArgs),
    /// Attach to this workspace's foreground host; observation is the default.
    #[command(display_order = 21)]
    Attach(AttachArgs),
    /// Deprecated through v0.5.0; use `xana setup`.
    #[command(hide = true)]
    Init(InitArgs),
    /// Inspect or explicitly reconcile interrupted operations.
    #[command(display_order = 22)]
    Operation(OperationArgs),
    /// Inspect exact child task routes without starting work.
    #[command(display_order = 23)]
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

#[derive(Debug, Clone, Args, PartialEq, Eq, Default)]
pub(crate) struct SetupArgs {
    /// Check local readiness and enter setup only when an interactive repair is needed.
    #[arg(long, help_heading = "Setup mode")]
    pub(crate) if_needed: bool,

    /// Never prompt; require durable choices as flags.
    #[arg(long, help_heading = "Setup mode")]
    pub(crate) non_interactive: bool,

    /// Run provider-neutral Quick Setup without asking which setup path to use.
    #[arg(
        long,
        conflicts_with_all = ["full", "section"],
        help_heading = "Setup mode"
    )]
    pub(crate) quick: bool,

    /// Select a provider or managed-runtime kind.
    #[arg(long, value_enum, value_name = "KIND", help_heading = "Connection")]
    pub(crate) kind: Option<ConnectionKindChoice>,

    /// Name the connection.
    #[arg(long, value_name = "NAME", help_heading = "Connection")]
    pub(crate) connection: Option<String>,

    /// Override the provider's absolute HTTP(S) base URL.
    #[arg(long, value_name = "URL", help_heading = "Connection")]
    pub(crate) base_url: Option<String>,

    /// Override the managed Codex executable.
    #[arg(long, value_name = "PROGRAM", help_heading = "Connection")]
    pub(crate) codex_program: Option<String>,

    /// Isolate managed Codex in an absolute CODEX_HOME.
    #[arg(long, value_name = "PATH", help_heading = "Connection")]
    pub(crate) codex_home: Option<PathBuf>,

    /// Resolve an API key from this environment variable.
    #[arg(
        long,
        value_name = "VARIABLE",
        conflicts_with = "key_from_stdin",
        help_heading = "Connection"
    )]
    pub(crate) credential_env: Option<String>,

    /// Read one API key from stdin; never place secrets in argv.
    #[arg(long, conflicts_with = "credential_env", help_heading = "Connection")]
    pub(crate) key_from_stdin: bool,

    /// Select an exact model from the established live catalog.
    #[arg(long, value_name = "MODEL", help_heading = "Connection")]
    pub(crate) model: Option<String>,

    /// Select a managed model's advertised reasoning effort.
    #[arg(long, value_name = "EFFORT", help_heading = "Connection")]
    pub(crate) reasoning_effort: Option<String>,

    /// Select the default permission policy.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        help_heading = "Permissions and shell"
    )]
    pub(crate) permission_mode: Option<PermissionChoice>,

    /// Confirm installation without a final terminal prompt.
    #[arg(long, help_heading = "Setup mode")]
    pub(crate) yes: bool,

    /// Establish and validate the connection but do not mutate durable state.
    #[arg(long, help_heading = "Setup mode")]
    pub(crate) dry_run: bool,

    /// Run every advanced setup section in one staged review.
    #[arg(long, conflicts_with = "section", help_heading = "Setup mode")]
    pub(crate) full: bool,

    /// Rerun one focused setup section.
    #[arg(long, value_enum, help_heading = "Setup mode")]
    pub(crate) section: Option<SetupSectionChoice>,

    /// Configure the native command shell.
    #[arg(long, value_enum, help_heading = "Permissions and shell")]
    pub(crate) shell: Option<ShellChoice>,

    /// Override the selected shell executable.
    #[arg(long, requires = "shell", help_heading = "Permissions and shell")]
    pub(crate) shell_program: Option<PathBuf>,

    /// Set a comma-separated logical capability set on the edited profile.
    #[arg(long, help_heading = "Profiles and routes")]
    pub(crate) capabilities: Option<String>,

    /// Name the profile edited by profiles/routes setup.
    #[arg(long, help_heading = "Profiles and routes")]
    pub(crate) profile: Option<String>,

    /// Select the exact connection for the edited profile.
    #[arg(long, help_heading = "Profiles and routes")]
    pub(crate) profile_connection: Option<String>,

    /// Select the exact model for the edited profile.
    #[arg(long, help_heading = "Profiles and routes")]
    pub(crate) profile_model: Option<String>,

    /// Name the exact task route edited by profiles/routes setup.
    #[arg(long, help_heading = "Profiles and routes")]
    pub(crate) route: Option<String>,

    /// Select the exact profile for the edited route.
    #[arg(long, help_heading = "Profiles and routes")]
    pub(crate) route_profile: Option<String>,

    /// Limit the number of children admitted by one orchestration step.
    #[arg(long, help_heading = "Orchestration limits")]
    pub(crate) max_fan_out: Option<usize>,
    /// Limit total descendants admitted beneath one root.
    #[arg(long, help_heading = "Orchestration limits")]
    pub(crate) max_descendants: Option<usize>,
    /// Limit concurrently running child executions.
    #[arg(long, help_heading = "Orchestration limits")]
    pub(crate) max_concurrency: Option<usize>,
    /// Bound orchestration wall-clock time in seconds.
    #[arg(long, help_heading = "Orchestration limits")]
    pub(crate) deadline_seconds: Option<u64>,
    /// Bound cumulative child context usage.
    #[arg(long, help_heading = "Orchestration limits")]
    pub(crate) max_context_tokens: Option<usize>,
    /// Bound retained child report bytes.
    #[arg(long, help_heading = "Orchestration limits")]
    pub(crate) max_report_bytes: Option<usize>,
    /// Bound retained child artifact bytes.
    #[arg(long, help_heading = "Orchestration limits")]
    pub(crate) max_artifact_bytes: Option<usize>,

    /// Add a validated permission rule as ID:DECISION:EFFECT[:WORKSPACE].
    #[arg(long, value_name = "RULE", help_heading = "Permissions and shell")]
    pub(crate) permission_rule: Vec<String>,

    /// Select the semantic color theme.
    #[arg(long, value_enum, help_heading = "Appearance")]
    pub(crate) theme: Option<ThemeChoice>,
    /// Select Unicode or ASCII presentation glyphs.
    #[arg(long, value_enum, help_heading = "Appearance")]
    pub(crate) glyphs: Option<GlyphChoice>,
    /// Select full or reduced presentation motion.
    #[arg(long, value_enum, help_heading = "Appearance")]
    pub(crate) motion: Option<MotionChoice>,
    /// Select comfortable or compact terminal density.
    #[arg(long, value_enum, help_heading = "Appearance")]
    pub(crate) density: Option<DensityChoice>,
    /// Select whether Enter submits or inserts a newline.
    #[arg(long, value_enum, help_heading = "Appearance")]
    pub(crate) composer: Option<ComposerChoice>,
    /// Select automatic, shown, or hidden activity presentation.
    #[arg(long, value_enum, help_heading = "Appearance")]
    pub(crate) activity: Option<ActivityChoice>,

    /// Start a new conversation immediately after a new-session change.
    #[arg(long, help_heading = "Setup mode")]
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
    #[value(name = "openai-compatible", alias = "openai_compat")]
    OpenAiCompat,
    #[value(name = "openai", alias = "open-ai")]
    OpenAi,
    #[value(name = "openrouter", alias = "open-router")]
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
    #[command(hide = true)]
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
    /// Review or explicitly apply the current configuration/private-state migration.
    Migrate {
        /// Apply the reviewed migration; without this flag the command is read-only.
        #[arg(long)]
        apply: bool,
    },
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
    /// Start Xana in a fresh native session or managed thread.
    New,
    /// Print bounded metadata without conversation content.
    Inspect { session_id: SessionId },
    /// Select one retained managed conversation for its next resume.
    #[command(name = "select", alias = "select-managed")]
    SelectManaged {
        connection: String,
        thread_id: String,
    },
    /// Remove one local managed conversation handle without deleting the vendor thread.
    #[command(name = "archive", alias = "archive-managed")]
    ArchiveManaged {
        connection: String,
        thread_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProjectOwnerChoice {
    Native,
    #[value(name = "codex")]
    ManagedCodex,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ProjectArgs {
    #[command(subcommand)]
    pub(crate) command: ProjectCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum ProjectCommand {
    /// List active projects, optionally including archived projects.
    List {
        #[arg(long)]
        all: bool,
    },
    /// Create a private project for one existing workspace directory.
    Create {
        name: String,
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
    },
    /// Inspect one project and its local workspace health.
    Inspect { project_id: ProjectId },
    /// Rename a project without changing its workspace or conversations.
    Rename { project_id: ProjectId, name: String },
    /// Hide a project from the active list without deleting owned data.
    Archive { project_id: ProjectId },
    /// Restore an archived project to the active list.
    Unarchive { project_id: ProjectId },
    /// Explicitly bind a missing or moved project to a replacement workspace.
    Relink {
        project_id: ProjectId,
        workspace: PathBuf,
    },
    /// Forget local project organization without deleting workspace or history.
    Forget {
        project_id: ProjectId,
        #[arg(long)]
        yes: bool,
    },
    /// Assign an existing same-workspace conversation to a project.
    Assign {
        project_id: ProjectId,
        conversation: String,
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
    },
    /// Return a conversation to the Ungrouped view.
    Ungroup { conversation: String },
    /// Inspect the optional project membership for one conversation.
    Membership { conversation: String },
    /// Review owner-correct same- or cross-workspace continuation placement.
    Continue {
        project_id: ProjectId,
        conversation: String,
        #[arg(long, value_name = "PATH")]
        source_workspace: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "native")]
        owner: ProjectOwnerChoice,
        /// Resolve and freeze this profile for the target conversation.
        #[arg(long)]
        profile: Option<String>,
        /// Commit the reviewed assignment/continuation. Without this, no state changes.
        #[arg(long)]
        apply: bool,
    },
    /// Explicitly create the minimal `.agents/xana/project.toml` for a project.
    Share { project_id: ProjectId },
    /// Read and validate portable project metadata without persistent mutation.
    InspectPortable {
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
    },
    /// Register an inspected portable project locally without enabling integrations.
    Register {
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
    },
    /// Accept the currently reviewed portable manifest digest for a registered project.
    Refresh { project_id: ProjectId },
    /// Compare the current portable manifest with its last registered digest.
    Diff { project_id: ProjectId },
    /// Bind one portable logical name to a private local connection/service name.
    Bind {
        project_id: ProjectId,
        logical: String,
        local: String,
    },
    /// Show redacted setup/readiness without exposing private binding values.
    #[command(visible_alias = "resolve")]
    Setup { project_id: ProjectId },
    /// Remove only the shared manifest; keep the local project and bindings.
    StopSharing {
        project_id: ProjectId,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct ProfileArgs {
    #[command(subcommand)]
    pub(crate) command: ProfileCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum ProfileCommand {
    /// List active profiles in global or project scope.
    List {
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        all: bool,
    },
    /// Create an arbitrary named profile. Project profiles require an authority ceiling.
    Create {
        name: String,
        #[arg(long)]
        connection: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long, requires = "project")]
        authority_profile: Option<String>,
    },
    /// Inspect one profile without resolving provider or process availability.
    Inspect {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Edit the common selection and guidance fields of one profile.
    Edit {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        connection: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        reasoning_effort: Option<String>,
        #[arg(long)]
        reasoning_summary: Option<String>,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long, value_enum)]
        permission_mode: Option<PermissionChoice>,
        #[arg(long)]
        max_tool_rounds: Option<usize>,
    },
    /// Copy a profile to a new independent identity; profiles never inherit.
    Duplicate {
        source: String,
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Rename a profile while preserving its stable identity.
    Rename {
        old: String,
        new: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Hide a profile from active selection without deleting it.
    Archive {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Restore an archived profile.
    Unarchive {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Delete a profile after an explicit confirmation flag.
    Delete {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        yes: bool,
    },
    /// Show exact effective values, provenance, and readiness.
    Resolve {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        json: bool,
    },
    /// Freeze a resolved profile for a new or existing conversation identity.
    Freeze {
        name: String,
        conversation: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Inspect the immutable redacted profile snapshot for a conversation.
    Snapshot { conversation: String },
    /// Create a linked continuation with a different immutable profile snapshot.
    Continue {
        name: String,
        conversation: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct SkillArgs {
    #[command(subcommand)]
    pub(crate) command: SkillCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum SkillCommand {
    /// List standards-compatible metadata without loading skill bodies.
    List {
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Inspect one skill's metadata and source without loading its body.
    Inspect {
        skill: String,
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Validate one skill directory with full bounded activation checks.
    Validate { path: PathBuf },
    /// Load one exact skill and its directly referenced Markdown resources.
    Activate {
        skill: String,
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Read one contained resource from an exact discovered skill.
    Read {
        skill: String,
        resource: PathBuf,
        #[arg(long, value_name = "PATH")]
        workspace: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Enable one exact skill for new conversations using a profile.
    Enable {
        skill: String,
        #[arg(long)]
        profile: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Disable one skill reference for new conversations using a profile.
    Disable {
        skill: String,
        #[arg(long)]
        profile: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
}

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct PluginArgs {
    #[command(subcommand)]
    pub(crate) command: PluginCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(crate) enum PluginCommand {
    /// List installed packages. Installation and enablement are separate.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Inspect one installed package, its health, scopes, and rollback state.
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Validate and review a local, linked, or exact-revision Git source.
    Review {
        source: String,
        #[arg(long, requires = "revision", conflicts_with = "linked")]
        git: bool,
        #[arg(long, value_name = "COMMIT", conflicts_with = "linked")]
        revision: Option<String>,
        #[arg(long, conflicts_with_all = ["git", "revision"])]
        linked: bool,
        #[arg(long)]
        json: bool,
    },
    /// Install a reviewed bundle while keeping every contribution disabled.
    Install {
        source: String,
        #[arg(long, requires = "revision", conflicts_with = "linked")]
        git: bool,
        #[arg(long, value_name = "COMMIT", conflicts_with = "linked")]
        revision: Option<String>,
        #[arg(long, conflicts_with_all = ["git", "revision"])]
        linked: bool,
        /// Apply the reviewed install. Without this flag only a review is shown.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Enable an installed package for the user, a project, or one profile.
    Enable {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Disable an installed package in one exact scope.
    Disable {
        name: String,
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Acquire and review a candidate update without applying it.
    UpdateCheck {
        name: String,
        #[arg(long, value_name = "COMMIT")]
        revision: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Apply the exact digest produced by update-check.
    Update {
        name: String,
        #[arg(long, value_name = "COMMIT")]
        revision: Option<String>,
        #[arg(long, value_name = "DIGEST")]
        digest: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Atomically restore the prior reviewed immutable revision.
    Rollback {
        name: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove a disabled, unreferenced package installation.
    Remove {
        name: String,
        #[arg(long)]
        yes: bool,
    },
    /// Delete immutable package trees no longer referenced by lifecycle state.
    Gc {
        #[arg(long)]
        yes: bool,
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
mod tests;
