use crate::{
    frontend::{ClientCommand, ClientCommandResult, ClientEvent, ClientSnapshot},
    identity::OperationId,
    workspace_host::{ConversationRef, WorkspaceSnapshot},
};
use serde::{Deserialize, Serialize};
use std::{fmt, path::Path};
use uuid::Uuid;
use zeroize::Zeroize;

pub(crate) const LOCAL_HOST_PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_WIRE_BYTES: usize = 1024 * 1024;
const MAX_CONVERSATIONS: usize = 512;
const MAX_LABEL_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClientRole {
    Observer,
    Controller,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientHello {
    pub(crate) version: u16,
    pub(crate) host_id: Uuid,
    pub(crate) workspace_id: String,
    pub(crate) capability: String,
    #[serde(default)]
    pub(crate) controller_reconnect: Option<String>,
    pub(crate) role: ClientRole,
}

impl Drop for ClientHello {
    fn drop(&mut self) {
        self.capability.zeroize();
        if let Some(reconnect) = &mut self.controller_reconnect {
            reconnect.zeroize();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ControlRequestId(Uuid);

impl ControlRequestId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum ClientFrame {
    Hello(ClientHello),
    RequestSnapshot,
    AcquireControl {
        request_id: ControlRequestId,
        conversation: String,
        takeover: bool,
    },
    ReleaseControl {
        request_id: ControlRequestId,
    },
    DecideManagedApproval {
        request_id: ControlRequestId,
        approval_id: Uuid,
        decision: ManagedApprovalDecision,
    },
    Command(ClientCommand),
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum ServerFrame {
    Snapshot {
        snapshot: Box<HostSnapshot>,
        role: ClientRole,
    },
    Observation(HostObservation),
    CommandResult(ClientCommandResult),
    ControlResult(ControlResult),
    ProtocolError {
        code: String,
        message: String,
    },
    Pong,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ControlResult {
    pub(crate) request_id: ControlRequestId,
    pub(crate) accepted: bool,
    pub(crate) reason: Option<String>,
    pub(crate) controller_reconnect: Option<String>,
}

impl fmt::Debug for ControlResult {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("ControlResult")
            .field("request_id", &self.request_id)
            .field("accepted", &self.accepted)
            .field("reason", &self.reason)
            .field(
                "controller_reconnect",
                &self.controller_reconnect.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl Drop for ControlResult {
    fn drop(&mut self) {
        if let Some(reconnect) = &mut self.controller_reconnect {
            reconnect.zeroize();
        }
    }
}

impl ControlResult {
    pub(crate) fn accepted(request_id: ControlRequestId, reconnect: String) -> Self {
        Self {
            request_id,
            accepted: true,
            reason: None,
            controller_reconnect: Some(reconnect),
        }
    }

    pub(crate) fn released(request_id: ControlRequestId) -> Self {
        Self {
            request_id,
            accepted: true,
            reason: None,
            controller_reconnect: None,
        }
    }

    pub(crate) fn rejected(request_id: ControlRequestId, reason: impl Into<String>) -> Self {
        Self {
            request_id,
            accepted: false,
            reason: Some(bounded_label(reason.into())),
            controller_reconnect: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversationOwner {
    Native,
    Managed,
    NewNative,
    NewManaged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostConversation {
    pub(crate) identity: String,
    pub(crate) owner: ConversationOwner,
    pub(crate) state: String,
    pub(crate) record_count: Option<usize>,
    pub(crate) selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostSnapshotSeed {
    pub(crate) workspace_id: String,
    pub(crate) workspace_name: String,
    pub(crate) conversations: Vec<HostConversation>,
    pub(crate) conversations_truncated: bool,
    pub(crate) active_conversation: Option<String>,
}

impl HostSnapshotSeed {
    pub(crate) fn from_workspace(snapshot: &WorkspaceSnapshot) -> Self {
        let conversations_truncated = snapshot.conversations.len() > MAX_CONVERSATIONS;
        let conversations = snapshot
            .conversations
            .iter()
            .take(MAX_CONVERSATIONS)
            .map(|conversation| HostConversation {
                identity: bounded_label(conversation.conversation.to_string()),
                owner: conversation_owner(&conversation.conversation),
                state: conversation.state.to_string(),
                record_count: conversation.record_count,
                selected: conversation.selected,
            })
            .collect();
        Self {
            workspace_id: workspace_identity(&snapshot.workspace),
            workspace_name: workspace_display_name(&snapshot.workspace),
            conversations,
            conversations_truncated,
            active_conversation: snapshot
                .active
                .as_ref()
                .map(|active| bounded_label(active.conversation.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HostSnapshot {
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) host_id: Uuid,
    pub(crate) workspace_id: String,
    pub(crate) workspace_name: String,
    pub(crate) conversations: Vec<HostConversation>,
    pub(crate) conversations_truncated: bool,
    pub(crate) active_conversation: Option<String>,
    pub(crate) controllable_conversation: Option<String>,
    pub(crate) controller: Option<ControllerSnapshot>,
    pub(crate) frontend: Option<ClientSnapshot>,
}

impl HostSnapshot {
    pub(crate) fn new(host_id: Uuid, seed: HostSnapshotSeed) -> Self {
        Self {
            version: LOCAL_HOST_PROTOCOL_VERSION,
            sequence: 0,
            host_id,
            workspace_id: seed.workspace_id,
            workspace_name: seed.workspace_name,
            conversations: seed.conversations,
            conversations_truncated: seed.conversations_truncated,
            active_conversation: seed.active_conversation,
            controllable_conversation: None,
            controller: None,
            frontend: None,
        }
    }

    pub(crate) fn with_controllable_conversation(mut self, conversation: String) -> Self {
        self.controllable_conversation = Some(bounded_label(conversation));
        self
    }

    pub(crate) fn with_frontend(mut self, snapshot: ClientSnapshot) -> Self {
        self.frontend = Some(snapshot);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControllerState {
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedApprovalDecision {
    AcceptOnce,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedApprovalSnapshot {
    pub(crate) approval_id: Uuid,
    pub(crate) operation_id: OperationId,
    pub(crate) method: String,
    pub(crate) reason: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) available_decisions: Vec<String>,
}

impl ManagedApprovalSnapshot {
    pub(crate) fn bounded(
        approval_id: Uuid,
        operation_id: OperationId,
        request: crate::managed::codex::ApprovalRequest,
    ) -> Self {
        Self {
            approval_id,
            operation_id,
            method: bounded_label(request.method),
            reason: request.reason.map(bounded_label),
            command: request.command.map(bounded_label),
            cwd: request.cwd.map(bounded_label),
            available_decisions: request
                .available_decisions
                .into_iter()
                .take(8)
                .map(bounded_label)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ControllerSnapshot {
    pub(crate) conversation: String,
    pub(crate) state: ControllerState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum HostEvent {
    Frontend(ClientEvent),
    ObserverCommandRejected {
        command: String,
    },
    ControllerChanged {
        controller: Option<ControllerSnapshot>,
        reason: String,
    },
    ManagedApprovalRequested(ManagedApprovalSnapshot),
    ManagedApprovalResolved {
        approval_id: Uuid,
        accepted: bool,
    },
    ManagedTurnFinished {
        operation_id: OperationId,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HostObservation {
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) event: HostEvent,
}

pub(crate) fn workspace_identity(workspace: &Path) -> String {
    blake3::hash(workspace.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string()
}

fn workspace_display_name(workspace: &Path) -> String {
    bounded_label(
        workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace")
            .to_owned(),
    )
}

fn conversation_owner(conversation: &ConversationRef) -> ConversationOwner {
    match conversation {
        ConversationRef::Native { .. } => ConversationOwner::Native,
        ConversationRef::Managed { .. } => ConversationOwner::Managed,
        ConversationRef::NewNative => ConversationOwner::NewNative,
        ConversationRef::NewManaged { .. } => ConversationOwner::NewManaged,
    }
}

pub(crate) fn command_kind(command: &ClientCommand) -> String {
    let encoded = serde_json::to_value(&command.value).ok();
    let name = encoded
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .and_then(|fields| fields.keys().next())
        .map_or("unknown", String::as_str);
    bounded_label(name.to_owned())
}

pub(crate) fn encode_frame(frame: &ServerFrame) -> Result<String, String> {
    let encoded = serde_json::to_string(frame)
        .map_err(|error| format!("could not encode local-host frame: {error}"))?;
    if encoded.len() > MAX_WIRE_BYTES {
        return Err(format!(
            "local-host frame is {} bytes; limit is {MAX_WIRE_BYTES}",
            encoded.len()
        ));
    }
    Ok(encoded)
}

pub(crate) fn decode_client_frame(encoded: &str) -> Result<ClientFrame, String> {
    if encoded.len() > MAX_WIRE_BYTES {
        return Err(format!(
            "local-host frame is {} bytes; limit is {MAX_WIRE_BYTES}",
            encoded.len()
        ));
    }
    serde_json::from_str(encoded).map_err(|_| "malformed local-host frame".to_owned())
}

pub(crate) fn decode_server_frame(encoded: &str) -> Result<ServerFrame, String> {
    if encoded.len() > MAX_WIRE_BYTES {
        return Err(format!(
            "local-host frame is {} bytes; limit is {MAX_WIRE_BYTES}",
            encoded.len()
        ));
    }
    serde_json::from_str(encoded).map_err(|_| "malformed local-host frame".to_owned())
}

fn bounded_label(mut value: String) -> String {
    if value.len() <= MAX_LABEL_BYTES {
        return value;
    }
    let mut end = MAX_LABEL_BYTES.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("...");
    value
}
