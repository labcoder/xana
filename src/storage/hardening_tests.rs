//! Combined lifecycle attacks over the real encrypted store, not marker-only mocks.
//! All state is disposable; no provider, OS keyring or real Xana home is used.
mod prompt;

use super::{ProtectedStore, RecoveryIdentity, TestCustody, backup::BackupPolicy, restore};
use crate::{
    memory::{
        MemoryContext, MemoryEdit, MemoryOwner, MemoryScope, MemoryState,
        candidates::{CandidateEdit, CandidatePayload},
        learning::LearningRoute,
    },
    paths::XanaPaths,
    usage_budget::{Admission, DispatchFacts, WorkClass},
};
use uuid::Uuid;

#[test]
fn correction_forgetting_lock_and_restore_do_not_revive_queued_work_or_refund_unknown_usage() {
    let directory = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
    let recovery = RecoveryIdentity::generate();
    let custody = TestCustody::default();
    let store = ProtectedStore::initialize(paths.data_dir(), &recovery, &custody).unwrap();
    let context = MemoryContext {
        conversation: Some(Uuid::new_v4()),
        ..Default::default()
    };
    let owner = MemoryOwner::new(store.clone(), context.clone());
    let fact = owner
        .remember(MemoryScope::User, "I prefer examples".into(), None)
        .unwrap();
    let draft = owner
        .stage_skill(
            MemoryScope::User,
            "unpublished-example".into(),
            "# Inert draft from the same Conversation".into(),
        )
        .unwrap();
    let route = LearningRoute {
        connection: "synthetic".into(),
        model: "no-network".into(),
        digest: "exact-offline-route".into(),
    };
    store
        .set_document(
            "memory/learning-route",
            &serde_json::to_vec(&route).unwrap(),
            4096,
        )
        .unwrap();
    assert!(
        owner
            .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
            .unwrap()
    );
    let queued = store.learning_batch().unwrap();
    assert_eq!(queued.len(), 1);
    let reservation = Admission {
        facts: DispatchFacts::default(),
        id: Uuid::new_v4().to_string(),
        operation: Uuid::new_v4().to_string(),
        root: "synthetic-root".into(),
        job: "synthetic-job".into(),
        route: "native/synthetic/no-network".into(),
        class: WorkClass::Foreground,
        reserved_tokens: 100,
    };
    // A provider request's fate is unknown: no settlement is invented.
    store.reserve_usage(&reservation, 100).unwrap();
    let snapshot = store
        .backup(&BackupPolicy::default(), 1000, false)
        .unwrap()
        .snapshot
        .unwrap();
    let corrected = owner
        .revise(
            fact.id,
            fact.revision,
            MemoryEdit::Correct {
                statement: "I prefer concise responses".into(),
                valid_until_unix_seconds: None,
            },
        )
        .unwrap();
    assert!(store.commit_learning(&queued, &[], &route).is_err());
    let forgotten = owner
        .revise(fact.id, corrected.revision, MemoryEdit::Forget)
        .unwrap();
    assert_eq!(forgotten.state, MemoryState::Forgotten);
    store.lock().unwrap();
    assert!(owner.eligible().is_err());
    assert!(owner.candidate(draft.id).is_err());
    assert!(store.reserve_usage(&reservation, 100).is_err());
    let reopened = ProtectedStore::unlock(paths.data_dir(), &custody).unwrap();
    // Unlock creates a new capability; cached clients remain revoked.
    assert!(owner.eligible().is_err());
    assert!(store.commit_learning(&queued, &[], &route).is_err());
    assert!(reopened.commit_learning(&queued, &[], &route).is_err());
    let ledger = reopened.usage_page(None, None, None).unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].charged_tokens, 100);
    assert!(ledger[0].receipt.is_none());
    drop(reopened);
    drop(owner);
    drop(store);

    let plan = restore::preview(&paths, &snapshot, &recovery).unwrap();
    restore::apply(&paths, &snapshot, &recovery, &plan.review).unwrap();
    let restored = ProtectedStore::recover(paths.data_dir(), &recovery).unwrap();
    let owner = MemoryOwner::new(restored.clone(), context.clone());
    assert_eq!(owner.record(fact.id).unwrap().state, MemoryState::Forgotten);
    assert!(
        !restored
            .source_eligible(context.conversation.unwrap())
            .unwrap()
    );
    assert!(restored.commit_learning(&queued, &[], &route).is_err());
    assert!(owner.eligible().unwrap().records.is_empty());
    let candidate = owner.candidate(draft.id).unwrap();
    assert!(!candidate.can_approve);
    assert!(matches!(
        candidate.record.payload,
        CandidatePayload::Skill { markdown: None, .. }
    ));

    let review = restored.review_restored_memory(None).unwrap();
    restored
        .review_restored_memory(review["review"].as_str())
        .unwrap();
    assert!(owner.eligible().unwrap().records.is_empty());
    assert!(
        owner
            .review_candidate(
                draft.id,
                draft.revision,
                CandidateEdit::Approve {
                    confirm_sensitive: false,
                }
            )
            .is_err()
    );
    assert!(restored.commit_learning(&queued, &[], &route).is_err());
    let mut fresh = reservation.clone();
    fresh.id = Uuid::new_v4().to_string();
    // Memory review cannot approve unknown restored spending or unattended work.
    assert!(restored.reserve_usage(&fresh, 100).is_err());
    assert!(restored.autonomy_claim(1000).is_err());
    let ledger = restored.usage_page(None, None, None).unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].charged_tokens, 100);
    assert!(ledger[0].receipt.is_none());
    assert!(
        restored
            .document("usage/restore-review-required", 4096)
            .unwrap()
            .is_some()
    );
    assert!(
        restored
            .document("restore/review-required", 4096)
            .unwrap()
            .is_some()
    );
    restored.verify_content().unwrap();
}
