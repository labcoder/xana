//! Terminal-independent state and update policy for the full-screen client.

use crate::{
    frontend::EmbeddedClient,
    identity::OperationId,
    message::{ContentBlock, Message, Role},
    runtime::{AgentEvent, OperationState},
};
use std::collections::VecDeque;

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_VISIBLE_MESSAGES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_ACTIVITY: usize = 128;
const MAX_ACTIVITY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutClass {
    Wide,
    Medium,
    Narrow,
}

impl LayoutClass {
    pub(super) fn for_width(width: u16) -> Self {
        if width >= 110 {
            Self::Wide
        } else if width >= 72 {
            Self::Medium
        } else {
            Self::Narrow
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageKind {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VisibleMessage {
    pub(super) kind: MessageKind,
    pub(super) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InputAction {
    Insert(String),
    Backspace,
    Submit,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpdateEffect {
    None,
    Submit {
        operation_id: OperationId,
        input: String,
    },
    Quit,
}

pub(super) struct TuiState {
    pub(super) connection: String,
    pub(super) model: String,
    pub(super) session: String,
    pub(super) status: String,
    pub(super) input: String,
    pub(super) messages: VecDeque<VisibleMessage>,
    pub(super) activity: VecDeque<String>,
    pub(super) busy: bool,
    streaming_operation: Option<OperationId>,
}

impl TuiState {
    pub(super) fn starting() -> Self {
        Self {
            connection: "loading".to_owned(),
            model: "resolving configuration".to_owned(),
            session: "not opened".to_owned(),
            status: "Starting Xana locally…".to_owned(),
            input: String::new(),
            messages: VecDeque::from([VisibleMessage {
                kind: MessageKind::System,
                text: "Xana is preparing the workspace runtime. The interface is ready.".to_owned(),
            }]),
            activity: VecDeque::from(["local frontend ready".to_owned()]),
            busy: true,
            streaming_operation: None,
        }
    }

    pub(super) fn from_client(client: &EmbeddedClient) -> Self {
        let snapshot = client.snapshot();
        let mut messages = snapshot
            .conversation
            .iter()
            .map(message_projection)
            .collect::<VecDeque<_>>();
        trim_front(&mut messages, MAX_VISIBLE_MESSAGES);
        let mut state = Self {
            connection: snapshot.connection.clone(),
            model: snapshot.model.clone(),
            session: snapshot.session_id.to_string(),
            status: "Ready".to_owned(),
            input: String::new(),
            messages,
            activity: VecDeque::new(),
            busy: snapshot.active_operation.is_some(),
            streaming_operation: snapshot.active_operation,
        };
        if snapshot.conversation_truncated {
            state.push_activity("older conversation content is outside the bounded snapshot");
        }
        state
    }

    pub(super) fn update_input(&mut self, action: InputAction) -> UpdateEffect {
        match action {
            InputAction::Insert(text) => {
                if self.input.len().saturating_add(text.len()) <= MAX_INPUT_BYTES {
                    self.input.push_str(&text);
                } else {
                    self.status = "Composer input reached the 1 MiB limit".to_owned();
                }
                UpdateEffect::None
            }
            InputAction::Backspace => {
                self.input.pop();
                UpdateEffect::None
            }
            InputAction::Submit if self.busy => {
                self.status = "A turn is already running".to_owned();
                UpdateEffect::None
            }
            InputAction::Submit => {
                let input = self.input.trim().to_owned();
                if input.is_empty() {
                    return UpdateEffect::None;
                }
                self.input.clear();
                self.push_message(MessageKind::User, input.clone());
                self.busy = true;
                self.status = "Working…".to_owned();
                let operation_id = OperationId::new();
                self.streaming_operation = Some(operation_id);
                UpdateEffect::Submit {
                    operation_id,
                    input,
                }
            }
            InputAction::Quit => UpdateEffect::Quit,
        }
    }

    pub(super) fn apply_runtime(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Running,
            } => {
                self.busy = true;
                self.streaming_operation = Some(*operation_id);
                self.status = "Working…".to_owned();
            }
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(outcome),
            } => {
                self.busy = false;
                self.streaming_operation = None;
                self.status = format!("Turn {outcome:?}");
                self.push_activity(format!("operation {operation_id}: {outcome:?}"));
            }
            AgentEvent::OperationStateChanged {
                state: OperationState::Suspended,
                ..
            } => self.status = "Waiting for a decision".to_owned(),
            AgentEvent::AssistantTextDelta {
                operation_id, text, ..
            } => self.push_assistant_delta(*operation_id, text),
            AgentEvent::AssistantMessage {
                operation_id,
                message,
            } => self.finish_assistant(*operation_id, message),
            AgentEvent::PermissionRequested { request } => {
                self.status = format!("Approval required: {}", request.tool_name);
                self.push_activity(format!("approval requested for {}", request.tool_name));
            }
            AgentEvent::OperationFailed { reason, .. } => {
                self.busy = false;
                self.streaming_operation = None;
                self.status = bounded(reason.clone(), MAX_ACTIVITY_BYTES);
                self.push_activity(format!("turn failed: {reason}"));
            }
            AgentEvent::ConversationCleared => {
                self.messages.clear();
                self.status = "Conversation cleared".to_owned();
            }
            AgentEvent::CommandRejected { reason } => {
                self.busy = false;
                self.status = bounded(format!("Command rejected: {reason}"), MAX_ACTIVITY_BYTES);
            }
            AgentEvent::InvocationIntentCommitted { intent } => {
                self.push_activity(format!(
                    "tool planned: {}",
                    intent.permission.request.tool_name
                ));
            }
            AgentEvent::ToolFinished { .. } => self.push_activity("tool finished"),
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle,
            } => self.push_activity(format!(
                "child {} [{}]: {lifecycle:?}",
                attribution.agent_id, attribution.route
            )),
            AgentEvent::ChildActivity { attribution, .. } => self.push_activity(format!(
                "child {} [{}] activity",
                attribution.agent_id, attribution.route
            )),
            AgentEvent::ChildReportCommitted { report } => self.push_activity(format!(
                "child {} report: {:?}",
                report.attribution.agent_id, report.status
            )),
            AgentEvent::InvocationResultCommitted { .. }
            | AgentEvent::PermissionAudited { .. }
            | AgentEvent::ChildListSnapshot { .. }
            | AgentEvent::ChildInspectionSnapshot { .. }
            | AgentEvent::ChildCancellationRequested { .. } => {}
        }
    }

    fn push_assistant_delta(&mut self, operation_id: OperationId, text: &str) {
        if self.streaming_operation != Some(operation_id)
            || !matches!(self.messages.back(), Some(message) if message.kind == MessageKind::Assistant)
        {
            self.push_message(MessageKind::Assistant, String::new());
            self.streaming_operation = Some(operation_id);
        }
        if let Some(message) = self.messages.back_mut() {
            append_bounded(&mut message.text, text, MAX_MESSAGE_BYTES);
        }
    }

    fn finish_assistant(&mut self, operation_id: OperationId, message: &Message) {
        let final_message = message_projection(message);
        if self.streaming_operation == Some(operation_id)
            && matches!(self.messages.back(), Some(message) if message.kind == MessageKind::Assistant)
        {
            if let Some(message) = self.messages.back_mut() {
                *message = final_message;
            }
        } else {
            self.messages.push_back(final_message);
            trim_front(&mut self.messages, MAX_VISIBLE_MESSAGES);
        }
    }

    fn push_message(&mut self, kind: MessageKind, text: impl Into<String>) {
        self.messages.push_back(VisibleMessage {
            kind,
            text: bounded(text.into(), MAX_MESSAGE_BYTES),
        });
        trim_front(&mut self.messages, MAX_VISIBLE_MESSAGES);
    }

    fn push_activity(&mut self, text: impl Into<String>) {
        self.activity
            .push_back(bounded(text.into(), MAX_ACTIVITY_BYTES));
        trim_front(&mut self.activity, MAX_ACTIVITY);
    }
}

fn message_projection(message: &Message) -> VisibleMessage {
    let kind = match message.role {
        Role::User => MessageKind::User,
        Role::Assistant => MessageKind::Assistant,
        Role::Tool => MessageKind::Tool,
        Role::System => MessageKind::System,
    };
    let mut text = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text(value) => append_bounded(&mut text, value, MAX_MESSAGE_BYTES),
            ContentBlock::Image(image) => append_bounded(
                &mut text,
                &format!("[image: {} bytes]", image.byte_len),
                MAX_MESSAGE_BYTES,
            ),
            ContentBlock::ToolCall(call) => append_bounded(
                &mut text,
                &format!("[tool call: {}]", call.name),
                MAX_MESSAGE_BYTES,
            ),
            ContentBlock::ToolResult(result) => {
                append_bounded(&mut text, &result.output, MAX_MESSAGE_BYTES)
            }
        }
    }
    VisibleMessage { kind, text }
}

fn bounded(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

fn append_bounded(target: &mut String, value: &str, limit: usize) {
    if target.len() >= limit {
        return;
    }
    let remaining = limit - target.len();
    if value.len() <= remaining {
        target.push_str(value);
        return;
    }
    let mut boundary = remaining.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    target.push_str(&value[..boundary]);
    target.push_str("...");
}

fn trim_front<T>(values: &mut VecDeque<T>, limit: usize) {
    while values.len() > limit {
        values.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::StepId, runtime::OperationOutcome};

    #[test]
    fn input_and_runtime_events_follow_one_explicit_update_path() {
        let mut state = TuiState::starting();
        state.busy = false;
        state.update_input(InputAction::Insert("hello".to_owned()));
        let UpdateEffect::Submit {
            operation_id,
            input,
        } = state.update_input(InputAction::Submit)
        else {
            panic!("submit effect");
        };
        assert_eq!(input, "hello");
        state.apply_runtime(&AgentEvent::AssistantTextDelta {
            operation_id,
            step_id: StepId::new(),
            text: "hi".to_owned(),
        });
        state.apply_runtime(&AgentEvent::AssistantMessage {
            operation_id,
            message: Message::text(Role::Assistant, "hi there"),
        });
        state.apply_runtime(&AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(OperationOutcome::Completed),
        });

        assert!(!state.busy);
        assert_eq!(state.messages.back().unwrap().text, "hi there");
    }

    #[test]
    fn composer_and_retained_views_are_bounded() {
        let mut state = TuiState::starting();
        state.busy = false;
        state.update_input(InputAction::Insert("x".repeat(MAX_INPUT_BYTES + 1)));
        assert!(state.input.is_empty());
        assert!(state.status.contains("limit"));
        for index in 0..(MAX_ACTIVITY + 20) {
            state.push_activity(format!("event {index}"));
        }
        assert_eq!(state.activity.len(), MAX_ACTIVITY);
    }
}
