//! TUI projection of Xana's shared typed command catalog.

use crate::command_catalog::{self, CommandSurface};
pub(super) use crate::command_catalog::{CommandAction as CommandId, CommandSpec, ParsedCommand};

pub(super) fn parse(value: &str) -> Result<ParsedCommand, String> {
    command_catalog::parse(value, CommandSurface::Tui)
}

pub(super) fn search(query: &str) -> Vec<CommandSpec> {
    command_catalog::search(query, CommandSurface::Tui)
}

pub(super) fn usages(id: CommandId) -> Vec<String> {
    command_catalog::usages(id, CommandSurface::Tui)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_and_palette_use_the_application_catalog() {
        for command in command_catalog::slash_commands_for(CommandSurface::Tui) {
            let parsed = parse(&format!("/{}", command.name)).unwrap();
            assert_eq!(parsed.action, command.action);
            assert!(
                search(command.name)
                    .iter()
                    .any(|found| found.action == command.action)
            );
        }
        assert!(parse("/not-a-command").unwrap_err().contains("/help"));
        assert!(parse("/reset").is_err());
        assert!(
            search("reset")
                .iter()
                .any(|command| command.action == CommandId::Reset)
        );
    }

    #[test]
    fn palette_search_accepts_slash_prefixed_commands_and_arguments() {
        assert!(
            search("  /ses")
                .iter()
                .any(|command| command.action == CommandId::Conversation)
        );
        assert!(
            search("/conversation archive")
                .iter()
                .any(|command| command.stable_id == "conversation.archive.v1")
        );
        assert_eq!(
            parse("/settings diagnostics").unwrap(),
            ParsedCommand {
                action: CommandId::Settings,
                stable_id: "settings.open.v1",
                arguments: "diagnostics".to_owned(),
            }
        );
        assert!(
            search("preferences")
                .iter()
                .any(|command| command.action == CommandId::Settings)
        );
    }
}
