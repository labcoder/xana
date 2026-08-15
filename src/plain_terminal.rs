//! Append-only terminal client for Xana's native runtime protocol.
//!
//! This module owns readline input, permission prompts, and human rendering. It
//! does not own conversation history or call providers and tools directly.

use crate::{
    agent::SessionUsage,
    artifact::ArtifactStore,
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    identity::{OperationId, PrincipalId, SessionId, ToolInvocationId},
    message::{ContentBlock, Message},
    model_catalog::{ExecutionKind, ModelManager},
    native_runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
    oneshot::{ExitCategory, OneShotFailure, OneShotSuccess},
    orchestration::{ChildActivity, ChildInspection},
    permission::{ControllerDecision, PermissionRequest, PermissionScope},
    presentation::{ResolvedPresentation, SemanticToken},
    vision::{
        DroppedImagePath, ImageIngestor, ImageLimits, MAX_IMAGE_BYTES_PER_TURN,
        MAX_IMAGES_PER_TURN, PendingImages, classify_dropped_image_path, image_paths_in_text,
    },
    workspace_host::{ConversationRef, WorkspaceHost},
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
    pub(crate) presentation: ResolvedPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatExit {
    Quit,
    Restart,
    NewConversation,
    Doctor(Option<SessionId>),
    Reset,
    Setup(String),
    ControlCommand { family: String, arguments: String },
}

#[derive(Debug, PartialEq, Eq)]
enum InputAction<'a> {
    Quit,
    Clear,
    Attach(&'a str),
    Model(&'a str),
    Doctor,
    Setup(&'a str),
    Usage,
    ControlCommand { family: &'a str, arguments: &'a str },
    Agents,
    Agent(&'a str),
    CancelAgent(&'a str),
    Ignore,
    Send(&'a str),
}

fn classify_input(line: &str) -> InputAction<'_> {
    let trimmed = line.trim();
    for family in [
        "project",
        "profile",
        "skill",
        "plugin",
        "mcp",
        "external-agent",
        "image",
    ] {
        let command = format!("/{family}");
        if trimmed == command {
            return InputAction::ControlCommand {
                family,
                arguments: "list",
            };
        }
        if let Some(arguments) = trimmed.strip_prefix(&format!("{command} ")) {
            return InputAction::ControlCommand {
                family,
                arguments: arguments.trim(),
            };
        }
    }
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
    if trimmed == "/setup" {
        return InputAction::Setup("");
    }
    if trimmed == "/usage" {
        return InputAction::Usage;
    }
    if trimmed == "/doctor" {
        return InputAction::Doctor;
    }
    if let Some(section) = trimmed.strip_prefix("/setup ") {
        return InputAction::Setup(section.trim());
    }
    match trimmed {
        "/quit" => InputAction::Quit,
        "/clear" => InputAction::Clear,
        "" => InputAction::Ignore,
        input => InputAction::Send(input),
    }
}

fn write_assistant<W: Write>(
    output: &mut W,
    message: &Message,
    presentation: ResolvedPresentation,
) -> io::Result<()> {
    write!(
        output,
        "{} ",
        presentation.paint(SemanticToken::Assistant, "xana>")
    )?;
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
    presentation: ResolvedPresentation,
    usage: SessionUsage,
}

impl<W: Write> EventRenderer<W> {
    fn new(output: W, presentation: ResolvedPresentation) -> Self {
        Self {
            output,
            streaming_text: false,
            streaming_step: None,
            presentation,
            usage: SessionUsage::default(),
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
                    write!(
                        self.output,
                        "{} ",
                        self.presentation.paint(SemanticToken::Assistant, "xana>")
                    )?;
                    self.streaming_text = true;
                    self.streaming_step = Some(*step_id);
                }
                write!(self.output, "{text}")?;
                self.output.flush()?;
            }
            AgentEvent::ProviderReasoningDelta { text, .. } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "{} {text}",
                    self.presentation
                        .paint(SemanticToken::Reasoning, "thinking>")
                )?;
            }
            AgentEvent::PermissionRequested { request } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "{}\ntool: {}\neffect: {:?}\nscope: {}\narguments: {}\nThis effect uses Xana's ordinary host permissions; it is not contained.",
                    self.presentation
                        .paint(SemanticToken::Approval, "xana> permission required"),
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
                    write_assistant(&mut self.output, message, self.presentation)?;
                }
            }
            AgentEvent::UsageObserved { usage, .. } => self.usage.observe(*usage),
            AgentEvent::OperationFailed { reason, .. } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "{} {reason}",
                    self.presentation
                        .paint(SemanticToken::Danger, "xana> error:")
                )?;
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
                ChildActivity::ProviderReasoningDelta { text, .. } => {
                    self.finish_stream()?;
                    writeln!(
                        self.output,
                        "xana> child {} [{}] thinking: {text}",
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
                ChildActivity::ManagedRuntime { notification } => {
                    self.finish_stream()?;
                    render_managed_child_activity(&mut self.output, attribution, notification)?;
                }
                ChildActivity::ExternalAgent { activity } => {
                    self.finish_stream()?;
                    render_external_agent_activity(&mut self.output, activity)?;
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
            AgentEvent::ExternalAgentActivity { activity, .. } => {
                self.finish_stream()?;
                render_external_agent_activity(&mut self.output, activity)?;
            }
        }
        Ok(())
    }

    fn write_usage(&mut self) -> io::Result<()> {
        self.finish_stream()?;
        writeln!(self.output, "xana> usage: {}", self.usage.render())
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

fn render_external_agent_activity(
    output: &mut impl Write,
    activity: &crate::a2a::ExternalAgentActivity,
) -> io::Result<()> {
    use crate::a2a::ExternalAgentActivityKind;

    let prefix = format!(
        "xana> external {} [{}]",
        activity.agent_name, activity.connection
    );
    match &activity.activity {
        ExternalAgentActivityKind::Sending {
            classes,
            total_bytes,
        } => {
            writeln!(
                output,
                "{prefix} sending {total_bytes} reviewed bytes as {classes:?}"
            )
        }
        ExternalAgentActivityKind::TaskIdentified { task_id, .. } => {
            writeln!(output, "{prefix} task {task_id}")
        }
        ExternalAgentActivityKind::Status { state, message } => writeln!(
            output,
            "{prefix} status {state}{}",
            message
                .as_deref()
                .map(|message| format!(": {message}"))
                .unwrap_or_default()
        ),
        ExternalAgentActivityKind::Message { text } => writeln!(output, "{prefix}> {text}"),
        ExternalAgentActivityKind::Artifact {
            name,
            media_type,
            byte_len,
        } => writeln!(
            output,
            "{prefix} artifact {name} ({media_type}, {byte_len} bytes)"
        ),
        ExternalAgentActivityKind::CancellationRequested { task_id } => {
            writeln!(output, "{prefix} requested cancellation for task {task_id}")
        }
        ExternalAgentActivityKind::Detached { task_id } => writeln!(
            output,
            "{prefix} task {task_id} detached; remote outcome is unknown"
        ),
    }
}

fn render_managed_child_activity(
    output: &mut impl Write,
    attribution: &crate::orchestration::ChildAttribution,
    notification: &crate::managed::codex::ManagedNotification,
) -> io::Result<()> {
    use crate::managed::codex::ManagedNotification;

    let prefix = format!(
        "xana> child {} [{}; Codex]",
        attribution.agent_id, attribution.route
    );
    match notification {
        ManagedNotification::ThreadStarted { thread_id } => {
            writeln!(output, "{prefix} opened managed thread {thread_id}")
        }
        ManagedNotification::AssistantDelta { delta, .. } => {
            writeln!(output, "{prefix}: {delta}")
        }
        ManagedNotification::ReasoningSummaryDelta { delta, .. } => {
            writeln!(output, "{prefix} summary> {delta}")
        }
        ManagedNotification::ReasoningDelta { delta, .. } => {
            writeln!(output, "{prefix} reasoning> {delta}")
        }
        ManagedNotification::PlanDelta { delta, .. } => {
            writeln!(output, "{prefix} plan> {delta}")
        }
        ManagedNotification::CommandOutputDelta { delta, .. } => {
            writeln!(output, "{prefix} tool> {delta}")
        }
        ManagedNotification::PlanUpdated { explanation, steps } => writeln!(
            output,
            "{prefix} plan updated: {} ({} steps)",
            explanation.as_deref().unwrap_or("no explanation"),
            steps.len()
        ),
        ManagedNotification::DiffUpdated(_) => {
            writeln!(output, "{prefix} working diff updated")
        }
        ManagedNotification::ItemStarted(item) => {
            writeln!(output, "{prefix} started {}", item.label)
        }
        ManagedNotification::ItemCompleted(item) => {
            writeln!(output, "{prefix} finished {}", item.label)
        }
        ManagedNotification::ModelRerouted {
            from_model,
            to_model,
            reason,
        } => writeln!(
            output,
            "{prefix} rerouted {from_model} to {to_model}: {reason}"
        ),
        ManagedNotification::TurnCompleted {
            turn_id,
            status,
            error,
        } => writeln!(
            output,
            "{prefix} turn {turn_id}: {status}{}",
            error
                .as_deref()
                .map(|value| format!(": {value}"))
                .unwrap_or_default()
        ),
        ManagedNotification::TokenUsageUpdated {
            turn_id,
            input_tokens,
            output_tokens,
            total_tokens,
            ..
        } => writeln!(
            output,
            "{prefix} turn {turn_id} usage: input {input_tokens}, output {output_tokens}, total {total_tokens}"
        ),
        ManagedNotification::Warning(message) => writeln!(output, "{prefix} warning: {message}"),
        ManagedNotification::ReasoningSummaryPartAdded { .. }
        | ManagedNotification::LoginCompleted { .. }
        | ManagedNotification::Other { .. } => Ok(()),
    }
}

fn embedded_client(runtime: RuntimeHandle, header: &ChatHeader) -> EmbeddedClient {
    let seed = ClientSnapshotSeed {
        session_id: header.session_id,
        connection: header.provider_name.clone(),
        execution_owner: "native".to_owned(),
        model: header.model.clone(),
        reasoning_effort: None,
        children: header.children.clone(),
    };
    EmbeddedClient::from_runtime(runtime, seed)
}

pub(crate) async fn run_chat(
    runtime: RuntimeHandle,
    header: ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
) -> Result<ChatExit> {
    let mut runtime = embedded_client(runtime, &header);
    debug_assert_eq!(runtime.snapshot().session_id, header.session_id);
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
    let mut renderer = EventRenderer::new(stdout.lock(), header.presentation);
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
                        println!("xana> usage: /attach WORKSPACE_RELATIVE_IMAGE_PATH|--clipboard");
                        continue;
                    }
                    let attachment = if path == "--clipboard" {
                        crate::vision::ingest_clipboard_image(
                            header.artifact_store.clone(),
                            header.owner,
                        )
                    } else {
                        ImageIngestor::new(header.artifact_store.clone(), ImageLimits::default())
                            .ingest_path(&header.workspace_root, path, header.owner)
                            .map_err(|error| format!("could not attach {path}: {error}"))
                    };
                    match attachment {
                        Ok(attachment) => {
                            pending_images.push(attachment);
                            println!(
                                "xana> staged image {path} ({} pending)",
                                pending_images.len()
                            );
                        }
                        Err(error) => println!("xana> {error}"),
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
                InputAction::Setup(section) => match crate::setup::args_for_request(section) {
                    Ok(_) => {
                        runtime.send(RuntimeCommand::Shutdown).await?;
                        exit = ChatExit::Setup(section.to_owned());
                        break;
                    }
                    Err(error) => println!("xana> {error}"),
                },
                InputAction::Usage => renderer.write_usage()?,
                InputAction::ControlCommand { family, arguments } => {
                    runtime.send(RuntimeCommand::Shutdown).await?;
                    exit = ChatExit::ControlCommand {
                        family: family.to_owned(),
                        arguments: arguments.to_owned(),
                    };
                    break;
                }
                InputAction::Doctor => {
                    runtime.send(RuntimeCommand::Shutdown).await?;
                    exit = ChatExit::Doctor(Some(header.session_id));
                    break;
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
                    let descriptor = header
                        .models
                        .descriptor(&header.provider_name, &header.model)?;
                    if descriptor.input_modalities.contains("image") {
                        let paths = image_paths_in_text(input);
                        if paths.len() > MAX_IMAGES_PER_TURN {
                            println!("xana> at most 8 images may be sent in one turn");
                            continue;
                        }
                        let ingestor = ImageIngestor::new(
                            header.artifact_store.clone(),
                            ImageLimits::default(),
                        );
                        let mut resolved = Vec::with_capacity(paths.len());
                        let mut external_paths = Vec::new();
                        let mut invalid = None;
                        for path in paths {
                            match classify_dropped_image_path(&header.workspace_root, &path) {
                                Ok(DroppedImagePath::Workspace { relative }) => {
                                    resolved.push(relative);
                                }
                                Ok(DroppedImagePath::External { canonical }) => {
                                    let canonical = canonical.to_string_lossy().into_owned();
                                    external_paths.push(canonical.clone());
                                    resolved.push(canonical);
                                }
                                Err(error) => {
                                    invalid =
                                        Some(format!("could not attach image {path}: {error}"));
                                    break;
                                }
                            }
                        }
                        if let Some(reason) = invalid {
                            println!("xana> {reason}; message was not sent");
                            continue;
                        }
                        if !external_paths.is_empty() {
                            println!("xana> external images requested:");
                            for path in &external_paths {
                                println!("  {path}");
                            }
                            let answer =
                                editor.readline("xana> read these external images once? [y/N] ");
                            if !matches!(
                                answer.as_deref().map(str::trim),
                                Ok("y" | "Y" | "yes" | "YES" | "Yes")
                            ) {
                                println!(
                                    "xana> external images were not read; message was not sent"
                                );
                                continue;
                            }
                        }
                        let attachments = resolved
                            .iter()
                            .map(|path| {
                                if external_paths.is_empty() {
                                    ingestor.ingest_dropped_path(
                                        &header.workspace_root,
                                        path,
                                        header.owner,
                                    )
                                } else {
                                    ingestor.ingest_approved_dropped_path(
                                        &header.workspace_root,
                                        path,
                                        header.owner,
                                    )
                                }
                            })
                            .collect::<Result<Vec<_>, _>>();
                        match attachments {
                            Ok(attachments) => {
                                for attachment in attachments {
                                    pending_images.push(attachment);
                                }
                            }
                            Err(error) => {
                                println!(
                                    "xana> could not attach images: {error}; message was not sent"
                                );
                                continue;
                            }
                        }
                    }
                    if pending_images.len() > MAX_IMAGES_PER_TURN {
                        println!("xana> at most 8 images may be sent in one turn");
                        continue;
                    }
                    if pending_images.len() > 0 && !descriptor.input_modalities.contains("image") {
                        println!(
                            "xana> {}/{} is not declared image-capable; refresh its catalog or add an explicit model override",
                            header.provider_name, header.model
                        );
                        continue;
                    }
                    let attachments = pending_images.take_for_turn();
                    let total_image_bytes = attachments
                        .iter()
                        .map(|attachment| attachment.image.byte_len)
                        .sum::<u64>();
                    if total_image_bytes > MAX_IMAGE_BYTES_PER_TURN {
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
                    let root_lease = match workspace_host.acquire_root(conversation.clone()) {
                        Ok(lease) => lease,
                        Err(error) => {
                            println!("xana> could not start turn: {error}");
                            continue;
                        }
                    };
                    runtime.send(command).await?;
                    let result = render_operation(&mut runtime, &mut renderer, operation_id).await;
                    drop(root_lease);
                    result?;
                }
            },
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                runtime.send(RuntimeCommand::Shutdown).await?;
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }

    if exit == ChatExit::Quit {
        println!("xana> session: {}", header.session_id);
        println!("xana> resume: xana --plain --resume {}", header.session_id);
        println!(
            "xana> source checkout: cargo run -- --plain --resume {}",
            header.session_id
        );
    }

    Ok(exit)
}

pub(crate) async fn run_one_shot(
    runtime: RuntimeHandle,
    header: &ChatHeader,
    input: String,
    activity: &mut impl Write,
    workspace_host: &WorkspaceHost,
    conversation: ConversationRef,
) -> Result<OneShotSuccess, OneShotFailure> {
    let mut client = embedded_client(runtime, header);
    let _root_lease = workspace_host
        .acquire_root(conversation)
        .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
    let operation_id = OperationId::new();
    client
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input,
        })
        .await
        .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;

    let mut final_text = None;
    let mut failure = None;
    let mut approval_required = false;
    loop {
        let event = client
            .next_event()
            .await
            .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
        match event {
            AgentEvent::PermissionRequested { request } if request.operation_id == operation_id => {
                approval_required = true;
                writeln!(activity, "approval required for {}", request.tool_name).map_err(
                    |error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()),
                )?;
                client
                    .send(RuntimeCommand::DecidePermission {
                        operation_id,
                        invocation_id: request.invocation_id,
                        decision: ControllerDecision::Deny,
                    })
                    .await
                    .map_err(|error| {
                        OneShotFailure::new(ExitCategory::Runtime, error.to_string())
                    })?;
            }
            AgentEvent::ChildActivity {
                attribution,
                activity: ChildActivity::PermissionRequested { request },
            } if attribution.parent_operation_id == operation_id => {
                approval_required = true;
                writeln!(
                    activity,
                    "approval required for child {} tool {}",
                    attribution.agent_id, request.tool_name
                )
                .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
                client
                    .send(RuntimeCommand::DecideChildPermission {
                        agent_id: attribution.agent_id,
                        operation_id: request.operation_id,
                        invocation_id: request.invocation_id,
                        decision: ControllerDecision::Deny,
                    })
                    .await
                    .map_err(|error| {
                        OneShotFailure::new(ExitCategory::Runtime, error.to_string())
                    })?;
            }
            AgentEvent::AssistantMessage { message, .. } => {
                final_text = Some(message_text(&message));
            }
            AgentEvent::OperationFailed { reason, .. } => failure = Some(reason),
            AgentEvent::CommandRejected { reason } => {
                return Err(OneShotFailure::new(ExitCategory::Runtime, reason));
            }
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle,
            } => {
                writeln!(
                    activity,
                    "child {} [{}]: {:?}",
                    attribution.agent_id, attribution.route, lifecycle
                )
                .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
            }
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(outcome),
            } if actual == operation_id => {
                return match outcome {
                    OperationOutcome::Completed => Ok(OneShotSuccess {
                        text: final_text.unwrap_or_default(),
                        session_id: Some(header.session_id),
                        execution_owner: "native",
                    }),
                    OperationOutcome::Declined if approval_required => Err(OneShotFailure::new(
                        ExitCategory::Approval,
                        "one-shot execution requires interactive approval and was denied",
                    )),
                    OperationOutcome::Interrupted => Err(OneShotFailure::new(
                        ExitCategory::Interrupted,
                        "one-shot execution was interrupted",
                    )),
                    OperationOutcome::Failed | OperationOutcome::Declined => {
                        Err(OneShotFailure::new(
                            ExitCategory::Runtime,
                            failure.unwrap_or_else(|| "one-shot execution failed".to_owned()),
                        ))
                    }
                };
            }
            _ => {}
        }
    }
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn write_child_summary<W: Write>(output: &mut W, child: &ChildInspection) -> io::Result<()> {
    let attribution = &child.handle.admission.attribution;
    write!(
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
    )?;
    if let Some(plan) = &child.handle.admission.plan {
        write!(
            output,
            " plan={} step={}[{}]",
            plan.plan_id, plan.step_id, plan.output_index
        )?;
    }
    writeln!(output)
}

async fn render_until_child_control_result<W: Write>(
    runtime: &mut EmbeddedClient,
    renderer: &mut EventRenderer<W>,
) -> Result<()> {
    loop {
        let event = runtime.next_event().await?;
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
    runtime: &mut EmbeddedClient,
    renderer: &mut EventRenderer<W>,
) -> Result<()> {
    let event = runtime.next_event().await?;
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
    runtime: &mut EmbeddedClient,
    renderer: &mut EventRenderer<W>,
    operation_id: OperationId,
) -> Result<OperationOutcome> {
    loop {
        let event = runtime.next_event().await?;
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
    runtime: &EmbeddedClient,
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
        PermissionScope::ExternalPath { canonical_path } => {
            format!("external path {}", canonical_path.display())
        }
        PermissionScope::Command {
            shell,
            canonical_cwd,
            command,
        } => format!(
            "command {command:?} via {shell} in {}",
            canonical_cwd.display()
        ),
        PermissionScope::External {
            recipient_identity_digest,
            operation,
        } => format!(
            "external operation {operation:?} for recipient {}",
            &recipient_identity_digest[..recipient_identity_digest.len().min(12)]
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
        assert_eq!(classify_input("/doctor"), InputAction::Doctor);
        assert_eq!(classify_input("/setup"), InputAction::Setup(""));
        assert_eq!(classify_input("/usage"), InputAction::Usage);
        assert_eq!(
            classify_input("/setup appearance"),
            InputAction::Setup("appearance")
        );
        assert_eq!(classify_input("/setupfoo"), InputAction::Send("/setupfoo"));
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
        assert_eq!(
            classify_input("/project"),
            InputAction::ControlCommand {
                family: "project",
                arguments: "list"
            }
        );
        assert_eq!(
            classify_input("/profile resolve review --json"),
            InputAction::ControlCommand {
                family: "profile",
                arguments: "resolve review --json"
            }
        );
        assert_eq!(
            classify_input("/skill activate project/review"),
            InputAction::ControlCommand {
                family: "skill",
                arguments: "activate project/review"
            }
        );
        assert_eq!(
            classify_input("/plugin list"),
            InputAction::ControlCommand {
                family: "plugin",
                arguments: "list"
            }
        );
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
            let mut renderer = EventRenderer::new(
                &mut output,
                crate::presentation::ResolvedPresentation::test_plain(),
            );

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
                plan: None,
                task_preview: "review".to_owned(),
                task_hash: blake3::hash(b"review").to_hex().to_string(),
                result_schema: crate::orchestration::ChildResultSchema::Summary,
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
            let mut renderer = EventRenderer::new(
                &mut output,
                crate::presentation::ResolvedPresentation::test_plain(),
            );
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
