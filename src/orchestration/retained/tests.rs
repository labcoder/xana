use super::*;
use crate::{
    config::{OrchestrationLimits, PermissionMode},
    identity::{OperationId, ThreadId},
    orchestration::{ChildAttribution, ChildResultSchema, ExecutionOwner},
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
};
mod runtime;

fn worker() -> RetainedWorker {
    let session = SessionId::new();
    let id = AgentId::new();
    RetainedWorker {
        version: 1,
        id,
        revision: 1,
        session,
        goal: "Keep a bounded task alive".into(),
        admission: ChildAdmission {
            completion: Default::default(),
            attribution: ChildAttribution {
                agent_id: id,
                parent_agent_id: AgentId::for_session(session),
                operation_id: OperationId::new(),
                parent_operation_id: OperationId::new(),
                thread_id: ThreadId::new(),
                route: "worker".into(),
                profile: "worker".into(),
                owner: ExecutionOwner::Native,
                connection: "local".into(),
                model: "fixture".into(),
            },
            plan: None,
            task_preview: "task".into(),
            task_hash: blake3::hash(b"task").to_hex().to_string(),
            result_schema: ChildResultSchema::Summary,
            capabilities: Vec::new(),
            permission_mode: PermissionMode::Deny,
            max_tool_rounds: 1,
            limits: OrchestrationLimits::default(),
            hard_token_limit: None,
            hard_spend_microusd: None,
        },
        scope: format!("conversation:{session}"),
        workspace_identity: "fixture".into(),
        configuration_digest: "fixture".into(),
        privacy_generation: 0,
        expires_at: i64::MAX,
        state: WorkerState::Idle,
        mailbox: Vec::new(),
        accepted_requests: Vec::new(),
        active: None,
        cancellation: Uuid::new_v4(),
        executions: 0,
        context_bytes: 0,
        context_operations: 0,
        context_receipt: None,
        evidence: Vec::new(),
        last_receipt: None,
    }
}

#[test]
fn protected_worker_cas_restart_and_cumulative_root_budget() {
    let home = tempfile::tempdir().unwrap();
    let custody = TestCustody::default();
    let store =
        ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody).unwrap();
    let first = worker();
    let mut second = worker();
    second.session = first.session;
    second.admission.attribution.parent_agent_id = AgentId::for_session(first.session);
    store.retained_create(first.clone()).unwrap();
    store.retained_create(second.clone()).unwrap();
    let (changed, ()) = store
        .retained_update(first.id, 1, |w| {
            w.context_bytes = CONTEXT_TOTAL_BYTES;
            w.context_operations = 1;
            Ok(())
        })
        .unwrap();
    assert!(store.retained_update(first.id, 1, |_| Ok(())).is_err());
    assert!(
        store
            .retained_update(second.id, 1, |w| {
                w.context_bytes = 1;
                w.context_operations = 1;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(store.retained_worker(second.id).unwrap().context_bytes, 0);
    store.lock().unwrap();
    let store = ProtectedStore::unlock(home.path(), &custody).unwrap();
    assert_eq!(
        store.retained_worker(first.id).unwrap().context_bytes,
        CONTEXT_TOTAL_BYTES
    );
    assert!(
        store
            .retained_update(first.id, changed.revision, |w| {
                w.context_bytes = 0;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(store.retained_page(None).unwrap().len(), 2);
    let mut stale_source = worker();
    stale_source.privacy_generation = store.privacy_generation().unwrap() + 1;
    assert!(store.retained_create(stale_source).is_err());
}

#[test]
fn interrupted_attempt_is_not_requeued_and_stop_is_sticky() {
    let mut worker = worker();
    let attempt = Uuid::new_v4();
    let message = FollowUp {
        id: Uuid::new_v4(),
        text: "Continue".into(),
    };
    worker.active = Some((attempt, message.clone()));
    worker.state = WorkerState::Stopped;
    let receipt = worker
        .finish(attempt, None, "uncertain process exit")
        .unwrap();
    assert_eq!(receipt.follow_up, message.id);
    assert_eq!(receipt.status, ChildTerminalStatus::Interrupted);
    assert!(worker.active.is_none() && worker.mailbox.is_empty());
    assert_eq!(worker.state, WorkerState::Stopped);
    assert!(worker.finish(attempt, None, "duplicate").is_err());
}

#[test]
fn unsupported_completion_needs_review_and_never_looks_completed() {
    let mut worker = worker();
    let attempt = Uuid::new_v4();
    worker.state = WorkerState::Running;
    worker.active = Some((
        attempt,
        FollowUp {
            id: Uuid::new_v4(),
            text: "finish".into(),
        },
    ));
    let mut report = ChildReport::completed(
        worker.admission.attribution.clone(),
        "all done".into(),
        super::super::ChildUsage::Unknown,
    );
    let mut evidence = crate::completion_evidence::CompletionEvidence::new(
        report.attribution.operation_id,
        crate::completion_evidence::WorkKind::Child,
        crate::completion_evidence::EvidenceOwner::Native,
        crate::completion_evidence::CompletionClaim::Completed,
        Default::default(),
    )
    .unwrap();
    evidence.revision = 2;
    evidence.omitted_observations = true;
    evidence.evaluate();
    report.apply_completion_evidence(evidence, 2048);
    assert_eq!(report.status, ChildTerminalStatus::Failed);
    assert_eq!(report.lifecycle(), super::super::ChildLifecycle::Failed);
    assert_eq!(report.output.as_deref(), Some("all done"));
    assert!(report.error.as_ref().unwrap().contains("Needs attention"));
    let receipt = worker.finish(attempt, Some(&report), "finished").unwrap();
    assert_eq!(receipt.status, ChildTerminalStatus::Failed);
    assert!(receipt.summary.contains("Needs attention"));
    assert_eq!(worker.state, WorkerState::NeedsReview);
}

#[test]
fn worker_drain_bounds_and_utf8_handoff_are_explicit() {
    let mut worker = worker();
    let attempt = Uuid::new_v4();
    worker.state = WorkerState::Draining;
    worker.active = Some((
        attempt,
        FollowUp {
            id: Uuid::new_v4(),
            text: "Done".into(),
        },
    ));
    worker
        .finish(attempt, None, "lost vendor handle; no vendor heap resume")
        .unwrap();
    assert_eq!(worker.state, WorkerState::Stopped);
    worker.mailbox = (0..9)
        .map(|_| FollowUp {
            id: Uuid::new_v4(),
            text: "消息".into(),
        })
        .collect();
    assert!(worker.validate().is_err());
    assert!(validate_text(&"界".repeat(3000)).is_err());
}
