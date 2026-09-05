//! Shared, presentation-neutral command and surface-capability catalog.
//!
//! The catalog describes user intent. It does not grant authority, validate
//! domain inputs, or perform side effects; application and runtime handlers
//! remain authoritative for those concerns.

use serde::{Deserialize, Serialize};

pub(crate) const COMMAND_CATALOG_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandAction {
    Activity,
    Approval,
    Artifact,
    Attach,
    Budget,
    Capabilities,
    Child,
    Clear,
    Compact,
    Composer,
    Connection,
    Continue,
    Conversation,
    Diagnostics,
    Doctor,
    Espejo,
    ExternalAgent,
    Header,
    Help,
    Image,
    Interrupt,
    Layout,
    Mcp,
    Model,
    Newline,
    Outbound,
    Plugin,
    Profile,
    Project,
    Queue,
    Quit,
    Reasoning,
    Reset,
    Route,
    Send,
    Serve,
    Settings,
    Setup,
    Skill,
    Steer,
    Stop,
    Storage,
    Usage,
    Vision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandSurface {
    Cli,
    Plain,
    Tui,
    Desktop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuthorityRequirement {
    Observer,
    Controller,
    Owner,
}

impl AuthorityRequirement {
    pub(crate) const fn rank(self) -> u8 {
        match self {
            Self::Observer => 0,
            Self::Controller => 1,
            Self::Owner => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandEffect {
    Inspect,
    Control,
    Configure,
    External,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InteractionMode {
    Any,
    Interactive,
    Noninteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfirmationPolicy {
    None,
    WhenInteractive,
    ExactScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "shape", content = "value")]
pub(crate) enum ArgumentSchema {
    None,
    Optional(&'static str),
    Required(&'static str),
    Variants(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceSet(u8);

impl SurfaceSet {
    const CLI: u8 = 1;
    const PLAIN: u8 = 1 << 1;
    const TUI: u8 = 1 << 2;
    const DESKTOP: u8 = 1 << 3;

    const fn new(cli: bool, plain: bool, tui: bool, desktop: bool) -> Self {
        Self(
            (if cli { Self::CLI } else { 0 })
                | (if plain { Self::PLAIN } else { 0 })
                | (if tui { Self::TUI } else { 0 })
                | (if desktop { Self::DESKTOP } else { 0 }),
        )
    }

    pub(crate) const fn contains(self, surface: CommandSurface) -> bool {
        let bit = match surface {
            CommandSurface::Cli => Self::CLI,
            CommandSurface::Plain => Self::PLAIN,
            CommandSurface::Tui => Self::TUI,
            CommandSurface::Desktop => Self::DESKTOP,
        };
        self.0 & bit != 0
    }
}

const ALL_SURFACES: SurfaceSet = SurfaceSet::new(true, true, true, true);
const CHAT_SURFACES: SurfaceSet = SurfaceSet::new(false, true, true, true);
const RICH_CHAT_SURFACES: SurfaceSet = SurfaceSet::new(false, false, true, true);
const LOCAL_INTERACTIVE: SurfaceSet = SurfaceSet::new(false, false, true, true);
const CLI_AND_CHAT: SurfaceSet = SurfaceSet::new(true, true, true, true);
const CLI_ONLY: SurfaceSet = SurfaceSet::new(true, false, false, false);
const DESKTOP_ONLY: SurfaceSet = SurfaceSet::new(false, false, false, true);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    /// Stable, versioned semantic identifier. Display labels may change.
    pub(crate) stable_id: &'static str,
    pub(crate) action: CommandAction,
    pub(crate) name: &'static str,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) mode: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) argument_schema: ArgumentSchema,
    pub(crate) authority: AuthorityRequirement,
    pub(crate) interaction: InteractionMode,
    pub(crate) confirmation: ConfirmationPolicy,
    pub(crate) effect: CommandEffect,
    pub(crate) success_code: &'static str,
    pub(crate) error_codes: &'static [&'static str],
    pub(crate) surfaces: SurfaceSet,
    pub(crate) slash: bool,
    pub(crate) implemented: bool,
    pub(crate) requires_configuration: bool,
}

impl CommandSpec {
    pub(crate) fn usage(self) -> String {
        if self.mode.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.mode)
        }
    }

    pub(crate) fn is_named(self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }

    /// Safe default used when a user chooses this exact row from a palette.
    pub(crate) fn palette_arguments(self) -> &'static str {
        match self.stable_id {
            "presentation.activity.auto.v1" => "view auto",
            "presentation.activity.hide.v1" => "view hide",
            "presentation.activity.show.v1" => "view show",
            "presentation.composer.newline.v1" => "newline",
            "presentation.composer.submit.v1" => "submit",
            "presentation.header.hide.v1" => "view hide",
            "presentation.header.show.v1" => "view show",
            "conversation.archive.v1" => "archive",
            "conversation.new.v1" => "new",
            "conversation.continue.v1" => "continue",
            "conversation.search.v1" => "search",
            "presentation.conversation_list.hide.v1" => "view hide",
            "presentation.conversation_list.show.v1" => "view show",
            "profile.manage.v1"
            | "project.manage.v1"
            | "skill.manage.v1"
            | "plugin.manage.v1"
            | "mcp.manage.v1"
            | "external_agent.manage.v1"
            | "image.manage.v1"
            | "connection.manage.v1"
            | "diagnostics.logs.v1"
            | "outbound.manage.v1"
            | "route.inspect.v1" => "list",
            _ => "",
        }
    }

    pub(crate) fn availability(self, context: CommandContext) -> CommandAvailability {
        if !self.surfaces.contains(context.surface) {
            return CommandAvailability::unavailable(
                AvailabilityCode::UnsupportedSurface,
                "This action is not projected on this surface.",
            );
        }
        if context.authority.rank() < self.authority.rank() {
            return CommandAvailability::unavailable(
                AvailabilityCode::AuthorityRequired,
                match self.authority {
                    AuthorityRequirement::Observer => "Observer access is required.",
                    AuthorityRequirement::Controller => {
                        "Attach as this Conversation's controller to use this action."
                    }
                    AuthorityRequirement::Owner => "The local Xana owner must perform this action.",
                },
            );
        }
        if self.interaction == InteractionMode::Interactive && !context.interactive {
            return CommandAvailability::unavailable(
                AvailabilityCode::InteractiveInputRequired,
                "This action requires an interactive surface.",
            );
        }
        if self.interaction == InteractionMode::Noninteractive && context.interactive {
            return CommandAvailability::unavailable(
                AvailabilityCode::NoninteractiveOnly,
                "This action is available only through deterministic noninteractive input.",
            );
        }
        if self.requires_configuration && !context.configured {
            return CommandAvailability::unavailable(
                AvailabilityCode::SetupRequired,
                "Complete setup before using this action.",
            );
        }
        if !self.implemented {
            return CommandAvailability::unavailable(
                AvailabilityCode::NotImplemented,
                "This semantic action is reserved but is not implemented on this surface yet.",
            );
        }
        CommandAvailability::AVAILABLE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandContext {
    pub(crate) surface: CommandSurface,
    pub(crate) authority: AuthorityRequirement,
    pub(crate) interactive: bool,
    pub(crate) configured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AvailabilityCode {
    Available,
    UnsupportedSurface,
    AuthorityRequired,
    InteractiveInputRequired,
    NoninteractiveOnly,
    SetupRequired,
    NotImplemented,
    UnknownCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandAvailability {
    pub(crate) enabled: bool,
    pub(crate) code: AvailabilityCode,
    pub(crate) reason: Option<&'static str>,
}

impl CommandAvailability {
    const AVAILABLE: Self = Self {
        enabled: true,
        code: AvailabilityCode::Available,
        reason: None,
    };

    const fn unavailable(code: AvailabilityCode, reason: &'static str) -> Self {
        Self {
            enabled: false,
            code,
            reason: Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedCommand {
    pub(crate) action: CommandAction,
    pub(crate) stable_id: &'static str,
    pub(crate) arguments: String,
}

macro_rules! command {
    ($id:literal, $action:ident, $name:literal, $aliases:expr, $mode:literal, $summary:literal,
     $args:expr, $authority:ident, $interaction:ident, $confirmation:ident, $effect:ident,
     $surfaces:expr, $slash:expr, $implemented:expr, $configured:expr) => {
        CommandSpec {
            stable_id: $id,
            action: CommandAction::$action,
            name: $name,
            aliases: $aliases,
            mode: $mode,
            summary: $summary,
            argument_schema: $args,
            authority: AuthorityRequirement::$authority,
            interaction: InteractionMode::$interaction,
            confirmation: ConfirmationPolicy::$confirmation,
            effect: CommandEffect::$effect,
            success_code: concat!($id, ".completed"),
            error_codes: &[concat!($id, ".unavailable"), concat!($id, ".invalid")],
            surfaces: $surfaces,
            slash: $slash,
            implemented: $implemented,
            requires_configuration: $configured,
        }
    };
}

/// M3-exit command inventory plus the M4 attach/preview and Espejo semantics.
/// Broad management families deliberately retain their runtime-owned parsers.
pub(crate) const COMMANDS: &[CommandSpec] = &[
    command!(
        "budget.manage.v1",
        Budget,
        "budget",
        &[],
        "[--daily-requests N] [--root-tokens N] ...",
        "Inspect or change durable local admission limits",
        ArgumentSchema::Optional("budget_options"),
        Owner,
        Any,
        None,
        Configure,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "storage.manage.v1",
        Storage,
        "storage",
        &[],
        "status|verify|lock|unlock ...",
        "Inspect protected storage; lock stops this terminal owner and exits",
        ArgumentSchema::Variants("storage_action"),
        Owner,
        Any,
        None,
        Control,
        SurfaceSet::new(true, true, true, false),
        true,
        true,
        false
    ),
    command!(
        "presentation.activity.auto.v1",
        Activity,
        "activity",
        &[],
        "view auto",
        "Show activity when it becomes relevant",
        ArgumentSchema::Variants("view auto|hide|show"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "presentation.activity.hide.v1",
        Activity,
        "activity",
        &[],
        "view hide",
        "Hide activity without changing model reasoning",
        ArgumentSchema::Variants("view auto|hide|show"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "presentation.activity.show.v1",
        Activity,
        "activity",
        &[],
        "view show",
        "Keep activity visible without changing model reasoning",
        ArgumentSchema::Variants("view auto|hide|show"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "artifact.inspect.v1",
        Artifact,
        "artifact",
        &[],
        "ARTIFACT_ID",
        "Act on a visible immutable artifact",
        ArgumentSchema::Required("artifact_id"),
        Observer,
        Any,
        None,
        Inspect,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "approval.decide.v1",
        Approval,
        "approval",
        &[],
        "PERMISSION_ID DECISION",
        "Resolve one exact pending approval",
        ArgumentSchema::Required("permission_id decision"),
        Controller,
        Interactive,
        ExactScope,
        Control,
        LOCAL_INTERACTIVE,
        false,
        true,
        true
    ),
    command!(
        "turn.attachment.stage.v1",
        Attach,
        "attach",
        &[],
        "PATH|--clipboard|list|clear",
        "Stage, inspect, or clear typed resources for the next turn",
        ArgumentSchema::Required("resource"),
        Controller,
        Interactive,
        None,
        Control,
        CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "capability.report.v1",
        Capabilities,
        "capabilities",
        &["capability"],
        "[--json]",
        "Report what Xana can do here without probing the network",
        ArgumentSchema::Optional("--json"),
        Observer,
        Any,
        None,
        Inspect,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "conversation.clear.v1",
        Clear,
        "clear",
        &[],
        "",
        "Clear visible context in the attached Conversation",
        ArgumentSchema::None,
        Controller,
        Any,
        WhenInteractive,
        Control,
        CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.compact.v1",
        Compact,
        "compact",
        &[],
        "",
        "Compact older native context without deleting raw history",
        ArgumentSchema::None,
        Controller,
        Any,
        WhenInteractive,
        Control,
        CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "child.list.v1",
        Child,
        "agents",
        &[],
        "",
        "List bounded child task records",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Inspect,
        SurfaceSet::new(false, true, false, true),
        true,
        true,
        true
    ),
    command!(
        "child.inspect.v1",
        Child,
        "agent",
        &[],
        "AGENT_ID",
        "Inspect one exact child task",
        ArgumentSchema::Required("agent_id"),
        Controller,
        Interactive,
        None,
        Inspect,
        SurfaceSet::new(false, true, false, true),
        true,
        true,
        true
    ),
    command!(
        "child.cancel.v1",
        Child,
        "cancel-agent",
        &[],
        "AGENT_ID",
        "Cancel one exact child task",
        ArgumentSchema::Required("agent_id"),
        Controller,
        Interactive,
        ExactScope,
        Control,
        SurfaceSet::new(false, true, false, true),
        true,
        true,
        true
    ),
    command!(
        "run.continue.v1",
        Continue,
        "continue",
        &[],
        "",
        "Continue the exact native Run suspended at its round budget",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Control,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "presentation.composer.newline.v1",
        Composer,
        "composer",
        &[],
        "newline",
        "Make Enter insert a newline",
        ArgumentSchema::Variants("newline|submit"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "presentation.composer.submit.v1",
        Composer,
        "composer",
        &[],
        "submit",
        "Make Enter submit the draft",
        ArgumentSchema::Variants("newline|submit"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "diagnostics.doctor.v1",
        Doctor,
        "doctor",
        &[],
        "",
        "Run read-only installation diagnostics",
        ArgumentSchema::None,
        Observer,
        Any,
        None,
        Inspect,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "presentation.header.hide.v1",
        Header,
        "header",
        &[],
        "view hide",
        "Collapse Xana's identity panel",
        ArgumentSchema::Variants("view hide|show"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "presentation.header.show.v1",
        Header,
        "header",
        &[],
        "view show",
        "Expand Xana's identity panel",
        ArgumentSchema::Variants("view hide|show"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "help.contextual.v1",
        Help,
        "help",
        &[],
        "",
        "Show commands available on this surface",
        ArgumentSchema::None,
        Observer,
        Any,
        None,
        Inspect,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "application.command_palette.v1",
        Help,
        "command-palette",
        &["palette"],
        "",
        "Search commands available in Xana Desktop",
        ArgumentSchema::None,
        Observer,
        Interactive,
        None,
        Inspect,
        DESKTOP_ONLY,
        false,
        true,
        false
    ),
    command!(
        "help.documentation.open.v1",
        Help,
        "documentation",
        &["docs"],
        "",
        "Open Xana's user documentation in the default browser",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        External,
        DESKTOP_ONLY,
        false,
        true,
        false
    ),
    command!(
        "configuration.file.open.v1",
        Settings,
        "configuration-file",
        &[],
        "",
        "Open Xana's configuration file with the system default application",
        ArgumentSchema::None,
        Owner,
        Interactive,
        None,
        External,
        DESKTOP_ONLY,
        false,
        true,
        true
    ),
    command!(
        "presentation.layout.manage.v1",
        Layout,
        "layout",
        &[],
        "reset|save|restore ...",
        "Manage bounded frontend layout state",
        ArgumentSchema::Variants("layout_action"),
        Controller,
        Interactive,
        ExactScope,
        Configure,
        LOCAL_INTERACTIVE,
        false,
        false,
        true
    ),
    command!(
        "application.window.minimize.v1",
        Layout,
        "minimize-window",
        &[],
        "",
        "Minimize the active Xana Desktop window",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Configure,
        DESKTOP_ONLY,
        false,
        true,
        false
    ),
    command!(
        "run.interrupt.v1",
        Interrupt,
        "interrupt",
        &[],
        "",
        "Interrupt the active Run",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Control,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "model.select.v1",
        Model,
        "model",
        &[],
        "[CONNECTION/MODEL]",
        "Inspect or select an exact model",
        ArgumentSchema::Optional("connection/model"),
        Controller,
        Any,
        None,
        Configure,
        CLI_AND_CHAT,
        true,
        true,
        true
    ),
    command!(
        "presentation.composer.insert_newline.v1",
        Newline,
        "newline",
        &[],
        "",
        "Insert a newline in the draft",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Control,
        LOCAL_INTERACTIVE,
        true,
        true,
        true
    ),
    command!(
        "profile.manage.v1",
        Profile,
        "profile",
        &[],
        "list|inspect|create|edit|duplicate|rename|archive|unarchive|delete|resolve|freeze|continue ...",
        "Manage Profiles through the shared application path",
        ArgumentSchema::Variants("profile_action"),
        Owner,
        Any,
        ExactScope,
        Configure,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "project.manage.v1",
        Project,
        "project",
        &[],
        "list|inspect|create|rename|archive|unarchive|assign|ungroup|continue|share|register|setup ...",
        "Manage Projects and Conversation placement",
        ArgumentSchema::Variants("project_action"),
        Owner,
        Any,
        ExactScope,
        Configure,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "skill.manage.v1",
        Skill,
        "skill",
        &[],
        "list|inspect|validate|activate|read|enable|disable ...",
        "Discover and explicitly activate Agent Skills",
        ArgumentSchema::Variants("skill_action"),
        Owner,
        Any,
        ExactScope,
        Configure,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "plugin.manage.v1",
        Plugin,
        "plugin",
        &[],
        "list|inspect|review|install|enable|disable|update-check|update|rollback|remove|gc ...",
        "Manage reviewed Agent Plugin packages and scoped enablement",
        ArgumentSchema::Variants("plugin_action"),
        Owner,
        Any,
        ExactScope,
        Configure,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "mcp.manage.v1",
        Mcp,
        "mcp",
        &[],
        "list|add-stdio|add-http|remove|login|logout|refresh|tools|resources|read|prompts|prompt|serve ...",
        "Manage and use Profile-allowlisted MCP primitives",
        ArgumentSchema::Variants("mcp_action"),
        Owner,
        Any,
        ExactScope,
        External,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "external_agent.manage.v1",
        ExternalAgent,
        "external-agent",
        &[],
        "list|show|add|refresh|trust|untrust|tasks|cancel|remove ...",
        "Manage explicit bounded A2A agent endpoints",
        ArgumentSchema::Variants("external_agent_action"),
        Owner,
        Any,
        ExactScope,
        External,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "image.manage.v1",
        Image,
        "image",
        &[],
        "list|inspect ROUTE|generate [PROMPT] [--route ROUTE] [--yes]",
        "Inspect or invoke an exposed image-generation route",
        ArgumentSchema::Variants("image_action"),
        Owner,
        Any,
        ExactScope,
        External,
        ALL_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "vision.manage.v1",
        Vision,
        "vision",
        &[],
        "[auto|ROUTE]",
        "Prefer native vision or select a specialist route",
        ArgumentSchema::Optional("route"),
        Controller,
        Any,
        None,
        External,
        CLI_AND_CHAT,
        true,
        true,
        true
    ),
    command!(
        "followup.list.v1",
        Queue,
        "queue",
        &[],
        "",
        "Inspect ordered follow-ups",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Inspect,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "followup.edit.v1",
        Queue,
        "queue",
        &[],
        "edit INDEX",
        "Move a follow-up into the composer",
        ArgumentSchema::Required("index"),
        Controller,
        Interactive,
        None,
        Control,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "followup.remove.v1",
        Queue,
        "queue",
        &[],
        "remove INDEX",
        "Remove an ordered follow-up",
        ArgumentSchema::Required("index"),
        Controller,
        Interactive,
        WhenInteractive,
        Destructive,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "application.quit.v1",
        Quit,
        "quit",
        &[],
        "",
        "Stop Xana and leave this surface",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Control,
        CHAT_SURFACES,
        true,
        true,
        false
    ),
    command!(
        "model.reasoning.select.v1",
        Reasoning,
        "reasoning",
        &[],
        "[EFFORT]",
        "Inspect or change managed reasoning effort",
        ArgumentSchema::Optional("effort"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "turn.submit.v1",
        Send,
        "send",
        &[],
        "[MESSAGE]",
        "Send the draft or provided message",
        ArgumentSchema::Optional("message"),
        Controller,
        Interactive,
        None,
        Control,
        CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.list.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "",
        "Show Conversations in this workspace",
        ArgumentSchema::None,
        Observer,
        Any,
        None,
        Inspect,
        ALL_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.search.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "search QUERY [--conversation ID] [--limit N] [--json]",
        "Search bounded retained Conversation text",
        ArgumentSchema::Required("query"),
        Observer,
        Any,
        None,
        Inspect,
        ALL_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.archive.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "archive [ID]",
        "Archive the viewed or named retained Conversation",
        ArgumentSchema::Optional("conversation_id"),
        Owner,
        Interactive,
        ExactScope,
        Destructive,
        CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.new.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "new",
        "Start a new Conversation with the current resolved configuration",
        ArgumentSchema::None,
        Controller,
        Any,
        None,
        Control,
        ALL_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.continue.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "continue",
        "Continue the latest compatible Conversation",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Control,
        ALL_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.preview.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "preview CONVERSATION_ID",
        "Preview retained history without acquiring control",
        ArgumentSchema::Required("conversation_id"),
        Observer,
        Any,
        None,
        Inspect,
        ALL_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "conversation.attach.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "attach CONVERSATION_ID",
        "Attach this composer as the Conversation controller",
        ArgumentSchema::Required("conversation_id"),
        Controller,
        Interactive,
        None,
        Control,
        ALL_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "presentation.conversation_list.hide.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "view hide",
        "Hide the Conversation list",
        ArgumentSchema::Variants("view hide|show"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "presentation.conversation_list.show.v1",
        Conversation,
        "conversation",
        &["session", "sessions"],
        "view show",
        "Show the Conversation list",
        ArgumentSchema::Variants("view hide|show"),
        Controller,
        Interactive,
        None,
        Configure,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "setup.run.v1",
        Setup,
        "setup",
        &[],
        "[quick|full|blank|connection|permissions-shell|profiles-routes|appearance]",
        "Run guided or focused setup",
        ArgumentSchema::Optional("section"),
        Owner,
        Any,
        ExactScope,
        Configure,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "settings.open.v1",
        Settings,
        "settings",
        &[],
        "[overview|appearance|connections|profiles|permissions|execution|diagnostics|integrations|advanced]",
        "Browse staged settings and preferences",
        ArgumentSchema::Optional("section"),
        Owner,
        Any,
        ExactScope,
        Configure,
        CLI_AND_CHAT,
        true,
        true,
        true
    ),
    command!(
        "usage.inspect.v1",
        Usage,
        "usage",
        &[],
        "[compact|details|--connection ID] [--model ID] [--refresh] [--json]",
        "Show provider-neutral usage and account observations",
        ArgumentSchema::Optional("usage_filters"),
        Observer,
        Any,
        None,
        Inspect,
        CLI_AND_CHAT,
        true,
        true,
        true
    ),
    command!(
        "usage.ledger.v1",
        Usage,
        "usage",
        &[],
        "ledger [--root ID] [--job ID] [--after SEQUENCE]",
        "Inspect durable, source-qualified usage reservations and receipts",
        ArgumentSchema::Optional("ledger_filters"),
        Observer,
        Any,
        None,
        Inspect,
        CLI_AND_CHAT,
        true,
        true,
        true
    ),
    command!(
        "run.steer.v1",
        Steer,
        "steer",
        &[],
        "MESSAGE",
        "Steer a capable active managed Run",
        ArgumentSchema::Required("message"),
        Controller,
        Interactive,
        None,
        Control,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "run.stop.v1",
        Stop,
        "stop",
        &[],
        "",
        "Stop the exact native Run suspended at its round budget",
        ArgumentSchema::None,
        Controller,
        Interactive,
        None,
        Control,
        RICH_CHAT_SURFACES,
        true,
        true,
        true
    ),
    command!(
        "configuration.reset.v1",
        Reset,
        "reset",
        &["clean"],
        "",
        "Preview a reset scope and require exact confirmation",
        ArgumentSchema::Optional("scope"),
        Owner,
        Interactive,
        ExactScope,
        Configure,
        SurfaceSet::new(true, false, true, true),
        false,
        true,
        false
    ),
    command!(
        "connection.manage.v1",
        Connection,
        "connection",
        &["auth"],
        "[--json] list|add|status|set-key|login|logout|delete-key|refresh|remove ...",
        "Manage model-provider connections without exposing credentials",
        ArgumentSchema::Variants("connection_action"),
        Owner,
        Any,
        ExactScope,
        Configure,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "configuration.inspect.v1",
        Settings,
        "config",
        &[],
        "path|check|list|get|explain|set|reset|migrate|edit ...",
        "Inspect or update bounded configuration",
        ArgumentSchema::Variants("config_action"),
        Owner,
        Any,
        ExactScope,
        Configure,
        CLI_ONLY,
        false,
        true,
        false
    ),
    command!(
        "diagnostics.logs.v1",
        Diagnostics,
        "logs",
        &["diagnostics"],
        "path|list|show|export ...",
        "Inspect metadata-only logs and crash diagnostics",
        ArgumentSchema::Variants("diagnostics_action"),
        Owner,
        Any,
        ExactScope,
        Inspect,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "outbound.manage.v1",
        Outbound,
        "outbound",
        &[],
        "list|revoke ...",
        "Inspect or revoke saved outbound-data decisions",
        ArgumentSchema::Variants("outbound_action"),
        Owner,
        Any,
        ExactScope,
        Configure,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "host.serve.v1",
        Serve,
        "serve",
        &[],
        "[--bind LOOPBACK] [--port PORT]",
        "Host Xana for authenticated local frontend attachment",
        ArgumentSchema::Optional("host_options"),
        Owner,
        Noninteractive,
        ExactScope,
        Control,
        CLI_ONLY,
        false,
        true,
        true
    ),
    command!(
        "host.attach.v1",
        Connection,
        "attach",
        &[],
        "[--control] [--takeover] [--print PROMPT]",
        "Attach to the workspace foreground host",
        ArgumentSchema::Optional("attach_options"),
        Observer,
        Any,
        ExactScope,
        Control,
        CLI_ONLY,
        false,
        true,
        true
    ),
    command!(
        "operation.reconcile.v1",
        Diagnostics,
        "operation",
        &[],
        "plan|resume ...",
        "Inspect or reconcile interrupted operations",
        ArgumentSchema::Variants("operation_action"),
        Owner,
        Any,
        ExactScope,
        Control,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "route.inspect.v1",
        Route,
        "route",
        &[],
        "list|check ...",
        "Inspect exact child task routes without starting work",
        ArgumentSchema::Variants("route_action"),
        Observer,
        Any,
        None,
        Inspect,
        CLI_AND_CHAT,
        true,
        true,
        true
    ),
    command!(
        "integration.connect.v1",
        Connection,
        "connect",
        &[],
        "[provider|profile|plugin|mcp|external-agent|image|vision]",
        "Open the provider-neutral integration hub",
        ArgumentSchema::Optional("integration"),
        Owner,
        Interactive,
        ExactScope,
        Configure,
        CLI_AND_CHAT,
        true,
        true,
        false
    ),
    command!(
        "run.resume.v1",
        Continue,
        "resume-run",
        &[],
        "OPERATION_ID",
        "Resume one exact interrupted or suspended Run",
        ArgumentSchema::Required("operation_id"),
        Controller,
        Any,
        ExactScope,
        Control,
        SurfaceSet::new(false, true, true, true),
        false,
        true,
        true
    ),
    command!(
        "application.shutdown.v1",
        Quit,
        "shutdown",
        &[],
        "",
        "Stop the attached application runtime",
        ArgumentSchema::None,
        Controller,
        Any,
        ExactScope,
        Control,
        SurfaceSet::new(false, true, true, true),
        false,
        true,
        false
    ),
    command!(
        "espejo.open.v1",
        Espejo,
        "espejo",
        &[],
        "[global|project]",
        "Open the bounded work-and-attention perspective",
        ArgumentSchema::Optional("scope"),
        Observer,
        Interactive,
        None,
        Inspect,
        LOCAL_INTERACTIVE,
        true,
        true,
        true
    ),
];

pub(crate) fn commands_for(surface: CommandSurface) -> impl Iterator<Item = CommandSpec> {
    COMMANDS
        .iter()
        .copied()
        .filter(move |command| command.surfaces.contains(surface))
}

pub(crate) fn slash_commands_for(surface: CommandSurface) -> impl Iterator<Item = CommandSpec> {
    commands_for(surface).filter(|command| command.slash)
}

pub(crate) fn find(stable_id: &str) -> Option<CommandSpec> {
    COMMANDS
        .iter()
        .copied()
        .find(|command| command.stable_id == stable_id)
}

/// Commands that temporarily leave an interactive chat surface and run the
/// same typed application command as the top-level CLI.
pub(crate) fn suspended_chat_control(stable_id: &str) -> Option<(&'static str, &'static str)> {
    match stable_id {
        "connection.manage.v1" => Some(("connection", "list")),
        "diagnostics.logs.v1" => Some(("logs", "list")),
        "outbound.manage.v1" => Some(("outbound", "list")),
        "storage.manage.v1" => Some(("storage", "status")),
        "budget.manage.v1" => Some(("budget", "")),
        "usage.ledger.v1" => Some(("usage", "ledger")),
        "operation.reconcile.v1" => Some(("operation", "")),
        "route.inspect.v1" => Some(("route", "list")),
        "integration.connect.v1" => Some(("connect", "")),
        _ => None,
    }
}

pub(crate) fn parse(value: &str, surface: CommandSurface) -> Result<ParsedCommand, String> {
    let value = value.trim();
    let value = value
        .strip_prefix('/')
        .ok_or_else(|| "commands must start with /".to_owned())?;
    let (name, arguments) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
    let arguments = arguments.trim();
    let candidates = slash_commands_for(surface)
        .filter(|command| command.is_named(name))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(format!(
            "unknown command /{name}; inspect available commands with /help"
        ));
    }
    let command = best_mode_match(&candidates, arguments).unwrap_or(candidates[0]);
    Ok(ParsedCommand {
        action: command.action,
        stable_id: command.stable_id,
        arguments: arguments.to_owned(),
    })
}

pub(crate) fn search(query: &str, surface: CommandSurface) -> Vec<CommandSpec> {
    let query = query
        .trim()
        .trim_start_matches('/')
        .trim_start()
        .to_ascii_lowercase();
    let mut matches = commands_for(surface)
        .filter(|command| command.slash || command.action == CommandAction::Reset)
        .filter(|command| {
            let aliases = command.aliases.join(" ");
            let searchable = format!(
                "{} {} {} {} {} {}",
                command.name,
                command.mode,
                command.name,
                aliases,
                command.summary,
                command.stable_id
            )
            .to_ascii_lowercase();
            query.is_empty() || searchable.contains(&query)
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.name
            .cmp(right.name)
            .then_with(|| left.mode.cmp(right.mode))
    });
    matches
}

pub(crate) fn usages(action: CommandAction, surface: CommandSurface) -> Vec<String> {
    slash_commands_for(surface)
        .filter(|command| command.action == action)
        .map(CommandSpec::usage)
        .collect()
}

fn best_mode_match(candidates: &[CommandSpec], arguments: &str) -> Option<CommandSpec> {
    let first = arguments.split_whitespace().next();
    candidates
        .iter()
        .copied()
        .filter(|candidate| {
            let mode = candidate.mode.split_whitespace().next();
            candidate.mode.is_empty()
                || mode.is_some_and(|mode| {
                    mode.starts_with('[')
                        || mode.starts_with('<')
                        || mode.chars().any(char::is_uppercase)
                        || Some(mode) == first
                })
        })
        .max_by_key(|candidate| {
            let mode = candidate.mode.split_whitespace().next().unwrap_or_default();
            usize::from(!mode.is_empty() && Some(mode) == first)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ColorCapability {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccessibilityCapability {
    PlainText,
    TerminalBestEffort,
    NativeSemanticTree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PresentationCapabilities {
    pub(crate) color: ColorCapability,
    pub(crate) unicode: bool,
    pub(crate) dimensions: bool,
    pub(crate) pointer: bool,
    pub(crate) clipboard: bool,
    pub(crate) inline_images: bool,
    pub(crate) inline_audio_video: bool,
    pub(crate) safe_link_open: bool,
    pub(crate) notifications: bool,
    pub(crate) accessibility: AccessibilityCapability,
    pub(crate) rich_markdown: bool,
    pub(crate) math: bool,
    pub(crate) composable_layout: bool,
}

impl PresentationCapabilities {
    pub(crate) const fn plain(color: ColorCapability, unicode: bool) -> Self {
        Self {
            color,
            unicode,
            dimensions: false,
            pointer: false,
            clipboard: false,
            inline_images: false,
            inline_audio_video: false,
            safe_link_open: false,
            notifications: false,
            accessibility: AccessibilityCapability::PlainText,
            rich_markdown: false,
            math: false,
            composable_layout: false,
        }
    }

    pub(crate) const fn tui(
        color: ColorCapability,
        unicode: bool,
        pointer: bool,
        clipboard: bool,
        inline_images: bool,
    ) -> Self {
        Self {
            color,
            unicode,
            dimensions: true,
            pointer,
            clipboard,
            inline_images,
            inline_audio_video: false,
            safe_link_open: false,
            notifications: false,
            accessibility: AccessibilityCapability::TerminalBestEffort,
            rich_markdown: true,
            math: false,
            composable_layout: true,
        }
    }

    pub(crate) const fn desktop() -> Self {
        Self {
            color: ColorCapability::TrueColor,
            unicode: true,
            dimensions: true,
            pointer: true,
            clipboard: true,
            inline_images: true,
            // M4 presents audio/video as typed metadata cards. Advertising
            // inline playback waits for a reviewed native adapter.
            inline_audio_video: false,
            safe_link_open: true,
            notifications: true,
            accessibility: AccessibilityCapability::NativeSemanticTree,
            rich_markdown: true,
            // The GPUI adapter currently presents bounded LaTeX source. Do
            // not claim rich math until a bundled nontrusting renderer lands.
            math: false,
            composable_layout: true,
        }
    }
}

/// User-facing Conversation state vocabulary shared by frontend projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversationDisplayState {
    AttachedHere,
    Running,
    NeedsInput,
    Idle,
    PreviewOnly,
    Archived,
}

impl ConversationDisplayState {
    pub(crate) const fn all() -> [Self; 6] {
        [
            Self::AttachedHere,
            Self::Running,
            Self::NeedsInput,
            Self::Idle,
            Self::PreviewOnly,
            Self::Archived,
        ]
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::AttachedHere => "Attached here",
            Self::Running => "Running",
            Self::NeedsInput => "Needs input",
            Self::Idle => "Idle",
            Self::PreviewOnly => "Preview only",
            Self::Archived => "Archived",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn stable_ids_are_namespaced_and_unique() {
        let mut ids = HashSet::new();
        for command in COMMANDS {
            assert!(command.stable_id.matches('.').count() >= 2);
            assert!(ids.insert(command.stable_id), "{}", command.stable_id);
            assert!(!command.success_code.is_empty());
            assert!(!command.error_codes.is_empty());
        }
    }

    #[test]
    fn conversation_is_canonical_and_session_spellings_remain_aliases() {
        for spelling in ["/conversation new", "/session new", "/sessions new"] {
            let parsed = parse(spelling, CommandSurface::Tui).unwrap();
            assert_eq!(parsed.stable_id, "conversation.new.v1");
            assert_eq!(parsed.action, CommandAction::Conversation);
        }
        assert_ne!(
            find("conversation.clear.v1").unwrap().stable_id,
            find("conversation.new.v1").unwrap().stable_id
        );
    }

    #[test]
    fn attach_and_preview_are_distinct_across_local_surfaces() {
        let preview = find("conversation.preview.v1").unwrap();
        let attach = find("conversation.attach.v1").unwrap();
        assert_eq!(preview.effect, CommandEffect::Inspect);
        assert_eq!(attach.effect, CommandEffect::Control);
        let context = CommandContext {
            surface: CommandSurface::Tui,
            authority: AuthorityRequirement::Controller,
            interactive: true,
            configured: true,
        };
        assert!(preview.availability(context).enabled);
        assert!(attach.availability(context).enabled);
        let desktop = CommandContext {
            surface: CommandSurface::Desktop,
            ..context
        };
        assert!(attach.availability(desktop).enabled);
        let cli = CommandContext {
            surface: CommandSurface::Cli,
            ..context
        };
        assert!(preview.availability(cli).enabled);
        assert!(attach.availability(cli).enabled);
    }

    #[test]
    fn observer_projection_never_enables_mutating_commands() {
        let context = CommandContext {
            surface: CommandSurface::Desktop,
            authority: AuthorityRequirement::Observer,
            interactive: true,
            configured: true,
        };
        for command in commands_for(CommandSurface::Desktop) {
            if command.effect != CommandEffect::Inspect {
                assert!(
                    !command.availability(context).enabled,
                    "{}",
                    command.stable_id
                );
            }
        }
    }

    #[test]
    fn noninteractive_projection_fails_closed_for_interactive_actions() {
        let context = CommandContext {
            surface: CommandSurface::Cli,
            authority: AuthorityRequirement::Owner,
            interactive: false,
            configured: true,
        };
        let setup = find("setup.run.v1").unwrap();
        assert!(setup.availability(context).enabled);
        let connect = find("integration.connect.v1").unwrap();
        assert_eq!(
            connect.availability(context).code,
            AvailabilityCode::InteractiveInputRequired
        );
    }

    #[test]
    fn terminal_management_commands_have_one_suspended_cli_projection() {
        for (stable_id, family, default_arguments) in [
            ("connection.manage.v1", "connection", "list"),
            ("diagnostics.logs.v1", "logs", "list"),
            ("outbound.manage.v1", "outbound", "list"),
            ("operation.reconcile.v1", "operation", ""),
            ("route.inspect.v1", "route", "list"),
            ("integration.connect.v1", "connect", ""),
        ] {
            let spec = find(stable_id).expect("management command");
            assert!(spec.surfaces.contains(CommandSurface::Plain));
            assert!(spec.surfaces.contains(CommandSurface::Tui));
            assert!(spec.slash);
            assert_eq!(
                suspended_chat_control(stable_id),
                Some((family, default_arguments))
            );
        }
    }

    #[test]
    fn presentation_fallbacks_are_explicit() {
        let plain = PresentationCapabilities::plain(ColorCapability::None, false);
        assert_eq!(plain.accessibility, AccessibilityCapability::PlainText);
        assert!(!plain.inline_images);
        assert!(!plain.pointer);
        let hostile =
            PresentationCapabilities::tui(ColorCapability::None, false, false, false, false);
        assert!(!hostile.inline_images);
        assert!(!hostile.clipboard);
        let desktop = PresentationCapabilities::desktop();
        assert!(desktop.composable_layout);
        assert!(desktop.inline_images);
        assert!(!desktop.inline_audio_video);
        assert!(!desktop.math);
    }

    #[test]
    fn additive_unknown_command_is_ignored_by_old_catalog() {
        #[derive(Deserialize)]
        struct WireCommand {
            stable_id: String,
        }
        let future: WireCommand = serde_json::from_str(
            r#"{"stable_id":"future.synthetic.command.v99","payload":{"new":true}}"#,
        )
        .unwrap();
        assert!(find(&future.stable_id).is_none());
    }
}
