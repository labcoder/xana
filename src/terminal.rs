//! Blocking terminal chat frontend.
//!
//! This module owns readline input, transient conversation history, and human
//! rendering. It receives a fully constructed `Agent` and never loads config,
//! reads environment variables, or knows provider wire types.

mod provisional_approval;

use crate::{
    agent::Agent,
    message::{ContentBlock, Message, Role},
};
use anyhow::{Context, Result};
use rustyline::{DefaultEditor, error::ReadlineError};

pub(crate) use provisional_approval::terminal_approver;

pub(crate) struct ChatHeader {
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) endpoint: String,
    pub(crate) context_report: String,
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

pub(crate) fn run_chat(agent: Agent, header: ChatHeader) -> Result<()> {
    println!("provider connection: {}", header.provider_name);
    println!("model: {}", header.model);
    println!("chat endpoint: {}", header.endpoint);
    println!("context plan:\n{}", header.context_report);

    let mut editor = DefaultEditor::new().context("could not initialize line editor")?;
    let mut messages = Vec::new();

    loop {
        match editor.readline("you> ") {
            Ok(line) => match classify_input(&line) {
                InputAction::Quit => break,
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
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
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
