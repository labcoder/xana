//! Pure application-owned projection from Xana's Desktop protocol to UI state.

use gpui_ai::prelude::{ChatMessage, ChatRole, MessageActions, StreamedContent};
use std::collections::HashMap;
use xana::desktop::{
    DesktopActivityItem, DesktopContent, DesktopConversationFacts, DesktopEvent, DesktopHostEvent,
    DesktopHostObservation, DesktopMessage, DesktopObservation, DesktopOperationId,
    DesktopOperationState, DesktopPermissionId, DesktopRole, DesktopRoundBudgetSuspension,
    DesktopSnapshot, NotificationPolicy,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectedMessage {
    id: String,
    role: ChatRole,
    text: String,
    lifecycle: MessageLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MessageLifecycle {
    Running,
    Complete,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingApproval {
    pub(crate) id: DesktopPermissionId,
    pub(crate) tool: String,
    pub(crate) effect: String,
    pub(crate) scope: String,
}

/// Xana Desktop owns this state; `gpui-ai` receives immutable snapshots.
pub(crate) struct ConversationProjection {
    connection: String,
    execution_owner: String,
    model: String,
    reasoning_effort: Option<String>,
    artifact_count: usize,
    messages: Vec<ProjectedMessage>,
    streams: HashMap<DesktopOperationId, String>,
    message_operations: HashMap<DesktopOperationId, String>,
    sequence: u64,
    active_operation: Option<DesktopOperationId>,
    pending_round_budget: Option<DesktopRoundBudgetSuspension>,
    notification_policy: NotificationPolicy,
    pending_approval_count: usize,
    pending_approvals: Vec<PendingApproval>,
    host_lifecycle: String,
    global_notice_count: usize,
    conversation_facts: DesktopConversationFacts,
    latest_activity: String,
    failure: Option<String>,
}

impl ConversationProjection {
    pub(crate) fn from_snapshot(snapshot: &DesktopSnapshot) -> Self {
        Self {
            connection: snapshot.connection.clone(),
            execution_owner: snapshot.execution_owner.clone(),
            model: snapshot.model.clone(),
            reasoning_effort: snapshot.reasoning_effort.clone(),
            artifact_count: snapshot.artifact_count,
            messages: snapshot.conversation.iter().map(project_message).collect(),
            streams: HashMap::new(),
            message_operations: HashMap::new(),
            sequence: snapshot.sequence,
            active_operation: snapshot.active_operation,
            pending_round_budget: None,
            notification_policy: snapshot.notification_policy.clone(),
            pending_approval_count: snapshot.pending_approval_count,
            pending_approvals: snapshot
                .pending_approvals
                .iter()
                .map(|approval| PendingApproval {
                    id: approval.id,
                    tool: approval.tool.clone(),
                    effect: approval.effect.clone(),
                    scope: approval.scope.clone(),
                })
                .collect(),
            host_lifecycle: snapshot.host_lifecycle.clone(),
            global_notice_count: snapshot.global_notices.len(),
            conversation_facts: snapshot.conversation_facts.clone(),
            latest_activity: format!(
                "{} / {} · session {}",
                snapshot.connection, snapshot.model, snapshot.session_id
            ),
            failure: None,
        }
    }

    pub(crate) fn replace_snapshot(&mut self, snapshot: &DesktopSnapshot) {
        *self = Self::from_snapshot(snapshot);
    }

    pub(crate) fn append_user(&mut self, operation_id: DesktopOperationId, text: String) {
        self.messages.push(ProjectedMessage {
            id: format!("desktop-user-{operation_id}"),
            role: ChatRole::User,
            text,
            lifecycle: MessageLifecycle::Complete,
        });
        self.active_operation = Some(operation_id);
        self.latest_activity = "Request queued".to_owned();
        self.failure = None;
    }

    pub(crate) fn reject_user(&mut self, operation_id: DesktopOperationId) {
        let id = format!("desktop-user-{operation_id}");
        self.messages.retain(|message| message.id != id);
        if self.active_operation == Some(operation_id) {
            self.active_operation = None;
        }
    }

    /// Applies one observation. `false` means the caller must request a snapshot.
    pub(crate) fn apply(&mut self, observation: DesktopObservation) -> bool {
        if observation.sequence != self.sequence.saturating_add(1) {
            return false;
        }
        self.sequence = observation.sequence;
        match observation.event {
            DesktopEvent::OperationState {
                operation_id,
                state,
            } => {
                self.active_operation = matches!(
                    state,
                    DesktopOperationState::Running | DesktopOperationState::Suspended
                )
                .then_some(operation_id);
                self.latest_activity = operation_label(state).to_owned();
                if matches!(
                    state,
                    DesktopOperationState::Completed
                        | DesktopOperationState::Declined
                        | DesktopOperationState::Interrupted
                ) {
                    self.finish_stream(operation_id, None);
                } else if state == DesktopOperationState::Failed {
                    self.finish_stream(operation_id, Some("Operation failed".to_owned()));
                }
            }
            DesktopEvent::AssistantDelta { operation_id, text } => {
                self.append_stream(operation_id, text);
                self.latest_activity = "Xana is responding".to_owned();
            }
            DesktopEvent::ReasoningDelta { .. } => {
                self.latest_activity = "Xana is reasoning".to_owned();
            }
            DesktopEvent::ExecutionSelectionChanged {
                model,
                reasoning_effort,
                receipt,
            } => {
                self.model = model;
                self.reasoning_effort = reasoning_effort;
                self.latest_activity = receipt;
                self.failure = None;
            }
            DesktopEvent::MessageFinal {
                operation_id,
                message,
            } => self.replace_stream_with_final(operation_id, message),
            DesktopEvent::PermissionRequired {
                permission_id,
                tool,
                effect,
                scope,
            } => {
                if let Some(existing) = self
                    .pending_approvals
                    .iter_mut()
                    .find(|approval| approval.id == permission_id)
                {
                    *existing = PendingApproval {
                        id: permission_id,
                        tool: tool.clone(),
                        effect,
                        scope,
                    };
                } else {
                    self.pending_approvals.push(PendingApproval {
                        id: permission_id,
                        tool: tool.clone(),
                        effect,
                        scope,
                    });
                }
                self.pending_approval_count = self.pending_approvals.len();
                self.latest_activity = format!("Approval required for {tool}");
            }
            DesktopEvent::PermissionResolved { permission_id } => {
                self.pending_approvals
                    .retain(|approval| approval.id != permission_id);
                self.pending_approval_count = self.pending_approvals.len();
                self.latest_activity = "Approval resolved".to_owned();
            }
            DesktopEvent::RoundBudgetReached(suspension) => {
                self.latest_activity = format!(
                    "Round budget reached: {} / {}; {} remain",
                    suspension.rounds_consumed,
                    suspension.hard_round_limit,
                    suspension.remaining_rounds,
                );
                self.pending_round_budget = Some(suspension);
            }
            DesktopEvent::RoundBudgetDecision {
                suspension_id,
                continued,
            } => {
                if self
                    .pending_round_budget
                    .as_ref()
                    .is_some_and(|pending| pending.id == suspension_id)
                {
                    self.pending_round_budget = None;
                }
                self.latest_activity = if continued {
                    "Continuing the same operation".to_owned()
                } else {
                    "Stopping the suspended operation".to_owned()
                };
            }
            DesktopEvent::Usage { .. } => {
                self.latest_activity = "Usage updated".to_owned();
            }
            DesktopEvent::ConversationCleared => {
                self.messages.clear();
                self.streams.clear();
                self.message_operations.clear();
                self.active_operation = None;
                self.pending_round_budget = None;
                self.latest_activity = "Conversation cleared".to_owned();
            }
            DesktopEvent::Activity { label } => self.latest_activity = label,
            DesktopEvent::ActivityUpserted(activity) => {
                self.upsert_activity(activity);
            }
            DesktopEvent::Error(error) => {
                self.failure = Some(error.message.clone());
                self.latest_activity = error.message;
            }
        }
        true
    }

    pub(crate) fn fail(&mut self, message: impl Into<String>) {
        let message = message.into();
        if let Some(operation_id) = self.active_operation.take() {
            self.finish_stream(operation_id, Some(message.clone()));
        }
        self.failure = Some(message.clone());
        self.latest_activity = message;
    }

    pub(crate) fn apply_host(&mut self, observation: &DesktopHostObservation) {
        match &observation.event {
            DesktopHostEvent::LifecycleChanged { state } => {
                self.host_lifecycle.clone_from(state);
            }
            DesktopHostEvent::GlobalNotice(_) => {
                self.global_notice_count = self.global_notice_count.saturating_add(1);
            }
            DesktopHostEvent::ControllerChanged { change, .. }
                if matches!(change.as_str(), "released" | "expired")
                    || change.starts_with("disconnected:") =>
            {
                self.latest_activity = "Conversation controller needs attention".to_owned();
            }
            DesktopHostEvent::ShutdownCompleted { .. } => {
                self.host_lifecycle = "stopped".to_owned();
            }
            _ => {}
        }
    }

    pub(crate) fn messages(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .map(|message| {
                let content = match &message.lifecycle {
                    MessageLifecycle::Running => StreamedContent::running(message.text.clone()),
                    MessageLifecycle::Complete => StreamedContent::done(message.text.clone()),
                    MessageLifecycle::Failed(reason) => {
                        StreamedContent::failed(message.text.clone(), reason.clone())
                    }
                };
                ChatMessage::new(message.id.clone(), message.role, content)
                    .actions(MessageActions::for_role(message.role))
            })
            .collect()
    }

    pub(crate) fn connection(&self) -> &str {
        &self.connection
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn execution_owner(&self) -> &str {
        &self.execution_owner
    }

    pub(crate) fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    pub(crate) fn artifact_count(&self) -> usize {
        self.artifact_count
    }

    pub(crate) fn is_running(&self) -> bool {
        self.active_operation.is_some() && self.pending_round_budget.is_none()
    }

    pub(crate) fn active_operation(&self) -> Option<DesktopOperationId> {
        self.active_operation
    }

    pub(crate) fn notification_policy(&self) -> &NotificationPolicy {
        &self.notification_policy
    }

    pub(crate) fn pending_approval_count(&self) -> usize {
        self.pending_approval_count
    }

    pub(crate) fn pending_approvals(&self) -> &[PendingApproval] {
        &self.pending_approvals
    }

    pub(crate) fn host_lifecycle(&self) -> &str {
        &self.host_lifecycle
    }

    pub(crate) fn global_notice_count(&self) -> usize {
        self.global_notice_count
    }

    pub(crate) fn pending_round_budget(&self) -> Option<&DesktopRoundBudgetSuspension> {
        self.pending_round_budget.as_ref()
    }

    pub(crate) fn latest_activity(&self) -> &str {
        &self.latest_activity
    }

    pub(crate) fn conversation_facts(&self) -> &DesktopConversationFacts {
        &self.conversation_facts
    }

    pub(crate) fn set_activity(&mut self, activity: impl Into<String>) {
        self.latest_activity = activity.into();
    }

    pub(crate) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub(crate) fn operation_for_message(&self, message_id: &str) -> Option<DesktopOperationId> {
        self.streams
            .iter()
            .chain(self.message_operations.iter())
            .find_map(|(operation_id, candidate)| {
                (candidate == message_id).then_some(*operation_id)
            })
    }

    pub(crate) fn preceding_user_text(&self, message_id: &str) -> Option<&str> {
        let index = self
            .messages
            .iter()
            .position(|message| message.id == message_id)?;
        self.messages[..index]
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::User)
            .map(|message| message.text.as_str())
    }

    fn append_stream(&mut self, operation_id: DesktopOperationId, delta: String) {
        let id = self
            .streams
            .entry(operation_id)
            .or_insert_with(|| format!("desktop-stream-{operation_id}"))
            .clone();
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            message.text.push_str(&delta);
            message.lifecycle = MessageLifecycle::Running;
        } else {
            self.messages.push(ProjectedMessage {
                id,
                role: ChatRole::Assistant,
                text: delta,
                lifecycle: MessageLifecycle::Running,
            });
        }
    }

    fn replace_stream_with_final(
        &mut self,
        operation_id: DesktopOperationId,
        message: DesktopMessage,
    ) {
        if let Some(stream_id) = self.streams.remove(&operation_id)
            && let Some(index) = self
                .messages
                .iter()
                .position(|candidate| candidate.id == stream_id)
        {
            let projected = project_message(&message);
            self.message_operations
                .insert(operation_id, projected.id.clone());
            self.messages[index] = projected;
            return;
        }
        if !self
            .messages
            .iter()
            .any(|candidate| candidate.id == message.id)
        {
            let projected = project_message(&message);
            self.message_operations
                .insert(operation_id, projected.id.clone());
            self.messages.push(projected);
        }
    }

    fn finish_stream(&mut self, operation_id: DesktopOperationId, failure: Option<String>) {
        let id = self
            .streams
            .get(&operation_id)
            .or_else(|| self.message_operations.get(&operation_id))
            .cloned()
            .unwrap_or_else(|| {
                let id = format!("desktop-stream-{operation_id}");
                self.streams.insert(operation_id, id.clone());
                id
            });
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            message.lifecycle = failure
                .map(MessageLifecycle::Failed)
                .unwrap_or(MessageLifecycle::Complete);
        } else if let Some(reason) = failure {
            self.messages.push(ProjectedMessage {
                id,
                role: ChatRole::Assistant,
                text: "The Run ended before Xana received an assistant response.".to_owned(),
                lifecycle: MessageLifecycle::Failed(reason),
            });
        }
    }

    fn upsert_activity(&mut self, activity: DesktopActivityItem) {
        self.latest_activity = activity
            .disclosed_text
            .clone()
            .unwrap_or_else(|| activity.summary_code.clone());
        if let Some(existing) = self
            .conversation_facts
            .activity
            .iter_mut()
            .find(|candidate| candidate.id == activity.id)
        {
            *existing = activity;
        } else {
            self.conversation_facts.activity.push(activity);
        }
    }
}

fn project_message(message: &DesktopMessage) -> ProjectedMessage {
    ProjectedMessage {
        id: message.id.clone(),
        role: project_role(message.role),
        text: message
            .content
            .iter()
            .map(project_content)
            .collect::<Vec<_>>()
            .join("\n"),
        lifecycle: MessageLifecycle::Complete,
    }
}

fn project_role(role: DesktopRole) -> ChatRole {
    match role {
        DesktopRole::System => ChatRole::System,
        DesktopRole::User => ChatRole::User,
        DesktopRole::Assistant => ChatRole::Assistant,
        DesktopRole::Tool => ChatRole::Tool,
    }
}

fn project_content(content: &DesktopContent) -> String {
    match content {
        DesktopContent::Text(text) => text.clone(),
        DesktopContent::Image {
            media_type,
            byte_len,
            width,
            height,
            ..
        } => format!(
            "[image: {media_type}, {byte_len} bytes{}]",
            width
                .zip(*height)
                .map(|(width, height)| format!(", {width}×{height}"))
                .unwrap_or_default()
        ),
        DesktopContent::ToolCall { name } => format!("[tool call: {name}]"),
        DesktopContent::ToolResult { succeeded, output } => format!(
            "[tool result: {}]\n{output}",
            if *succeeded { "completed" } else { "failed" }
        ),
    }
}

fn operation_label(state: DesktopOperationState) -> &'static str {
    match state {
        DesktopOperationState::Running => "Working",
        DesktopOperationState::Suspended => "Waiting for input",
        DesktopOperationState::Completed => "Completed",
        DesktopOperationState::Failed => "Failed",
        DesktopOperationState::Declined => "Declined",
        DesktopOperationState::Interrupted => "Interrupted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xana::desktop::{
        DesktopActivityDisclosure, DesktopActivityOwner, DesktopActivityState, DesktopError,
        DesktopErrorCode, DesktopFactFreshness, DesktopFactSource,
    };

    fn empty_snapshot() -> DesktopSnapshot {
        DesktopSnapshot {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 0,
            session_id: "session".to_owned(),
            connection: "ollama".to_owned(),
            execution_owner: "native".to_owned(),
            model: "fixture".to_owned(),
            reasoning_effort: None,
            notification_policy: xana::desktop::NotificationPolicy::default(),
            conversation: Vec::new(),
            conversation_truncated: false,
            active_operation: None,
            pending_approval_count: 0,
            pending_approvals: Vec::new(),
            activity_count: 0,
            artifact_count: 0,
            host_sequence: 0,
            hosted_workspace_count: 1,
            hosted_conversation_count: 1,
            hosted_conversations: Vec::new(),
            attached_conversation: Some("native/session".to_owned()),
            controllers: Vec::new(),
            host_lifecycle: "running".into(),
            global_notices: Vec::new(),
            navigation: xana::desktop::DesktopNavigationSnapshot::empty(
                xana::desktop::DesktopSidebarMode::Full,
            ),
            layout: xana::desktop::DesktopResolvedLayout {
                layout: xana::desktop::DesktopWorkbenchLayout::recovery(),
                source: xana::desktop::DesktopLayoutSource::Recovery,
                warning: None,
            },
            settings: xana::desktop::DesktopSettingsSnapshot {
                version: 1,
                revision: "fixture".to_owned(),
                warnings: Vec::new(),
                entries: Vec::new(),
                truncated: false,
            },
            conversation_facts: xana::desktop::DesktopConversationFacts {
                profile: Some("default".to_owned()),
                activity: Vec::new(),
                execution: Vec::new(),
                usage: Vec::new(),
                completions: Vec::new(),
                capabilities: Vec::new(),
                prompt_ledger: xana::desktop::DesktopPromptLedger {
                    operation_id: None,
                    estimated_input_tokens: None,
                    input_budget_tokens: None,
                    context_window_tokens: None,
                    context_window_source: None,
                    attachment_count: None,
                    attachment_bytes: None,
                    omitted_source_count: None,
                    unavailable_reason: Some("not observed".to_owned()),
                },
            },
        }
    }

    #[test]
    fn sequence_gap_requires_an_atomic_snapshot() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let operation_id = DesktopOperationId::new();

        assert!(!projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::OperationState {
                operation_id,
                state: DesktopOperationState::Running,
            },
        }));
        assert!(!projection.is_running());
    }

    #[test]
    fn progressive_delta_is_replaced_by_authoritative_final() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let operation_id = DesktopOperationId::new();
        assert!(projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::AssistantDelta {
                operation_id,
                text: "Hel".to_owned(),
            },
        }));
        assert!(projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::MessageFinal {
                operation_id,
                message: DesktopMessage {
                    id: "final".to_owned(),
                    role: DesktopRole::Assistant,
                    content: vec![DesktopContent::Text("Hello".to_owned())],
                },
            },
        }));

        let messages = projection.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id().as_ref(), "final");
        assert_eq!(messages[0].content().text(), "Hello");
    }

    #[test]
    fn terminal_failure_marks_the_authoritative_message_retryable() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let operation_id = DesktopOperationId::new();
        assert!(projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::MessageFinal {
                operation_id,
                message: DesktopMessage {
                    id: "managed-final".to_owned(),
                    role: DesktopRole::Assistant,
                    content: vec![DesktopContent::Text("partial result".to_owned())],
                },
            },
        }));
        assert!(projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::OperationState {
                operation_id,
                state: DesktopOperationState::Failed,
            },
        }));

        let messages = projection.messages();
        assert!(matches!(
            messages[0].content().state(),
            gpui_ai::prelude::ProgressState::Failed(reason) if reason.as_ref() == "Operation failed"
        ));
        assert_eq!(
            projection.operation_for_message("managed-final"),
            Some(operation_id)
        );
    }

    #[test]
    fn managed_selection_receipt_updates_only_later_turn_controls() {
        let mut snapshot = empty_snapshot();
        snapshot.execution_owner = "managed_codex".to_owned();
        let mut projection = ConversationProjection::from_snapshot(&snapshot);
        assert!(projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::ExecutionSelectionChanged {
                model: "gpt-next".to_owned(),
                reasoning_effort: Some("high".to_owned()),
                receipt: "thread retained".to_owned(),
            },
        }));

        assert_eq!(projection.model(), "gpt-next");
        assert_eq!(projection.reasoning_effort(), Some("high"));
        assert_eq!(projection.latest_activity(), "thread retained");
    }

    #[test]
    fn fatal_update_remains_visible() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let error = DesktopError {
            code: DesktopErrorCode::RuntimeCrashed,
            message: "runtime stopped".to_owned(),
        };
        projection.fail(error.message.clone());

        assert_eq!(projection.failure(), Some("runtime stopped"));
    }

    #[test]
    fn activity_updates_replace_by_stable_identity() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let activity = |state, detail: &str| DesktopActivityItem {
            id: "tool-1".to_owned(),
            parent_id: None,
            operation_id: Some("run-1".to_owned()),
            owner: DesktopActivityOwner::XanaRoot,
            state,
            summary_code: "tool.state".to_owned(),
            summary_parameters: Vec::new(),
            disclosed_text: Some(detail.to_owned()),
            disclosure: DesktopActivityDisclosure::Summary,
            source: DesktopFactSource::Runtime,
            freshness: DesktopFactFreshness {
                observed_at_unix_millis: 1,
                max_age_millis: None,
            },
            started_at_unix_millis: Some(1),
            finished_at_unix_millis: None,
        };

        assert!(projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::ActivityUpserted(activity(
                DesktopActivityState::Working,
                "Running",
            )),
        }));
        assert!(projection.apply(DesktopObservation {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::ActivityUpserted(activity(
                DesktopActivityState::Completed,
                "Done",
            )),
        }));

        assert_eq!(projection.conversation_facts().activity.len(), 1);
        assert_eq!(
            projection.conversation_facts().activity[0].state,
            DesktopActivityState::Completed
        );
        assert_eq!(projection.latest_activity(), "Done");
    }
}
