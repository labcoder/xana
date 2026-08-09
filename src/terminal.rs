//! Terminal client for Xana's foreground runtime protocol.
//!
//! This module owns readline input, permission prompts, and human rendering. It
//! does not own conversation history or call providers and tools directly.

use crate::{
    artifact::ArtifactStore,
    identity::{OperationId, PrincipalId, SessionId, ToolInvocationId},
    message::{ContentBlock, Message},
    model::{ExecutionKind, ModelManager},
    orchestration::{ChildActivity, ChildInspection},
    permission::{ControllerDecision, PermissionRequest, PermissionScope},
    runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
    vision::{ImageIngestor, ImageLimits, PendingImages},
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
    pub(crate) children: Vec<ChildInspection>,
    pub(crate) workspace_root: PathBuf,
    pub(crate) artifact_store: ArtifactStore,
    pub(crate) owner: PrincipalId,
    pub(crate) models: ModelManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatExit {
    Quit,
    Restart,
}

#[derive(Debug, PartialEq, Eq)]
enum InputAction<'a> {
    Quit,
    Clear,
    Attach(&'a str),
    Model(&'a str),
    Agents,
    Agent(&'a str),
    CancelAgent(&'a str),
    Ignore,
    Send(&'a str),
}

fn classify_input(line: &str) -> InputAction<'_> {
    let trimmed = line.trim();
    if let Some(agent_id) = trimmed.strip_prefix("/cancel-agent") {
        return InputAction::CancelAgent(agent_id.trim());
    }
    if trimmed == "/agents" {
        return InputAction::Agents;
    }
    if let Some(agent_id) = trimmed.strip_prefix("/agent") {
        return InputAction::Agent(agent_id.trim());
    }
    if let Some(path) = trimmed.strip_prefix("/attach") {
        return InputAction::Attach(path.trim());
    }
    if let Some(selection) = trimmed.strip_prefix("/model") {
        return InputAction::Model(selection.trim());
    }
    match trimmed {
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
            ContentBlock::Image(image) => {
                write!(output, "[image attached: {} bytes]", image.byte_len)?
            }
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
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle,
            } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "xana> child {} [{} via {}/{}]: {:?}",
                    attribution.agent_id,
                    attribution.route,
                    attribution.connection,
                    attribution.model,
                    lifecycle
                )?;
            }
            AgentEvent::ChildActivity {
                attribution,
                activity,
            } => match activity {
                ChildActivity::AssistantTextDelta { text, .. } => {
                    self.finish_stream()?;
                    writeln!(
                        self.output,
                        "xana> child {} [{}]: {text}",
                        attribution.agent_id, attribution.route
                    )?;
                }
                ChildActivity::PermissionRequested { request } => {
                    self.finish_stream()?;
                    writeln!(
                        self.output,
                        "xana> child {} [{}] requires permission\ntool: {}\neffect: {:?}\nscope: {}\narguments: {}\nThis effect uses Xana's ordinary host permissions; it is not contained.",
                        attribution.agent_id,
                        attribution.route,
                        request.tool_name,
                        request.effect_class,
                        display_scope(&request.scope),
                        request.final_arguments
                    )?;
                }
                ChildActivity::PermissionAudited { .. }
                | ChildActivity::ToolFinished { .. }
                | ChildActivity::Suspended => {}
                ChildActivity::Warning { message } => {
                    self.finish_stream()?;
                    writeln!(
                        self.output,
                        "xana> child {} [{}] warning: {message}",
                        attribution.agent_id, attribution.route
                    )?;
                }
            },
            AgentEvent::ChildReportCommitted { report } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "xana> child {} report: {:?}",
                    report.attribution.agent_id, report.status
                )?;
            }
            AgentEvent::ChildListSnapshot { children } => {
                self.finish_stream()?;
                if children.is_empty() {
                    writeln!(self.output, "xana> no child agents")?;
                } else {
                    writeln!(self.output, "xana> child agents:")?;
                    for child in children {
                        write_child_summary(&mut self.output, child)?;
                    }
                }
            }
            AgentEvent::ChildInspectionSnapshot { child } => {
                self.finish_stream()?;
                writeln!(self.output, "xana> child detail:")?;
                write_child_summary(&mut self.output, child)?;
                writeln!(
                    self.output,
                    "    parent operation={} child operation={} thread={} profile={} usage={:?} report={:?}",
                    child.handle.admission.attribution.parent_operation_id,
                    child.handle.admission.attribution.operation_id,
                    child.handle.admission.attribution.thread_id,
                    child.handle.admission.attribution.profile,
                    child.handle.usage,
                    child.handle.report,
                )?;
            }
            AgentEvent::ChildCancellationRequested { receipt } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "xana> child {} cancellation {} (current state: {:?}); wait for its terminal event",
                    receipt.handle.admission.attribution.agent_id,
                    if receipt.newly_requested {
                        "requested"
                    } else {
                        "was already requested or terminal"
                    },
                    receipt.handle.lifecycle,
                )?;
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

pub(crate) async fn run_chat(mut runtime: RuntimeHandle, header: ChatHeader) -> Result<ChatExit> {
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
    if !header.children.is_empty() {
        println!(
            "restored child records (read-only): {}",
            header.children.len()
        );
        for child in &header.children {
            println!(
                "  {} [{}]: {:?}{}",
                child.handle.admission.attribution.agent_id,
                child.handle.admission.attribution.route,
                child.handle.lifecycle,
                if child.projected_interruption {
                    " (projected after restart)"
                } else {
                    ""
                }
            );
        }
    }

    let mut editor = DefaultEditor::new().context("could not initialize line editor")?;
    let stdout = anstream::stdout();
    let mut renderer = EventRenderer::new(stdout.lock());
    let mut pending_images = PendingImages::default();
    let mut exit = ChatExit::Quit;

    loop {
        match editor.readline("you> ") {
            Ok(line) => match classify_input(&line) {
                InputAction::Quit => {
                    runtime.send(RuntimeCommand::Shutdown).await?;
                    break;
                }
                InputAction::Clear => {
                    let cleared_images = pending_images.clear();
                    if cleared_images > 0 {
                        println!("xana> cleared {cleared_images} pending image attachment(s)");
                    }
                    runtime.send(RuntimeCommand::ClearConversation).await?;
                    render_until_clear_result(&mut runtime, &mut renderer).await?;
                }
                InputAction::Attach(path) => {
                    if path.is_empty() {
                        println!("xana> usage: /attach WORKSPACE_RELATIVE_IMAGE_PATH");
                        continue;
                    }
                    match ImageIngestor::new(header.artifact_store.clone(), ImageLimits::default())
                        .ingest_path(&header.workspace_root, path, header.owner)
                    {
                        Ok(attachment) => {
                            pending_images.push(attachment);
                            println!(
                                "xana> staged image {path} ({} pending)",
                                pending_images.len()
                            );
                        }
                        Err(error) => println!("xana> could not attach {path}: {error}"),
                    }
                }
                InputAction::Model(selection) => {
                    if selection.is_empty() {
                        write_models(&header.models)?;
                        continue;
                    }
                    let Some((connection, model)) = selection.split_once('/') else {
                        println!("xana> usage: /model CONNECTION/MODEL");
                        continue;
                    };
                    match header.models.select(connection, model) {
                        Ok(_) => {
                            println!(
                                "xana> selected {connection}/{model}; starting a new conversation so runtime ownership remains explicit"
                            );
                            runtime.send(RuntimeCommand::Shutdown).await?;
                            exit = ChatExit::Restart;
                            break;
                        }
                        Err(error) => println!("xana> could not select model: {error}"),
                    }
                }
                InputAction::Agents => {
                    runtime.send(RuntimeCommand::ListChildren).await?;
                    render_until_child_control_result(&mut runtime, &mut renderer).await?;
                }
                InputAction::Agent(value) => {
                    let agent_id = match value.parse() {
                        Ok(agent_id) => agent_id,
                        Err(error) => {
                            println!("xana> invalid child agent id: {error}");
                            continue;
                        }
                    };
                    runtime
                        .send(RuntimeCommand::InspectChild { agent_id })
                        .await?;
                    render_until_child_control_result(&mut runtime, &mut renderer).await?;
                }
                InputAction::CancelAgent(value) => {
                    let agent_id = match value.parse() {
                        Ok(agent_id) => agent_id,
                        Err(error) => {
                            println!("xana> invalid child agent id: {error}");
                            continue;
                        }
                    };
                    runtime
                        .send(RuntimeCommand::CancelChild { agent_id })
                        .await?;
                    render_until_child_control_result(&mut runtime, &mut renderer).await?;
                }
                InputAction::Ignore => {}
                InputAction::Send(input) => {
                    editor
                        .add_history_entry(input)
                        .context("could not add input to editor history")?;

                    let operation_id = OperationId::new();
                    if pending_images.len() > 8 {
                        println!("xana> at most 8 images may be sent in one turn");
                        continue;
                    }
                    if pending_images.len() > 0 {
                        let descriptor = header
                            .models
                            .descriptor(&header.provider_name, &header.model)?;
                        if !descriptor.input_modalities.contains("image") {
                            println!(
                                "xana> {}/{} is not declared image-capable; refresh its catalog or add an explicit model override",
                                header.provider_name, header.model
                            );
                            continue;
                        }
                    }
                    let attachments = pending_images.take_for_turn();
                    let total_image_bytes = attachments
                        .iter()
                        .map(|attachment| attachment.image.byte_len)
                        .sum::<u64>();
                    if total_image_bytes > 20 * 1024 * 1024 {
                        for attachment in attachments {
                            pending_images.push(attachment);
                        }
                        println!("xana> image attachments exceed the 20 MiB per-turn budget");
                        continue;
                    }
                    let images = attachments
                        .into_iter()
                        .map(|attachment| attachment.image)
                        .collect::<Vec<_>>();
                    let command = if images.is_empty() {
                        RuntimeCommand::SubmitTurn {
                            operation_id,
                            input: input.to_owned(),
                        }
                    } else {
                        RuntimeCommand::SubmitTurnWithImages {
                            operation_id,
                            input: input.to_owned(),
                            images,
                        }
                    };
                    runtime.send(command).await?;
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

    Ok(exit)
}

fn write_child_summary<W: Write>(output: &mut W, child: &ChildInspection) -> io::Result<()> {
    let attribution = &child.handle.admission.attribution;
    writeln!(
        output,
        "  {} parent={} route={} owner={} connection={} model={} state={:?}{}",
        attribution.agent_id,
        attribution.parent_agent_id,
        attribution.route,
        attribution.owner.as_str(),
        attribution.connection,
        attribution.model,
        child.handle.lifecycle,
        if child.projected_interruption {
            " (projected after restart)"
        } else {
            ""
        }
    )
}

async fn render_until_child_control_result<W: Write>(
    runtime: &mut RuntimeHandle,
    renderer: &mut EventRenderer<W>,
) -> Result<()> {
    loop {
        let Some(event) = runtime.next_event().await else {
            bail!("Xana's foreground runtime stopped during child control");
        };
        let finished = matches!(
            event,
            AgentEvent::ChildListSnapshot { .. }
                | AgentEvent::ChildInspectionSnapshot { .. }
                | AgentEvent::ChildCancellationRequested { .. }
                | AgentEvent::CommandRejected { .. }
        );
        renderer.render(&event)?;
        if finished {
            return Ok(());
        }
    }
}

fn write_models(models: &ModelManager) -> Result<()> {
    let selected = models.selected()?;
    for summary in models.summaries() {
        let execution = match summary.execution {
            ExecutionKind::Native => "native",
            ExecutionKind::Managed => "managed",
        };
        println!("xana> {} ({execution})", summary.id);
        for model in summary.models {
            let marker = if summary.id == selected.connection && model.id == selected.model {
                "*"
            } else {
                " "
            };
            println!("xana>   {marker} {} — {}", model.id, model.display_name);
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
            AgentEvent::ChildActivity {
                attribution,
                activity: ChildActivity::PermissionRequested { request },
            } if attribution.parent_operation_id == operation_id => {
                let decision = prompt_permission_decision(&request)?;
                runtime
                    .send(RuntimeCommand::DecideChildPermission {
                        agent_id: attribution.agent_id,
                        operation_id: request.operation_id,
                        invocation_id: request.invocation_id,
                        decision,
                    })
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
        assert_eq!(
            classify_input("/attach assets/photo.png"),
            InputAction::Attach("assets/photo.png")
        );
        assert_eq!(classify_input("/attach"), InputAction::Attach(""));
        assert_eq!(classify_input("/agents"), InputAction::Agents);
        assert_eq!(
            classify_input("/agent 018f0000-0000-7000-8000-000000000000"),
            InputAction::Agent("018f0000-0000-7000-8000-000000000000")
        );
        assert_eq!(
            classify_input("/cancel-agent child-id"),
            InputAction::CancelAgent("child-id")
        );
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
    fn child_control_renderer_captures_stable_attributed_output() {
        let session_id = SessionId::new();
        let attribution = crate::orchestration::ChildAttribution {
            agent_id: crate::identity::AgentId::new(),
            parent_agent_id: crate::identity::AgentId::for_session(session_id),
            operation_id: OperationId::new(),
            parent_operation_id: OperationId::new(),
            thread_id: crate::identity::ThreadId::new(),
            route: "worker".to_owned(),
            profile: "reviewer".to_owned(),
            owner: crate::orchestration::ExecutionOwner::Native,
            connection: "local".to_owned(),
            model: "small".to_owned(),
        };
        let mut handle = crate::orchestration::AgentHandleSnapshot::admitted(
            crate::orchestration::ChildAdmission {
                attribution,
                task_preview: "review".to_owned(),
                task_hash: blake3::hash(b"review").to_hex().to_string(),
                capabilities: Vec::new(),
                permission_mode: crate::config::PermissionMode::Deny,
                max_tool_rounds: 1,
                limits: crate::config::OrchestrationLimits::default(),
                hard_token_limit: None,
                hard_spend_microusd: None,
            },
        );
        handle.apply_lifecycle(crate::orchestration::ChildLifecycle::Running);
        let child = ChildInspection {
            handle: handle.clone(),
            report: None,
            projected_interruption: false,
        };
        let mut output = Vec::new();
        {
            let mut renderer = EventRenderer::new(&mut output);
            renderer
                .render(&AgentEvent::ChildListSnapshot {
                    children: vec![child.clone()],
                })
                .expect("child list");
            renderer
                .render(&AgentEvent::ChildInspectionSnapshot {
                    child: Box::new(child),
                })
                .expect("child detail");
            renderer
                .render(&AgentEvent::ChildCancellationRequested {
                    receipt: crate::orchestration::ChildCancellationReceipt {
                        handle,
                        newly_requested: true,
                    },
                })
                .expect("cancellation receipt");
        }
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.contains("route=worker owner=native connection=local model=small"));
        assert!(output.contains("parent operation="));
        assert!(output.contains("cancellation requested"));
        assert!(output.contains("wait for its terminal event"));
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
