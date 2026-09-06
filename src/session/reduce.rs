use super::compaction::{
    COMPACTION_CHECKPOINT_VERSION, CompactionCheckpoint, CompactionSourceProof, source_digest,
    validate_summary,
};
use super::record::{
    ConversationEntry, NativeBranchLineage, RecordEnvelope, SESSION_RECORD_VERSION, SessionRecord,
};
use crate::{
    artifact::ArtifactRecord,
    context::persisted::{ContextRecord, ContextViewRecord},
    identity::*,
    message::Message,
    native_runtime::{OperationOutcome, OperationState},
    operation::{
        DurableValueRef, InvocationIntent, InvocationResultRecord, NamedValueRecord,
        RecoveryDecision,
    },
    orchestration::{
        AgentHandleSnapshot, CHILD_REPORT_VERSION, ChildInspection, ChildLifecycle, ChildReport,
        ChildReportReference, ChildTerminalStatus,
    },
    permission::PermissionAuditFact,
};
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt,
    path::PathBuf,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RestoredSession {
    pub(crate) session_id: SessionId,
    pub(crate) thread_id: ThreadId,
    pub(crate) workspace_root: PathBuf,
    pub(crate) branch: Option<NativeBranchLineage>,
    pub(crate) head: Option<ConversationEntryId>,
    pub(crate) entries: BTreeMap<ConversationEntryId, ConversationEntry>,
    pub(crate) operations: BTreeMap<OperationId, OperationState>,
    pub(crate) audits: Vec<PermissionAuditFact>,
    pub(crate) artifacts: BTreeMap<ArtifactId, ArtifactRecord>,
    #[serde(with = "super::hydration::context_map")]
    pub(crate) contexts: BTreeMap<(ContextId, u64), ContextRecord>,
    pub(crate) views: BTreeMap<ContextViewId, ContextViewRecord>,
    pub(crate) named_context: BTreeMap<String, (ContextId, u64)>,
    pub(crate) operation_details: BTreeMap<OperationId, RestoredOperation>,
    /// Latest bounded completion receipts survive disposable execution eviction.
    #[serde(default)]
    pub(crate) completion_evidence: Vec<crate::completion_evidence::CompletionEvidence>,
    pub(crate) named_values: BTreeMap<NamedValueId, NamedValueRecord>,
    pub(crate) orchestration_plans:
        BTreeMap<OrchestrationPlanId, crate::orchestration::OrchestrationPlanStart>,
    pub(crate) children: BTreeMap<AgentId, RestoredChild>,
    pub(crate) compactions: Vec<CompactionCheckpoint>,
    /// Verified immutable prefix no longer retained as in-memory message bodies.
    #[serde(default)]
    pub(crate) archived_prefix: Option<CompactionCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RestoredChild {
    pub(crate) handle: AgentHandleSnapshot,
    pub(crate) report: Option<ChildReport>,
}

impl RestoredChild {
    pub(crate) fn inspection(&self) -> ChildInspection {
        if self.handle.lifecycle.is_terminal() {
            return ChildInspection {
                handle: self.handle.clone(),
                report: self.report.clone(),
                projected_interruption: false,
            };
        }
        let mut handle = self.handle.clone();
        let report = ChildReport::interrupted_with_schema(
            handle.admission.attribution.clone(),
            handle.admission.result_schema,
            "child work was interrupted when its owning Xana runtime stopped".to_owned(),
            handle.admission.limits.max_report_bytes,
        );
        handle.apply_report(&report);
        ChildInspection {
            handle,
            report: Some(report),
            projected_interruption: true,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RestoredOperation {
    pub(crate) operation_id: OperationId,
    pub(crate) thread_id: ThreadId,
    pub(crate) input_entry_id: ConversationEntryId,
    pub(crate) step_order: Vec<StepId>,
    pub(crate) steps: BTreeMap<StepId, ConversationEntryId>,
    pub(crate) invocation_order: Vec<ToolInvocationId>,
    pub(crate) intents: BTreeMap<ToolInvocationId, InvocationIntent>,
    pub(crate) results: BTreeMap<ToolInvocationId, InvocationResultRecord>,
    pub(crate) recovery_decisions: Vec<(ToolInvocationId, RecoveryDecision)>,
    pub(crate) suspensions: Vec<crate::operation::SuspensionReason>,
    pub(crate) round_budget_decisions: Vec<crate::native_runtime::RoundBudgetDecision>,
    pub(crate) finished: Option<OperationOutcome>,
}

pub(crate) fn reduce(records: &[RecordEnvelope]) -> Result<RestoredSession, ReductionError> {
    let first = records.first().ok_or(ReductionError::MissingCreation)?;
    let SessionRecord::SessionCreated {
        thread_id,
        workspace_root,
    } = &first.record
    else {
        return Err(ReductionError::MissingCreation);
    };
    if first.version != SESSION_RECORD_VERSION {
        return Err(ReductionError::UnsupportedVersion(first.version));
    }
    let mut state = RestoredSession {
        session_id: first.session_id,
        thread_id: *thread_id,
        workspace_root: workspace_root.clone(),
        branch: None,
        head: None,
        entries: BTreeMap::new(),
        operations: BTreeMap::new(),
        audits: Vec::new(),
        artifacts: BTreeMap::new(),
        contexts: BTreeMap::new(),
        views: BTreeMap::new(),
        named_context: BTreeMap::new(),
        operation_details: BTreeMap::new(),
        completion_evidence: Vec::new(),
        named_values: BTreeMap::new(),
        orchestration_plans: BTreeMap::new(),
        children: BTreeMap::new(),
        compactions: Vec::new(),
        archived_prefix: None,
    };
    let mut record_ids = HashSet::new();

    for (index, envelope) in records.iter().enumerate() {
        validate_envelope(&state, &record_ids, envelope, index)?;
        record_ids.insert(envelope.record_id);
        if index != 0 {
            apply_validated(&mut state, &envelope.record);
        }
    }

    state.validate_conversation_path()?;
    state.validate_branch_history()?;
    Ok(state)
}

pub(crate) fn validate_envelope(
    state: &RestoredSession,
    record_ids: &HashSet<RecordId>,
    envelope: &RecordEnvelope,
    index: usize,
) -> Result<(), ReductionError> {
    validate_envelope_with_compaction_proof(state, record_ids, envelope, index, None)
}

pub(crate) fn validate_envelope_with_compaction_proof(
    state: &RestoredSession,
    record_ids: &HashSet<RecordId>,
    envelope: &RecordEnvelope,
    index: usize,
    source_proof: Option<&CompactionSourceProof>,
) -> Result<(), ReductionError> {
    if envelope.version != SESSION_RECORD_VERSION {
        return Err(ReductionError::UnsupportedVersion(envelope.version));
    }
    if envelope.session_id != state.session_id {
        return Err(ReductionError::WrongSession { index });
    }
    if record_ids.contains(&envelope.record_id) {
        return Err(ReductionError::DuplicateRecord { index });
    }
    if index == 0 {
        return Ok(());
    }

    match &envelope.record {
        SessionRecord::SessionCreated { .. } => Err(ReductionError::SecondCreation { index }),
        SessionRecord::ConversationBranched { lineage } => {
            if index != 1
                || state.branch.is_some()
                || !state.entries.is_empty()
                || lineage.source_session_id == state.session_id
                || lineage.shared_entry_count == 0
            {
                Err(ReductionError::InvalidBranchLineage { index })
            } else {
                Ok(())
            }
        }
        SessionRecord::ConversationEntryAppended { entry } => {
            for block in &entry.message.content {
                if let crate::message::ContentBlock::ToolResult(result) = block
                    && let Some(artifact) = &result.artifact
                    && state.artifacts.get(&artifact.reference.id) != Some(artifact.as_ref())
                {
                    return Err(ReductionError::UnknownArtifact {
                        artifact: artifact.reference.id,
                    });
                }
            }
            if state.entries.contains_key(&entry.id) {
                Err(ReductionError::DuplicateEntry { entry: entry.id })
            } else if entry
                .parent
                .is_some_and(|parent| !state.entries.contains_key(&parent))
            {
                Err(ReductionError::UnknownParent { entry: entry.id })
            } else {
                Ok(())
            }
        }
        SessionRecord::ThreadHeadMoved { thread_id, head } => {
            if *thread_id != state.thread_id {
                Err(ReductionError::UnknownThread { thread: *thread_id })
            } else if head.is_some_and(|entry| !state.entries.contains_key(&entry)) {
                Err(ReductionError::UnknownHead { head: *head })
            } else {
                Ok(())
            }
        }
        SessionRecord::OperationStateChanged {
            operation_id,
            state: next,
        } => {
            let previous = state.operations.get(operation_id).copied();
            valid_operation_transition(previous, *next)
                .then_some(())
                .ok_or(ReductionError::InvalidOperationTransition {
                    operation: *operation_id,
                    previous,
                    next: *next,
                })
        }
        SessionRecord::PermissionAudited { .. } => Ok(()),
        SessionRecord::ArtifactRegistered { artifact } => {
            (!state.artifacts.contains_key(&artifact.reference.id))
                .then_some(())
                .ok_or(ReductionError::DuplicateArtifact {
                    artifact: artifact.reference.id,
                })
        }
        SessionRecord::ContextRegistered { context } => {
            let Some(artifact) = state.artifacts.get(&context.artifact.id) else {
                return Err(ReductionError::UnknownArtifact {
                    artifact: context.artifact.id,
                });
            };
            if artifact.reference.content_hash != context.content_hash
                || context.artifact.content_hash != context.content_hash
                || artifact.byte_len != context.logical_size
            {
                return Err(ReductionError::ContextArtifactMismatch {
                    context: context.id,
                });
            }
            let expected = state
                .contexts
                .range((context.id, 0)..=(context.id, u64::MAX))
                .next_back()
                .map_or(1, |((_, version), _)| version + 1);
            if context.version != expected {
                Err(ReductionError::NonMonotonicContext {
                    context: context.id,
                    expected,
                    actual: context.version,
                })
            } else if state.contexts.contains_key(&(context.id, context.version)) {
                Err(ReductionError::DuplicateContext {
                    context: context.id,
                    version: context.version,
                })
            } else {
                Ok(())
            }
        }
        SessionRecord::ContextViewRegistered { view } => {
            if !state
                .contexts
                .contains_key(&(view.source, view.source_version))
            {
                Err(ReductionError::UnknownContextVersion {
                    context: view.source,
                    version: view.source_version,
                })
            } else if state.views.contains_key(&view.id) {
                Err(ReductionError::DuplicateView { view: view.id })
            } else {
                Ok(())
            }
        }
        SessionRecord::NamedContextSet {
            name,
            context_id,
            version,
        } => {
            if name.trim().is_empty() {
                Err(ReductionError::InvalidContextName)
            } else if !state.contexts.contains_key(&(*context_id, *version)) {
                Err(ReductionError::UnknownContextVersion {
                    context: *context_id,
                    version: *version,
                })
            } else {
                Ok(())
            }
        }
        SessionRecord::OperationAccepted {
            operation_id,
            thread_id,
            input_entry_id,
        }
        | SessionRecord::FiniteOperationAccepted {
            operation_id,
            thread_id,
            input_entry_id,
            ..
        } => {
            if let SessionRecord::FiniteOperationAccepted { completion, .. } = &envelope.record
                && (completion.generation != *operation_id
                    || !crate::completion_evidence::valid_transition(None, completion)
                    || !completion_capacity(state, *operation_id))
            {
                return Err(ReductionError::InvalidCompletionEvidence {
                    operation: *operation_id,
                });
            }
            if *thread_id != state.thread_id {
                Err(ReductionError::UnknownThread { thread: *thread_id })
            } else if !state.entries.contains_key(input_entry_id) {
                Err(ReductionError::UnknownOperationInput {
                    entry: *input_entry_id,
                })
            } else if state.operations.contains_key(operation_id)
                || state.operation_details.contains_key(operation_id)
            {
                Err(ReductionError::DuplicateOperation {
                    operation: *operation_id,
                })
            } else {
                Ok(())
            }
        }
        SessionRecord::StepStarted {
            operation_id,
            step_id,
            assistant_entry_id,
        } => {
            if !state.entries.contains_key(assistant_entry_id) {
                return Err(ReductionError::UnknownStepEntry {
                    entry: *assistant_entry_id,
                });
            }
            let operation = state.operation_details.get(operation_id).ok_or(
                ReductionError::UnknownOperation {
                    operation: *operation_id,
                },
            )?;
            if operation.finished.is_some() || operation.steps.contains_key(step_id) {
                Err(ReductionError::DuplicateOrFinishedStep { step: *step_id })
            } else {
                Ok(())
            }
        }
        SessionRecord::InvocationIntentAppended { intent } => {
            if intent.permission.request.operation_id != intent.operation_id
                || intent.permission.request.invocation_id != intent.invocation_id
                || intent.permission.request.final_arguments != intent.final_arguments
            {
                return Err(ReductionError::IntentPermissionMismatch {
                    invocation: intent.invocation_id,
                });
            }
            if state.operation_details.values().any(|operation| {
                operation.intents.contains_key(&intent.invocation_id)
                    || operation
                        .intents
                        .values()
                        .any(|existing| existing.result_id == intent.result_id)
            }) {
                return Err(ReductionError::DuplicateInvocationIdentity {
                    invocation: intent.invocation_id,
                });
            }
            let operation = state.operation_details.get(&intent.operation_id).ok_or(
                ReductionError::UnknownOperation {
                    operation: intent.operation_id,
                },
            )?;
            if operation.finished.is_some() || !operation.steps.contains_key(&intent.step_id) {
                Err(ReductionError::UnknownStep {
                    step: intent.step_id,
                })
            } else {
                Ok(())
            }
        }
        SessionRecord::InvocationResultAppended { result } => {
            let operation = state.operation_details.get(&result.operation_id).ok_or(
                ReductionError::UnknownOperation {
                    operation: result.operation_id,
                },
            )?;
            let intent = operation.intents.get(&result.invocation_id).ok_or(
                ReductionError::UnknownInvocation {
                    invocation: result.invocation_id,
                },
            )?;
            if intent.result_id != result.result_id || operation.finished.is_some() {
                Err(ReductionError::ResultMismatch {
                    invocation: result.invocation_id,
                })
            } else if operation.results.contains_key(&result.invocation_id) {
                Err(ReductionError::DuplicateResult {
                    invocation: result.invocation_id,
                })
            } else {
                Ok(())
            }
        }
        SessionRecord::OperationSuspended { operation_id, .. } => {
            let operation = state.operation_details.get(operation_id).ok_or(
                ReductionError::UnknownOperation {
                    operation: *operation_id,
                },
            )?;
            if operation.finished.is_some() {
                return Err(ReductionError::OperationAlreadyFinished {
                    operation: *operation_id,
                });
            }
            let previous = state.operations.get(operation_id).copied();
            valid_operation_transition(previous, OperationState::Suspended)
                .then_some(())
                .ok_or(ReductionError::InvalidOperationTransition {
                    operation: *operation_id,
                    previous,
                    next: OperationState::Suspended,
                })
        }
        SessionRecord::RoundBudgetDecisionAppended { decision } => {
            let operation = state.operation_details.get(&decision.operation_id).ok_or(
                ReductionError::UnknownOperation {
                    operation: decision.operation_id,
                },
            )?;
            let suspension = operation.suspensions.iter().rev().find_map(|reason| {
                let crate::operation::SuspensionReason::RoundBudgetReached(suspension) = reason
                else {
                    return None;
                };
                Some(suspension)
            });
            if operation.finished.is_some()
                || state.operations.get(&decision.operation_id) != Some(&OperationState::Suspended)
                || suspension.is_none_or(|suspension| suspension.id != decision.suspension_id)
                || operation
                    .round_budget_decisions
                    .iter()
                    .any(|existing| existing.suspension_id == decision.suspension_id)
            {
                return Err(ReductionError::InvalidRoundBudgetDecision {
                    operation: decision.operation_id,
                });
            }
            let suspension = suspension.expect("checked suspension exists");
            if !suspension.allowed_actions.contains(&decision.action) {
                return Err(ReductionError::InvalidRoundBudgetDecision {
                    operation: decision.operation_id,
                });
            }
            if decision.action == crate::native_runtime::RoundBudgetAction::Stop
                && operation
                    .invocation_order
                    .iter()
                    .any(|id| !operation.results.contains_key(id))
            {
                return Err(ReductionError::PendingInvocationAtFinish {
                    operation: decision.operation_id,
                });
            }
            Ok(())
        }
        SessionRecord::OperationFinished {
            operation_id,
            outcome,
        } => {
            let operation = state.operation_details.get(operation_id).ok_or(
                ReductionError::UnknownOperation {
                    operation: *operation_id,
                },
            )?;
            if operation.finished.is_some() {
                return Err(ReductionError::OperationAlreadyFinished {
                    operation: *operation_id,
                });
            }
            if operation
                .invocation_order
                .iter()
                .any(|id| !operation.results.contains_key(id))
            {
                return Err(ReductionError::PendingInvocationAtFinish {
                    operation: *operation_id,
                });
            }
            let next = OperationState::Finished(*outcome);
            let previous = state.operations.get(operation_id).copied();
            valid_operation_transition(previous, next)
                .then_some(())
                .ok_or(ReductionError::InvalidOperationTransition {
                    operation: *operation_id,
                    previous,
                    next,
                })
        }
        SessionRecord::RecoveryDecisionAppended {
            operation_id,
            invocation_id,
            ..
        } => {
            let operation = state.operation_details.get(operation_id).ok_or(
                ReductionError::UnknownOperation {
                    operation: *operation_id,
                },
            )?;
            if !operation.intents.contains_key(invocation_id)
                || operation.results.contains_key(invocation_id)
                || operation.finished.is_some()
            {
                Err(ReductionError::InvalidRecoveryDecision {
                    invocation: *invocation_id,
                })
            } else {
                Ok(())
            }
        }
        SessionRecord::NamedValueSet { value } => {
            if value.name.trim().is_empty()
                || !state.operation_details.contains_key(&value.operation_id)
                || !durable_value_exists(state, &value.value)
            {
                Err(ReductionError::InvalidNamedValue { value: value.id })
            } else if state.named_values.contains_key(&value.id) {
                Err(ReductionError::DuplicateNamedValue { value: value.id })
            } else {
                Ok(())
            }
        }
        SessionRecord::ChildAdmitted { handle } => {
            validate_child_admission(state, handle, ChildLifecycle::Admitted)
        }
        SessionRecord::ChildrenBatchAdmitted { handles } => {
            if handles.is_empty() || handles.len() > 64 {
                return Err(ReductionError::InvalidChildBatch {
                    size: handles.len(),
                });
            }
            let mut batch_ids = HashSet::with_capacity(handles.len());
            let mut batch_operation_ids = HashSet::with_capacity(handles.len());
            for handle in handles {
                let agent_id = handle.admission.attribution.agent_id;
                if !batch_ids.insert(agent_id) {
                    return Err(ReductionError::DuplicateChild { agent: agent_id });
                }
                if !batch_operation_ids.insert(handle.admission.attribution.operation_id) {
                    return Err(ReductionError::InvalidChildAdmission { agent: agent_id });
                }
                validate_child_admission(state, handle, ChildLifecycle::Queued)?;
            }
            Ok(())
        }
        SessionRecord::OrchestrationPlanStarted { start } => {
            if !state.operation_details.contains_key(&start.operation_id)
                || state.orchestration_plans.contains_key(&start.plan_id)
                || start.fingerprint.len() != 64
                || !start
                    .fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || start.step_ids.is_empty()
                || start.step_ids.len() > crate::orchestration::MAX_PLAN_STEPS
                || start.step_ids.iter().any(|id| id.is_empty())
                || start.step_ids.iter().collect::<HashSet<_>>().len() != start.step_ids.len()
            {
                return Err(ReductionError::InvalidOrchestrationPlan {
                    plan: start.plan_id,
                });
            }
            Ok(())
        }
        SessionRecord::ChildLifecycleChanged {
            agent_id,
            lifecycle,
        } => {
            let child = state
                .children
                .get(agent_id)
                .ok_or(ReductionError::UnknownChild { agent: *agent_id })?;
            if lifecycle.is_terminal()
                || !valid_child_transition(child.handle.lifecycle, *lifecycle)
            {
                Err(ReductionError::InvalidChildTransition {
                    agent: *agent_id,
                    previous: child.handle.lifecycle,
                    next: *lifecycle,
                })
            } else {
                Ok(())
            }
        }
        SessionRecord::ChildReportCommitted { report } => {
            let agent_id = report.attribution.agent_id;
            let child = state
                .children
                .get(&agent_id)
                .ok_or(ReductionError::UnknownChild { agent: agent_id })?;
            if child.report.is_some() {
                return Err(ReductionError::DuplicateChildReport { agent: agent_id });
            }
            if report.version != CHILD_REPORT_VERSION
                || report.attribution != child.handle.admission.attribution
                || report.schema != child.handle.admission.result_schema
                || !valid_child_terminal_source(child.handle.lifecycle)
                || !valid_child_report(
                    state,
                    report,
                    child.handle.admission.limits.max_report_bytes,
                    child.handle.admission.limits.max_artifact_bytes,
                )
            {
                Err(ReductionError::InvalidChildReport { agent: agent_id })
            } else {
                Ok(())
            }
        }
        SessionRecord::ConversationCompacted { checkpoint } => {
            validate_compaction_checkpoint(state, checkpoint, source_proof)
        }
        SessionRecord::CompletionEvidenceRecorded { evidence } => {
            let known = state.operation_details.contains_key(&evidence.generation)
                || state.children.values().any(|child| {
                    child.handle.admission.attribution.operation_id == evidence.generation
                });
            let previous = state
                .completion_evidence
                .iter()
                .find(|previous| previous.generation == evidence.generation);
            if !known
                || !crate::completion_evidence::valid_transition(previous, evidence)
                || !completion_capacity(state, evidence.generation)
            {
                return Err(ReductionError::InvalidCompletionEvidence {
                    operation: evidence.generation,
                });
            }
            Ok(())
        }
    }
}

pub(crate) fn apply_validated(state: &mut RestoredSession, record: &SessionRecord) {
    match record {
        SessionRecord::SessionCreated { .. } => unreachable!("creation is never appended"),
        SessionRecord::ConversationBranched { lineage } => {
            state.branch = Some(lineage.clone());
        }
        SessionRecord::ConversationEntryAppended { entry } => {
            state.entries.insert(entry.id, entry.clone());
        }
        SessionRecord::ThreadHeadMoved { head, .. } => state.head = *head,
        SessionRecord::OperationStateChanged {
            operation_id,
            state: next,
        } => {
            state.operations.insert(*operation_id, *next);
        }
        SessionRecord::PermissionAudited { fact } => state.audits.push(fact.clone()),
        SessionRecord::CompletionEvidenceRecorded { evidence } => {
            retain_completion(state, evidence);
        }
        SessionRecord::ArtifactRegistered { artifact } => {
            state
                .artifacts
                .insert(artifact.reference.id, artifact.clone());
        }
        SessionRecord::ContextRegistered { context } => {
            state
                .contexts
                .insert((context.id, context.version), context.clone());
        }
        SessionRecord::ContextViewRegistered { view } => {
            state.views.insert(view.id, view.clone());
        }
        SessionRecord::NamedContextSet {
            name,
            context_id,
            version,
        } => {
            state
                .named_context
                .insert(name.clone(), (*context_id, *version));
        }
        SessionRecord::OperationAccepted {
            operation_id,
            thread_id,
            input_entry_id,
        }
        | SessionRecord::FiniteOperationAccepted {
            operation_id,
            thread_id,
            input_entry_id,
            ..
        } => {
            state.operation_details.insert(
                *operation_id,
                RestoredOperation {
                    operation_id: *operation_id,
                    thread_id: *thread_id,
                    input_entry_id: *input_entry_id,
                    step_order: Vec::new(),
                    steps: BTreeMap::new(),
                    invocation_order: Vec::new(),
                    intents: BTreeMap::new(),
                    results: BTreeMap::new(),
                    recovery_decisions: Vec::new(),
                    suspensions: Vec::new(),
                    round_budget_decisions: Vec::new(),
                    finished: None,
                },
            );
            state
                .operations
                .insert(*operation_id, OperationState::Running);
            if let SessionRecord::FiniteOperationAccepted { completion, .. } = record {
                retain_completion(state, completion);
            }
        }
        SessionRecord::StepStarted {
            operation_id,
            step_id,
            assistant_entry_id,
        } => {
            let operation = state
                .operation_details
                .get_mut(operation_id)
                .expect("validated operation exists");
            operation.steps.insert(*step_id, *assistant_entry_id);
            operation.step_order.push(*step_id);
        }
        SessionRecord::InvocationIntentAppended { intent } => {
            let operation = state
                .operation_details
                .get_mut(&intent.operation_id)
                .expect("validated operation exists");
            operation.invocation_order.push(intent.invocation_id);
            operation
                .intents
                .insert(intent.invocation_id, intent.clone());
        }
        SessionRecord::InvocationResultAppended { result } => {
            state
                .operation_details
                .get_mut(&result.operation_id)
                .expect("validated operation exists")
                .results
                .insert(result.invocation_id, result.clone());
        }
        SessionRecord::OperationSuspended {
            operation_id,
            reason,
        } => {
            state
                .operation_details
                .get_mut(operation_id)
                .expect("validated operation exists")
                .suspensions
                .push(reason.clone());
            state
                .operations
                .insert(*operation_id, OperationState::Suspended);
        }
        SessionRecord::RoundBudgetDecisionAppended { decision } => {
            let operation = state
                .operation_details
                .get_mut(&decision.operation_id)
                .expect("validated operation exists");
            operation.round_budget_decisions.push(decision.clone());
            match decision.action {
                crate::native_runtime::RoundBudgetAction::Continue => {
                    state
                        .operations
                        .insert(decision.operation_id, OperationState::Running);
                }
                crate::native_runtime::RoundBudgetAction::Stop => {
                    operation.finished = Some(OperationOutcome::Declined);
                    state.operations.insert(
                        decision.operation_id,
                        OperationState::Finished(OperationOutcome::Declined),
                    );
                }
            }
        }
        SessionRecord::OperationFinished {
            operation_id,
            outcome,
        } => {
            state
                .operation_details
                .get_mut(operation_id)
                .expect("validated operation exists")
                .finished = Some(*outcome);
            state
                .operations
                .insert(*operation_id, OperationState::Finished(*outcome));
        }
        SessionRecord::RecoveryDecisionAppended {
            operation_id,
            invocation_id,
            decision,
        } => {
            state
                .operation_details
                .get_mut(operation_id)
                .expect("validated operation exists")
                .recovery_decisions
                .push((*invocation_id, decision.clone()));
        }
        SessionRecord::NamedValueSet { value } => {
            state.named_values.insert(value.id, value.clone());
        }
        SessionRecord::OrchestrationPlanStarted { start } => {
            state
                .orchestration_plans
                .insert(start.plan_id, start.clone());
        }
        SessionRecord::ChildAdmitted { handle } => {
            state.children.insert(
                handle.admission.attribution.agent_id,
                RestoredChild {
                    handle: handle.clone(),
                    report: None,
                },
            );
        }
        SessionRecord::ChildrenBatchAdmitted { handles } => {
            for handle in handles {
                state.children.insert(
                    handle.admission.attribution.agent_id,
                    RestoredChild {
                        handle: handle.clone(),
                        report: None,
                    },
                );
            }
        }
        SessionRecord::ChildLifecycleChanged {
            agent_id,
            lifecycle,
        } => {
            state
                .children
                .get_mut(agent_id)
                .expect("validated child exists")
                .handle
                .apply_lifecycle(*lifecycle);
        }
        SessionRecord::ChildReportCommitted { report } => {
            let child = state
                .children
                .get_mut(&report.attribution.agent_id)
                .expect("validated child exists");
            child.handle.apply_report(report);
            child.report = Some(report.clone());
        }
        SessionRecord::ConversationCompacted { checkpoint } => {
            state.compactions.push(checkpoint.clone());
        }
    }
}

fn completion_inflight(state: &RestoredSession, generation: OperationId) -> bool {
    state
        .operation_details
        .get(&generation)
        .is_some_and(|operation| operation.finished.is_none())
        || state.children.values().any(|child| {
            child.handle.admission.attribution.operation_id == generation && child.report.is_none()
        })
}

fn completion_capacity(state: &RestoredSession, generation: OperationId) -> bool {
    state.completion_evidence.len() < 128
        || state.completion_evidence.iter().any(|item| {
            item.generation == generation || !completion_inflight(state, item.generation)
        })
}

fn retain_completion(
    state: &mut RestoredSession,
    evidence: &crate::completion_evidence::CompletionEvidence,
) {
    state
        .completion_evidence
        .retain(|previous| previous.generation != evidence.generation);
    if state.completion_evidence.len() >= 128 {
        let index = state
            .completion_evidence
            .iter()
            .position(|item| !completion_inflight(state, item.generation))
            .expect("validated completion capacity");
        state.completion_evidence.remove(index);
    }
    state.completion_evidence.push(evidence.clone());
}

fn validate_compaction_checkpoint(
    state: &RestoredSession,
    checkpoint: &CompactionCheckpoint,
    source_proof: Option<&CompactionSourceProof>,
) -> Result<(), ReductionError> {
    let previous = state
        .active_compaction()
        .map_err(|_| ReductionError::InvalidCompaction {
            compaction: checkpoint.id,
        })?;
    if checkpoint.version != COMPACTION_CHECKPOINT_VERSION
        || checkpoint
            .semantic
            .as_ref()
            .is_some_and(|provenance| !provenance.valid_for(&checkpoint.summary))
        || checkpoint.source_entry_count == 0
        || checkpoint.source_digest.len() != 64
        || !checkpoint
            .source_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !checkpoint.budget.is_valid_checkpoint_plan()
        || checkpoint.previous_checkpoint != previous.map(|checkpoint| checkpoint.id)
        || previous
            .is_some_and(|previous| checkpoint.source_entry_count <= previous.source_entry_count)
        || state.compactions.iter().any(|existing| {
            existing.id == checkpoint.id || existing.operation_id == checkpoint.operation_id
        })
    {
        return Err(ReductionError::InvalidCompaction {
            compaction: checkpoint.id,
        });
    }
    let path = state
        .conversation_entry_path()
        .map_err(|_| ReductionError::InvalidCompaction {
            compaction: checkpoint.id,
        })?;
    let offset = state.retained_offset();
    let Some(retained_count) = checkpoint.source_entry_count.checked_sub(offset) else {
        return Err(ReductionError::InvalidCompaction {
            compaction: checkpoint.id,
        });
    };
    if retained_count == 0 || retained_count >= path.len() {
        return Err(ReductionError::InvalidCompaction {
            compaction: checkpoint.id,
        });
    }
    let source = &path[..retained_count];
    let valid = if offset == 0 {
        source
            .first()
            .is_some_and(|entry| entry.id == checkpoint.source_start)
            && source
                .last()
                .is_some_and(|entry| entry.id == checkpoint.source_end)
            && path[retained_count].id == checkpoint.retained_tail_start
            && source_digest(source.iter().map(|entry| (entry.id, &entry.message)))
                == checkpoint.source_digest
    } else {
        source_proof.is_some_and(|proof| proof.matches(state.session_id, checkpoint))
            && state
                .archived_prefix
                .as_ref()
                .is_some_and(|prefix| prefix.source_start == checkpoint.source_start)
            && source
                .last()
                .is_some_and(|entry| entry.id == checkpoint.source_end)
            && path[retained_count].id == checkpoint.retained_tail_start
    } && validate_summary(&checkpoint.summary, checkpoint.budget.summary_max_bytes);
    valid
        .then_some(())
        .ok_or(ReductionError::InvalidCompaction {
            compaction: checkpoint.id,
        })
}

fn valid_child_transition(previous: ChildLifecycle, next: ChildLifecycle) -> bool {
    matches!(
        (previous, next),
        (ChildLifecycle::Admitted, ChildLifecycle::Queued)
            | (ChildLifecycle::Queued, ChildLifecycle::Running)
            | (ChildLifecycle::Running, ChildLifecycle::Suspended)
            | (ChildLifecycle::Suspended, ChildLifecycle::Running)
    )
}

fn validate_child_admission(
    state: &RestoredSession,
    handle: &AgentHandleSnapshot,
    required_lifecycle: ChildLifecycle,
) -> Result<(), ReductionError> {
    let attribution = &handle.admission.attribution;
    let parent_operation_is_valid = state
        .operation_details
        .get(&attribution.parent_operation_id)
        .is_some_and(|operation| {
            operation.thread_id == attribution.thread_id && operation.finished.is_none()
        });
    let child_operation_is_unique = attribution.operation_id != attribution.parent_operation_id
        && !state
            .operation_details
            .contains_key(&attribution.operation_id)
        && state.children.values().all(|child| {
            child.handle.admission.attribution.operation_id != attribution.operation_id
        });
    if handle.lifecycle != required_lifecycle
        || handle.report.is_some()
        || handle.usage != crate::orchestration::ChildUsage::Unknown
        || attribution.agent_id == attribution.parent_agent_id
        || attribution.parent_agent_id != AgentId::for_session(state.session_id)
        || attribution.thread_id != state.thread_id
        || !parent_operation_is_valid
        || !child_operation_is_unique
        || attribution.route.trim().is_empty()
        || attribution.profile.trim().is_empty()
        || attribution.connection.trim().is_empty()
        || attribution.model.trim().is_empty()
        || handle.admission.task_hash.len() != 64
        || handle.admission.task_preview.len() > crate::orchestration::MAX_CHILD_TASK_PREVIEW_BYTES
        || !handle.admission.plan.as_ref().is_none_or(|plan| {
            state
                .orchestration_plans
                .get(&plan.plan_id)
                .is_some_and(|start| {
                    start.operation_id == attribution.parent_operation_id
                        && start.step_ids.contains(&plan.step_id)
                })
        })
    {
        Err(ReductionError::InvalidChildAdmission {
            agent: attribution.agent_id,
        })
    } else if state.children.contains_key(&attribution.agent_id) {
        Err(ReductionError::DuplicateChild {
            agent: attribution.agent_id,
        })
    } else {
        Ok(())
    }
}

fn valid_child_terminal_source(previous: ChildLifecycle) -> bool {
    matches!(
        previous,
        ChildLifecycle::Admitted
            | ChildLifecycle::Queued
            | ChildLifecycle::Running
            | ChildLifecycle::Suspended
    )
}

fn valid_child_report(
    state: &RestoredSession,
    report: &ChildReport,
    max_inline_bytes: usize,
    max_artifact_bytes: usize,
) -> bool {
    if let Some(evidence) = &report.evidence
        && (evidence.generation != report.attribution.operation_id
            || evidence.validate().is_err()
            || !state
                .completion_evidence
                .iter()
                .any(|saved| saved == evidence))
    {
        return false;
    }
    let rejected_completion = report.status == ChildTerminalStatus::Failed
        && report.output.is_some()
        && report
            .error
            .as_ref()
            .is_some_and(|error| !error.is_empty() && error.len() <= max_inline_bytes)
        && report.evidence.as_ref().is_some_and(|evidence| {
            evidence.claim == crate::completion_evidence::CompletionClaim::Completed
                && !evidence.supported()
        });
    let content = match report.status {
        ChildTerminalStatus::Failed if rejected_completion => report.output.as_deref(),
        ChildTerminalStatus::Completed if report.error.is_none() => report.output.as_deref(),
        ChildTerminalStatus::Failed
        | ChildTerminalStatus::Cancelled
        | ChildTerminalStatus::Interrupted
            if report.output.is_none() =>
        {
            report.error.as_deref()
        }
        _ => None,
    };
    let Some(content) = content else {
        return false;
    };
    match &report.reference {
        ChildReportReference::Inline { byte_len } => {
            *byte_len == content.len()
                && *byte_len <= max_inline_bytes
                && valid_result_schema(report, content)
        }
        ChildReportReference::Artifact {
            artifact,
            byte_len,
            preview_byte_len,
        } => {
            (report.status == ChildTerminalStatus::Completed || rejected_completion)
                && *preview_byte_len == content.len()
                && *preview_byte_len <= max_inline_bytes
                && usize::try_from(*byte_len)
                    .is_ok_and(|size| size > max_inline_bytes && size <= max_artifact_bytes)
                && state.artifacts.get(&artifact.id).is_some_and(|record| {
                    record.reference == *artifact && record.byte_len == *byte_len
                })
        }
    }
}

fn valid_result_schema(report: &ChildReport, content: &str) -> bool {
    if report.status != ChildTerminalStatus::Completed {
        return true;
    }
    match report.schema {
        crate::orchestration::ChildResultSchema::Summary => true,
        crate::orchestration::ChildResultSchema::Json => {
            serde_json::from_str::<serde_json::Value>(content)
                .ok()
                .and_then(|value| serde_json::to_string(&value).ok())
                .is_some_and(|canonical| canonical == content)
        }
    }
}

fn durable_value_exists(state: &RestoredSession, value: &DurableValueRef) -> bool {
    match value {
        DurableValueRef::InlineJson(value) => serde_json::to_vec(value)
            .is_ok_and(|bytes| bytes.len() <= crate::operation::MAX_INLINE_VALUE_BYTES),
        DurableValueRef::Artifact(reference) => state
            .artifacts
            .get(&reference.id)
            .is_some_and(|artifact| artifact.reference == *reference),
        DurableValueRef::Context { id, version } => state.contexts.contains_key(&(*id, *version)),
    }
}

impl RestoredSession {
    pub(crate) fn conversation_entry_path(
        &self,
    ) -> Result<Vec<&ConversationEntry>, ReductionError> {
        let mut path = Vec::new();
        let mut cursor = self.head;
        let mut seen = HashSet::new();
        while let Some(id) = cursor {
            if self
                .archived_prefix
                .as_ref()
                .is_some_and(|prefix| prefix.source_end == id)
            {
                break;
            }
            if !seen.insert(id) {
                return Err(ReductionError::CyclicConversation { entry: id });
            }
            let entry = self
                .entries
                .get(&id)
                .ok_or(ReductionError::UnknownHead { head: Some(id) })?;
            path.push(entry);
            cursor = entry.parent;
        }
        path.reverse();
        Ok(path)
    }

    pub(crate) fn conversation_path(&self) -> Result<Vec<Message>, ReductionError> {
        self.conversation_entry_path().map(|path| {
            path.into_iter()
                .map(|entry| entry.message.clone())
                .collect()
        })
    }

    pub(crate) fn active_compaction(
        &self,
    ) -> Result<Option<&CompactionCheckpoint>, ReductionError> {
        let path = self.conversation_entry_path()?;
        let offset = self.retained_offset();
        Ok(self.compactions.iter().rev().find(|checkpoint| {
            let Some(index) = checkpoint.source_entry_count.checked_sub(offset) else {
                return false;
            };
            index < path.len()
                && (if offset == 0 {
                    path.first()
                        .is_some_and(|entry| entry.id == checkpoint.source_start)
                } else {
                    self.archived_prefix
                        .as_ref()
                        .is_some_and(|prefix| prefix.source_start == checkpoint.source_start)
                })
                && (if index == 0 {
                    self.archived_prefix
                        .as_ref()
                        .is_some_and(|prefix| prefix.source_end == checkpoint.source_end)
                } else {
                    path.get(index - 1)
                        .is_some_and(|entry| entry.id == checkpoint.source_end)
                })
                && path
                    .get(index)
                    .is_some_and(|entry| entry.id == checkpoint.retained_tail_start)
        }))
    }

    pub(crate) fn retained_offset(&self) -> usize {
        let Some(prefix) = &self.archived_prefix else {
            return 0;
        };
        self.conversation_entry_path()
            .ok()
            .and_then(|path| path.first().map(|entry| entry.parent))
            .filter(|parent| *parent == Some(prefix.source_end))
            .map_or(0, |_| prefix.source_entry_count)
    }

    fn validate_conversation_path(&self) -> Result<(), ReductionError> {
        self.conversation_path().map(|_| ())
    }

    fn validate_branch_history(&self) -> Result<(), ReductionError> {
        let Some(branch) = &self.branch else {
            return Ok(());
        };
        let path = self.conversation_entry_path()?;
        let offset = self.retained_offset();
        if branch.shared_entry_count <= offset {
            return Ok(());
        }
        let shared = branch.shared_entry_count - offset;
        if path.len() < shared
            || path
                .get(shared - 1)
                .is_none_or(|entry| entry.id != branch.source_entry_id)
        {
            return Err(ReductionError::InvalidBranchHistory);
        }
        Ok(())
    }

    pub(crate) fn unfinished_operations(&self) -> Vec<(OperationId, OperationState)> {
        self.operations
            .iter()
            .filter_map(|(id, state)| {
                (!matches!(state, OperationState::Finished(_))).then_some((*id, *state))
            })
            .collect()
    }
}

fn valid_operation_transition(previous: Option<OperationState>, next: OperationState) -> bool {
    matches!(
        (previous, next),
        (None, OperationState::Running)
            | (Some(OperationState::Running), OperationState::Suspended)
            | (Some(OperationState::Running), OperationState::Finished(_))
            | (Some(OperationState::Suspended), OperationState::Running)
            | (Some(OperationState::Suspended), OperationState::Finished(_))
    )
}

#[derive(Debug, PartialEq)]
pub(crate) enum ReductionError {
    InvalidCompletionEvidence {
        operation: OperationId,
    },
    MissingCreation,
    UnsupportedVersion(u32),
    WrongSession {
        index: usize,
    },
    DuplicateRecord {
        index: usize,
    },
    SecondCreation {
        index: usize,
    },
    InvalidBranchLineage {
        index: usize,
    },
    InvalidBranchHistory,
    DuplicateEntry {
        entry: ConversationEntryId,
    },
    UnknownParent {
        entry: ConversationEntryId,
    },
    UnknownThread {
        thread: ThreadId,
    },
    UnknownHead {
        head: Option<ConversationEntryId>,
    },
    CyclicConversation {
        entry: ConversationEntryId,
    },
    InvalidOperationTransition {
        operation: OperationId,
        previous: Option<OperationState>,
        next: OperationState,
    },
    DuplicateArtifact {
        artifact: ArtifactId,
    },
    UnknownArtifact {
        artifact: ArtifactId,
    },
    ContextArtifactMismatch {
        context: ContextId,
    },
    DuplicateContext {
        context: ContextId,
        version: u64,
    },
    NonMonotonicContext {
        context: ContextId,
        expected: u64,
        actual: u64,
    },
    UnknownContextVersion {
        context: ContextId,
        version: u64,
    },
    DuplicateView {
        view: ContextViewId,
    },
    InvalidContextName,
    UnknownOperationInput {
        entry: ConversationEntryId,
    },
    DuplicateOperation {
        operation: OperationId,
    },
    UnknownOperation {
        operation: OperationId,
    },
    UnknownStepEntry {
        entry: ConversationEntryId,
    },
    DuplicateOrFinishedStep {
        step: StepId,
    },
    UnknownStep {
        step: StepId,
    },
    IntentPermissionMismatch {
        invocation: ToolInvocationId,
    },
    DuplicateInvocationIdentity {
        invocation: ToolInvocationId,
    },
    UnknownInvocation {
        invocation: ToolInvocationId,
    },
    ResultMismatch {
        invocation: ToolInvocationId,
    },
    DuplicateResult {
        invocation: ToolInvocationId,
    },
    OperationAlreadyFinished {
        operation: OperationId,
    },
    PendingInvocationAtFinish {
        operation: OperationId,
    },
    InvalidRecoveryDecision {
        invocation: ToolInvocationId,
    },
    InvalidRoundBudgetDecision {
        operation: OperationId,
    },
    InvalidNamedValue {
        value: NamedValueId,
    },
    DuplicateNamedValue {
        value: NamedValueId,
    },
    InvalidOrchestrationPlan {
        plan: OrchestrationPlanId,
    },
    InvalidChildAdmission {
        agent: AgentId,
    },
    InvalidChildBatch {
        size: usize,
    },
    DuplicateChild {
        agent: AgentId,
    },
    UnknownChild {
        agent: AgentId,
    },
    InvalidChildTransition {
        agent: AgentId,
        previous: ChildLifecycle,
        next: ChildLifecycle,
    },
    DuplicateChildReport {
        agent: AgentId,
    },
    InvalidChildReport {
        agent: AgentId,
    },
    InvalidCompaction {
        compaction: CompactionId,
    },
}

impl fmt::Display for ReductionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid durable session record sequence: {self:?}"
        )
    }
}

impl Error for ReductionError {}
