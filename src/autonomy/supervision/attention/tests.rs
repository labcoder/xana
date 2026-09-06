use super::*;
use crate::autonomy::{RunReceipt, tests::fixture};
use crate::host_lifecycle::{
    ClientFocus, NotificationDestination, NotificationPlanner, NotificationPolicy,
};

fn finish(store: &ProtectedStore, job: &Job, outcome: RunOutcome) {
    let running = store.autonomy_claim(job.next.at).unwrap().unwrap();
    assert_eq!(running.id, job.id);
    store
        .autonomy_finish(
            running.id,
            RunReceipt {
                completion: None,
                occurrence: running.occurrence.unwrap(),
                scheduled_at: running.next.at,
                finished_at: running.next.at,
                outcome,
                detail: "private receipt must not enter notifications".into(),
                coalesced: false,
                dst_adjusted: false,
            },
        )
        .unwrap();
}

#[test]
fn baseline_and_unchanged_polls_are_quiet_and_read_only() {
    let (_home, store, _custody, job) = fixture();
    store.autonomy_create(job.clone()).unwrap();
    let before = store.autonomy_job(job.id).unwrap();
    let policy = store.autonomy_policy().unwrap();
    let memory = store
        .memory_controls(
            crate::memory::MemoryScope::User,
            crate::memory::MemoryControlEdit {
                no_memory: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let mut observer = AttentionObserver::default();
    assert!(observer.poll(&store).unwrap().is_empty());
    assert!(observer.poll(&store).unwrap().is_empty());
    assert_eq!(store.autonomy_job(job.id).unwrap(), before);
    assert_eq!(store.autonomy_policy().unwrap(), policy);
    assert_eq!(
        store
            .memory_controls(crate::memory::MemoryScope::User, Default::default())
            .unwrap(),
        memory
    );
    finish(&store, &job, RunOutcome::Completed);
    let notes = observer.poll(&store).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].kind, BackgroundAttentionKind::Completed);
    assert!(observer.poll(&store).unwrap().is_empty());
    assert!(
        AttentionObserver::default()
            .poll(&store)
            .unwrap()
            .is_empty()
    );
    let json = serde_json::to_string(&notes).unwrap();
    assert!(!json.contains(&job.name));
    assert!(!json.contains("private receipt"));
    assert!(!json.contains(&job.scope.workspace.to_string_lossy().to_string()));
}

#[test]
fn changed_and_new_needs_you_edges_notify_once_with_quiet_reconnect() {
    let (_home, store, _custody, job) = fixture();
    store.autonomy_create(job.clone()).unwrap();
    let mut observer = AttentionObserver::default();
    assert!(observer.poll(&store).unwrap().is_empty());
    finish(&store, &job, RunOutcome::NeedsYou);
    assert_eq!(
        observer.poll(&store).unwrap()[0].kind,
        BackgroundAttentionKind::NeedsYou
    );
    assert!(observer.poll(&store).unwrap().is_empty());
    assert!(
        AttentionObserver::default()
            .poll(&store)
            .unwrap()
            .is_empty()
    );
    let mut next = job.clone();
    next.id = Uuid::new_v4();
    next.conversation = Uuid::new_v4();
    store.autonomy_create(next.clone()).unwrap();
    finish(&store, &next, RunOutcome::NeedsYou);
    let notes = observer.poll(&store).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].task, next.id.to_string());
}

#[test]
fn bounded_pages_eventually_observe_late_jobs_without_replaying_receipts() {
    let (_home, store, _custody, prototype) = fixture();
    let mut jobs = Vec::new();
    for index in 0..40 {
        let mut job = prototype.clone();
        job.id = Uuid::new_v4();
        job.conversation = Uuid::new_v4();
        job.next.at += index;
        job.not_before += index;
        job.schedule = crate::autonomy::Schedule::Once { at: job.next.at };
        store.autonomy_create(job.clone()).unwrap();
        jobs.push(job);
    }
    let mut observer = AttentionObserver::default();
    for _ in 0..3 {
        assert!(observer.poll(&store).unwrap().is_empty());
    }
    assert_eq!(store.autonomy_attention_jobs(0).unwrap().len(), 16);
    for job in &jobs {
        finish(&store, job, RunOutcome::Completed);
    }
    let mut seen = BTreeSet::new();
    for _ in 0..4 {
        let notes = observer.poll(&store).unwrap();
        assert!(notes.len() <= 32);
        for note in notes {
            assert!(seen.insert(note.task));
        }
    }
    assert_eq!(seen.len(), jobs.len());
    assert!(observer.poll(&store).unwrap().is_empty());
}

#[test]
fn a_new_receipt_is_not_lost_during_the_first_multi_page_baseline() {
    let (_home, store, _custody, prototype) = fixture();
    for _ in 0..17 {
        let mut job = prototype.clone();
        job.id = Uuid::new_v4();
        job.conversation = Uuid::new_v4();
        job.next.at += 100;
        job.not_before += 100;
        job.schedule = crate::autonomy::Schedule::Once { at: job.next.at };
        store.autonomy_create(job).unwrap();
    }
    store.autonomy_create(prototype.clone()).unwrap();
    let mut observer = AttentionObserver::default();
    assert!(observer.poll(&store).unwrap().is_empty());
    finish(&store, &prototype, RunOutcome::NeedsYou);
    let notes = observer.poll(&store).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].task, prototype.id.to_string());
    for _ in 0..3 {
        assert!(observer.poll(&store).unwrap().is_empty());
    }
}

#[test]
fn notification_policy_uses_background_language_and_schedules_destination() {
    let (_home, _store, _custody, job) = fixture();
    let note = BackgroundAttention {
        task: job.id.to_string(),
        conversation: job.conversation.to_string(),
        occurrence: None,
        revision: 1,
        kind: BackgroundAttentionKind::NeedsYou,
    };
    let signal = note.signal();
    let mut planner = NotificationPlanner::default();
    let mut policy = NotificationPolicy::default();
    assert!(
        planner
            .plan(&policy, ClientFocus::Focused, &signal)
            .is_none()
    );
    policy.failures = false;
    assert!(
        planner
            .plan(&policy, ClientFocus::Unfocused, &signal)
            .is_none()
    );
    policy.failures = true;
    let notification = planner
        .plan(&policy, ClientFocus::Unfocused, &signal)
        .unwrap();
    assert_eq!(notification.destination, NotificationDestination::Schedules);
    assert!(notification.title.contains("background"));
    assert!(!notification.body.contains("failed"));
    assert!(
        planner
            .plan(&policy, ClientFocus::Unfocused, &signal)
            .is_none()
    );
}

#[test]
fn attention_deduplication_storage_remains_bounded() {
    let (_home, _store, _custody, job) = fixture();
    let mut observer = AttentionObserver::default();
    let mut notes = Vec::new();
    for _ in 0..300 {
        let occurrence = Uuid::new_v4();
        observer.emit(
            &mut notes,
            &job,
            BackgroundAttentionKind::Completed,
            Some(occurrence),
        );
        observer.emit(
            &mut notes,
            &job,
            BackgroundAttentionKind::Completed,
            Some(occurrence),
        );
    }
    assert_eq!(notes.len(), 300);
    assert_eq!(observer.keys.len(), 256);
    assert_eq!(observer.key_order.len(), 256);
}

#[test]
fn selected_home_change_establishes_a_separate_quiet_baseline() {
    let (_first_home, first, _first_custody, first_job) = fixture();
    first.autonomy_create(first_job.clone()).unwrap();
    let mut observer = AttentionObserver::default();
    assert!(observer.poll(&first).unwrap().is_empty());
    finish(&first, &first_job, RunOutcome::Completed);
    assert_eq!(observer.poll(&first).unwrap().len(), 1);
    let (_second_home, second, _second_custody, second_job) = fixture();
    second.autonomy_create(second_job.clone()).unwrap();
    assert!(observer.poll(&second).unwrap().is_empty());
    finish(&second, &second_job, RunOutcome::Completed);
    let notes = observer.poll(&second).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].task, second_job.id.to_string());
}
