use crate::{
    artifact::ArtifactRecord,
    context::persisted::{ContextRecord, ContextViewRecord},
    identity::*,
    message::Message,
    native_runtime::OperationState,
    operation::{
        InvocationIntent, InvocationResultRecord, NamedValueRecord, RecoveryDecision,
        SuspensionReason,
    },
    orchestration::{AgentHandleSnapshot, ChildLifecycle, ChildReport, OrchestrationPlanStart},
    permission::PermissionAuditFact,
    session::CompactionCheckpoint,
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
    VisionReceiptRecorded {
        receipt: crate::vision::receipt::VisionReceipt,
    },
    SessionCreated {
        thread_id: ThreadId,
        workspace_root: PathBuf,
    },
    ConversationBranched {
        lineage: NativeBranchLineage,
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
    OperationAccepted {
        operation_id: OperationId,
        thread_id: ThreadId,
        input_entry_id: ConversationEntryId,
    },
    AdapterOperationAccepted {
        operation_id: OperationId,
        thread_id: ThreadId,
        input_entry_id: ConversationEntryId,
        binding: crate::operation::adapter::DesktopCommandKey,
    },
    AdapterOperationFinished {
        operation_id: OperationId,
        outcome: crate::native_runtime::OperationOutcome,
        result_entry: Option<crate::operation::adapter::DesktopCommandResultRef>,
    },
    CompletionEvidenceRecorded {
        evidence: crate::completion_evidence::CompletionEvidence,
    },
    FiniteOperationAccepted {
        operation_id: OperationId,
        thread_id: ThreadId,
        input_entry_id: ConversationEntryId,
        completion: crate::completion_evidence::CompletionEvidence,
    },
    StepStarted {
        operation_id: OperationId,
        step_id: StepId,
        assistant_entry_id: ConversationEntryId,
    },
    InvocationIntentAppended {
        intent: InvocationIntent,
    },
    InvocationResultAppended {
        result: InvocationResultRecord,
    },
    OperationSuspended {
        operation_id: OperationId,
        reason: SuspensionReason,
    },
    RoundBudgetDecisionAppended {
        decision: crate::native_runtime::RoundBudgetDecision,
    },
    OperationFinished {
        operation_id: OperationId,
        outcome: crate::native_runtime::OperationOutcome,
    },
    RecoveryDecisionAppended {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: RecoveryDecision,
    },
    NamedValueSet {
        value: NamedValueRecord,
    },
    OrchestrationPlanStarted {
        start: OrchestrationPlanStart,
    },
    ChildAdmitted {
        handle: AgentHandleSnapshot,
    },
    ChildrenBatchAdmitted {
        handles: Vec<AgentHandleSnapshot>,
    },
    ChildLifecycleChanged {
        agent_id: AgentId,
        lifecycle: ChildLifecycle,
    },
    ChildReportCommitted {
        report: ChildReport,
    },
    ConversationCompacted {
        checkpoint: CompactionCheckpoint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeBranchLineage {
    pub(crate) source_session_id: SessionId,
    pub(crate) source_entry_id: ConversationEntryId,
    pub(crate) shared_entry_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConversationEntry {
    pub(crate) id: ConversationEntryId,
    pub(crate) parent: Option<ConversationEntryId>,
    pub(crate) agent_id: AgentId,
    pub(crate) message: Message,
}
