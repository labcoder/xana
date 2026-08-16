//! Native and managed runtime event projection into terminal state.

use super::*;

impl TuiState {
    pub(in crate::tui) fn apply_runtime(&mut self, event: &AgentEvent) {
        let viewing_background = self.viewed_conversation != self.runtime_conversation;
        if viewing_background {
            let background = self.background_messages.get_or_insert_with(VecDeque::new);
            std::mem::swap(&mut self.messages, background);
        }
        match event {
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Running,
            } => {
                self.busy = true;
                self.work_indicator_frame = 0;
                self.active_operation = Some(*operation_id);
                self.status = "Working…".to_owned();
            }
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(outcome),
            } => {
                self.busy = false;
                self.work_indicator_frame = 0;
                self.active_operation = None;
                self.status = format!("Turn {outcome:?}");
                self.push_card(ActivityCard::new(
                    "Xana root",
                    operation_id.to_string(),
                    ActivityKind::Status,
                    ActivityState::Complete,
                    format!("operation {outcome:?}"),
                    "",
                ));
            }
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Suspended,
            } => {
                self.busy = false;
                self.work_indicator_frame = 0;
                self.active_operation = None;
                self.status =
                    "Turn interrupted; any uncertain effects remain recoverable".to_owned();
                self.push_card(ActivityCard::new(
                    "Xana root",
                    operation_id.to_string(),
                    ActivityKind::Warning,
                    ActivityState::Failed,
                    "operation suspended",
                    "Any uncertain effects remain recoverable.",
                ));
            }
            AgentEvent::AssistantTextDelta {
                operation_id, text, ..
            } => self.push_assistant_delta(*operation_id, text),
            AgentEvent::ProviderReasoningDelta {
                operation_id,
                step_id,
                text,
            } => {
                let identity = format!("{operation_id}:{step_id}");
                if let Some(card) = self.activity.iter_mut().rev().find(|card| {
                    card.owner == "Xana native"
                        && card.identity == identity
                        && card.kind == ActivityKind::ReasoningRaw
                }) {
                    card.append_detail(text);
                } else {
                    self.push_card(ActivityCard::new(
                        "Xana native",
                        identity,
                        ActivityKind::ReasoningRaw,
                        ActivityState::Running,
                        "model reasoning",
                        text.clone(),
                    ));
                }
            }
            AgentEvent::AssistantMessage {
                operation_id,
                message,
            } => self.finish_assistant(*operation_id, message),
            AgentEvent::UsageObserved { usage, .. } => self.native_usage.observe(*usage),
            AgentEvent::PermissionRequested { request } => {
                self.status = format!("Approval required: {}", request.tool_name);
                self.open_approval(ApprovalPrompt::native(request.clone()));
            }
            AgentEvent::OperationFailed { reason, .. } => {
                self.busy = false;
                self.work_indicator_frame = 0;
                self.active_operation = None;
                self.status = bounded(reason.clone(), MAX_ACTIVITY_BYTES);
                self.push_card(ActivityCard::new(
                    "Xana root",
                    "turn",
                    ActivityKind::Error,
                    ActivityState::Failed,
                    "turn failed",
                    reason.clone(),
                ));
            }
            AgentEvent::ConversationCleared => {
                self.messages.clear();
                self.status = "Conversation cleared".to_owned();
            }
            AgentEvent::CommandRejected { reason } => {
                self.status = bounded(format!("Command rejected: {reason}"), MAX_ACTIVITY_BYTES);
            }
            AgentEvent::InvocationIntentCommitted { intent } => self.push_card(ActivityCard::new(
                "Xana root",
                intent.invocation_id.to_string(),
                ActivityKind::Tool,
                ActivityState::Running,
                format!("tool planned: {}", intent.permission.request.tool_name),
                intent.permission.request.final_arguments.to_string(),
            )),
            AgentEvent::ToolFinished {
                invocation_id,
                result,
                ..
            } => {
                let projected = message_projection(result);
                let detail = projected.text.clone();
                self.push_message(MessageKind::Tool, projected.text);
                self.push_card(ActivityCard::new(
                    "Xana root",
                    invocation_id.to_string(),
                    ActivityKind::Tool,
                    ActivityState::Complete,
                    "tool finished",
                    detail,
                ));
            }
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle,
            } => self.push_card(ActivityCard::new(
                format!("Xana child {}", attribution.agent_id),
                attribution.agent_id.to_string(),
                ActivityKind::Child,
                ActivityState::Running,
                format!("{}: {lifecycle:?}", attribution.route),
                format!(
                    "{}/{} via {}",
                    attribution.connection, attribution.model, attribution.profile
                ),
            )),
            AgentEvent::ChildActivity {
                attribution,
                activity,
            } => match activity {
                crate::orchestration::ChildActivity::PermissionRequested { request } => {
                    self.open_approval(ApprovalPrompt::child(attribution.clone(), request.clone()));
                }
                crate::orchestration::ChildActivity::ManagedRuntime { notification } => {
                    if let Some(event) = ManagedClientEvent::from_notification(notification.clone())
                    {
                        self.apply_managed_activity(
                            format!("Codex child {}", attribution.agent_id),
                            &event,
                        );
                    }
                }
                crate::orchestration::ChildActivity::ExternalAgent { activity } => {
                    let (state, summary, detail) = external_activity_card(activity);
                    self.push_card(ActivityCard::new(
                        format!("External A2A {}", activity.agent_name),
                        attribution.agent_id.to_string(),
                        ActivityKind::Child,
                        state,
                        summary,
                        detail,
                    ));
                }
                crate::orchestration::ChildActivity::Warning { message } => {
                    self.push_card(ActivityCard::new(
                        format!("Xana child {}", attribution.agent_id),
                        attribution.agent_id.to_string(),
                        ActivityKind::Warning,
                        ActivityState::Failed,
                        "child warning",
                        message.clone(),
                    ))
                }
                crate::orchestration::ChildActivity::ProviderReasoningDelta { step_id, text } => {
                    let identity = format!("{}:{step_id}", attribution.agent_id);
                    self.push_card(ActivityCard::new(
                        format!("Xana child {}", attribution.agent_id),
                        identity,
                        ActivityKind::ReasoningRaw,
                        ActivityState::Running,
                        "child model reasoning",
                        text.clone(),
                    ));
                }
                crate::orchestration::ChildActivity::AssistantTextDelta { .. }
                | crate::orchestration::ChildActivity::PermissionAudited { .. }
                | crate::orchestration::ChildActivity::ToolFinished { .. }
                | crate::orchestration::ChildActivity::Suspended => {
                    self.push_card(ActivityCard::new(
                        format!("Xana child {}", attribution.agent_id),
                        attribution.agent_id.to_string(),
                        ActivityKind::Child,
                        ActivityState::Running,
                        format!("{} activity", attribution.route),
                        "",
                    ))
                }
            },
            AgentEvent::ChildReportCommitted { report } => self.push_card(ActivityCard::new(
                format!("Xana child {}", report.attribution.agent_id),
                report.attribution.agent_id.to_string(),
                ActivityKind::Child,
                ActivityState::Complete,
                format!("child report: {:?}", report.status),
                report
                    .output
                    .clone()
                    .or_else(|| report.error.clone())
                    .unwrap_or_default(),
            )),
            AgentEvent::ExternalAgentActivity {
                operation_id,
                activity,
            } => {
                let (state, summary, detail) = external_activity_card(activity);
                self.push_card(ActivityCard::new(
                    format!("External A2A {}", activity.agent_name),
                    operation_id.to_string(),
                    ActivityKind::Child,
                    state,
                    summary,
                    detail,
                ));
            }
            AgentEvent::InvocationResultCommitted { .. }
            | AgentEvent::PermissionAudited { .. }
            | AgentEvent::ChildListSnapshot { .. }
            | AgentEvent::ChildInspectionSnapshot { .. }
            | AgentEvent::ChildCancellationRequested { .. } => {}
        }
        if viewing_background {
            let background = self.background_messages.get_or_insert_with(VecDeque::new);
            std::mem::swap(&mut self.messages, background);
            if let Some(row) = self
                .sessions
                .iter_mut()
                .find(|row| row.conversation == self.runtime_conversation)
            {
                row.unread = matches!(
                    event,
                    AgentEvent::AssistantMessage { .. }
                        | AgentEvent::OperationStateChanged {
                            state: OperationState::Finished(_),
                            ..
                        }
                );
                row.error |= matches!(event, AgentEvent::OperationFailed { .. });
            }
        }
    }

    pub(super) fn interrupt(&mut self) -> UpdateEffect {
        let Some(operation_id) = self.active_operation else {
            self.status = "No root turn is active".to_owned();
            return UpdateEffect::None;
        };
        if !self.capabilities.interrupt {
            self.status = "This execution owner does not support interruption".to_owned();
            return UpdateEffect::None;
        }
        self.status = "Interrupt requested…".to_owned();
        UpdateEffect::Interrupt { operation_id }
    }

    fn push_assistant_delta(&mut self, operation_id: OperationId, text: &str) {
        if self.active_operation != Some(operation_id)
            || !matches!(self.messages.back(), Some(message) if message.kind == MessageKind::Assistant)
        {
            self.push_message(MessageKind::Assistant, String::new());
            self.active_operation = Some(operation_id);
        }
        if let Some(message) = self.messages.back_mut() {
            append_bounded(&mut message.text, text, MAX_MESSAGE_BYTES);
            message.document.stream_append(text);
        }
    }

    fn finish_assistant(&mut self, operation_id: OperationId, message: &Message) {
        let final_message = message_projection(message);
        if self.active_operation == Some(operation_id)
            && matches!(self.messages.back(), Some(message) if message.kind == MessageKind::Assistant)
        {
            if let Some(message) = self.messages.back_mut() {
                *message = final_message;
            }
        } else {
            if self.scroll > 0 {
                self.scroll = self.scroll.saturating_add(
                    message_row_estimate(&final_message).min(usize::from(u16::MAX)) as u16,
                );
            }
            self.messages.push_back(final_message);
            trim_front(&mut self.messages, MAX_VISIBLE_MESSAGES);
        }
    }

    pub(super) fn push_message(&mut self, kind: MessageKind, text: impl Into<String>) {
        let text = bounded(text.into(), MAX_MESSAGE_BYTES);
        let message = VisibleMessage {
            kind,
            document: RichDocument::plain(&text),
            text,
        };
        if self.scroll > 0 {
            self.scroll = self
                .scroll
                .saturating_add(message_row_estimate(&message).min(usize::from(u16::MAX)) as u16);
        }
        self.messages.push_back(message);
        trim_front(&mut self.messages, MAX_VISIBLE_MESSAGES);
    }

    pub(super) fn push_activity(&mut self, text: impl Into<String>) {
        self.push_card(ActivityCard::new(
            "Xana",
            format!("event-{}", self.activity.len()),
            ActivityKind::Status,
            ActivityState::Complete,
            text,
            "",
        ));
    }

    pub(super) fn push_card(&mut self, card: ActivityCard) {
        if self.activity_visibility == ActivityVisibility::Auto && card.is_substantive() {
            self.auto_activity_open = true;
        }
        activity::push(&mut self.activity, card);
        trim_front(&mut self.activity, MAX_ACTIVITY);
    }

    fn open_approval(&mut self, prompt: ApprovalPrompt) {
        self.push_card(ActivityCard::new(
            prompt.owner.clone(),
            "approval",
            ActivityKind::Approval,
            ActivityState::Waiting,
            prompt.title.clone(),
            prompt.details.join("\n"),
        ));
        self.overlay = Some(Overlay::Approval {
            prompt: Box::new(prompt),
            selected: 0,
        });
        self.status = "Waiting for approval".to_owned();
    }

    pub(in crate::tui) fn open_managed_approval(
        &mut self,
        request: crate::managed::codex::ApprovalRequest,
    ) {
        self.open_approval(ApprovalPrompt::managed(request));
    }

    pub(super) fn confirm_approval(
        &mut self,
        prompt: ApprovalPrompt,
        selected: usize,
    ) -> UpdateEffect {
        let choice = approval_choices(&prompt).get(selected).copied();
        let Some(choice) = choice else {
            self.status = "Approval offered no safe decision".to_owned();
            return UpdateEffect::None;
        };
        self.status = format!("Approval decision: {choice:?}");
        match prompt.target {
            ApprovalTarget::Native(request) => UpdateEffect::DecideNativeApproval {
                operation_id: request.operation_id,
                invocation_id: request.invocation_id,
                decision: controller_decision(choice, request.scope),
            },
            ApprovalTarget::Child {
                attribution,
                request,
            } => UpdateEffect::DecideChildApproval {
                agent_id: attribution.agent_id,
                operation_id: request.operation_id,
                invocation_id: request.invocation_id,
                decision: controller_decision(choice, request.scope),
            },
            ApprovalTarget::Managed(request) => {
                let decision = match choice {
                    ApprovalChoice::Once => crate::managed::codex::ApprovalDecision::AcceptOnce,
                    ApprovalChoice::Session => {
                        crate::managed::codex::ApprovalDecision::AcceptForSession
                    }
                    ApprovalChoice::Deny if request.available_decisions.contains("decline") => {
                        crate::managed::codex::ApprovalDecision::Decline
                    }
                    ApprovalChoice::Deny => crate::managed::codex::ApprovalDecision::Cancel,
                    ApprovalChoice::SaveAllow | ApprovalChoice::SaveDeny => {
                        crate::managed::codex::ApprovalDecision::Cancel
                    }
                };
                UpdateEffect::DecideManagedApproval(decision)
            }
        }
    }

    pub(in crate::tui) fn apply_managed_event(&mut self, event: &ManagedClientEvent) {
        match event {
            ManagedClientEvent::AssistantDelta(delta) => {
                let operation_id = self.active_operation.unwrap_or_else(OperationId::new);
                self.push_assistant_delta(operation_id, delta);
            }
            ManagedClientEvent::TurnCompleted { status, error } => {
                self.busy = false;
                self.work_indicator_frame = 0;
                self.active_operation = None;
                self.status = error.as_ref().map_or_else(
                    || format!("Managed turn {status}"),
                    |error| format!("Managed turn failed: {error}"),
                );
            }
            ManagedClientEvent::ModelRerouted { to_model, .. } => {
                self.model = to_model.clone();
            }
            ManagedClientEvent::TokenUsageUpdated {
                input_tokens,
                output_tokens,
                total_tokens,
            } => {
                self.managed_usage = Some((*input_tokens, *output_tokens, *total_tokens));
            }
            _ => {}
        }
        self.apply_managed_activity("Codex".to_owned(), event);
    }

    pub(in crate::tui) fn set_managed_thread(&mut self, connection: &str, thread_id: String) {
        self.session = thread_id.clone();
        let conversation = ConversationRef::Managed {
            connection: connection.to_owned(),
            thread_id,
        };
        self.runtime_conversation = conversation.clone();
        self.viewed_conversation = conversation;
    }

    pub(in crate::tui) fn finish_managed_turn(
        &mut self,
        operation_id: OperationId,
        error: Option<String>,
    ) {
        if self.active_operation == Some(operation_id) {
            self.active_operation = None;
            self.busy = false;
            self.work_indicator_frame = 0;
        }
        if let Some(error) = error {
            self.status = bounded(format!("Managed turn failed: {error}"), MAX_ACTIVITY_BYTES);
            self.push_card(ActivityCard::new(
                "Codex",
                operation_id.to_string(),
                ActivityKind::Error,
                ActivityState::Failed,
                "managed turn failed",
                error,
            ));
        } else {
            self.status = "Managed turn completed".to_owned();
        }
    }

    pub(in crate::tui) fn managed_cleared(&mut self, connection: &str) {
        self.messages.clear();
        self.activity.clear();
        self.session = "new".to_owned();
        self.runtime_conversation = ConversationRef::NewManaged {
            connection: connection.to_owned(),
        };
        self.viewed_conversation = self.runtime_conversation.clone();
        self.status = "New managed thread will start with the next message".to_owned();
    }

    pub(in crate::tui) fn set_model(&mut self, model: String) {
        self.model = model;
        self.status = "Model updated for subsequent turns; managed context is unchanged".to_owned();
    }

    pub(in crate::tui) fn show_artifact_preview(
        &mut self,
        record: crate::artifact::ArtifactRecord,
        preview: String,
    ) {
        let label = format!("{} · {} bytes", record.media_type, record.byte_len);
        self.overlay = Some(Overlay::Artifact {
            artifact: Box::new(ArtifactView { record, label }),
            selected: 0,
            preview: Some(bounded(preview, 64 * 1024)),
        });
    }

    pub(in crate::tui) fn insert_artifact_reference(
        &mut self,
        record: &crate::artifact::ArtifactRecord,
    ) {
        let reference = format!("artifact:{}", record.reference.id);
        if let Err(reason) = self.composer.insert(&reference) {
            self.status = reason;
        } else {
            self.status = "Artifact reference inserted into the draft".to_owned();
        }
    }

    fn apply_managed_activity(&mut self, owner: String, event: &ManagedClientEvent) {
        if let Some(mut card) = activity::from_managed(event) {
            card.owner = owner;
            self.push_card(card);
        }
    }
}

fn external_activity_card(
    activity: &crate::a2a::ExternalAgentActivity,
) -> (ActivityState, String, String) {
    use crate::a2a::ExternalAgentActivityKind;

    let detail = format!(
        "connection={} ownership={}",
        activity.connection, activity.ownership
    );
    match &activity.activity {
        ExternalAgentActivityKind::Sending { total_bytes, .. } => (
            ActivityState::Running,
            format!("sending {total_bytes} reviewed bytes"),
            detail,
        ),
        ExternalAgentActivityKind::TaskIdentified { task_id, .. } => (
            ActivityState::Running,
            format!("remote task {task_id}"),
            detail,
        ),
        ExternalAgentActivityKind::Status { state, message } => (
            if matches!(state.as_str(), "completed" | "canceled") {
                ActivityState::Complete
            } else if matches!(state.as_str(), "failed" | "rejected") {
                ActivityState::Failed
            } else {
                ActivityState::Running
            },
            format!("status {state}"),
            message.clone().unwrap_or(detail),
        ),
        ExternalAgentActivityKind::Message { text } => (
            ActivityState::Running,
            "remote message".into(),
            text.clone(),
        ),
        ExternalAgentActivityKind::Artifact { name, byte_len, .. } => (
            ActivityState::Running,
            format!("artifact {name} ({byte_len} bytes)"),
            detail,
        ),
        ExternalAgentActivityKind::CancellationRequested { task_id } => (
            ActivityState::Running,
            format!("cancelling remote task {task_id}"),
            detail,
        ),
        ExternalAgentActivityKind::Detached { task_id } => (
            ActivityState::Failed,
            format!("remote task {task_id} detached"),
            "Remote cancellation could not be confirmed; outcome is unknown.".into(),
        ),
    }
}
