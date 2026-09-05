use crate::{
    artifact::ArtifactRecord,
    controller::{ControllerChangeKind, ControllerLeaseSnapshot, ControllerTakeoverConfirmation},
    frontend::{ClientCommand, ClientCommandResult, ClientEvent, ClientSnapshot},
    identity::{ArtifactId, OperationId},
    workspace_host::{ConversationRef, WorkspaceSnapshot},
};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;
use zeroize::Zeroize;

pub(crate) const LOCAL_HOST_PROTOCOL_VERSION: u16 = 6;
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
    pub(crate) host_generation: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ArtifactRequestId(Uuid);

impl ArtifactRequestId {
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
        takeover: Option<ControllerTakeoverConfirmation>,
    },
    ReleaseControl {
        request_id: ControlRequestId,
    },
    RenewControl {
        request_id: ControlRequestId,
    },
    DecideManagedApproval {
        request_id: ControlRequestId,
        approval_id: Uuid,
        decision: ManagedApprovalDecision,
    },
    GetArtifact {
        request_id: ArtifactRequestId,
        artifact_id: ArtifactId,
        offset: u64,
        max_bytes: usize,
    },
    Command(ClientCommand),
    Ping,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum ServerFrame {
    Snapshot {
        snapshot: Box<HostSnapshot>,
        role: ClientRole,
        controller_reconnect: Option<ReconnectCapability>,
    },
    Observation(HostObservation),
    CommandResult(ClientCommandResult),
    ControlResult(ControlResult),
    ArtifactResult(ArtifactResult),
    ProtocolError {
        code: String,
        message: String,
    },
    HostShuttingDown {
        graceful_ms: u64,
        hard_ms: u64,
    },
    Pong,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactResult {
    pub(crate) request_id: ArtifactRequestId,
    pub(crate) accepted: bool,
    pub(crate) record: Option<ArtifactRecord>,
    pub(crate) range_offset: u64,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) preview: Vec<u8>,
    pub(crate) preview_truncated: bool,
    pub(crate) reason: Option<String>,
}

impl ArtifactResult {
    pub(crate) fn accepted(
        request_id: ArtifactRequestId,
        record: ArtifactRecord,
        range_offset: u64,
        preview: Vec<u8>,
        preview_truncated: bool,
    ) -> Self {
        let total_bytes = record.byte_len;
        Self {
            request_id,
            accepted: true,
            record: Some(record),
            range_offset,
            total_bytes: Some(total_bytes),
            preview,
            preview_truncated,
            reason: None,
        }
    }

    pub(crate) fn rejected(request_id: ArtifactRequestId, reason: impl Into<String>) -> Self {
        Self {
            request_id,
            accepted: false,
            record: None,
            range_offset: 0,
            total_bytes: None,
            preview: Vec::new(),
            preview_truncated: false,
            reason: Some(bounded_label(reason.into())),
        }
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ControlResult {
    pub(crate) request_id: ControlRequestId,
    pub(crate) accepted: bool,
    pub(crate) code: ControlResultCode,
    pub(crate) reason: Option<String>,
    pub(crate) controller: Option<ControllerSnapshot>,
    pub(crate) controller_reconnect: Option<ReconnectCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlResultCode {
    Accepted,
    Released,
    TakeoverConfirmationRequired,
    PendingApproval,
    NotController,
    InvalidReconnect,
    ExpiredReconnect,
    Unavailable,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ReconnectCapability(String);

impl ReconnectCapability {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    pub(crate) fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for ReconnectCapability {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("[REDACTED]")
    }
}

impl Drop for ReconnectCapability {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for ControlResult {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("ControlResult")
            .field("request_id", &self.request_id)
            .field("accepted", &self.accepted)
            .field("code", &self.code)
            .field("reason", &self.reason)
            .field("controller", &self.controller)
            .field("controller_reconnect", &self.controller_reconnect)
            .finish()
    }
}

impl ControlResult {
    pub(crate) fn accepted(
        request_id: ControlRequestId,
        controller: ControllerSnapshot,
        reconnect: String,
    ) -> Self {
        Self {
            request_id,
            accepted: true,
            code: ControlResultCode::Accepted,
            reason: None,
            controller: Some(controller),
            controller_reconnect: Some(ReconnectCapability::new(reconnect)),
        }
    }

    pub(crate) fn released(request_id: ControlRequestId) -> Self {
        Self {
            request_id,
            accepted: true,
            code: ControlResultCode::Released,
            reason: None,
            controller: None,
            controller_reconnect: None,
        }
    }

    pub(crate) fn rejected(
        request_id: ControlRequestId,
        code: ControlResultCode,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            request_id,
            accepted: false,
            code,
            reason: Some(bounded_label(reason.into())),
            controller: None,
            controller_reconnect: None,
        }
    }

    pub(crate) fn takeover_confirmation_required(
        request_id: ControlRequestId,
        controller: ControllerSnapshot,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            request_id,
            accepted: false,
            code: ControlResultCode::TakeoverConfirmationRequired,
            reason: Some(bounded_label(reason.into())),
            controller: Some(controller),
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
            workspace_id: snapshot.workspace_id.clone(),
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
    #[serde(default)]
    pub(crate) scheduled_jobs: Vec<crate::autonomy::host::JobSummary>,
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) host_id: Uuid,
    pub(crate) host_generation: u64,
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
    pub(crate) fn new(host_id: Uuid, host_generation: u64, seed: HostSnapshotSeed) -> Self {
        Self {
            version: LOCAL_HOST_PROTOCOL_VERSION,
            scheduled_jobs: Vec::new(),
            sequence: 0,
            host_id,
            host_generation,
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

pub(crate) type ControllerSnapshot = ControllerLeaseSnapshot<String>;

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
            reason: request.reason,
            // The adapter has already bounded these fields. Truncating a
            // command or cwd for transport hides what the controller approves.
            command: request.command,
            cwd: request.cwd,
            available_decisions: request
                .available_decisions
                .into_iter()
                .take(8)
                .map(bounded_label)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum HostEvent {
    ScheduledJobsChanged {
        jobs: Vec<crate::autonomy::host::JobSummary>,
    },
    Frontend(ClientEvent),
    ObserverCommandRejected {
        command: String,
    },
    ControllerChanged {
        controller: Option<ControllerSnapshot>,
        change: ControllerChangeKind,
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

fn workspace_display_name(workspace: &std::path::Path) -> String {
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
