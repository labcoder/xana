mod agent;
mod config;
mod message;
mod provider;
mod tool;

use agent::Agent;
use anyhow::{Context, Result};
use config::{XanaConfig, config_path};
use message::{ContentBlock, Message, Role};
use provider::openai_compat::OpenAiCompatClient;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::path::PathBuf;
use tool::ToolRegistry;

const MAX_TOOL_ROUNDS: usize = 8;

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
    let provider = OpenAiCompatClient::new(config.base_url, config.model);
    println!("chat endpoint: {}", provider.endpoint());

    let tools = ToolRegistry::builtins().context("could not build tool registry")?;
    let agent = Agent::new(provider, tools, workspace_root, MAX_TOOL_ROUNDS);
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

fn main() -> anyhow::Result<()> {
    let path = config_path().context("could not resolve Xana config path")?;

    println!("loading Xana config from {}", path.display());

    let config = XanaConfig::load_from(&path)
        .with_context(|| format!("failed to load config from {}", path.display()))?;

    let workspace_root =
        std::env::current_dir().context("could not resolve Xana workspace root")?;

    run_chat(config, workspace_root)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
