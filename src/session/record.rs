use crate::{
    artifact::ArtifactRecord,
    context::persisted::{ContextRecord, ContextViewRecord},
    identity::*,
    message::Message,
    permission::PermissionAuditFact,
    runtime::OperationState,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const SESSION_RECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RecordEnvelope {
    pub(crate) version: u32,
    pub(crate) record_id: RecordId,
    pub(crate) session_id: SessionId,
    #[serde(flatten)]
    pub(crate) record: SessionRecord,
}

impl RecordEnvelope {
    pub(crate) fn new(session_id: SessionId, record: SessionRecord) -> Self {
        Self {
            version: SESSION_RECORD_VERSION,
            record_id: RecordId::new(),
            session_id,
            record,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub(crate) enum SessionRecord {
    SessionCreated {
        thread_id: ThreadId,
        workspace_root: PathBuf,
    },
    ConversationEntryAppended {
        entry: ConversationEntry,
    },
    ThreadHeadMoved {
        thread_id: ThreadId,
        head: Option<ConversationEntryId>,
    },
    OperationStateChanged {
        operation_id: OperationId,
        state: OperationState,
    },
    PermissionAudited {
        fact: PermissionAuditFact,
    },
    ArtifactRegistered {
        artifact: ArtifactRecord,
    },
    ContextRegistered {
        context: ContextRecord,
    },
    ContextViewRegistered {
        view: ContextViewRecord,
    },
    NamedContextSet {
        name: String,
        context_id: ContextId,
        version: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConversationEntry {
    pub(crate) id: ConversationEntryId,
    pub(crate) parent: Option<ConversationEntryId>,
    pub(crate) agent_id: AgentId,
    pub(crate) message: Message,
}
