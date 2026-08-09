//! One typed command registry shared by slash input and the command palette.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandId {
    Help,
    Send,
    Newline,
    Interrupt,
    Steer,
    Model,
    Reasoning,
    Sessions,
    Activity,
    Attach,
    Queue,
    Clear,
    Composer,
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CommandSpec {
    pub(super) id: CommandId,
    pub(super) name: &'static str,
    pub(super) usage: &'static str,
    pub(super) summary: &'static str,
}

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::Activity,
        name: "activity",
        usage: "/activity auto|open|hidden",
        summary: "Persist activity-pane visibility without changing model reasoning",
    },
    CommandSpec {
        id: CommandId::Attach,
        name: "attach",
        usage: "/attach WORKSPACE_RELATIVE_PATH",
        summary: "Stage an image for the next submitted or queued turn",
    },
    CommandSpec {
        id: CommandId::Clear,
        name: "clear",
        usage: "/clear",
        summary: "Clear the current conversation when no turn is active",
    },
    CommandSpec {
        id: CommandId::Composer,
        name: "composer",
        usage: "/composer submit|newline",
        summary: "Persist the Enter-key composer preset",
    },
    CommandSpec {
        id: CommandId::Help,
        name: "help",
        usage: "/help",
        summary: "Show contextual TUI help",
    },
    CommandSpec {
        id: CommandId::Interrupt,
        name: "interrupt",
        usage: "/interrupt",
        summary: "Interrupt the correlated active turn",
    },
    CommandSpec {
        id: CommandId::Model,
        name: "model",
        usage: "/model [CONNECTION/MODEL]",
        summary: "Open the model picker or select an exact model",
    },
    CommandSpec {
        id: CommandId::Newline,
        name: "newline",
        usage: "/newline",
        summary: "Insert a newline in the current draft",
    },
    CommandSpec {
        id: CommandId::Queue,
        name: "queue",
        usage: "/queue [remove|edit INDEX]",
        summary: "Inspect, remove, or edit an ordered follow-up",
    },
    CommandSpec {
        id: CommandId::Quit,
        name: "quit",
        usage: "/quit",
        summary: "Stop the foreground owner and leave the TUI",
    },
    CommandSpec {
        id: CommandId::Reasoning,
        name: "reasoning",
        usage: "/reasoning [EFFORT]",
        summary: "Inspect or change managed-runtime reasoning effort",
    },
    CommandSpec {
        id: CommandId::Send,
        name: "send",
        usage: "/send [MESSAGE]",
        summary: "Send the current draft or the provided message",
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "sessions",
        usage: "/sessions [expanded|collapsed]",
        summary: "Search conversations or persist the wide session rail",
    },
    CommandSpec {
        id: CommandId::Steer,
        name: "steer",
        usage: "/steer MESSAGE",
        summary: "Explicitly steer a capable active managed turn",
    },
];

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
    let query = query.trim().to_ascii_lowercase();
    COMMANDS
        .iter()
        .copied()
        .filter(|command| {
            query.is_empty()
                || command.name.contains(&query)
                || command.summary.to_ascii_lowercase().contains(&query)
        })
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
    }
}
