//! Completion uses its admitted identity, not an unrelated mailbox revision.
use super::*;
use crate::orchestration::{
    ChildReport,
    context_ops::{ContextWorkReceipt, ContextWorkState},
};

#[test]
fn terminal_settlement_preserves_a_followup_enqueued_after_its_revision_snapshot() {
    let mut f = Fixture::new();
    f.queue();
    let execution = Uuid::new_v4();
    f.worker = f
        .store
        .retained_admit(f.worker.id, f.worker.revision, |worker| {
            worker.active = Some((execution, worker.mailbox.remove(0)));
            worker.state = WorkerState::Running;
            Ok(())
        })
        .unwrap()
        .0;
    let terminal_snapshot = f.store.retained_worker(f.worker.id).unwrap();
    let followup = Uuid::new_v4();
    f.command(serde_json::json!({"command":"follow_up", "target":{"id":f.worker.id,"revision":terminal_snapshot.revision}, "request_id":followup, "text":"Next task"})).unwrap();
    let report = ChildReport::completed(
        f.worker.admission.attribution.clone(),
        "Completed".into(),
        ChildUsage::Unknown,
    );
    let (settled, _) = f
        .store
        .retained_settle(f.worker.id, false, |worker| {
            worker.finish(execution, Some(&report), "")
        })
        .unwrap();
    assert_eq!(settled.state, WorkerState::Idle);
    assert!(settled.active.is_none());
    assert_eq!(settled.mailbox[0].id, followup);
    assert_eq!(settled.revision, terminal_snapshot.revision + 2);
    assert!(
        f.store
            .retained_settle(f.worker.id, false, |worker| worker.finish(
                execution,
                Some(&report),
                ""
            ))
            .is_err()
    );
}

#[test]
fn context_settlement_preserves_a_followup_without_replacing_its_reservation() {
    let f = Fixture::new();
    let reservation = Uuid::new_v4();
    let (reserved, _) = f
        .store
        .retained_admit(f.worker.id, f.worker.revision, |worker| {
            worker.context_operations = 1;
            worker.context_bytes = 16;
            worker.context_receipt = Some(ContextWorkReceipt {
                completion: None,
                id: reservation,
                state: ContextWorkState::Reserved,
                verified_bytes: 16,
                selected_bytes: 8,
                input_count: 1,
                model_calls: 0,
                result: None,
                preview: String::new(),
                error: None,
            });
            Ok(())
        })
        .unwrap();
    let followup = Uuid::new_v4();
    f.command(serde_json::json!({"command":"follow_up", "target":{"id":f.worker.id,"revision":reserved.revision}, "request_id":followup, "text":"Next task"})).unwrap();
    let (settled, _) = f
        .store
        .retained_settle(f.worker.id, true, |worker| {
            let receipt = worker.context_receipt.as_mut().unwrap();
            ensure!(
                receipt.id == reservation
                    && receipt.state == ContextWorkState::Reserved
                    && worker.cancellation == reserved.cancellation,
                "context reservation no longer belongs to this operation"
            );
            receipt.state = ContextWorkState::Completed;
            Ok(())
        })
        .unwrap();
    assert_eq!(settled.mailbox[0].id, followup);
    assert_eq!(settled.context_operations, 1);
    assert_eq!(settled.context_bytes, 16);
    assert_eq!(
        settled.context_receipt.unwrap().state,
        ContextWorkState::Completed
    );
}
