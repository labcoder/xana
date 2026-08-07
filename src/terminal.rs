//! Terminal client for Xana's foreground runtime protocol.
//!
//! This module owns readline input, permission prompts, and human rendering. It
//! does not own conversation history or call providers and tools directly.

use crate::{
    identity::{OperationId, SessionId, ToolInvocationId},
    message::{ContentBlock, Message},
    permission::{ControllerDecision, PermissionRequest, PermissionScope},
    runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
};
use anyhow::{Context, Result, bail};
use rustyline::{DefaultEditor, error::ReadlineError};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

pub(crate) struct ChatHeader {
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) endpoint: String,
    pub(crate) context_report: String,
    pub(crate) session_id: SessionId,
    pub(crate) session_path: PathBuf,
    pub(crate) resumed: bool,
    pub(crate) repair_truncate_to: Option<u64>,
    pub(crate) unfinished: Vec<(OperationId, OperationState)>,
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
            AgentEvent::PermissionRequested { request } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "xana> permission required\ntool: {}\neffect: {:?}\nscope: {}\narguments: {}\nThis effect uses Xana's ordinary host permissions; it is not contained.",
                    request.tool_name,
                    request.effect_class,
                    display_scope(&request.scope),
                    request.final_arguments
                )?;
            }
            AgentEvent::PermissionAudited { .. } => {}
            AgentEvent::InvocationIntentCommitted { .. }
            | AgentEvent::InvocationResultCommitted { .. } => {}
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
    println!("session: {}", header.session_id);
    println!("session file: {}", header.session_path.display());
    if header.resumed {
        println!("resumed: yes");
    }
    if let Some(offset) = header.repair_truncate_to {
        println!("repaired torn session tail at byte {offset}");
    }
    if !header.unfinished.is_empty() {
        println!(
            "unfinished operations restored without replay: {}",
            header.unfinished.len()
        );
    }

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
            AgentEvent::PermissionRequested { request } if request.operation_id == operation_id => {
                let decision = prompt_permission_decision(&request)?;
                send_permission_decision(runtime, operation_id, request.invocation_id, decision)
                    .await?;
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

async fn send_permission_decision(
    runtime: &RuntimeHandle,
    operation_id: OperationId,
    invocation_id: ToolInvocationId,
    decision: ControllerDecision,
) -> Result<()> {
    runtime
        .send(RuntimeCommand::DecidePermission {
            operation_id,
            invocation_id,
            decision,
        })
        .await?;
    Ok(())
}

pub(crate) fn prompt_permission_decision(
    request: &PermissionRequest,
) -> io::Result<ControllerDecision> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = anstream::stdout();
    let mut output = stdout.lock();
    permission_decision_with_io(request, &mut input, &mut output)
}

fn permission_decision_with_io<R: BufRead, W: Write>(
    request: &PermissionRequest,
    input: &mut R,
    output: &mut W,
) -> io::Result<ControllerDecision> {
    write!(output, "decision [d=deny/o=once/s=session; default d]: ")?;
    output.flush()?;

    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        return Ok(ControllerDecision::Deny);
    }

    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "o" | "once" | "y" | "yes" => ControllerDecision::AllowOnce,
        "s" | "session" => ControllerDecision::AllowSession {
            scope: request.scope.clone(),
        },
        _ => ControllerDecision::Deny,
    })
}

fn display_scope(scope: &PermissionScope) -> String {
    match scope {
        PermissionScope::WorkspacePath { canonical_path } => {
            format!("workspace path {}", canonical_path.display())
        }
        PermissionScope::Command {
            shell,
            canonical_cwd,
            command,
        } => format!(
            "command {command:?} via {shell} in {}",
            canonical_cwd.display()
        ),
        PermissionScope::Unscoped => "unscoped".to_owned(),
    }
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
    fn permission_prompt_offers_once_session_and_fail_closed_default() {
        let request = PermissionRequest {
            operation_id: OperationId::new(),
            invocation_id: ToolInvocationId::new(),
            tool_name: "read_file".to_owned(),
            effect_class: crate::tool::EffectClass::Read,
            final_arguments: serde_json::json!({"path": "README.md"}),
            scope: PermissionScope::Unscoped,
        };
        for (answer, expected) in [
            ("o\n", ControllerDecision::AllowOnce),
            (
                "session\r\n",
                ControllerDecision::AllowSession {
                    scope: PermissionScope::Unscoped,
                },
            ),
            ("\n", ControllerDecision::Deny),
            ("", ControllerDecision::Deny),
        ] {
            let mut input = io::Cursor::new(answer.as_bytes());
            let mut output = Vec::new();
            assert_eq!(
                permission_decision_with_io(&request, &mut input, &mut output)
                    .expect("permission answer"),
                expected
            );
            assert_eq!(output, b"decision [d=deny/o=once/s=session; default d]: ");
        }
    }
}
