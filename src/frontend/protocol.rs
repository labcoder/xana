//! Versioned, bounded, repository-private frontend protocol values.
//!
//! Frontends send correlated commands and render bounded observations. This
//! module contains no terminal, network, provider-wire, or presentation
//! concerns. The embedded adapter is the reference transport; later transport
//! projections must preserve these semantics rather than invent another API.

use super::managed::ManagedClientEvent;
use crate::{
    identity::{AgentId, OperationId, SessionId, ToolInvocationId},
    message::Message,
    native_runtime::{AgentEvent, RuntimeCommand},
    orchestration::{ChildInspection, ChildLifecycle},
    permission::ControllerDecision,
    vision::ImageRef,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) const FRONTEND_PROTOCOL_VERSION: u16 = 2;
const MAX_SNAPSHOT_MESSAGES: usize = 512;
const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_OMISSION_LABEL_BYTES: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ClientCommandId(Uuid);

impl ClientCommandId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClientCommand {
    pub(crate) version: u16,
    pub(crate) id: ClientCommandId,
    pub(crate) value: ClientCommandValue,
}

impl ClientCommand {
    pub(crate) fn new(value: impl Into<ClientCommandValue>) -> Self {
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            id: ClientCommandId::new(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ClientCommandValue {
    SubmitTurn {
        operation_id: OperationId,
        input: String,
        images: Vec<ImageRef>,
    },
    ClearConversation,
    ResumeOperation {
        session_id: SessionId,
        operation_id: OperationId,
    },
    InterruptOperation {
        operation_id: OperationId,
    },
    SteerOperation {
        operation_id: OperationId,
        input: String,
    },
    DecidePermission {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    DecideChildPermission {
        agent_id: AgentId,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    ListChildren,
    InspectChild {
        agent_id: AgentId,
    },
    CancelChild {
        agent_id: AgentId,
    },
    Shutdown,
}

impl From<RuntimeCommand> for ClientCommandValue {
    fn from(command: RuntimeCommand) -> Self {
        match command {
            RuntimeCommand::SubmitTurn {
                operation_id,
                input,
            } => Self::SubmitTurn {
                operation_id,
                input,
                images: Vec::new(),
            },
            RuntimeCommand::SubmitTurnWithImages {
                operation_id,
                input,
                images,
            } => Self::SubmitTurn {
                operation_id,
                input,
                images,
            },
            RuntimeCommand::ClearConversation => Self::ClearConversation,
            RuntimeCommand::ResumeOperation {
                session_id,
                operation_id,
            } => Self::ResumeOperation {
                session_id,
                operation_id,
            },
            RuntimeCommand::InterruptOperation { operation_id } => {
                Self::InterruptOperation { operation_id }
            }
            RuntimeCommand::SteerOperation {
                operation_id,
                input,
            } => Self::SteerOperation {
                operation_id,
                input,
            },
            RuntimeCommand::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            } => Self::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            },
            RuntimeCommand::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            } => Self::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            },
            RuntimeCommand::ListChildren => Self::ListChildren,
            RuntimeCommand::InspectChild { agent_id } => Self::InspectChild { agent_id },
            RuntimeCommand::CancelChild { agent_id } => Self::CancelChild { agent_id },
            RuntimeCommand::Shutdown => Self::Shutdown,
        }
    }
}

impl From<ClientCommandValue> for RuntimeCommand {
    fn from(command: ClientCommandValue) -> Self {
        match command {
            ClientCommandValue::SubmitTurn {
                operation_id,
                input,
                images,
            } if images.is_empty() => Self::SubmitTurn {
                operation_id,
                input,
            },
            ClientCommandValue::SubmitTurn {
                operation_id,
                input,
                images,
            } => Self::SubmitTurnWithImages {
                operation_id,
                input,
                images,
            },
            ClientCommandValue::ClearConversation => Self::ClearConversation,
            ClientCommandValue::ResumeOperation {
                session_id,
                operation_id,
            } => Self::ResumeOperation {
                session_id,
                operation_id,
            },
            ClientCommandValue::InterruptOperation { operation_id } => {
                Self::InterruptOperation { operation_id }
            }
            ClientCommandValue::SteerOperation {
                operation_id,
                input,
            } => Self::SteerOperation {
                operation_id,
                input,
            },
            ClientCommandValue::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            } => Self::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            },
            ClientCommandValue::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            } => Self::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            },
            ClientCommandValue::ListChildren => Self::ListChildren,
            ClientCommandValue::InspectChild { agent_id } => Self::InspectChild { agent_id },
            ClientCommandValue::CancelChild { agent_id } => Self::CancelChild { agent_id },
            ClientCommandValue::Shutdown => Self::Shutdown,
        }
    }
}

impl ClientCommandValue {
    pub(super) fn validate(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("could not encode frontend command: {error}"))?
            .len();
        if bytes > MAX_COMMAND_BYTES {
            return Err(format!(
                "frontend command is {bytes} bytes; limit is {MAX_COMMAND_BYTES}"
            ));
        }
        if let Self::SubmitTurn { images, .. } = self
            && images.len() > 8
        {
            return Err("a frontend turn may contain at most 8 images".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClientCommandResult {
    pub(crate) version: u16,
    pub(crate) command_id: ClientCommandId,
    pub(crate) accepted: bool,
    pub(crate) reason: Option<String>,
}

impl ClientCommandResult {
    pub(crate) fn accepted(command_id: ClientCommandId) -> Self {
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            command_id,
            accepted: true,
            reason: None,
        }
    }

    pub(crate) fn rejected(command_id: ClientCommandId, reason: impl Into<String>) -> Self {
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            command_id,
            accepted: false,
            reason: Some(bounded_text(reason.into(), MAX_OMISSION_LABEL_BYTES)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChildSnapshot {
    pub(crate) agent_id: AgentId,
    pub(crate) route: String,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) lifecycle: ChildLifecycle,
}

impl From<&ChildInspection> for ChildSnapshot {
    fn from(child: &ChildInspection) -> Self {
        Self {
            agent_id: child.handle.admission.attribution.agent_id,
            route: bounded_text(
                child.handle.admission.attribution.route.clone(),
                MAX_OMISSION_LABEL_BYTES,
            ),
            connection: bounded_text(
                child.handle.admission.attribution.connection.clone(),
                MAX_OMISSION_LABEL_BYTES,
            ),
            model: bounded_text(
                child.handle.admission.attribution.model.clone(),
                MAX_OMISSION_LABEL_BYTES,
            ),
            lifecycle: child.handle.lifecycle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClientSnapshot {
    pub(crate) version: u16,
    /// Last observation included in the snapshot. The initial embedded
    /// snapshot is captured before forwarding starts, so this is zero.
    pub(crate) sequence: u64,
    pub(crate) session_id: SessionId,
    pub(crate) connection: String,
    pub(crate) execution_owner: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) conversation: Vec<Message>,
    pub(crate) conversation_truncated: bool,
    pub(crate) active_operation: Option<OperationId>,
    pub(crate) children: Vec<ChildSnapshot>,
    pub(crate) pending_approval_count: usize,
    pub(crate) activity_count: usize,
    pub(crate) artifact_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ClientSnapshotSeed {
    pub(crate) session_id: SessionId,
    pub(crate) connection: String,
    pub(crate) execution_owner: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) children: Vec<ChildInspection>,
}

impl ClientSnapshot {
    pub(super) fn initial(seed: ClientSnapshotSeed, history: Vec<Message>) -> Self {
        let (conversation, conversation_truncated) = bounded_history(history);
        let artifact_count = conversation
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, crate::message::ContentBlock::Image(_)))
            .count();
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: 0,
            session_id: seed.session_id,
            connection: bounded_text(seed.connection, MAX_OMISSION_LABEL_BYTES),
            execution_owner: bounded_text(seed.execution_owner, MAX_OMISSION_LABEL_BYTES),
            model: bounded_text(seed.model, MAX_OMISSION_LABEL_BYTES),
            reasoning_effort: seed
                .reasoning_effort
                .map(|value| bounded_text(value, MAX_OMISSION_LABEL_BYTES)),
            conversation,
            conversation_truncated,
            active_operation: None,
            children: seed
                .children
                .iter()
                .take(64)
                .map(ChildSnapshot::from)
                .collect(),
            pending_approval_count: 0,
            activity_count: 0,
            artifact_count,
        }
    }

    pub(crate) fn apply(&mut self, event: &ClientEvent, sequence: u64) {
        self.sequence = sequence;
        match event {
            ClientEvent::Runtime(event) => match event.as_ref() {
                AgentEvent::OperationStateChanged {
                    operation_id,
                    state:
                        crate::native_runtime::OperationState::Running
                        | crate::native_runtime::OperationState::Suspended,
                } => self.active_operation = Some(*operation_id),
                AgentEvent::OperationStateChanged {
                    state: crate::native_runtime::OperationState::Finished(_),
                    ..
                }
                | AgentEvent::OperationFailed { .. } => self.active_operation = None,
                AgentEvent::AssistantMessage { message, .. } => {
                    let mut conversation = std::mem::take(&mut self.conversation);
                    conversation.push(message.clone());
                    let (conversation, truncated) = bounded_history(conversation);
                    self.conversation = conversation;
                    self.conversation_truncated |= truncated;
                    self.artifact_count = self
                        .conversation
                        .iter()
                        .flat_map(|message| &message.content)
                        .filter(|block| matches!(block, crate::message::ContentBlock::Image(_)))
                        .count();
                }
                AgentEvent::ConversationCleared => {
                    self.conversation.clear();
                    self.conversation_truncated = false;
                    self.active_operation = None;
                    self.artifact_count = 0;
                }
                AgentEvent::PermissionRequested { .. } => {
                    self.pending_approval_count = self.pending_approval_count.saturating_add(1);
                }
                AgentEvent::PermissionAudited { .. } => {
                    self.pending_approval_count = self.pending_approval_count.saturating_sub(1);
                }
                AgentEvent::ChildListSnapshot { children } => {
                    self.children = children.iter().take(64).map(ChildSnapshot::from).collect();
                }
                _ => self.activity_count = self.activity_count.saturating_add(1),
            },
            ClientEvent::Managed(_) | ClientEvent::PayloadOmitted { .. } => {
                self.activity_count = self.activity_count.saturating_add(1);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ClientEvent {
    Runtime(Box<AgentEvent>),
    Managed(Box<ManagedClientEvent>),
    PayloadOmitted {
        kind: String,
        encoded_bytes: usize,
        limit: usize,
    },
}

impl ClientEvent {
    pub(crate) fn bounded(event: AgentEvent) -> Self {
        let kind = event_kind(&event);
        let redacted = serde_json::to_value(event).and_then(|mut value| {
            redact_sensitive_fields(&mut value);
            serde_json::from_value::<AgentEvent>(value)
        });
        match redacted.and_then(|event| serde_json::to_vec(&event).map(|encoded| (event, encoded)))
        {
            Ok((event, encoded)) if encoded.len() <= MAX_EVENT_BYTES => {
                Self::Runtime(Box::new(event))
            }
            Ok((_, encoded)) => Self::PayloadOmitted {
                kind: kind.to_owned(),
                encoded_bytes: encoded.len(),
                limit: MAX_EVENT_BYTES,
            },
            Err(_) => Self::PayloadOmitted {
                kind: kind.to_owned(),
                encoded_bytes: 0,
                limit: MAX_EVENT_BYTES,
            },
        }
    }
}

fn redact_sensitive_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
                if normalized.contains("authorization")
                    || normalized.contains("accesstoken")
                    || normalized.contains("refreshtoken")
                    || normalized.contains("apikey")
                    || normalized.contains("password")
                    || normalized == "secret"
                {
                    *value = serde_json::Value::String("[REDACTED]".to_owned());
                } else {
                    redact_sensitive_fields(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_sensitive_fields(value);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClientObservation {
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) event: ClientEvent,
}

fn bounded_history(history: Vec<Message>) -> (Vec<Message>, bool) {
    let original_len = history.len();
    let start = original_len.saturating_sub(MAX_SNAPSHOT_MESSAGES);
    let mut retained = history.into_iter().skip(start).collect::<Vec<_>>();
    let mut truncated = start > 0;
    while !retained.is_empty()
        && serde_json::to_vec(&retained).map_or(true, |encoded| encoded.len() > MAX_SNAPSHOT_BYTES)
    {
        retained.remove(0);
        truncated = true;
    }
    (retained, truncated)
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

fn event_kind(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::OperationStateChanged { .. } => "operation state",
        AgentEvent::AssistantTextDelta { .. } => "assistant delta",
        AgentEvent::ProviderReasoningDelta { .. } => "provider reasoning delta",
        AgentEvent::PermissionRequested { .. } => "permission request",
        AgentEvent::PermissionAudited { .. } => "permission audit",
        AgentEvent::InvocationIntentCommitted { .. } => "invocation intent",
        AgentEvent::InvocationResultCommitted { .. } => "invocation result",
        AgentEvent::ToolFinished { .. } => "tool result",
        AgentEvent::AssistantMessage { .. } => "assistant message",
        AgentEvent::UsageObserved { .. } => "usage observation",
        AgentEvent::OperationFailed { .. } => "operation failure",
        AgentEvent::ConversationCleared => "conversation clear",
        AgentEvent::CommandRejected { .. } => "command rejection",
        AgentEvent::ChildLifecycleChanged { .. } => "child lifecycle",
        AgentEvent::ChildActivity { .. } => "child activity",
        AgentEvent::ChildReportCommitted { .. } => "child report",
        AgentEvent::ChildListSnapshot { .. } => "child list",
        AgentEvent::ChildInspectionSnapshot { .. } => "child inspection",
        AgentEvent::ChildCancellationRequested { .. } => "child cancellation",
        AgentEvent::ExternalAgentActivity { .. } => "external-agent activity",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed::codex::ManagedNotification;
    use crate::message::{ContentBlock, Role};
    use crate::{permission::PermissionRequest, tool::EffectClass};

    #[test]
    fn snapshot_keeps_recent_bounded_history() {
        let history = (0..(MAX_SNAPSHOT_MESSAGES + 8))
            .map(|index| Message::text(Role::User, format!("message {index}")))
            .collect();
        let (bounded, truncated) = bounded_history(history);

        assert!(truncated);
        assert_eq!(bounded.len(), MAX_SNAPSHOT_MESSAGES);
        assert!(matches!(
            bounded.first().and_then(|message| message.content.first()),
            Some(ContentBlock::Text(text)) if text == "message 8"
        ));
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= MAX_SNAPSHOT_BYTES);
    }

    #[test]
    fn oversized_observation_becomes_bounded_omission() {
        let event = AgentEvent::CommandRejected {
            reason: "x".repeat(MAX_EVENT_BYTES + 1),
        };
        let bounded = ClientEvent::bounded(event);

        assert!(matches!(
            bounded,
            ClientEvent::PayloadOmitted {
                kind,
                encoded_bytes,
                limit: MAX_EVENT_BYTES,
            } if kind == "command rejection" && encoded_bytes > MAX_EVENT_BYTES
        ));
    }

    #[test]
    fn protocol_serialization_does_not_expose_provider_wire_fields() {
        let event = ClientObservation {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: 1,
            event: ClientEvent::Runtime(Box::new(AgentEvent::AssistantMessage {
                operation_id: OperationId::new(),
                message: Message::text(Role::Assistant, "safe result"),
            })),
        };
        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains("safe result"));
        assert!(!json.contains("authorization"));
        assert!(!json.contains("access_token"));
        assert!(!json.contains("jsonrpc"));
    }

    #[test]
    fn interrupt_and_steer_commands_keep_exact_operation_correlation() {
        let operation_id = OperationId::new();
        for value in [
            ClientCommandValue::InterruptOperation { operation_id },
            ClientCommandValue::SteerOperation {
                operation_id,
                input: "focus on the failing test".to_owned(),
            },
        ] {
            let command = ClientCommand::new(value.clone());
            let encoded = serde_json::to_vec(&command).unwrap();
            let decoded: ClientCommand = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded.version, FRONTEND_PROTOCOL_VERSION);
            assert_eq!(decoded.value, value);
            let runtime: RuntimeCommand = decoded.value.into();
            assert!(matches!(
                runtime,
                RuntimeCommand::InterruptOperation { operation_id: actual }
                    | RuntimeCommand::SteerOperation { operation_id: actual, .. }
                    if actual == operation_id
            ));
        }
    }

    #[test]
    fn runtime_observation_redacts_secret_shaped_fields_before_delivery() {
        let event = ClientEvent::bounded(AgentEvent::PermissionRequested {
            request: PermissionRequest {
                operation_id: OperationId::new(),
                invocation_id: ToolInvocationId::new(),
                tool_name: "remote_call".to_owned(),
                effect_class: EffectClass::Network,
                final_arguments: serde_json::json!({
                    "api_key": "top-secret",
                    "nested": {"accessToken": "also-secret"},
                    "path": "README.md"
                }),
                scope: crate::permission::PermissionScope::Unscoped,
                outbound_review: None,
            },
        });
        let json = serde_json::to_string(&event).unwrap();

        assert!(!json.contains("top-secret"));
        assert!(!json.contains("also-secret"));
        assert!(json.contains("[REDACTED]"));
        assert!(json.contains("README.md"));
    }

    #[test]
    fn managed_notifications_cross_the_same_bounded_frontend_vocabulary() {
        let event = ManagedClientEvent::from_notification(ManagedNotification::AssistantDelta {
            item_id: Some("vendor-item-id".to_owned()),
            delta: "managed result".to_owned(),
        })
        .expect("conversation notification");
        let observation = ClientObservation {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: 4,
            event: ClientEvent::Managed(Box::new(event)),
        };
        let json = serde_json::to_string(&observation).unwrap();

        assert!(json.contains("managed result"));
        assert!(!json.contains("vendor-item-id"));
        assert!(!json.contains("jsonrpc"));
        assert!(
            ManagedClientEvent::from_notification(ManagedNotification::Other {
                method: "vendor/private/method".to_owned(),
            })
            .is_none()
        );
    }
}
