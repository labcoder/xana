//! Append-only terminal client for Xana's native runtime protocol.
//!
//! This module owns readline input, permission prompts, and human rendering. It
//! does not own conversation history or call providers and tools directly.

use crate::{
    agent::SessionUsage,
    app::{ChatExit, ChatHeader},
    frontend::{ClientEvent, ClientSnapshotSeed, EmbeddedClient},
    identity::{OperationId, ToolInvocationId},
    message::{ContentBlock, Message},
    model_catalog::{ExecutionKind, ModelManager},
    native_runtime::{
        AgentEvent, OperationOutcome, OperationState, RoundBudgetAction, RoundBudgetSuspension,
        RuntimeCommand, RuntimeHandle,
    },
    oneshot::{ExitCategory, OneShotFailure, OneShotReporter, OneShotSuccess},
    orchestration::{ChildActivity, ChildInspection},
    paths::XanaPaths,
    permission::{ControllerDecision, PermissionRequest, PermissionScope},
    presentation::{ResolvedPresentation, SemanticToken},
    vision::{
        DroppedImagePath, ImageIngestor, ImageLimits, MAX_IMAGE_BYTES_PER_TURN,
        MAX_IMAGES_PER_TURN, PendingImages, classify_dropped_image_path, image_paths_in_text,
    },
    workspace_host::{ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result, bail};
use rustyline::{Editor, error::ReadlineError, history::DefaultHistory};
use std::io::{self, BufRead, Write};

#[derive(Debug, PartialEq, Eq)]
enum InputAction<'a> {
    Quit,
    Clear,
    Compact,
    Attach(&'a str),
    Model(&'a str),
    Vision(&'a str),
    Doctor,
    Setup(&'a str),
    Settings(&'a str),
    Help,
    Usage(&'a str),
    Capabilities,
    ControlCommand { family: &'a str, arguments: &'a str },
    Agents,
    Agent(&'a str),
    CancelAgent(&'a str),
    Ignore,
    Send(&'a str),
}

fn classify_input(line: &str) -> InputAction<'_> {
    let trimmed = line.trim();
    if let Ok(parsed) =
        crate::command_catalog::parse(trimmed, crate::command_catalog::CommandSurface::Plain)
    {
        let arguments = trimmed
            .trim_start_matches('/')
            .split_once(char::is_whitespace)
            .map_or("", |(_, arguments)| arguments.trim());
        if let Some((family, default_arguments)) =
            crate::command_catalog::suspended_chat_control(parsed.stable_id)
        {
            return InputAction::ControlCommand {
                family,
                arguments: if arguments.is_empty() {
                    default_arguments
                } else {
                    arguments
                },
            };
        }
        use crate::command_catalog::CommandAction;
        match parsed.action {
            CommandAction::Project
            | CommandAction::Profile
            | CommandAction::Skill
            | CommandAction::Plugin
            | CommandAction::Mcp
            | CommandAction::ExternalAgent
            | CommandAction::Image => {
                let family = trimmed
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or_default();
                return InputAction::ControlCommand {
                    family,
                    arguments: if arguments.is_empty() {
                        "list"
                    } else {
                        arguments
                    },
                };
            }
            CommandAction::Capabilities => return InputAction::Capabilities,
            CommandAction::Attach => return InputAction::Attach(arguments),
            CommandAction::Model => return InputAction::Model(arguments),
            CommandAction::Vision => return InputAction::Vision(arguments),
            CommandAction::Setup => return InputAction::Setup(arguments),
            CommandAction::Settings => return InputAction::Settings(arguments),
            CommandAction::Help => return InputAction::Help,
            CommandAction::Usage => return InputAction::Usage(arguments),
            CommandAction::Doctor => return InputAction::Doctor,
            CommandAction::Quit => return InputAction::Quit,
            CommandAction::Clear => return InputAction::Clear,
            CommandAction::Compact => return InputAction::Compact,
            CommandAction::Send => return InputAction::Send(arguments),
            CommandAction::Conversation if arguments == "new" => {
                return InputAction::ControlCommand {
                    family: "conversation",
                    arguments,
                };
            }
            CommandAction::Conversation => {
                return InputAction::ControlCommand {
                    family: "conversation",
                    arguments: if arguments.is_empty() {
                        "list"
                    } else {
                        arguments
                    },
                };
            }
            _ => {}
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
    if let Some(selection) = trimmed.strip_prefix("/vision") {
        return InputAction::Vision(selection.trim());
    }
    if trimmed == "/setup" {
        return InputAction::Setup("");
    }
    if trimmed == "/settings" {
        return InputAction::Settings("");
    }
    if let Some(section) = trimmed.strip_prefix("/settings ") {
        return InputAction::Settings(section.trim());
    }
    if trimmed == "/help" {
        return InputAction::Help;
    }
    if let Some(arguments) = trimmed.strip_prefix("/usage")
        && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
    {
        return InputAction::Usage(arguments.trim());
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
        "/compact" => InputAction::Compact,
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
                render_permission_request(&mut self.output, request, &self.presentation)?;
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
            AgentEvent::UsageObserved { usage, .. } => self.usage.observe(usage.clone()),
            AgentEvent::RoundBudgetReached { suspension } => {
                self.finish_stream()?;
                writeln!(
                    self.output,
                    "xana> round budget reached after {} / {} rounds; {} committed tool result(s), {} round(s) remain",
                    suspension.rounds_consumed,
                    suspension.hard_round_limit,
                    suspension.committed.results,
                    suspension.remaining_rounds,
                )?;
                if suspension.repeated_tool_patterns > 0 {
                    writeln!(
                        self.output,
                        "xana> diagnostic: {} repeated tool-call pattern(s) observed",
                        suspension.repeated_tool_patterns,
                    )?;
                }
            }
            AgentEvent::RoundBudgetDecisionCommitted { decision } => {
                writeln!(
                    self.output,
                    "xana> round-budget decision committed: {:?}",
                    decision.action
                )?;
            }
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
            AgentEvent::PromptPlanUpdated { ledger, .. } => {
                writeln!(
                    self.output,
                    "xana> prompt estimate: {} / {} input tokens (window {}, cache facts unavailable)",
                    ledger.estimated_input_tokens,
                    ledger.budget.input_budget_tokens,
                    ledger.budget.context_window_tokens,
                )?;
            }
            AgentEvent::CompactionStarted { reason, .. } => {
                self.finish_stream()?;
                writeln!(self.output, "xana> compacting older context ({reason:?})…")?;
            }
            AgentEvent::ConversationCompacted { checkpoint } => {
                writeln!(
                    self.output,
                    "xana> compacted {} canonical entries; raw history remains unchanged (checkpoint {})",
                    checkpoint.source_entry_count, checkpoint.id,
                )?;
            }
            AgentEvent::CompactionUnavailable { reason, .. } => {
                writeln!(self.output, "xana> compaction unavailable: {reason}")?;
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
                        "xana> child {} [{}] requires permission",
                        attribution.agent_id, attribution.route,
                    )?;
                    render_permission_request(&mut self.output, request, &self.presentation)?;
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

    fn write_usage_details(
        &mut self,
        snapshot: &crate::frontend::ClientSnapshot,
    ) -> io::Result<()> {
        self.finish_stream()?;
        writeln!(self.output, "xana> usage: {}", self.usage.render())?;
        if let Some((run_id, ledger)) = snapshot.prompt_plans.last() {
            writeln!(
                self.output,
                "xana> prompt plan: run {run_id}, ~{} / {} input tokens, {} attachment(s)",
                ledger.estimated_input_tokens,
                ledger.budget.input_budget_tokens,
                ledger.attachment_count,
            )?;
        } else {
            writeln!(self.output, "xana> prompt plan: unavailable")?;
        }
        if snapshot.semantic.execution_facts.is_empty() {
            writeln!(self.output, "xana> execution facts: unavailable")?;
        } else {
            for facts in snapshot.semantic.execution_facts.iter().rev().take(8).rev() {
                writeln!(
                    self.output,
                    "xana> execution: run {} · owner {:?} · host {:?} · workspace {:?} · {}/{} · approval {}",
                    facts.run_id,
                    facts.owner,
                    facts.host,
                    facts.workspace_authority,
                    facts.connection.as_deref().unwrap_or("unknown"),
                    facts.model.as_deref().unwrap_or("unknown"),
                    facts.approval_policy,
                )?;
            }
        }
        if snapshot.semantic.completion_receipts.is_empty() {
            writeln!(self.output, "xana> completion receipts: unavailable")?;
        } else {
            for receipt in snapshot
                .semantic
                .completion_receipts
                .iter()
                .rev()
                .take(8)
                .rev()
            {
                writeln!(
                    self.output,
                    "xana> completion: {} · run {} · {:?} · usage observations {}{}",
                    receipt.id,
                    receipt.run_id,
                    receipt.status,
                    receipt.usage.observation_count,
                    if receipt.usage.incomplete {
                        " · incomplete"
                    } else {
                        ""
                    },
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
        host_location: crate::frontend::semantic::HostLocationV1::Embedded,
        approval_policy: header.permission_mode.as_str().to_owned(),
        children: header.children.clone(),
        resource_policy: header.resource_policy.clone(),
    };
    EmbeddedClient::from_runtime(runtime, seed)
}

pub(crate) async fn run_chat(
    runtime: RuntimeHandle,
    header: ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
    paths: &XanaPaths,
) -> Result<ChatExit> {
    let mut runtime = embedded_client(runtime, &header);
    debug_assert_eq!(runtime.snapshot().session_id, header.session_id);
    println!("provider connection: {}", header.provider_name);
    println!("model: {}", header.model);
    println!("chat endpoint: {}", header.endpoint);
    println!("context plan:\n{}", header.context_report);
    println!("session: {}", header.session_id);
    println!("session file: {}", header.session_path.display());
    println!("commands: /help | /settings [SECTION] | /setup | /doctor | /quit");
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

    let stdout = anstream::stdout();
    let mut renderer = EventRenderer::new(stdout.lock(), header.presentation);
    if let Some(suspension) = &header.round_budget_suspension {
        let root_lease = workspace_host
            .acquire_root(conversation.clone())
            .context("could not resume the suspended native turn")?;
        let result = render_operation(&mut runtime, &mut renderer, suspension.operation_id).await;
        drop(root_lease);
        result?;
    }
    let (mut composer_history, history_load) =
        crate::terminal_productivity::ComposerHistoryStore::open(paths, &header.workspace_root)?;
    if let Some(warning) = history_load.warning {
        println!("xana> warning: {warning}");
    }
    let mut editor =
        Editor::<crate::terminal_productivity::WorkspaceCompleter, DefaultHistory>::new()
            .context("could not initialize line editor")?;
    editor.set_helper(Some(crate::terminal_productivity::WorkspaceCompleter::new(
        header.workspace_root.clone(),
    )));
    for entry in history_load.entries {
        editor
            .add_history_entry(entry)
            .context("could not restore composer history")?;
    }
    let mut pending_images = PendingImages::default();
    let mut pending_vision_route: Option<String> = None;
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
                InputAction::Compact => {
                    let operation_id = OperationId::new();
                    runtime
                        .send(RuntimeCommand::CompactConversation { operation_id })
                        .await?;
                    render_until_compaction_result(&mut runtime, &mut renderer).await?;
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
                InputAction::Vision(selection) => {
                    if selection.is_empty() {
                        let statuses = header.vision.statuses()?;
                        if statuses.is_empty() {
                            println!("xana> no vision.analyze routes are exposed by this profile");
                        } else {
                            println!("xana> vision specialist routes:");
                            for status in statuses {
                                println!(
                                    "  {} [{}]{}",
                                    status.route,
                                    if status.ready { "ready" } else { "unavailable" },
                                    status
                                        .reason
                                        .as_deref()
                                        .map(|reason| format!(" - {reason}"))
                                        .unwrap_or_default()
                                );
                            }
                            println!(
                                "xana> use /vision ROUTE for the next image turn, or /vision auto"
                            );
                        }
                        continue;
                    }
                    if selection == "auto" {
                        pending_vision_route = None;
                        println!(
                            "xana> native vision will be preferred; a default specialist is used only for a text-only model"
                        );
                        continue;
                    }
                    match header.vision.plan(Some(selection)) {
                        Ok(plan) => {
                            println!(
                                "xana> selected {} for the next image turn; {}",
                                selection,
                                plan.preview(0)
                            );
                            pending_vision_route = Some(selection.to_owned());
                        }
                        Err(error) => println!("xana> could not select vision route: {error:#}"),
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
                InputAction::Settings(section) => {
                    if !section.is_empty()
                        && crate::settings::SettingsSection::parse(section).is_none()
                    {
                        println!(
                            "xana> {}",
                            crate::settings::SettingsError::UnknownSection(section.to_owned())
                        );
                        continue;
                    }
                    runtime.send(RuntimeCommand::Shutdown).await?;
                    exit = ChatExit::Settings(section.to_owned());
                    break;
                }
                InputAction::Help => {
                    println!("xana> conversation commands:");
                    for command in crate::command_catalog::slash_commands_for(
                        crate::command_catalog::CommandSurface::Plain,
                    ) {
                        println!("  {:<38} {}", command.usage(), command.summary);
                    }
                }
                InputAction::Usage(arguments) => match arguments {
                    "" | "compact" => renderer.write_usage()?,
                    "details" => renderer.write_usage_details(runtime.snapshot())?,
                    _ => println!("xana> usage: /usage [compact|details]"),
                },
                InputAction::Capabilities => {
                    runtime.send(RuntimeCommand::Shutdown).await?;
                    exit = ChatExit::ControlCommand {
                        family: "capabilities".to_owned(),
                        arguments: String::new(),
                    };
                    break;
                }
                InputAction::ControlCommand { family, arguments } => {
                    runtime.send(RuntimeCommand::Shutdown).await?;
                    let arguments = if matches!(family, "conversation" | "session" | "sessions")
                        && (arguments == "search" || arguments.starts_with("search "))
                    {
                        let conversation = conversation.to_string();
                        let selector = shlex::try_quote(&conversation)
                            .map(|value| value.into_owned())
                            .unwrap_or(conversation);
                        format!("{arguments} --conversation {selector}")
                    } else {
                        arguments.to_owned()
                    };
                    exit = ChatExit::ControlCommand {
                        family: family.to_owned(),
                        arguments,
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
                    match composer_history.record(input) {
                        Ok(Some(entry)) => {
                            editor
                                .add_history_entry(entry)
                                .context("could not add input to editor history")?;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            println!("xana> warning: composer history was not saved: {error:#}")
                        }
                    }

                    let operation_id = OperationId::new();
                    let descriptor = header
                        .models
                        .descriptor(&header.provider_name, &header.model)?;
                    {
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
                        .iter()
                        .map(|attachment| attachment.image.clone())
                        .collect::<Vec<_>>();
                    let native_vision = descriptor.input_modalities.contains("image");
                    let mut turn_input = input.to_owned();
                    let mut turn_images = images;
                    let specialist_plan = if turn_images.is_empty() {
                        None
                    } else {
                        match header
                            .vision
                            .route_turn(native_vision, pending_vision_route.as_deref())
                        {
                            Ok(crate::app::vision::VisionTurnRoute::Native) => None,
                            Ok(crate::app::vision::VisionTurnRoute::SpecialistDenied(_)) => {
                                for attachment in attachments {
                                    pending_images.push(attachment);
                                }
                                println!(
                                    "xana> vision specialist use is denied by the active profile"
                                );
                                continue;
                            }
                            Ok(crate::app::vision::VisionTurnRoute::SpecialistAllowed(plan)) => {
                                Some((plan, false))
                            }
                            Ok(
                                crate::app::vision::VisionTurnRoute::SpecialistApprovalRequired(
                                    plan,
                                ),
                            ) => Some((plan, true)),
                            Err(error) => {
                                for attachment in attachments {
                                    pending_images.push(attachment);
                                }
                                println!(
                                    "xana> image input is unsupported for {}/{} and no usable vision specialist was selected: {error:#}; configure one with `xana connect vision` or choose an image-capable model",
                                    header.provider_name, header.model
                                );
                                continue;
                            }
                        }
                    };
                    if let Some((plan, approval_required)) = specialist_plan {
                        println!("xana> {}", plan.preview(turn_images.len()));
                        let decision = if approval_required {
                            match editor
                                .readline("xana> outbound decision: [o]nce, [a]lways allow, [n]o once, always [d]eny, [c]ancel [n]: ")
                                .as_deref()
                                .map(str::trim)
                            {
                                Ok("o" | "O" | "once" | "y" | "Y" | "yes") => Some(crate::outbound::OutboundApprovalDecision::AllowOnce),
                                Ok("a" | "A" | "always") => Some(crate::outbound::OutboundApprovalDecision::SaveAllow),
                                Ok("d" | "D" | "deny") => Some(crate::outbound::OutboundApprovalDecision::SaveDeny),
                                Ok("c" | "C" | "cancel") => Some(crate::outbound::OutboundApprovalDecision::Cancel),
                                Ok("" | "n" | "N" | "no") => Some(crate::outbound::OutboundApprovalDecision::DenyOnce),
                                _ => {
                                    for attachment in attachments {
                                        pending_images.push(attachment);
                                    }
                                    println!("xana> specialist vision was not authorized; message was not sent");
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        println!(
                            "xana> analyzing attached images with the named vision specialist..."
                        );
                        let cancellation = tokio_util::sync::CancellationToken::new();
                        let execution = header.vision.execute(
                            operation_id,
                            turn_input,
                            turn_images,
                            *plan,
                            decision,
                            cancellation.clone(),
                        );
                        tokio::pin!(execution);
                        let prepared = tokio::select! {
                            result = &mut execution => result,
                            signal = tokio::signal::ctrl_c() => {
                                signal.context("could not listen for vision cancellation")?;
                                cancellation.cancel();
                                execution.await
                            }
                        };
                        let prepared = match prepared {
                            Ok(prepared) => prepared,
                            Err(error) => {
                                for attachment in attachments {
                                    pending_images.push(attachment);
                                }
                                if matches!(
                                    decision,
                                    Some(
                                        crate::outbound::OutboundApprovalDecision::DenyOnce
                                            | crate::outbound::OutboundApprovalDecision::SaveDeny
                                            | crate::outbound::OutboundApprovalDecision::Cancel
                                    )
                                ) {
                                    println!(
                                        "xana> specialist vision was not authorized; message was not sent"
                                    );
                                } else {
                                    println!(
                                        "xana> vision specialist failed: {error:#}; message was not sent"
                                    );
                                }
                                continue;
                            }
                        };
                        println!(
                            "xana> specialist description ready: route {}, model {}, source artifacts {}; cost {}",
                            prepared.receipt.route,
                            prepared.receipt.model,
                            prepared.receipt.source_artifact_ids.join(", "),
                            if prepared.receipt.cost_available {
                                "reported"
                            } else {
                                "unavailable"
                            }
                        );
                        turn_input = prepared.model_input;
                        turn_images = Vec::new();
                        pending_vision_route = None;
                    } else if !turn_images.is_empty() {
                        println!(
                            "xana> native image input: {}/{} receives {} immutable source artifact(s) directly; no specialist request",
                            header.provider_name,
                            header.model,
                            turn_images.len()
                        );
                    }
                    let command = if turn_images.is_empty() {
                        RuntimeCommand::SubmitTurn {
                            operation_id,
                            input: turn_input,
                        }
                    } else {
                        RuntimeCommand::SubmitTurnWithImages {
                            operation_id,
                            input: turn_input,
                            images: turn_images,
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
    reporter: &mut OneShotReporter<'_>,
    workspace_host: &WorkspaceHost,
    conversation: ConversationRef,
) -> Result<OneShotSuccess, OneShotFailure> {
    if let Some(suspension) = &header.round_budget_suspension {
        return Err(round_budget_incomplete_failure(
            header.session_id,
            suspension,
        ));
    }
    let conversation_id = conversation
        .conversation_id()
        .expect("composed native one-shot has a Conversation identity");
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
        let observation = client
            .next_observation()
            .await
            .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
        reporter
            .native_observation(&observation)
            .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
        let event = match observation.event {
            ClientEvent::Runtime(event) => *event,
            ClientEvent::Managed(_) => AgentEvent::CommandRejected {
                reason: "managed observation reached a native one-shot".to_owned(),
            },
            ClientEvent::Semantic(_) => AgentEvent::CommandRejected {
                reason: "semantic observation cannot be reduced to a native event".to_owned(),
            },
            ClientEvent::PayloadOmitted {
                kind,
                encoded_bytes,
                limit,
            } => AgentEvent::CommandRejected {
                reason: format!(
                    "frontend omitted oversized {kind} observation ({encoded_bytes} bytes; limit {limit})"
                ),
            },
        };
        match event {
            AgentEvent::PermissionRequested { request } if request.operation_id == operation_id => {
                approval_required = true;
                reporter
                    .activity(
                        "approval.required",
                        &format!("approval required for {}", request.tool_name),
                    )
                    .map_err(|error| {
                        OneShotFailure::new(ExitCategory::Runtime, error.to_string())
                    })?;
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
                reporter
                    .activity(
                        "approval.child_required",
                        &format!(
                            "approval required for child {} tool {}",
                            attribution.agent_id, request.tool_name
                        ),
                    )
                    .map_err(|error| {
                        OneShotFailure::new(ExitCategory::Runtime, error.to_string())
                    })?;
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
            AgentEvent::RoundBudgetReached { suspension }
                if suspension.operation_id == operation_id =>
            {
                return Err(round_budget_incomplete_failure(
                    header.session_id,
                    &suspension,
                ));
            }
            AgentEvent::OperationFailed { reason, .. } => failure = Some(reason),
            AgentEvent::CommandRejected { reason } => {
                return Err(OneShotFailure::new(ExitCategory::Runtime, reason));
            }
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle,
            } => {
                reporter
                    .activity(
                        "child.lifecycle_changed",
                        &format!(
                            "child {} [{}]: {:?}",
                            attribution.agent_id, attribution.route, lifecycle
                        ),
                    )
                    .map_err(|error| {
                        OneShotFailure::new(ExitCategory::Runtime, error.to_string())
                    })?;
            }
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(outcome),
            } if actual == operation_id => {
                reporter
                    .native_summary(client.snapshot(), operation_id)
                    .map_err(|error| {
                        OneShotFailure::new(ExitCategory::Runtime, error.to_string())
                    })?;
                return match outcome {
                    OperationOutcome::Completed => Ok(OneShotSuccess {
                        text: final_text.unwrap_or_default(),
                        session_id: Some(header.session_id),
                        conversation_id,
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

fn round_budget_incomplete_failure(
    session_id: crate::identity::SessionId,
    suspension: &RoundBudgetSuspension,
) -> OneShotFailure {
    OneShotFailure::new(
        ExitCategory::Incomplete,
        format!(
            "native operation {} is incomplete at round-budget suspension {}; {} / {} rounds consumed and {} remain; resume session {} in an interactive Xana surface to continue or stop it",
            suspension.operation_id,
            suspension.id,
            suspension.rounds_consumed,
            suspension.hard_round_limit,
            suspension.remaining_rounds,
            session_id,
        ),
    )
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

async fn render_until_compaction_result<W: Write>(
    runtime: &mut EmbeddedClient,
    renderer: &mut EventRenderer<W>,
) -> Result<()> {
    loop {
        let event = runtime.next_event().await?;
        let finished = matches!(
            event,
            AgentEvent::ConversationCompacted { .. }
                | AgentEvent::CompactionUnavailable { .. }
                | AgentEvent::CommandRejected { .. }
        );
        renderer.render(&event)?;
        if finished {
            return Ok(());
        }
    }
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
            AgentEvent::RoundBudgetReached { suspension }
                if suspension.operation_id == operation_id =>
            {
                let action = prompt_round_budget_decision(&suspension)?;
                runtime
                    .send(RuntimeCommand::DecideRoundBudget {
                        operation_id,
                        suspension_id: suspension.id,
                        action,
                    })
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

fn prompt_round_budget_decision(
    suspension: &crate::native_runtime::RoundBudgetSuspension,
) -> Result<RoundBudgetAction> {
    let can_continue = suspension
        .allowed_actions
        .contains(&RoundBudgetAction::Continue);
    if !can_continue {
        println!("xana> hard round ceiling reached; stopping the operation");
        return Ok(RoundBudgetAction::Stop);
    }
    print!("xana> continue the same operation? [c]ontinue/[s]top [s]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Ok(RoundBudgetAction::Stop);
    }
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "c" | "continue" | "y" | "yes" => RoundBudgetAction::Continue,
        _ => RoundBudgetAction::Stop,
    })
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
    let external_scope = matches!(request.scope, PermissionScope::External { .. });
    let exact_external = external_scope && request.outbound_review.is_some();
    if exact_external {
        write!(
            output,
            "decision [d=deny/o=once/a=always allow/n=always deny; default d]: "
        )?;
    } else {
        write!(output, "decision [d=deny/o=once")?;
        if !external_scope {
            write!(output, "/s=session")?;
        }
        write!(output, "; default d]: ")?;
    }
    output.flush()?;

    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        return Ok(ControllerDecision::Deny);
    }

    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "o" | "once" | "y" | "yes" => ControllerDecision::AllowOnce,
        "s" | "session" if !external_scope => ControllerDecision::AllowSession {
            scope: request.scope.clone(),
        },
        "a" | "always" | "allow" if exact_external => ControllerDecision::SaveOutboundAllow,
        "n" | "never" if exact_external => ControllerDecision::SaveOutboundDeny,
        _ => ControllerDecision::Deny,
    })
}

fn render_permission_request(
    output: &mut dyn Write,
    request: &PermissionRequest,
    presentation: &ResolvedPresentation,
) -> io::Result<()> {
    writeln!(
        output,
        "{}\ntool: {}\neffect: {:?}",
        presentation.paint(SemanticToken::Approval, "xana> permission required"),
        request.tool_name,
        request.effect_class,
    )?;
    if let Some(review) = &request.outbound_review {
        writeln!(output, "{}", review.render())?;
    } else {
        writeln!(output, "scope: {}", display_scope(&request.scope))?;
        if matches!(request.scope, PermissionScope::Unscoped) {
            writeln!(output, "arguments: {}", request.final_arguments)?;
        }
    }
    writeln!(
        output,
        "This effect uses Xana's ordinary host permissions; it is not contained."
    )
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
        PermissionScope::BuiltInResource { id } => {
            format!("immutable built-in resource {id}")
        }
        PermissionScope::Unscoped => "unscoped".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SessionId;

    #[test]
    fn classifies_commands_blanks_and_messages() {
        assert_eq!(classify_input("/quit"), InputAction::Quit);
        assert_eq!(classify_input("/doctor"), InputAction::Doctor);
        assert_eq!(classify_input("/setup"), InputAction::Setup(""));
        assert_eq!(classify_input("/settings"), InputAction::Settings(""));
        assert_eq!(
            classify_input("/settings appearance"),
            InputAction::Settings("appearance")
        );
        assert_eq!(classify_input("/help"), InputAction::Help);
        assert_eq!(classify_input("/usage"), InputAction::Usage(""));
        assert_eq!(
            classify_input("/usage details"),
            InputAction::Usage("details")
        );
        assert_eq!(
            classify_input("/usage compact"),
            InputAction::Usage("compact")
        );
        assert_eq!(classify_input("/usagefoo"), InputAction::Send("/usagefoo"));
        assert_eq!(classify_input("/capabilities"), InputAction::Capabilities);
        assert_eq!(
            classify_input("/setup appearance"),
            InputAction::Setup("appearance")
        );
        assert_eq!(classify_input("/setupfoo"), InputAction::Send("/setupfoo"));
        assert_eq!(
            classify_input("/settingsfoo"),
            InputAction::Send("/settingsfoo")
        );
        assert_eq!(classify_input("  /clear  "), InputAction::Clear);
        assert_eq!(classify_input("   "), InputAction::Ignore);
        assert_eq!(
            classify_input("  hello Xana  "),
            InputAction::Send("hello Xana")
        );
        assert_eq!(classify_input("clear"), InputAction::Send("clear"));
        assert_eq!(
            classify_input("/sessions"),
            InputAction::ControlCommand {
                family: "conversation",
                arguments: "list",
            }
        );
        assert_eq!(
            classify_input("/attach assets/photo.png"),
            InputAction::Attach("assets/photo.png")
        );
        assert_eq!(classify_input("/attach"), InputAction::Attach(""));
        assert_eq!(classify_input("/vision"), InputAction::Vision(""));
        assert_eq!(
            classify_input("/vision describe"),
            InputAction::Vision("describe")
        );
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
        for (input, family, arguments) in [
            ("/connection", "connection", "list"),
            ("/connection status local", "connection", "status local"),
            ("/logs", "logs", "list"),
            ("/outbound", "outbound", "list"),
            (
                "/operation plan --session abc",
                "operation",
                "plan --session abc",
            ),
            ("/route", "route", "list"),
            ("/connect", "connect", ""),
            ("/connect provider", "connect", "provider"),
        ] {
            assert_eq!(
                classify_input(input),
                InputAction::ControlCommand { family, arguments },
                "{input}"
            );
        }
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
    fn one_shot_round_boundary_is_typed_incomplete_with_exact_resume_identity() {
        let session_id = SessionId::new();
        let operation_id = OperationId::new();
        let suspension_id = crate::identity::RoundBudgetId::new();
        let suspension = RoundBudgetSuspension {
            id: suspension_id,
            operation_id,
            soft_round_limit: 8,
            last_tranche_rounds: 8,
            rounds_consumed: 8,
            hard_round_limit: 256,
            remaining_rounds: 248,
            continuations_used: 0,
            committed: crate::native_runtime::RoundBudgetCommitFacts {
                steps: 8,
                invocations: 8,
                results: 8,
            },
            repeated_tool_patterns: 0,
            usage: crate::agent::AgentTurnUsage::empty(),
            allowed_actions: vec![RoundBudgetAction::Continue, RoundBudgetAction::Stop],
        };

        let failure = round_budget_incomplete_failure(session_id, &suspension);

        assert_eq!(failure.category, ExitCategory::Incomplete);
        assert_eq!(failure.exit_code(), std::process::ExitCode::from(7));
        assert!(failure.message.contains(&operation_id.to_string()));
        assert!(failure.message.contains(&suspension_id.to_string()));
        assert!(failure.message.contains(&session_id.to_string()));
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
            outbound_review: None,
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

    #[test]
    fn external_permission_prompt_offers_exact_saved_choices_without_session_scope() {
        let recipient = crate::outbound::RecipientIdentity::new(
            crate::outbound::RecipientKind::ExternalAgent,
            "reviewer",
            "https://agent.example.test",
            b"reviewer",
        )
        .unwrap();
        let review = crate::outbound::OutboundRequest::new(
            OperationId::new(),
            recipient.clone(),
            "delegate review",
            vec![
                crate::outbound::OutboundItem::new(
                    crate::config::OutboundDataClass::PromptText,
                    "delegated task",
                    None,
                    "current request",
                    b"review".to_vec(),
                )
                .unwrap(),
            ],
        )
        .unwrap()
        .review();
        let request = PermissionRequest {
            operation_id: OperationId::new(),
            invocation_id: ToolInvocationId::new(),
            tool_name: "delegate_agent".to_owned(),
            effect_class: crate::tool::EffectClass::External,
            final_arguments: serde_json::json!({"task": "review"}),
            scope: PermissionScope::External {
                recipient_identity_digest: recipient.identity_digest,
                operation: "delegate".to_owned(),
            },
            outbound_review: Some(review),
        };
        for (answer, expected) in [
            ("a\n", ControllerDecision::SaveOutboundAllow),
            ("n\n", ControllerDecision::SaveOutboundDeny),
            ("s\n", ControllerDecision::Deny),
        ] {
            let mut input = io::Cursor::new(answer.as_bytes());
            let mut output = Vec::new();
            assert_eq!(
                permission_decision_with_io(&request, &mut input, &mut output).unwrap(),
                expected
            );
            assert_eq!(
                output,
                b"decision [d=deny/o=once/a=always allow/n=always deny; default d]: "
            );
        }
    }
}
