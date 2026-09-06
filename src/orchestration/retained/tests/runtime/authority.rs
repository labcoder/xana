//! Owner controls exercise real privacy and concurrent mailbox boundaries.
use super::*;

#[test]
fn conflicting_followups_have_one_durable_winner_without_losing_its_message() {
    let f = Fixture::new();
    let ready = Arc::new(std::sync::Barrier::new(3));
    let handles = (0..2).map(|index| {
        let paths=f.paths.clone();
        let store=f.store.clone();
        let ready=ready.clone();
        let id=f.worker.id;
        std::thread::spawn(move || {
            ready.wait();
            worker_commands::control(&paths,&store,serde_json::from_value(serde_json::json!({"command":"follow_up","target":{"id":id,"revision":1},"request_id":Uuid::new_v4(),"text":format!("follow-up {index}")})).unwrap(),&CancellationToken::new()).is_ok()
        })
    }).collect::<Vec<_>>();
    ready.wait();
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .filter(|won| *won)
            .count(),
        1
    );
    let current = f.store.retained_worker(f.worker.id).unwrap();
    assert_eq!(current.revision, 2);
    assert_eq!(current.mailbox.len(), 1);
    assert_eq!(current.accepted_requests.len(), 1);
}

#[tokio::test]
async fn forgetting_during_execution_cancels_and_never_restores_source_authority() {
    use crate::memory::{MemoryContext, MemoryEdit, MemoryOwner, MemoryScope};
    let mut f = Fixture::new();
    let owner = MemoryOwner::new(
        f.store.clone(),
        MemoryContext {
            conversation: Some(f.worker.session.to_string().parse().unwrap()),
            ..Default::default()
        },
    );
    let record = owner
        .remember(MemoryScope::User, "My favorite color is blue".into(), None)
        .unwrap();
    let generation = f.store.privacy_generation().unwrap();
    f.worker = f
        .store
        .retained_update(f.worker.id, f.worker.revision, |worker| {
            worker.privacy_generation = generation;
            Ok(())
        })
        .unwrap()
        .0;
    f.queue();
    let pending = execution::run_with_factory(
        f.paths.clone(),
        f.store.clone(),
        f.worker.clone(),
        CancellationToken::new(),
        Some(f.factory(true)),
    );
    tokio::pin!(pending);
    tokio::select! {
        started = tokio::time::timeout(std::time::Duration::from_secs(5), f.started.notified()) => started.expect("worker should start"),
        result = &mut pending => panic!("unexpected completion {result:?}"),
    }
    owner
        .revise(record.id, record.revision, MemoryEdit::Forget)
        .unwrap();
    let finished = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.state, WorkerState::NeedsReview);
    assert!(finished.active.is_none());
    f.command(serde_json::json!({"command":"recover","target":{"id":finished.id,"revision":finished.revision},"review_unknown":true})).unwrap();
    let recovered = f.store.retained_worker(f.worker.id).unwrap();
    assert!(f.command(serde_json::json!({"command":"follow_up","target":{"id":recovered.id,"revision":recovered.revision},"request_id":Uuid::new_v4(),"text":"Continue"})).is_err());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
