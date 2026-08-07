//! Terminal client for Xana's foreground runtime protocol.
//!
//! This module owns readline input, approval prompts, and human rendering. It
//! does not own conversation history or call providers and tools directly.

use crate::{
    identity::{OperationId, ToolInvocationId},
    message::{ContentBlock, Message},
    runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
};
use anyhow::{Context, Result, bail};
use rustyline::{DefaultEditor, error::ReadlineError};
use std::io::{self, BufRead, Write};

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

fn write_assistant<W: Write>(output: &mut W, message: &Message) -> io::Result<()> {
    write!(output, "xana> ")?;
    for block in &message.content {
        match block {
            ContentBlock::Text(text) => write!(output, "{text}")?,
            ContentBlock::ToolCall(tool_call) => {
                write!(output, "[tool call requested: {}]", tool_call.name)?;
            }
            ContentBlock::ToolResult(_) => {}
        }
    }
    writeln!(output)
}

struct EventRenderer<W> {
    output: W,
    streaming_text: bool,
    streaming_step: Option<crate::identity::StepId>,
}

impl<W: Write> EventRenderer<W> {
    fn new(output: W) -> Self {
        Self {
            output,
            streaming_text: false,
            streaming_step: None,
        }
    }

    fn render(&mut self, event: &AgentEvent) -> io::Result<()> {
        match event {
            AgentEvent::OperationStateChanged {
                state: OperationState::Running,
                ..
            } => {}
            AgentEvent::OperationStateChanged {
                state: OperationState::Suspended,
                ..
            } => {}
            AgentEvent::OperationStateChanged {
                state: OperationState::Finished(_),
                ..
            } => {
                self.finish_stream()?;
            }
            AgentEvent::AssistantTextDelta { step_id, text, .. } => {
                if self.streaming_step.is_some_and(|active| active != *step_id) {
                    self.finish_stream()?;
                }
                if !self.streaming_text {
                    write!(self.output, "xana> ")?;
                    self.streaming_text = true;
                    self.streaming_step = Some(*step_id);
                }
                write!(self.output, "{text}")?;
                self.output.flush()?;
            }
            AgentEvent::ProvisionalApprovalRequested { action, .. } => {
                self.finish_stream()?;
                writeln!(self.output, "xana> approval required:\n{action}")?;
            }
            AgentEvent::ToolFinished { .. } => {
                self.finish_stream()?;
            }
            AgentEvent::AssistantMessage { message, .. } => {
                if self.streaming_text {
                    self.finish_stream()?;
                } else {
                    write_assistant(&mut self.output, message)?;
                }
            }
            AgentEvent::OperationFailed { reason, .. } => {
                self.finish_stream()?;
                writeln!(self.output, "xana> error: {reason}")?;
            }
            AgentEvent::ConversationCleared => {
                writeln!(self.output, "xana> conversation cleared")?;
            }
            AgentEvent::CommandRejected { reason } => {
                writeln!(self.output, "xana> command rejected: {reason}")?;
            }
        }
        Ok(())
    }

    fn finish_stream(&mut self) -> io::Result<()> {
        if self.streaming_text {
            writeln!(self.output)?;
            self.streaming_text = false;
            self.streaming_step = None;
        }
        Ok(())
    }
}

pub(crate) async fn run_chat(mut runtime: RuntimeHandle, header: ChatHeader) -> Result<()> {
    println!("provider connection: {}", header.provider_name);
    println!("model: {}", header.model);
    println!("chat endpoint: {}", header.endpoint);
    println!("context plan:\n{}", header.context_report);

    let mut editor = DefaultEditor::new().context("could not initialize line editor")?;
    let stdout = anstream::stdout();
    let mut renderer = EventRenderer::new(stdout.lock());

    loop {
        match editor.readline("you> ") {
            Ok(line) => match classify_input(&line) {
                InputAction::Quit => {
                    runtime.send(RuntimeCommand::Shutdown).await?;
                    break;
                }
                InputAction::Clear => {
                    runtime.send(RuntimeCommand::ClearConversation).await?;
                    render_until_clear_result(&mut runtime, &mut renderer).await?;
                }
                InputAction::Ignore => {}
                InputAction::Send(input) => {
                    editor
                        .add_history_entry(input)
                        .context("could not add input to editor history")?;

                    let operation_id = OperationId::new();
                    runtime
                        .send(RuntimeCommand::SubmitTurn {
                            operation_id,
                            input: input.to_owned(),
                        })
                        .await?;
                    render_operation(&mut runtime, &mut renderer, operation_id).await?;
                }
            },
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                runtime.send(RuntimeCommand::Shutdown).await?;
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

async fn render_until_clear_result<W: Write>(
    runtime: &mut RuntimeHandle,
    renderer: &mut EventRenderer<W>,
) -> Result<()> {
    let Some(event) = runtime.next_event().await else {
        bail!("Xana's foreground runtime stopped before clearing the conversation");
    };
    let finished = matches!(
        event,
        AgentEvent::ConversationCleared | AgentEvent::CommandRejected { .. }
    );
    renderer.render(&event)?;
    if !finished {
        bail!("foreground runtime returned an unexpected clear response");
    }
    Ok(())
}

async fn render_operation<W: Write>(
    runtime: &mut RuntimeHandle,
    renderer: &mut EventRenderer<W>,
    operation_id: OperationId,
) -> Result<OperationOutcome> {
    loop {
        let Some(event) = runtime.next_event().await else {
            bail!("Xana's foreground runtime stopped during operation {operation_id}");
        };
        renderer.render(&event)?;

        match event {
            AgentEvent::ProvisionalApprovalRequested {
                operation_id: requested_operation,
                invocation_id,
                ..
            } if requested_operation == operation_id => {
                let approved = confirm_once()?;
                send_approval(runtime, operation_id, invocation_id, approved).await?;
            }
            AgentEvent::OperationStateChanged {
                operation_id: finished_operation,
                state: OperationState::Finished(outcome),
            } if finished_operation == operation_id => return Ok(outcome),
            AgentEvent::CommandRejected { .. } => return Ok(OperationOutcome::Declined),
            _ => {}
        }
    }
}

async fn send_approval(
    runtime: &RuntimeHandle,
    operation_id: OperationId,
    invocation_id: ToolInvocationId,
    approved: bool,
) -> Result<()> {
    runtime
        .send(RuntimeCommand::DecideProvisionalApproval {
            operation_id,
            invocation_id,
            approved,
        })
        .await?;
    Ok(())
}

fn confirm_once() -> io::Result<bool> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = anstream::stdout();
    let mut output = stdout.lock();
    confirm_with_io(&mut input, &mut output)
}

fn confirm_with_io<R: BufRead, W: Write>(input: &mut R, output: &mut W) -> io::Result<bool> {
    write!(output, "approve once? [y/N] ")?;
    output.flush()?;

    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        return Ok(false);
    }

    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
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
    fn pure_renderer_streams_once_and_reports_terminal_outcome() {
        let operation_id = OperationId::new();
        let step_id = crate::identity::StepId::new();
        let mut output = Vec::new();
        {
            let mut renderer = EventRenderer::new(&mut output);

            renderer
                .render(&AgentEvent::AssistantTextDelta {
                    operation_id,
                    step_id,
                    text: "hel".to_owned(),
                })
                .expect("first delta");
            renderer
                .render(&AgentEvent::AssistantTextDelta {
                    operation_id,
                    step_id,
                    text: "lo".to_owned(),
                })
                .expect("second delta");
            renderer
                .render(&AgentEvent::AssistantMessage {
                    operation_id,
                    message: Message::text(crate::message::Role::Assistant, "hello"),
                })
                .expect("final message");
            renderer
                .render(&AgentEvent::OperationStateChanged {
                    operation_id,
                    state: OperationState::Finished(OperationOutcome::Completed),
                })
                .expect("terminal state");
        }
        assert_eq!(String::from_utf8(output).expect("UTF-8"), "xana> hello\n");
    }

    #[test]
    fn approval_defaults_to_denied_and_requires_explicit_yes() {
        for (answer, expected) in [("y\n", true), ("YES\r\n", true), ("\n", false), ("", false)] {
            let mut input = io::Cursor::new(answer.as_bytes());
            let mut output = Vec::new();
            assert_eq!(
                confirm_with_io(&mut input, &mut output).expect("approval answer"),
                expected
            );
            assert_eq!(output, b"approve once? [y/N] ");
        }
    }
}
