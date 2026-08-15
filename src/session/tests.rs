use super::*;
use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ContentHash},
    config::{OrchestrationLimits, PermissionMode},
    context::persisted::{
        ContextKind, ContextRecord, ContextViewRecord, MaterializationBudget, Provenance,
        TrustClass, ViewSelector,
    },
    identity::*,
    message::{Message, Role},
    native_runtime::{OperationOutcome, OperationState},
    operation::{
        DurableValueRef, InvocationIntent, InvocationOutcome, InvocationResultRecord,
        InvocationTarget, NamedValueRecord, RecoveryDecision, SuspensionReason,
    },
    orchestration::{
        AgentHandleSnapshot, ChildAdmission, ChildAttribution, ChildLifecycle, ChildReport,
        ChildReportReference, ChildTerminalStatus, ChildUsage, ExecutionOwner,
        OrchestrationPlanStart, PlanChildAttribution,
    },
    permission::{PermissionAuditFact, PermissionRequest, PermissionScope, PolicyDecision},
    tool::{EffectClass, ReplaySafety},
};
use std::{fs, io::Write, path::PathBuf};
use tempfile::tempdir;

fn created(session_id: SessionId, thread_id: ThreadId) -> RecordEnvelope {
    RecordEnvelope::new(
        session_id,
        SessionRecord::SessionCreated {
            thread_id,
            workspace_root: PathBuf::from("/workspace"),
        },
    )
}

fn audit(operation_id: OperationId) -> PermissionAuditFact {
    PermissionAuditFact {
        request: PermissionRequest {
            operation_id,
            invocation_id: ToolInvocationId::new(),
            tool_name: "read_file".to_owned(),
            effect_class: EffectClass::Read,
            final_arguments: serde_json::json!({"path": "README.md"}),
            scope: PermissionScope::Unscoped,
            outbound_review: None,
        },
        policy_evaluation: PolicyDecision::Allow,
        controller_decision: None,
        effective: PolicyDecision::Allow,
    }
}

fn child_handle(
    session_id: SessionId,
    thread_id: ThreadId,
    parent_operation_id: OperationId,
) -> AgentHandleSnapshot {
    AgentHandleSnapshot::admitted(ChildAdmission {
        attribution: ChildAttribution {
            agent_id: AgentId::new(),
            parent_agent_id: AgentId::for_session(session_id),
            operation_id: OperationId::new(),
            parent_operation_id,
            thread_id,
            route: "worker".to_owned(),
            profile: "worker".to_owned(),
            owner: ExecutionOwner::Native,
            connection: "local".to_owned(),
            model: "small".to_owned(),
        },
        plan: None,
        task_preview: "bounded task".to_owned(),
        task_hash: blake3::hash(b"bounded task").to_hex().to_string(),
        result_schema: crate::orchestration::ChildResultSchema::Summary,
        capabilities: Vec::new(),
        permission_mode: PermissionMode::Deny,
        max_tool_rounds: 1,
        limits: OrchestrationLimits::default(),
        hard_token_limit: None,
        hard_spend_microusd: None,
    })
}

fn child_operation_prefix(
    session_id: SessionId,
    thread_id: ThreadId,
    operation_id: OperationId,
) -> Vec<RecordEnvelope> {
    let input = ConversationEntry {
        id: ConversationEntryId::new(),
        parent: None,
        agent_id: AgentId::for_session(session_id),
        message: Message::text(Role::User, "delegate bounded child"),
    };
    vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: input.clone(),
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationAccepted {
                operation_id,
                thread_id,
                input_entry_id: input.id,
            },
        ),
    ]
}

#[test]
fn child_lifecycle_and_report_reduce_in_legal_durable_order() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let handle = child_handle(session_id, thread_id, operation_id);
    let agent_id = handle.admission.attribution.agent_id;
    let report = ChildReport {
        version: crate::orchestration::CHILD_REPORT_VERSION,
        attribution: handle.admission.attribution.clone(),
        status: ChildTerminalStatus::Completed,
        schema: crate::orchestration::ChildResultSchema::Summary,
        output: Some("done".to_owned()),
        error: None,
        usage: ChildUsage::Unknown,
        reference: ChildReportReference::Inline { byte_len: 4 },
    };
    let mut records = child_operation_prefix(session_id, thread_id, operation_id);
    records.extend([
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildAdmitted {
                handle: handle.clone(),
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildLifecycleChanged {
                agent_id,
                lifecycle: ChildLifecycle::Queued,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildLifecycleChanged {
                agent_id,
                lifecycle: ChildLifecycle::Running,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildReportCommitted {
                report: report.clone(),
            },
        ),
    ]);

    let restored = reduce(&records).expect("reduce child records");
    let child = restored.children.get(&agent_id).expect("restored child");
    assert_eq!(child.handle.lifecycle, ChildLifecycle::Completed);
    assert_eq!(child.report.as_ref(), Some(&report));

    for record in records.into_iter().skip(1) {
        let encoded = serde_json::to_vec(&record).expect("encode child record");
        let decoded: RecordEnvelope =
            serde_json::from_slice(&encoded).expect("decode child record");
        assert_eq!(decoded, record);
    }
}

#[test]
fn artifact_backed_child_report_requires_a_matching_prior_registration() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let handle = child_handle(session_id, thread_id, operation_id);
    let agent_id = handle.admission.attribution.agent_id;
    let byte_len = handle.admission.limits.max_report_bytes as u64 + 1;
    let artifact = ArtifactRecord {
        reference: ArtifactRef {
            id: ArtifactId::new(),
            content_hash: ContentHash::for_bytes(b"fixture identity only"),
        },
        media_type: "text/plain; charset=utf-8".to_owned(),
        byte_len,
        owner: PrincipalId::new(),
    };
    let report = ChildReport {
        version: crate::orchestration::CHILD_REPORT_VERSION,
        attribution: handle.admission.attribution.clone(),
        status: ChildTerminalStatus::Completed,
        schema: crate::orchestration::ChildResultSchema::Summary,
        output: Some("preview".to_owned()),
        error: None,
        usage: ChildUsage::Unknown,
        reference: ChildReportReference::Artifact {
            artifact: artifact.reference.clone(),
            byte_len,
            preview_byte_len: "preview".len(),
        },
    };
    let mut base = child_operation_prefix(session_id, thread_id, operation_id);
    base.extend([
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildAdmitted {
                handle: handle.clone(),
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildLifecycleChanged {
                agent_id,
                lifecycle: ChildLifecycle::Queued,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildLifecycleChanged {
                agent_id,
                lifecycle: ChildLifecycle::Running,
            },
        ),
    ]);
    let mut missing = base.clone();
    missing.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildReportCommitted {
            report: report.clone(),
        },
    ));
    assert!(matches!(
        reduce(&missing),
        Err(crate::session::reduce::ReductionError::InvalidChildReport { agent: actual })
            if actual == agent_id
    ));

    let mut registered = base;
    registered.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ArtifactRegistered {
            artifact: artifact.clone(),
        },
    ));
    registered.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildReportCommitted {
            report: report.clone(),
        },
    ));
    let restored = reduce(&registered).expect("registered artifact report");
    assert_eq!(restored.children[&agent_id].report.as_ref(), Some(&report));
}

#[test]
fn plan_start_and_child_step_attribution_restore_without_evaluator_state() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let input = ConversationEntry {
        id: ConversationEntryId::new(),
        parent: None,
        agent_id: AgentId::for_session(session_id),
        message: Message::text(Role::User, "execute bounded plan"),
    };
    let plan_id = OrchestrationPlanId::new();
    let start = OrchestrationPlanStart {
        plan_id,
        operation_id,
        fingerprint: blake3::hash(b"plan").to_hex().to_string(),
        step_ids: vec!["workers".to_owned(), "results".to_owned()],
    };
    let mut handle = child_handle(session_id, thread_id, operation_id);
    handle.apply_lifecycle(ChildLifecycle::Queued);
    handle.admission.attribution.parent_operation_id = operation_id;
    handle.admission.plan = Some(PlanChildAttribution {
        plan_id,
        step_id: "workers".to_owned(),
        output_index: 0,
    });
    let records = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: input.clone(),
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationAccepted {
                operation_id,
                thread_id,
                input_entry_id: input.id,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OrchestrationPlanStarted {
                start: start.clone(),
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildrenBatchAdmitted {
                handles: vec![handle.clone()],
            },
        ),
    ];
    let restored = reduce(&records).expect("restore plan attribution");
    assert_eq!(restored.orchestration_plans[&plan_id], start);
    assert_eq!(
        restored.children[&handle.admission.attribution.agent_id]
            .handle
            .admission
            .plan,
        handle.admission.plan
    );

    let mut duplicate = records;
    duplicate.push(RecordEnvelope::new(
        session_id,
        SessionRecord::OrchestrationPlanStarted {
            start: restored.orchestration_plans[&plan_id].clone(),
        },
    ));
    assert!(matches!(
        reduce(&duplicate),
        Err(crate::session::reduce::ReductionError::InvalidOrchestrationPlan { plan })
            if plan == plan_id
    ));
}

#[test]
fn child_batch_admission_is_one_atomic_queued_fact() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let mut first = child_handle(session_id, thread_id, operation_id);
    first.apply_lifecycle(ChildLifecycle::Queued);
    let mut second = child_handle(session_id, thread_id, operation_id);
    second.apply_lifecycle(ChildLifecycle::Queued);
    let first_id = first.admission.attribution.agent_id;
    let second_id = second.admission.attribution.agent_id;
    let record = RecordEnvelope::new(
        session_id,
        SessionRecord::ChildrenBatchAdmitted {
            handles: vec![first.clone(), second.clone()],
        },
    );

    let mut records = child_operation_prefix(session_id, thread_id, operation_id);
    records.push(record.clone());
    let restored = reduce(&records).expect("reduce atomic child batch");
    assert_eq!(restored.children[&first_id].handle, first);
    assert_eq!(restored.children[&second_id].handle, second);

    let encoded = serde_json::to_vec(&record).expect("encode batch record");
    let decoded: RecordEnvelope = serde_json::from_slice(&encoded).expect("decode batch record");
    assert_eq!(decoded, record);
}

#[test]
fn invalid_child_batch_member_rejects_the_whole_record() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let mut valid = child_handle(session_id, thread_id, operation_id);
    valid.apply_lifecycle(ChildLifecycle::Queued);
    let mut duplicate = valid.clone();
    duplicate.admission.task_preview = "different task".to_owned();
    let mut records = child_operation_prefix(session_id, thread_id, operation_id);
    records.extend([RecordEnvelope::new(
        session_id,
        SessionRecord::ChildrenBatchAdmitted {
            handles: vec![valid, duplicate],
        },
    )]);

    assert!(matches!(
        reduce(&records),
        Err(crate::session::reduce::ReductionError::DuplicateChild { .. })
    ));

    let mut first = child_handle(session_id, thread_id, operation_id);
    first.apply_lifecycle(ChildLifecycle::Queued);
    let mut duplicate_operation = child_handle(session_id, thread_id, operation_id);
    duplicate_operation.apply_lifecycle(ChildLifecycle::Queued);
    duplicate_operation.admission.attribution.operation_id =
        first.admission.attribution.operation_id;
    let mut records = child_operation_prefix(session_id, thread_id, operation_id);
    records.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildrenBatchAdmitted {
            handles: vec![first, duplicate_operation],
        },
    ));
    assert!(matches!(
        reduce(&records),
        Err(crate::session::reduce::ReductionError::InvalidChildAdmission { .. })
    ));
}

#[test]
fn child_admission_rejects_invalid_thread_and_operation_ownership() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let parent_operation_id = OperationId::new();
    let prefix = child_operation_prefix(session_id, thread_id, parent_operation_id);

    let mut wrong_thread = child_handle(session_id, thread_id, parent_operation_id);
    wrong_thread.admission.attribution.thread_id = ThreadId::new();
    let mut records = prefix.clone();
    records.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildAdmitted {
            handle: wrong_thread,
        },
    ));
    assert!(matches!(
        reduce(&records),
        Err(crate::session::reduce::ReductionError::InvalidChildAdmission { .. })
    ));

    let unknown_parent = child_handle(session_id, thread_id, OperationId::new());
    let mut records = prefix.clone();
    records.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildAdmitted {
            handle: unknown_parent,
        },
    ));
    assert!(matches!(
        reduce(&records),
        Err(crate::session::reduce::ReductionError::InvalidChildAdmission { .. })
    ));

    let mut reused_parent = child_handle(session_id, thread_id, parent_operation_id);
    reused_parent.admission.attribution.operation_id = parent_operation_id;
    let mut records = prefix.clone();
    records.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildAdmitted {
            handle: reused_parent,
        },
    ));
    assert!(matches!(
        reduce(&records),
        Err(crate::session::reduce::ReductionError::InvalidChildAdmission { .. })
    ));

    let mut finished_parent = prefix.clone();
    finished_parent.push(RecordEnvelope::new(
        session_id,
        SessionRecord::OperationFinished {
            operation_id: parent_operation_id,
            outcome: OperationOutcome::Completed,
        },
    ));
    finished_parent.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildAdmitted {
            handle: child_handle(session_id, thread_id, parent_operation_id),
        },
    ));
    assert!(matches!(
        reduce(&finished_parent),
        Err(crate::session::reduce::ReductionError::InvalidChildAdmission { .. })
    ));

    let first = child_handle(session_id, thread_id, parent_operation_id);
    let mut duplicate_operation = child_handle(session_id, thread_id, parent_operation_id);
    duplicate_operation.admission.attribution.operation_id =
        first.admission.attribution.operation_id;
    let mut records = prefix;
    records.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildAdmitted { handle: first },
    ));
    records.push(RecordEnvelope::new(
        session_id,
        SessionRecord::ChildAdmitted {
            handle: duplicate_operation,
        },
    ));
    assert!(matches!(
        reduce(&records),
        Err(crate::session::reduce::ReductionError::InvalidChildAdmission { .. })
    ));
}

#[test]
fn child_reducer_rejects_skipped_and_duplicate_terminal_transitions() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let handle = child_handle(session_id, thread_id, operation_id);
    let agent_id = handle.admission.attribution.agent_id;
    let mut skipped = child_operation_prefix(session_id, thread_id, operation_id);
    skipped.extend([
        RecordEnvelope::new(session_id, SessionRecord::ChildAdmitted { handle }),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ChildLifecycleChanged {
                agent_id,
                lifecycle: ChildLifecycle::Running,
            },
        ),
    ]);

    assert!(matches!(
        reduce(&skipped),
        Err(crate::session::reduce::ReductionError::InvalidChildTransition { .. })
    ));
}

#[test]
fn every_nonterminal_child_prefix_projects_interrupted_without_mutating_facts() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let handle = child_handle(session_id, thread_id, operation_id);
    let agent_id = handle.admission.attribution.agent_id;
    let transitions = [
        ChildLifecycle::Queued,
        ChildLifecycle::Running,
        ChildLifecycle::Suspended,
    ];

    for prefix_len in 0..=transitions.len() {
        let mut records = child_operation_prefix(session_id, thread_id, operation_id);
        records.extend([RecordEnvelope::new(
            session_id,
            SessionRecord::ChildAdmitted {
                handle: handle.clone(),
            },
        )]);
        records.extend(transitions[..prefix_len].iter().map(|lifecycle| {
            RecordEnvelope::new(
                session_id,
                SessionRecord::ChildLifecycleChanged {
                    agent_id,
                    lifecycle: *lifecycle,
                },
            )
        }));

        let restored = reduce(&records).expect("reduce nonterminal prefix");
        let durable = restored.children.get(&agent_id).expect("durable child");
        let durable_lifecycle = if prefix_len == 0 {
            ChildLifecycle::Admitted
        } else {
            transitions[prefix_len - 1]
        };
        assert_eq!(durable.handle.lifecycle, durable_lifecycle);

        let projected = durable.inspection();
        assert_eq!(projected.handle.lifecycle, ChildLifecycle::Interrupted);
        assert!(projected.projected_interruption);
        assert_eq!(
            projected.report.as_ref().map(|report| report.status),
            Some(ChildTerminalStatus::Interrupted)
        );
        assert_eq!(
            restored
                .children
                .get(&agent_id)
                .expect("unchanged durable child")
                .handle
                .lifecycle,
            durable_lifecycle,
            "projection must not rewrite durable state"
        );
    }
}

#[test]
fn every_v1_record_kind_round_trips_without_collapsing_audit_into_conversation() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let artifact = ArtifactRecord {
        reference: ArtifactRef {
            id: ArtifactId::new(),
            content_hash: ContentHash::for_bytes(b"context"),
        },
        media_type: "text/plain".to_owned(),
        byte_len: 7,
        owner: PrincipalId::new(),
    };
    let context = ContextRecord {
        id: ContextId::new(),
        version: 1,
        artifact: artifact.reference.clone(),
        kind: ContextKind::ProjectInstructions,
        content_hash: artifact.reference.content_hash.clone(),
        logical_size: artifact.byte_len,
        provenance: Provenance::ProjectFile {
            relative_path: PathBuf::from("AGENTS.md"),
        },
        trust: TrustClass::Project,
        owner: artifact.owner,
    };
    let view = ContextViewRecord {
        id: ContextViewId::new(),
        source: context.id,
        source_version: 1,
        selector: ViewSelector::Full,
        content_hash: context.content_hash.clone(),
        budget: MaterializationBudget {
            max_bytes: 100,
            max_estimated_tokens: 25,
        },
    };
    let entry = ConversationEntry {
        id: ConversationEntryId::new(),
        parent: None,
        agent_id: AgentId::new(),
        message: Message::text(Role::User, "hello"),
    };
    let records = vec![
        SessionRecord::SessionCreated {
            thread_id,
            workspace_root: PathBuf::from("/workspace"),
        },
        SessionRecord::ConversationEntryAppended {
            entry: entry.clone(),
        },
        SessionRecord::ThreadHeadMoved {
            thread_id,
            head: Some(entry.id),
        },
        SessionRecord::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        },
        SessionRecord::PermissionAudited {
            fact: audit(operation_id),
        },
        SessionRecord::ArtifactRegistered {
            artifact: artifact.clone(),
        },
        SessionRecord::ContextRegistered {
            context: context.clone(),
        },
        SessionRecord::ContextViewRegistered { view },
        SessionRecord::NamedContextSet {
            name: "project:AGENTS.md".to_owned(),
            context_id: context.id,
            version: 1,
        },
    ];

    for record in records {
        let envelope = RecordEnvelope::new(session_id, record);
        let encoded = serde_json::to_vec(&envelope).expect("encode record");
        let decoded: RecordEnvelope = serde_json::from_slice(&encoded).expect("decode record");
        assert_eq!(decoded, envelope);
    }

    let audit_json = serde_json::to_string(&RecordEnvelope::new(
        session_id,
        SessionRecord::PermissionAudited {
            fact: audit(operation_id),
        },
    ))
    .expect("encode audit");
    assert!(audit_json.contains("permission_audited"));
    assert!(!audit_json.contains("conversation_entry_appended"));
}

#[test]
fn crash_recovery_record_kinds_round_trip_with_semantic_identities() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let step_id = StepId::new();
    let invocation_id = ToolInvocationId::new();
    let result_id = ToolResultId::new();
    let input_entry_id = ConversationEntryId::new();
    let assistant_entry_id = ConversationEntryId::new();
    let arguments = serde_json::json!({"path": "README.md"});
    let fact = PermissionAuditFact {
        request: PermissionRequest {
            operation_id,
            invocation_id,
            tool_name: "read_file".to_owned(),
            effect_class: EffectClass::Read,
            final_arguments: arguments.clone(),
            scope: PermissionScope::Unscoped,
            outbound_review: None,
        },
        policy_evaluation: PolicyDecision::Allow,
        controller_decision: None,
        effective: PolicyDecision::Allow,
    };
    let intent = InvocationIntent {
        operation_id,
        step_id,
        invocation_id,
        result_id,
        model_call_id: "call-1".to_owned(),
        target: InvocationTarget::Tool {
            name: "read_file".to_owned(),
            contract_version: 1,
        },
        final_arguments: arguments,
        permission: fact,
        saved_replay_safety: ReplaySafety::Safe,
    };
    let records = vec![
        SessionRecord::OperationAccepted {
            operation_id,
            thread_id,
            input_entry_id,
        },
        SessionRecord::StepStarted {
            operation_id,
            step_id,
            assistant_entry_id,
        },
        SessionRecord::InvocationIntentAppended {
            intent: intent.clone(),
        },
        SessionRecord::OperationSuspended {
            operation_id,
            reason: SuspensionReason::ProcessInterrupted,
        },
        SessionRecord::RecoveryDecisionAppended {
            operation_id,
            invocation_id,
            decision: RecoveryDecision::Replay,
        },
        SessionRecord::InvocationResultAppended {
            result: InvocationResultRecord {
                operation_id,
                invocation_id,
                result_id,
                outcome: InvocationOutcome::Completed {
                    output: DurableValueRef::InlineJson(serde_json::json!("contents")),
                },
            },
        },
        SessionRecord::NamedValueSet {
            value: NamedValueRecord {
                id: NamedValueId::new(),
                operation_id,
                name: format!("tool_output:{invocation_id}"),
                value: DurableValueRef::InlineJson(serde_json::json!("contents")),
            },
        },
        SessionRecord::OperationFinished {
            operation_id,
            outcome: OperationOutcome::Interrupted,
        },
    ];

    for record in records {
        let envelope = RecordEnvelope::new(session_id, record);
        let encoded = serde_json::to_vec(&envelope).expect("encode recovery record");
        assert_eq!(
            serde_json::from_slice::<RecordEnvelope>(&encoded).expect("decode recovery record"),
            envelope
        );
    }
}

fn valid_recovery_sequence() -> (
    Vec<RecordEnvelope>,
    OperationId,
    ToolInvocationId,
    ToolResultId,
) {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let step_id = StepId::new();
    let invocation_id = ToolInvocationId::new();
    let result_id = ToolResultId::new();
    let input_entry_id = ConversationEntryId::new();
    let assistant_entry_id = ConversationEntryId::new();
    let arguments = serde_json::json!({"path": "README.md"});
    let fact = PermissionAuditFact {
        request: PermissionRequest {
            operation_id,
            invocation_id,
            tool_name: "read_file".to_owned(),
            effect_class: EffectClass::Read,
            final_arguments: arguments.clone(),
            scope: PermissionScope::Unscoped,
            outbound_review: None,
        },
        policy_evaluation: PolicyDecision::Allow,
        controller_decision: None,
        effective: PolicyDecision::Allow,
    };
    let records = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: ConversationEntry {
                    id: input_entry_id,
                    parent: None,
                    agent_id: AgentId::new(),
                    message: Message::text(Role::User, "recover"),
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: ConversationEntry {
                    id: assistant_entry_id,
                    parent: Some(input_entry_id),
                    agent_id: AgentId::new(),
                    message: Message::text(Role::Assistant, "read"),
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationAccepted {
                operation_id,
                thread_id,
                input_entry_id,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::StepStarted {
                operation_id,
                step_id,
                assistant_entry_id,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::InvocationIntentAppended {
                intent: InvocationIntent {
                    operation_id,
                    step_id,
                    invocation_id,
                    result_id,
                    model_call_id: "call-1".to_owned(),
                    target: InvocationTarget::Tool {
                        name: "read_file".to_owned(),
                        contract_version: 1,
                    },
                    final_arguments: arguments,
                    permission: fact,
                    saved_replay_safety: ReplaySafety::Safe,
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::InvocationResultAppended {
                result: InvocationResultRecord {
                    operation_id,
                    invocation_id,
                    result_id,
                    outcome: InvocationOutcome::Completed {
                        output: DurableValueRef::InlineJson(serde_json::json!("contents")),
                    },
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::NamedValueSet {
                value: NamedValueRecord {
                    id: NamedValueId::new(),
                    operation_id,
                    name: format!("tool_output:{invocation_id}"),
                    value: DurableValueRef::InlineJson(serde_json::json!("contents")),
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationFinished {
                operation_id,
                outcome: OperationOutcome::Completed,
            },
        ),
    ];
    (records, operation_id, invocation_id, result_id)
}

#[test]
fn reducer_accepts_complete_recovery_and_rejects_ambiguous_effect_sequences() {
    let (records, operation_id, invocation_id, result_id) = valid_recovery_sequence();
    let restored = reduce(&records).expect("valid recovery records");
    let operation = &restored.operation_details[&operation_id];
    assert_eq!(operation.results[&invocation_id].result_id, result_id);
    assert_eq!(operation.finished, Some(OperationOutcome::Completed));
    assert_eq!(restored.named_values.len(), 1);

    let mut pending_finish = records.clone();
    pending_finish.remove(pending_finish.len() - 2);
    pending_finish.remove(pending_finish.len() - 2);
    assert!(matches!(
        reduce(&pending_finish),
        Err(reduce::ReductionError::PendingInvocationAtFinish { .. })
    ));

    let mut duplicate_result = records.clone();
    let duplicate_record = duplicate_result[duplicate_result.len() - 3].record.clone();
    let duplicate_session = duplicate_result[0].session_id;
    duplicate_result.insert(
        duplicate_result.len() - 2,
        RecordEnvelope::new(duplicate_session, duplicate_record),
    );
    assert!(matches!(
        reduce(&duplicate_result),
        Err(reduce::ReductionError::DuplicateResult { .. })
    ));

    let mut mismatched_result = records.clone();
    let result_record = mismatched_result
        .iter_mut()
        .find(|record| {
            matches!(
                record.record,
                SessionRecord::InvocationResultAppended { .. }
            )
        })
        .expect("result record");
    if let SessionRecord::InvocationResultAppended { result } = &mut result_record.record {
        result.result_id = ToolResultId::new();
    }
    assert!(matches!(
        reduce(&mismatched_result),
        Err(reduce::ReductionError::ResultMismatch { .. })
    ));
}

#[test]
fn checked_in_v1_fixture_stays_decodable() {
    let line = include_str!("fixtures/v1-session.jsonl").trim_end();
    let envelope: RecordEnvelope = serde_json::from_str(line).expect("decode v1 fixture");

    assert_eq!(envelope.version, 1);
    assert!(matches!(
        envelope.record,
        SessionRecord::SessionCreated { .. }
    ));
}

#[test]
fn inspect_distinguishes_torn_tail_interior_corruption_and_future_version() {
    let directory = tempdir().expect("session tempdir");
    let sessions = directory.path().join("sessions");
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let store =
        SessionStore::create(&sessions, created(session_id, thread_id)).expect("create session");
    let path = store.path().to_owned();
    drop(store);
    let committed_len = fs::metadata(&path).expect("session metadata").len();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open tail")
        .write_all(b"{\"version\":1")
        .expect("write torn tail");

    let loaded = SessionStore::inspect(&path).expect("inspect torn session");
    assert_eq!(
        loaded.repair,
        Some(TornTailRepair {
            truncate_to: committed_len
        })
    );

    fs::write(&path, b"not-json\nmore\n").expect("write interior corruption");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::CorruptRecord { offset: 0, .. })
    ));

    let future =
        include_str!("fixtures/v1-session.jsonl").replacen("\"version\":1", "\"version\":2", 1);
    fs::write(&path, future).expect("write future record");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::UnsupportedVersion { version: 2 })
    ));
}

#[test]
fn inspect_is_read_only_and_resume_rechecks_before_repair() {
    let directory = tempdir().expect("session tempdir");
    let sessions = directory.path().join("sessions");
    let session_id = SessionId::new();
    let store = SessionStore::create(&sessions, created(session_id, ThreadId::new()))
        .expect("create session");
    let path = store.path().to_owned();
    drop(store);
    let before = fs::read(&path).expect("session bytes");
    let loaded = SessionStore::inspect(&path).expect("inspect session");
    assert_eq!(fs::read(&path).expect("unchanged bytes"), before);

    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open changed session")
        .write_all(b"x")
        .expect("change session");
    assert!(matches!(
        SessionStore::open_for_resume(&path, loaded),
        Err(store::SessionError::ChangedAfterInspection { .. })
    ));
}

#[test]
fn resume_restores_exact_history_and_exposes_unfinished_work_without_replay() {
    let directory = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    let mut session = DurableSession::create(
        directory.path(),
        workspace
            .path()
            .canonicalize()
            .expect("canonical workspace"),
    )
    .expect("create durable session");
    let session_id = session.session_id();
    let operation_id = OperationId::new();
    session
        .append_message(Message::text(Role::User, "persist me"))
        .expect("append conversation");
    session
        .append_operation_state(operation_id, OperationState::Running)
        .expect("append unfinished state");
    drop(session);

    let (resumed, summary) =
        DurableSession::resume(directory.path(), session_id).expect("resume durable session");

    assert_eq!(
        resumed.conversation().expect("restored conversation"),
        vec![Message::text(Role::User, "persist me")]
    );
    assert_eq!(
        summary.unfinished,
        vec![(operation_id, OperationState::Running)]
    );
}

#[test]
fn latest_session_selection_is_bounded_to_the_exact_workspace() {
    let directory = tempdir().expect("session tempdir");
    let workspace = directory.path().join("workspace");
    let other_workspace = directory.path().join("other");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&other_workspace).expect("other workspace");
    let matching =
        DurableSession::create(directory.path(), workspace.clone()).expect("matching session");
    let matching_id = matching.session_id();
    drop(matching);
    drop(DurableSession::create(directory.path(), other_workspace).expect("unrelated session"));
    std::fs::write(
        directory.path().join("sessions/not-a-session.jsonl"),
        b"corrupt",
    )
    .expect("corrupt candidate");

    assert_eq!(
        DurableSession::latest_for_workspace(directory.path(), &workspace)
            .expect("latest compatible session"),
        Some(matching_id)
    );
    assert_eq!(
        DurableSession::latest_for_workspace(directory.path(), &directory.path().join("missing"))
            .expect("no compatible session"),
        None
    );
}

#[test]
fn append_rejection_does_not_change_the_committed_file() {
    let directory = tempdir().expect("session tempdir");
    let session_id = SessionId::new();
    let mut store = SessionStore::create(
        &directory.path().join("sessions"),
        created(session_id, ThreadId::new()),
    )
    .expect("create session");
    let before = fs::read(store.path()).expect("committed bytes");
    let oversized = RecordEnvelope::new(
        session_id,
        SessionRecord::ConversationEntryAppended {
            entry: ConversationEntry {
                id: ConversationEntryId::new(),
                parent: None,
                agent_id: AgentId::new(),
                message: Message::text(Role::User, "x".repeat(store::MAX_RECORD_BYTES)),
            },
        },
    );

    assert!(matches!(
        store.append(&oversized),
        Err(store::SessionError::RecordTooLarge { .. })
    ));
    assert_eq!(fs::read(store.path()).expect("unchanged bytes"), before);
}

#[test]
fn semantically_invalid_append_never_reaches_the_journal() {
    let directory = tempdir().expect("session tempdir");
    let mut session =
        DurableSession::create(directory.path(), PathBuf::from("/workspace")).expect("session");
    let before = fs::read(session.path()).expect("committed bytes");

    let error = session
        .append_record(SessionRecord::ThreadHeadMoved {
            thread_id: session.thread_id(),
            head: Some(ConversationEntryId::new()),
        })
        .expect_err("unknown head must be rejected");

    assert!(error.to_string().contains("failed validation"));
    assert_eq!(fs::read(session.path()).expect("unchanged bytes"), before);
}

#[test]
fn empty_and_oversized_sessions_are_rejected() {
    let directory = tempdir().expect("session tempdir");
    let path = directory.path().join("empty.jsonl");
    fs::write(&path, []).expect("empty session");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::MissingCreationRecord)
    ));

    fs::write(&path, vec![b'x'; store::MAX_SESSION_BYTES + 1]).expect("oversized session");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::SessionTooLarge { .. })
    ));
}

#[test]
fn reduction_derives_only_the_head_path_and_keeps_bookkeeping_separate() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let agent_id = AgentId::new();
    let operation_id = OperationId::new();
    let first = ConversationEntryId::new();
    let branch = ConversationEntryId::new();
    let records = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: ConversationEntry {
                    id: first,
                    parent: None,
                    agent_id,
                    message: Message::text(Role::User, "root"),
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: ConversationEntry {
                    id: branch,
                    parent: Some(first),
                    agent_id,
                    message: Message::text(Role::Assistant, "branch"),
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ThreadHeadMoved {
                thread_id,
                head: Some(branch),
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationStateChanged {
                operation_id,
                state: OperationState::Running,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::PermissionAudited {
                fact: audit(operation_id),
            },
        ),
    ];
    let restored = reduce(&records).expect("reduce records");

    assert_eq!(
        restored.conversation_path().expect("conversation path"),
        vec![
            Message::text(Role::User, "root"),
            Message::text(Role::Assistant, "branch")
        ]
    );
    assert_eq!(restored.audits.len(), 1);
    assert_eq!(restored.operations.len(), 1);
    assert_eq!(restored.conversation_path().expect("path again").len(), 2);
}

#[test]
fn reduction_rejects_unknown_heads_and_invalid_operation_transitions() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let unknown_head = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ThreadHeadMoved {
                thread_id,
                head: Some(ConversationEntryId::new()),
            },
        ),
    ];
    assert!(matches!(
        reduce(&unknown_head),
        Err(reduce::ReductionError::UnknownHead { .. })
    ));

    let operation_id = OperationId::new();
    let invalid = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(OperationOutcome::Completed),
            },
        ),
    ];
    assert!(matches!(
        reduce(&invalid),
        Err(reduce::ReductionError::InvalidOperationTransition { .. })
    ));
}

#[test]
fn project_context_versions_refresh_only_on_change_and_old_bytes_remain_materializable() {
    let directory = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    fs::write(workspace.path().join("AGENTS.md"), "first\nneedle one\n")
        .expect("first project context");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let mut session =
        DurableSession::create(directory.path(), workspace_root).expect("create durable session");

    session.refresh_project_context().expect("first refresh");
    let first_key = session.restored().named_context["project:AGENTS.md"];
    let first_context = session.restored().contexts[&first_key].clone();
    session
        .refresh_project_context()
        .expect("unchanged refresh");
    assert_eq!(session.restored().contexts.len(), 1);

    fs::write(workspace.path().join("AGENTS.md"), "second\nneedle two\n")
        .expect("changed project context");
    let (old_line, _) = session
        .materialize(
            &first_context,
            &ViewSelector::Lines { start: 1, end: 1 },
            MaterializationBudget {
                max_bytes: 32,
                max_estimated_tokens: 8,
            },
        )
        .expect("materialize old version");
    assert_eq!(old_line, "first");

    session.refresh_project_context().expect("changed refresh");
    let second_key = session.restored().named_context["project:AGENTS.md"];
    assert_eq!(second_key.0, first_key.0);
    assert_eq!(second_key.1, 2);

    fs::remove_file(workspace.path().join("AGENTS.md")).expect("remove live source");
    assert!(
        session
            .refresh_project_context()
            .expect("missing refresh")
            .is_empty()
    );
    assert_eq!(
        session.restored().named_context["project:AGENTS.md"],
        second_key
    );
}

#[test]
fn an_unstarted_created_session_can_be_rolled_back_exactly() {
    let directory = tempdir().expect("Xana data tempdir");
    let session_id = SessionId::new();
    let session = DurableSession::create_with_id(
        directory.path(),
        PathBuf::from("/target-workspace"),
        session_id,
    )
    .expect("create exact session");
    let session_path = SessionStore::path_for(&directory.path().join("sessions"), session_id);
    assert!(session_path.exists());

    session.discard_unstarted().expect("discard empty session");

    assert!(!session_path.exists());
}

#[test]
fn materialization_enforces_byte_and_token_bounds_for_search() {
    let directory = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    fs::write(
        workspace.path().join("AGENTS.md"),
        "match alpha\nignore\nmatch beta\nmatch gamma\n",
    )
    .expect("project context");
    let mut session = DurableSession::create(
        directory.path(),
        workspace
            .path()
            .canonicalize()
            .expect("canonical workspace"),
    )
    .expect("create session");
    session.refresh_project_context().expect("refresh context");
    let key = session.restored().named_context["project:AGENTS.md"];
    let context = session.restored().contexts[&key].clone();

    let (text, _) = session
        .materialize(
            &context,
            &ViewSelector::LiteralSearch {
                query: "match".to_owned(),
                max_matches: 3,
            },
            MaterializationBudget {
                max_bytes: 10,
                max_estimated_tokens: 2,
            },
        )
        .expect("bounded search");
    assert!(text.len() <= 10);
    assert!(crate::context::estimate_tokens(&text) <= 2);
}
