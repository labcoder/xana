use crate::{
    identity::{OperationId, SessionId},
    message::{Message, Role},
    prompt::{ModelBudgetFacts, PromptBudgetPlan, PromptBudgetPolicy},
    session::{CompactionReason, DurableSession},
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
};

fn budget() -> PromptBudgetPlan {
    PromptBudgetPlan::derive(
        &PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        ModelBudgetFacts {
            connection: "fixture".into(),
            model: "fixture".into(),
            context_tokens: None,
            max_output_tokens: None,
            reasoning: false,
        },
    )
    .unwrap()
}

fn reopened() -> (tempfile::TempDir, ProtectedStore, DurableSession) {
    let directory = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = SessionId::new();
    let mut session =
        DurableSession::create_protected(store.clone(), directory.path().into(), id).unwrap();
    for index in 0..386 {
        session
            .append_message(Message::text(Role::User, format!("Original fact {index}")))
            .unwrap();
    }
    session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    drop(session);
    let (mut session, _) = DurableSession::resume_protected(store.clone(), id).unwrap();
    assert_eq!(session.restored().retained_offset(), 385);
    session
        .append_message(Message::text(Role::User, "Newest retained request"))
        .unwrap();
    (directory, store, session)
}

#[test]
fn first_compaction_after_reopen_does_not_synchronously_read_the_archived_prefix() {
    let (_directory, _store, session) = reopened();
    let preparation = session
        .begin_compaction(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    assert!(
        !preparation.is_ready(),
        "begin must yield before hashing the 385 archived original entries"
    );
    assert_eq!(preparation.progress(), (0, 387));
    let preparation = preparation.advance().unwrap();
    assert_eq!(preparation.progress(), (128, 387));
    assert!(
        preparation.finish().is_err(),
        "partial proof cannot reach the helper or commit"
    );
}

#[test]
fn paged_preparation_preserves_originals_and_commits_once_with_verified_references() {
    let (_directory, store, mut session) = reopened();
    let id = session.session_id();
    let revision = store.history_metadata(id).unwrap().revision;
    let old_checkpoint = session
        .restored()
        .active_compaction()
        .unwrap()
        .unwrap()
        .clone();
    let cancelled = session
        .begin_compaction(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap()
        .advance()
        .unwrap();
    drop(cancelled);
    assert_eq!(store.history_metadata(id).unwrap().revision, revision);
    assert_eq!(
        session.restored().active_compaction().unwrap(),
        Some(&old_checkpoint)
    );

    let full = store.active_prefix_proof(id, 386).unwrap();
    let mut preparation = session
        .begin_compaction(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    while !preparation.is_ready() {
        preparation = preparation.advance().unwrap();
    }
    let candidate = preparation.finish().unwrap();
    assert_eq!(candidate.checkpoint.source_digest, full.digest());
    assert_eq!(candidate.checkpoint.source_entry_count, 386);
    let reference = candidate.helper_messages.as_ref().unwrap().last().unwrap();
    assert_eq!(reference.role, Role::User);
    let crate::message::ContentBlock::Text(reference) = &reference.content[0] else {
        panic!("recovery reference")
    };
    assert!(reference.contains(&id.to_string()));
    assert!(reference.contains(&full.start().to_string()));
    assert!(reference.contains(&full.end().to_string()));
    let checkpoint = session.commit_compaction(candidate).unwrap();
    assert_eq!(store.history_metadata(id).unwrap().revision, revision + 1);
    assert_eq!(
        session.restored().active_compaction().unwrap(),
        Some(&checkpoint)
    );
    assert_eq!(store.history_page(id, None, Some(0), 1).unwrap().total, 387);
}

#[test]
fn paged_preparation_rejects_owner_change_and_never_commits_partial_proof() {
    let (_directory, store, mut session) = reopened();
    let preparation = session
        .begin_compaction(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap()
        .advance()
        .unwrap();
    session
        .append_message(Message::text(
            Role::User,
            "New owner request during preparation",
        ))
        .unwrap();
    let revision = store
        .history_metadata(session.session_id())
        .unwrap()
        .revision;
    assert!(
        preparation
            .advance()
            .err()
            .unwrap()
            .to_string()
            .contains("history changed")
    );
    assert_eq!(
        store
            .history_metadata(session.session_id())
            .unwrap()
            .revision,
        revision
    );
    assert_eq!(
        session
            .restored()
            .active_compaction()
            .unwrap()
            .unwrap()
            .source_entry_count,
        385
    );
}

#[test]
fn cached_ready_preparation_carries_a_live_nonpersistent_source_guard() {
    let (_directory, _store, mut session) = reopened();
    session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    session
        .append_message(Message::text(
            Role::User,
            "Next request after cached compaction",
        ))
        .unwrap();
    let preparation = session
        .begin_compaction(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    assert!(
        preparation.is_ready(),
        "the original-byte accumulator avoids a second full proof"
    );
    let candidate = preparation.finish().unwrap();
    let guard = candidate
        .source_guard
        .as_ref()
        .expect("even cached proof requires live source authority");
    guard.recheck().unwrap();
    let encoded = serde_json::to_string(&candidate.checkpoint).unwrap();
    assert!(!encoded.contains("source_guard"));
    assert!(!encoded.contains("privacy_generation"));
    session
        .append_message(Message::text(
            Role::User,
            "New owner request before helper disclosure",
        ))
        .unwrap();
    assert!(guard.recheck().is_err());
    assert!(session.commit_compaction(candidate).is_err());
}

#[test]
fn cached_ready_preparation_rechecks_owner_snapshot_at_finish() {
    let (_directory, _store, mut session) = reopened();
    session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    session
        .append_message(Message::text(Role::User, "Next retained request"))
        .unwrap();
    let preparation = session
        .begin_compaction(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    assert!(preparation.is_ready());
    session
        .append_message(Message::text(Role::User, "Owner changed before finish"))
        .unwrap();
    assert!(
        preparation
            .finish()
            .err()
            .unwrap()
            .to_string()
            .contains("history changed")
    );
}
