//! One typed command registry shared by slash input and the command palette.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandId {
    Help,
    Header,
    Send,
    Newline,
    Interrupt,
    Steer,
    Model,
    Reasoning,
    Sessions,
    Project,
    Profile,
    Skill,
    Plugin,
    Mcp,
    ExternalAgent,
    Image,
    Setup,
    Activity,
    Artifact,
    Attach,
    Queue,
    Clear,
    Composer,
    Doctor,
    Reset,
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CommandSpec {
    pub(super) id: CommandId,
    pub(super) name: &'static str,
    pub(super) mode: &'static str,
    pub(super) summary: &'static str,
    pub(super) arguments: &'static str,
}

impl CommandSpec {
    pub(super) fn usage(self) -> String {
        if self.mode.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.mode)
        }
    }
}

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::Activity,
        name: "activity",
        mode: "view auto",
        summary: "Show activity when it becomes relevant",
        arguments: "view auto",
    },
    CommandSpec {
        id: CommandId::Activity,
        name: "activity",
        mode: "view hide",
        summary: "Hide activity without changing model reasoning",
        arguments: "view hide",
    },
    CommandSpec {
        id: CommandId::Activity,
        name: "activity",
        mode: "view show",
        summary: "Keep activity visible without changing model reasoning",
        arguments: "view show",
    },
    CommandSpec {
        id: CommandId::Artifact,
        name: "artifact",
        mode: "ARTIFACT_ID",
        summary: "Act on a visible immutable artifact",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Attach,
        name: "attach",
        mode: "WORKSPACE_RELATIVE_PATH",
        summary: "Stage an image for the next turn",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Clear,
        name: "clear",
        mode: "",
        summary: "Clear the current conversation",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Composer,
        name: "composer",
        mode: "newline",
        summary: "Make Enter insert a newline",
        arguments: "newline",
    },
    CommandSpec {
        id: CommandId::Composer,
        name: "composer",
        mode: "submit",
        summary: "Make Enter submit the draft",
        arguments: "submit",
    },
    CommandSpec {
        id: CommandId::Doctor,
        name: "doctor",
        mode: "",
        summary: "Run read-only installation diagnostics",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Header,
        name: "header",
        mode: "view hide",
        summary: "Collapse Xana's identity panel",
        arguments: "view hide",
    },
    CommandSpec {
        id: CommandId::Header,
        name: "header",
        mode: "view show",
        summary: "Expand Xana's identity panel",
        arguments: "view show",
    },
    CommandSpec {
        id: CommandId::Help,
        name: "help",
        mode: "",
        summary: "Show contextual TUI help",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Interrupt,
        name: "interrupt",
        mode: "",
        summary: "Interrupt the active turn",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Model,
        name: "model",
        mode: "[CONNECTION/MODEL]",
        summary: "Open the picker or select an exact model",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Newline,
        name: "newline",
        mode: "",
        summary: "Insert a newline in the draft",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Profile,
        name: "profile",
        mode: "list|inspect|create|edit|duplicate|rename|archive|unarchive|delete|resolve|freeze|continue ...",
        summary: "Manage profiles through the shared typed command path",
        arguments: "list",
    },
    CommandSpec {
        id: CommandId::Project,
        name: "project",
        mode: "list|inspect|create|rename|archive|unarchive|assign|ungroup|continue|share|register|setup ...",
        summary: "Manage projects and conversation placement",
        arguments: "list",
    },
    CommandSpec {
        id: CommandId::Skill,
        name: "skill",
        mode: "list|inspect|validate|activate|read|enable|disable ...",
        summary: "Discover and explicitly activate Agent Skills",
        arguments: "list",
    },
    CommandSpec {
        id: CommandId::Plugin,
        name: "plugin",
        mode: "list|inspect|review|install|enable|disable|update-check|update|rollback|remove|gc ...",
        summary: "Manage reviewed Agent Plugin packages and scoped enablement",
        arguments: "list",
    },
    CommandSpec {
        id: CommandId::Mcp,
        name: "mcp",
        mode: "list|refresh|tools|resources|read|prompts|prompt ...",
        summary: "Discover and explicitly use profile-allowlisted MCP primitives",
        arguments: "list",
    },
    CommandSpec {
        id: CommandId::ExternalAgent,
        name: "external-agent",
        mode: "list|show|add|refresh|trust|untrust|remove ...",
        summary: "Discover and explicitly trust bounded A2A agent endpoints",
        arguments: "list",
    },
    CommandSpec {
        id: CommandId::Image,
        name: "image",
        mode: "list|inspect ROUTE|generate [PROMPT] [--route ROUTE] [--yes]",
        summary: "Inspect or invoke an exposed image-generation route",
        arguments: "list",
    },
    CommandSpec {
        id: CommandId::Queue,
        name: "queue",
        mode: "",
        summary: "Inspect ordered follow-ups",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Queue,
        name: "queue",
        mode: "edit INDEX",
        summary: "Move a follow-up into the composer",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Queue,
        name: "queue",
        mode: "remove INDEX",
        summary: "Remove an ordered follow-up",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Quit,
        name: "quit",
        mode: "",
        summary: "Stop Xana and leave the TUI",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Reasoning,
        name: "reasoning",
        mode: "[EFFORT]",
        summary: "Inspect or change managed reasoning effort",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Send,
        name: "send",
        mode: "[MESSAGE]",
        summary: "Send the draft or provided message",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "sessions",
        mode: "",
        summary: "Show all workspace sessions",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "sessions",
        mode: "archive [ID]",
        summary: "Archive the viewed or named managed session",
        arguments: "archive",
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "sessions",
        mode: "new",
        summary: "Start a new session with the current resolved configuration",
        arguments: "new",
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "sessions",
        mode: "view hide",
        summary: "Hide the wide sessions panel",
        arguments: "view hide",
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "sessions",
        mode: "view show",
        summary: "Show the wide sessions panel",
        arguments: "view show",
    },
    CommandSpec {
        id: CommandId::Setup,
        name: "setup",
        mode: "[quick|full|connection|permissions-shell|profiles-routes|appearance]",
        summary: "Run guided or focused setup",
        arguments: "",
    },
    CommandSpec {
        id: CommandId::Steer,
        name: "steer",
        mode: "MESSAGE",
        summary: "Steer a capable active managed turn",
        arguments: "",
    },
];

const PALETTE_ONLY: &[CommandSpec] = &[CommandSpec {
    id: CommandId::Reset,
    name: "reset",
    mode: "",
    summary: "Preview a reset scope and confirm",
    arguments: "",
}];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedCommand {
    pub(super) id: CommandId,
    pub(super) arguments: String,
}

pub(super) fn parse(value: &str) -> Result<ParsedCommand, String> {
    let value = value.trim();
    let value = value
        .strip_prefix('/')
        .ok_or_else(|| "commands must start with /".to_owned())?;
    let (name, arguments) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
    let command = COMMANDS
        .iter()
        .find(|command| command.name == name)
        .ok_or_else(|| format!("unknown command /{name}; open the command palette with Ctrl+P"))?;
    Ok(ParsedCommand {
        id: command.id,
        arguments: arguments.trim().to_owned(),
    })
}

pub(super) fn search(query: &str) -> Vec<CommandSpec> {
    let query = query
        .trim()
        .trim_start_matches('/')
        .trim_start()
        .to_ascii_lowercase();
    let mut matches = COMMANDS
        .iter()
        .chain(PALETTE_ONLY)
        .copied()
        .filter(|command| {
            let searchable = format!("{} {} {}", command.name, command.mode, command.summary)
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

pub(super) fn usages(id: CommandId) -> Vec<String> {
    COMMANDS
        .iter()
        .filter(|command| command.id == id)
        .map(|command| command.usage())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_and_palette_use_one_registry() {
        for command in COMMANDS {
            let parsed = parse(&format!("/{}", command.name)).unwrap();
            assert_eq!(parsed.id, command.id);
            assert!(
                search(command.name)
                    .iter()
                    .any(|found| found.id == command.id)
            );
        }
        assert!(
            parse("/not-a-command")
                .unwrap_err()
                .contains("command palette")
        );
        assert!(parse("/reset").is_err());
        assert!(
            search("reset")
                .iter()
                .any(|command| command.id == CommandId::Reset)
        );
    }

    #[test]
    fn palette_search_accepts_slash_prefixed_commands_and_arguments() {
        assert!(
            search("  /ses")
                .iter()
                .any(|command| command.id == CommandId::Sessions)
        );
        assert!(
            search("/sessions archive")
                .iter()
                .any(|command| command.id == CommandId::Sessions)
        );
    }
}
