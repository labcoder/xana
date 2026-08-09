use crate::{
    frontend::{ClientCommand, ClientCommandResult, ClientEvent},
    workspace_host::{ConversationRef, WorkspaceSnapshot},
};
use serde::{Deserialize, Serialize};
use std::path::Path;
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
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientHello {
    pub(crate) version: u16,
    pub(crate) host_id: Uuid,
    pub(crate) workspace_id: String,
    pub(crate) capability: String,
    pub(crate) role: ClientRole,
}

impl Drop for ClientHello {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum ClientFrame {
    Hello(ClientHello),
    RequestSnapshot,
    Command(ClientCommand),
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum ServerFrame {
    Snapshot(HostSnapshot),
    Observation(HostObservation),
    CommandResult(ClientCommandResult),
    ProtocolError { code: String, message: String },
    Pong,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostSnapshot {
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) host_id: Uuid,
    pub(crate) workspace_id: String,
    pub(crate) workspace_name: String,
    pub(crate) conversations: Vec<HostConversation>,
    pub(crate) conversations_truncated: bool,
    pub(crate) active_conversation: Option<String>,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum HostEvent {
    Frontend(ClientEvent),
    ObserverCommandRejected { command: String },
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
