//! Pure application-owned projection from Xana's Desktop protocol to UI state.

use gpui_ai::prelude::{ChatMessage, ChatRole, MessageActions, StreamedContent};
use std::collections::HashMap;
use xana::desktop::{
    DesktopContent, DesktopEvent, DesktopMessage, DesktopObservation, DesktopOperationId,
    DesktopOperationState, DesktopRole, DesktopRoundBudgetSuspension, DesktopSnapshot,
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

/// Xana Desktop owns this state; `gpui-ai` receives immutable snapshots.
pub(crate) struct ConversationProjection {
    messages: Vec<ProjectedMessage>,
    streams: HashMap<DesktopOperationId, String>,
    sequence: u64,
    active_operation: Option<DesktopOperationId>,
    pending_round_budget: Option<DesktopRoundBudgetSuspension>,
    latest_activity: String,
    failure: Option<String>,
}

impl ConversationProjection {
    pub(crate) fn from_snapshot(snapshot: &DesktopSnapshot) -> Self {
        Self {
            messages: snapshot.conversation.iter().map(project_message).collect(),
            streams: HashMap::new(),
            sequence: snapshot.sequence,
            active_operation: snapshot.active_operation,
            pending_round_budget: None,
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
            DesktopEvent::MessageFinal {
                operation_id,
                message,
            } => self.replace_stream_with_final(operation_id, message),
            DesktopEvent::PermissionRequired { tool, .. } => {
                self.latest_activity = format!("Approval required for {tool}");
            }
            DesktopEvent::PermissionResolved { .. } => {
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
                self.active_operation = None;
                self.pending_round_budget = None;
                self.latest_activity = "Conversation cleared".to_owned();
            }
            DesktopEvent::Activity { label } => self.latest_activity = label,
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

    pub(crate) fn is_running(&self) -> bool {
        self.active_operation.is_some() && self.pending_round_budget.is_none()
    }

    pub(crate) fn pending_round_budget(&self) -> Option<&DesktopRoundBudgetSuspension> {
        self.pending_round_budget.as_ref()
    }

    pub(crate) fn latest_activity(&self) -> &str {
        &self.latest_activity
    }

    pub(crate) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
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
            self.messages[index] = project_message(&message);
            return;
        }
        if !self
            .messages
            .iter()
            .any(|candidate| candidate.id == message.id)
        {
            self.messages.push(project_message(&message));
        }
    }

    fn finish_stream(&mut self, operation_id: DesktopOperationId, failure: Option<String>) {
        let Some(id) = self.streams.get(&operation_id) else {
            return;
        };
        if let Some(message) = self.messages.iter_mut().find(|message| &message.id == id) {
            message.lifecycle = failure
                .map(MessageLifecycle::Failed)
                .unwrap_or(MessageLifecycle::Complete);
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
    use xana::desktop::{DesktopError, DesktopErrorCode};

    fn empty_snapshot() -> DesktopSnapshot {
        DesktopSnapshot {
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 0,
            session_id: "session".to_owned(),
            connection: "ollama".to_owned(),
            execution_owner: "native".to_owned(),
            model: "fixture".to_owned(),
            reasoning_effort: None,
            conversation: Vec::new(),
            conversation_truncated: false,
            active_operation: None,
            pending_approval_count: 0,
            activity_count: 0,
            artifact_count: 0,
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
    fn fatal_update_remains_visible() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let error = DesktopError {
            code: DesktopErrorCode::RuntimeCrashed,
            message: "runtime stopped".to_owned(),
        };
        projection.fail(error.message.clone());

        assert_eq!(projection.failure(), Some("runtime stopped"));
    }
}
