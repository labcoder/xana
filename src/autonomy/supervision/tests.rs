use super::*;
use crate::autonomy::{JobEdit, RunReceipt, tests::fixture};

#[test]
fn projection_tracks_durable_edits_and_never_copies_payloads() {
    let (_home, store, _custody, mut job) = fixture();
    job.action = Action::Reminder {
        text: "SECRET-PROMPT-CANARY".into(),
    };
    store.autonomy_create(job.clone()).unwrap();
    let initial = page(&store, 0).unwrap();
    assert_eq!(initial.jobs[0].group, WorkGroup::ComingUp);
    assert_eq!(initial, page(&store, 0).unwrap());
    let paused = store
        .autonomy_edit(job.id, 1, JobEdit::Pause, job.next.at)
        .unwrap();
    assert_eq!(page(&store, 0).unwrap().jobs[0].group, WorkGroup::Paused);
    assert!(
        store
            .autonomy_edit(job.id, 1, JobEdit::Cancel, job.next.at)
            .is_err()
    );
    store
        .autonomy_edit(
            job.id,
            paused.revision,
            JobEdit::Resume {
                review_unknown: false,
            },
            job.next.at,
        )
        .unwrap();
    let active = store.autonomy_claim(job.next.at).unwrap().unwrap();
    let requested = store
        .autonomy_edit(job.id, active.revision, JobEdit::Cancel, job.next.at)
        .unwrap();
    let projected = overview(&requested);
    assert!(projected.cancellation_pending);
    assert_eq!(projected.group, WorkGroup::InMotion);
    store
        .autonomy_finish(
            job.id,
            RunReceipt {
                occurrence: active.occurrence.unwrap(),
                scheduled_at: job.next.at,
                finished_at: job.next.at + 1,
                outcome: RunOutcome::Unknown,
                detail: "SECRET-RESULT-CANARY".into(),
                coalesced: false,
                dst_adjusted: false,
            },
        )
        .unwrap();
    let final_page = page(&store, 0).unwrap();
    assert_eq!(final_page.jobs[0].group, WorkGroup::NeedsYou);
    let encoded = serde_json::to_string(&final_page).unwrap();
    assert!(!encoded.contains("CANARY"));
    assert!(encoded.contains("outcome unknown"));
}

#[test]
fn projection_pages_without_materializing_entire_queue() {
    let (_home, store, _custody, job) = fixture();
    for _ in 0..PAGE_SIZE + 1 {
        let mut next = job.clone();
        next.id = uuid::Uuid::new_v4();
        next.conversation = uuid::Uuid::new_v4();
        store.autonomy_create(next).unwrap();
    }
    let first = page(&store, 0).unwrap();
    assert_eq!(first.jobs.len(), PAGE_SIZE);
    let next = page(&store, first.next_after.unwrap()).unwrap();
    assert_eq!(next.jobs.len(), 1);
    assert!(next.next_after.is_none());
    assert!(!first.jobs.iter().any(|job| job.id == next.jobs[0].id));
}

#[test]
fn exact_review_exposes_saved_action_without_leaking_it_into_pages_or_attention() {
    for action in [
        Action::Reminder {
            text: "PRIVATE-ACTION-CANARY reminder".into(),
        },
        Action::NativeTask {
            prompt: "PRIVATE-ACTION-CANARY fixed task".into(),
            workspace_reads: false,
        },
    ] {
        let (home, store, _custody, mut job) = fixture();
        let paths = XanaPaths::resolve(Some(home.path().as_os_str().to_owned())).unwrap();
        let mut observer = attention::AttentionObserver::default();
        assert!(observer.poll(&store).unwrap().is_empty());
        job.action = action.clone();
        store.autonomy_create(job.clone()).unwrap();
        let reviewed = review(&paths, &store, &job).unwrap();
        assert_eq!(
            reviewed["saved_action"],
            serde_json::to_value(&action).unwrap()
        );
        assert!(
            !serde_json::to_string(&page(&store, 0).unwrap())
                .unwrap()
                .contains("CANARY")
        );
        let active = store.autonomy_claim(job.next.at).unwrap().unwrap();
        store
            .autonomy_finish(
                job.id,
                RunReceipt {
                    occurrence: active.occurrence.unwrap(),
                    scheduled_at: job.next.at,
                    finished_at: job.next.at + 1,
                    outcome: RunOutcome::Completed,
                    detail: "PRIVATE-RESULT-CANARY".into(),
                    coalesced: false,
                    dst_adjusted: false,
                },
            )
            .unwrap();
        let notes = observer.poll(&store).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(!serde_json::to_string(&notes).unwrap().contains("CANARY"));
    }
}
