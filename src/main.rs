mod config;
mod message;
mod provider;
mod tool;

use anyhow::{Context, Result, bail};
use config::{XanaConfig, config_path};
use message::{ContentBlock, Message, Role, ToolCall};
use provider::openai_compat::OpenAiCompatClient;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::path::{Path, PathBuf};
use tool::ToolDefinition;

const MAX_TOOL_ROUNDS: usize = 8;

fn requested_tools(message: &Message) -> Vec<ToolCall> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call.clone()),
            ContentBlock::Text(_) | ContentBlock::ToolResult(_) => None,
        })
        .collect()
}

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

fn run_turn(
    provider: &OpenAiCompatClient,
    definitions: &[ToolDefinition],
    workspace_root: &Path,
    messages: &mut Vec<Message>,
) -> Result<()> {
    for _ in 0..MAX_TOOL_ROUNDS {
        let assistant = provider.send_message(messages, definitions)?;
        let calls = requested_tools(&assistant);

        if calls.is_empty() {
            print_assistant(&assistant);
            messages.push(assistant);
            return Ok(());
        }

        messages.push(assistant);

        for call in calls {
            println!("xana> using {}", call.name);
            let result = tool::execute(&call, workspace_root);
            messages.push(Message::tool_result(result));
        }
    }

    bail!("model exceeded the {MAX_TOOL_ROUNDS}-round tool limit")
}

fn run_chat(config: XanaConfig, workspace_root: PathBuf) -> Result<()> {
    let provider = OpenAiCompatClient::new(config.base_url, config.model);
    let definitions = tool::definitions();
    let mut editor = DefaultEditor::new().context("could not initialize line editor")?;
    let mut messages = Vec::new();

    println!("chat endpoint: {}", provider.endpoint());

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
                    run_turn(&provider, &definitions, &workspace_root, &mut messages)?;
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

    #[test]
    fn requested_tools_preserve_model_order_and_values() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("I'll inspect both.".to_owned()),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-a".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-b".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "b.txt"}),
                }),
            ],
        };

        let calls = requested_tools(&message);

        assert_eq!(
            calls,
            vec![
                ToolCall {
                    id: "call-a".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                ToolCall {
                    id: "call-b".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "b.txt"}),
                },
            ]
        );
    }

    #[test]
    fn requested_tools_are_empty_for_final_text() {
        let message = Message::text(Role::Assistant, "Finished.");

        assert!(requested_tools(&message).is_empty());
    }
}
