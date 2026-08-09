use super::record::{ConversationEntry, RecordEnvelope, SESSION_RECORD_VERSION, SessionRecord};
use crate::{
    artifact::ArtifactRecord,
    context::persisted::{ContextRecord, ContextViewRecord},
    identity::*,
    message::Message,
    operation::{
        DurableValueRef, InvocationIntent, InvocationResultRecord, NamedValueRecord,
        RecoveryDecision,
    },
    orchestration::{
        AgentHandleSnapshot, CHILD_REPORT_VERSION, ChildInspection, ChildLifecycle, ChildReport,
        ChildReportReference, ChildTerminalStatus,
    },
    permission::PermissionAuditFact,
    runtime::{OperationOutcome, OperationState},
};
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt,
    path::PathBuf,
};

#[derive(Debug)]
pub(crate) struct RestoredSession {
    pub(crate) session_id: SessionId,
    pub(crate) thread_id: ThreadId,
    pub(crate) workspace_root: PathBuf,
    pub(crate) head: Option<ConversationEntryId>,
    pub(crate) entries: BTreeMap<ConversationEntryId, ConversationEntry>,
    pub(crate) operations: BTreeMap<OperationId, OperationState>,
    pub(crate) audits: Vec<PermissionAuditFact>,
    pub(crate) artifacts: BTreeMap<ArtifactId, ArtifactRecord>,
    pub(crate) contexts: BTreeMap<(ContextId, u64), ContextRecord>,
    pub(crate) views: BTreeMap<ContextViewId, ContextViewRecord>,
    pub(crate) named_context: BTreeMap<String, (ContextId, u64)>,
    pub(crate) operation_details: BTreeMap<OperationId, RestoredOperation>,
    pub(crate) named_values: BTreeMap<NamedValueId, NamedValueRecord>,
    pub(crate) children: BTreeMap<AgentId, RestoredChild>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
        let report = ChildReport::interrupted(
            handle.admission.attribution.clone(),
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

#[derive(Debug, Clone)]
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
        head: None,
        entries: BTreeMap::new(),
        operations: BTreeMap::new(),
        audits: Vec::new(),
        artifacts: BTreeMap::new(),
        contexts: BTreeMap::new(),
        views: BTreeMap::new(),
        named_context: BTreeMap::new(),
        operation_details: BTreeMap::new(),
        named_values: BTreeMap::new(),
        children: BTreeMap::new(),
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
    Ok(state)
}

pub(crate) fn validate_envelope(
    state: &RestoredSession,
    record_ids: &HashSet<RecordId>,
    envelope: &RecordEnvelope,
    index: usize,
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
        SessionRecord::ConversationEntryAppended { entry } => {
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
        } => {
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
            let attribution = &handle.admission.attribution;
            if handle.lifecycle != ChildLifecycle::Admitted
                || handle.report.is_some()
                || handle.usage != crate::orchestration::ChildUsage::Unknown
                || attribution.agent_id == attribution.parent_agent_id
                || attribution.parent_agent_id != AgentId::for_session(state.session_id)
                || attribution.route.trim().is_empty()
                || attribution.profile.trim().is_empty()
                || attribution.connection.trim().is_empty()
                || attribution.model.trim().is_empty()
                || handle.admission.task_hash.len() != 64
                || handle.admission.task_preview.len()
                    > crate::orchestration::MAX_CHILD_TASK_PREVIEW_BYTES
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
                || !valid_child_terminal_source(child.handle.lifecycle)
                || !valid_child_report(report, child.handle.admission.limits.max_report_bytes)
            {
                Err(ReductionError::InvalidChildReport { agent: agent_id })
            } else {
                Ok(())
            }
        }
    }
}

pub(crate) fn apply_validated(state: &mut RestoredSession, record: &SessionRecord) {
    match record {
        SessionRecord::SessionCreated { .. } => unreachable!("creation is never appended"),
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
                    finished: None,
                },
            );
            state
                .operations
                .insert(*operation_id, OperationState::Running);
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
        SessionRecord::OperationSuspended { operation_id, .. } => {
            state
                .operations
                .insert(*operation_id, OperationState::Suspended);
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
        SessionRecord::ChildAdmitted { handle } => {
            state.children.insert(
                handle.admission.attribution.agent_id,
                RestoredChild {
                    handle: handle.clone(),
                    report: None,
                },
            );
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
    }
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

fn valid_child_terminal_source(previous: ChildLifecycle) -> bool {
    matches!(
        previous,
        ChildLifecycle::Admitted
            | ChildLifecycle::Queued
            | ChildLifecycle::Running
            | ChildLifecycle::Suspended
    )
}

fn valid_child_report(report: &ChildReport, max_bytes: usize) -> bool {
    let content = match report.status {
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
    match report.reference {
        ChildReportReference::Inline { byte_len } => {
            byte_len == content.len() && byte_len <= max_bytes
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
    pub(crate) fn conversation_path(&self) -> Result<Vec<Message>, ReductionError> {
        let mut path = Vec::new();
        let mut cursor = self.head;
        let mut seen = HashSet::new();
        while let Some(id) = cursor {
            if !seen.insert(id) {
                return Err(ReductionError::CyclicConversation { entry: id });
            }
            let entry = self
                .entries
                .get(&id)
                .ok_or(ReductionError::UnknownHead { head: Some(id) })?;
            path.push(entry.message.clone());
            cursor = entry.parent;
        }
        path.reverse();
        Ok(path)
    }

    fn validate_conversation_path(&self) -> Result<(), ReductionError> {
        self.conversation_path().map(|_| ())
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
    InvalidNamedValue {
        value: NamedValueId,
    },
    DuplicateNamedValue {
        value: NamedValueId,
    },
    InvalidChildAdmission {
        agent: AgentId,
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
