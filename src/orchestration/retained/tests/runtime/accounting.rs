//! Historical admissions survive bounded execution hydration, including batches.
use super::*;

#[test]
fn admission_accounting_counts_each_batch_child_once_after_hydration() {
    let f = Fixture::new();
    let (mut session, summary) =
        DurableSession::resume_protected(f.store.clone(), f.worker.session).unwrap();
    assert!(
        summary.children.is_empty(),
        "completed handles leave execution memory"
    );
    assert_eq!(session.orchestration_reservations().unwrap().len(), 1);
    let operation = OperationId::new();
    let input = session
        .append_message(Message::text(Role::User, "Batch work"))
        .unwrap();
    session
        .append_record(SessionRecord::OperationAccepted {
            operation_id: operation,
            thread_id: session.thread_id(),
            input_entry_id: input,
        })
        .unwrap();
    let handles = (0..2)
        .map(|_| {
            let mut admission = f.worker.admission.clone();
            admission.attribution.agent_id = AgentId::new();
            admission.attribution.operation_id = OperationId::new();
            admission.attribution.parent_operation_id = operation;
            let mut handle = AgentHandleSnapshot::admitted(admission);
            handle.apply_lifecycle(ChildLifecycle::Queued);
            handle
        })
        .collect();
    session
        .append_record(SessionRecord::ChildrenBatchAdmitted { handles })
        .unwrap();
    drop(session);
    let (session, summary) =
        DurableSession::resume_protected(f.store.clone(), f.worker.session).unwrap();
    assert_eq!(
        summary.children.len(),
        2,
        "only the active batch stays hydrated"
    );
    let reservations = session.orchestration_reservations().unwrap();
    assert_eq!(
        reservations.len(),
        3,
        "one completed plus two active, not five"
    );
    assert!(reservations.iter().all(|r| r.tool_rounds == 1));
}

#[tokio::test]
async fn corrupt_admission_index_refuses_resume_before_consuming_the_mailbox() {
    let mut f = Fixture::new();
    f.queue();
    f.store
        .corrupt_admission_fixture(f.worker.session, crate::storage::AdmissionFault::WrongIndex)
        .unwrap();
    assert!(
        execution::run_with_factory(
            f.paths.clone(),
            f.store.clone(),
            f.worker.clone(),
            CancellationToken::new(),
            Some(f.factory(false)),
        )
        .await
        .is_err()
    );
    let current = f.store.retained_worker(f.worker.id).unwrap();
    assert_eq!(current.state, WorkerState::Idle);
    assert_eq!(current.mailbox.len(), 1);
    assert!(current.active.is_none());
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_admission_index_cannot_reset_historical_accounting() {
    let f = Fixture::new();
    f.store
        .corrupt_admission_fixture(
            f.worker.session,
            crate::storage::AdmissionFault::MissingIndex,
        )
        .unwrap();
    assert!(
        f.store
            .history_orchestration_reservations(f.worker.session)
            .is_err()
    );
}

#[test]
fn oversized_admission_record_is_refused_before_decoding() {
    let f = Fixture::new();
    f.store
        .corrupt_admission_fixture(
            f.worker.session,
            crate::storage::AdmissionFault::OversizeRecord,
        )
        .unwrap();
    let error = f
        .store
        .history_orchestration_reservations(f.worker.session)
        .unwrap_err();
    assert!(error.to_string().contains("bounded accounting inspection"));
}

#[test]
fn changed_admission_charge_cannot_lower_historical_consumption() {
    let f = Fixture::new();
    f.store
        .corrupt_admission_fixture(
            f.worker.session,
            crate::storage::AdmissionFault::ChangedCharge,
        )
        .unwrap();
    let error = f
        .store
        .history_orchestration_reservations(f.worker.session)
        .unwrap_err();
    assert!(error.to_string().contains("immutable digest"));
}
