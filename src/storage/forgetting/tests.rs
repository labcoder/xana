use super::*;
use crate::{
    memory::{MemoryContext, MemoryControlEdit, MemoryEdit, MemoryOwner, MemoryScope, MemoryState},
    storage::{RecoveryIdentity, TestCustody},
};

fn fixture() -> (
    tempfile::TempDir,
    MemoryOwner,
    RecoveryIdentity,
    TestCustody,
) {
    let home = tempfile::tempdir().unwrap();
    let key = RecoveryIdentity::generate();
    let custody = TestCustody::default();
    let store = ProtectedStore::initialize(home.path(), &key, &custody).unwrap();
    let owner = MemoryOwner::new(
        store,
        MemoryContext {
            conversation: Some(Uuid::new_v4()),
            ..Default::default()
        },
    );
    (home, owner, key, custody)
}

#[test]
fn forget_invalidates_stale_work_and_survives_reopen_without_erasing_owner_inspection() {
    let (home, owner, _, custody) = fixture();
    let record = owner
        .remember(MemoryScope::User, "My favorite color is blue".into(), None)
        .unwrap();
    let admitted = owner.store.privacy_generation().unwrap();
    let forgotten = owner
        .revise(record.id, record.revision, MemoryEdit::Forget)
        .unwrap();
    assert_eq!(forgotten.state, MemoryState::Forgotten);
    assert!(owner.store.privacy_generation().unwrap() > admitted);
    assert!(
        !owner
            .store
            .source_eligible(owner.context.conversation.unwrap())
            .unwrap()
    );
    assert!(owner.eligible().unwrap().records.is_empty());
    assert_eq!(owner.record(record.id).unwrap().statement, record.statement);
    assert!(
        owner
            .revise(
                record.id,
                record.revision,
                MemoryEdit::Correct {
                    statement: "blue".into(),
                    valid_until_unix_seconds: None
                }
            )
            .is_err()
    );
    assert!(
        owner
            .revise(
                record.id,
                forgotten.revision,
                MemoryEdit::Correct {
                    statement: "blue".into(),
                    valid_until_unix_seconds: None
                }
            )
            .is_err()
    );
    let context = owner.context.clone();
    drop(owner);
    let reopened = MemoryOwner::new(
        ProtectedStore::open(home.path(), &custody).unwrap(),
        context,
    );
    assert!(reopened.eligible().unwrap().records.is_empty());
    assert!(
        !reopened
            .store
            .source_eligible(reopened.context.conversation.unwrap())
            .unwrap()
    );
}

#[test]
fn fresh_explicit_restore_does_not_reauthorize_old_sources_or_stale_jobs() {
    let (_home, owner, _, _) = fixture();
    let record = owner
        .remember(MemoryScope::User, "Prefer short answers".into(), None)
        .unwrap();
    let record = owner
        .revise(record.id, record.revision, MemoryEdit::Forget)
        .unwrap();
    let generation = owner.store.privacy_generation().unwrap();
    assert!(
        owner
            .revise(
                record.id,
                record.revision,
                MemoryEdit::Restore { confirm: false }
            )
            .is_err()
    );
    owner
        .revise(
            record.id,
            record.revision,
            MemoryEdit::Restore { confirm: true },
        )
        .unwrap();
    assert_eq!(owner.eligible().unwrap().records.len(), 1);
    assert!(owner.store.privacy_generation().unwrap() > generation);
    assert!(
        !owner
            .store
            .source_eligible(owner.context.conversation.unwrap())
            .unwrap()
    );
    assert!(owner.store.source_eligible(Uuid::new_v4()).unwrap());
}

#[test]
fn corrections_and_control_changes_invalidate_inflight_selections() {
    let (_home, owner, _, _) = fixture();
    let before = owner.store.privacy_generation().unwrap();
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                learning_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(owner.store.privacy_generation().unwrap() > before);
}

#[test]
fn restoring_old_snapshot_reconciles_later_forgetting_before_recall() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::paths::XanaPaths::resolve(Some(home.path().as_os_str().to_owned())).unwrap();
    let key = RecoveryIdentity::generate();
    let owner = MemoryOwner::new(
        ProtectedStore::initialize(paths.data_dir(), &key, &TestCustody::default()).unwrap(),
        MemoryContext {
            conversation: Some(Uuid::new_v4()),
            ..Default::default()
        },
    );
    let record = owner
        .remember(MemoryScope::User, "Forget this canary".into(), None)
        .unwrap();
    let backup = owner
        .store
        .backup(
            &crate::storage::backup::BackupPolicy::default(),
            1000,
            false,
        )
        .unwrap()
        .snapshot
        .unwrap();
    owner
        .revise(record.id, record.revision, MemoryEdit::Forget)
        .unwrap();
    let context = owner.context.clone();
    drop(owner);
    let plan = crate::storage::restore::preview(&paths, &backup, &key).unwrap();
    crate::storage::restore::apply(&paths, &backup, &key, &plan.review).unwrap();
    let restored = ProtectedStore::recover(paths.data_dir(), &key).unwrap();
    let restored_owner = MemoryOwner::new(restored, context);
    assert!(restored_owner.eligible().unwrap().records.is_empty());
    assert_eq!(
        restored_owner.record(record.id).unwrap().state,
        MemoryState::Forgotten
    );
    assert!(restored_owner.record(record.id).unwrap().revision > record.revision);
    assert!(
        !restored_owner
            .store
            .source_eligible(restored_owner.context.conversation.unwrap())
            .unwrap()
    );
    assert!(backup.exists());
}

fn seed_history(owner: &MemoryOwner) -> crate::identity::SessionId {
    use crate::{
        identity::ThreadId,
        session::{RecordEnvelope, SessionRecord},
    };
    let id = owner
        .context
        .conversation
        .unwrap()
        .to_string()
        .parse()
        .unwrap();
    owner
        .store
        .create_history(&[RecordEnvelope::new(
            id,
            SessionRecord::SessionCreated {
                thread_id: ThreadId::new(),
                workspace_root: std::path::PathBuf::from("C:/synthetic/workspace"),
            },
        )])
        .unwrap();
    id
}

#[test]
fn source_deletion_requires_exact_preview_inactive_writer_and_retains_shared_artifacts() {
    let (_home, owner, _, _) = fixture();
    let id = seed_history(&owner);
    let conversation = owner.context.conversation.unwrap();
    let preview = owner.deletion_preview(conversation).unwrap();
    let content = b"shared encrypted artifact retained";
    let (hash, length, _) = owner
        .store
        .put_artifact(&mut content.as_slice(), 1024, |_| Ok(()))
        .unwrap();
    assert!(
        owner
            .delete_source(conversation, "not the reviewed source")
            .is_err()
    );
    let writer = owner.store.session_writer(id).unwrap();
    assert!(owner.delete_source(conversation, &preview.review).is_err());
    drop(writer);
    let receipt = owner.delete_source(conversation, &preview.review).unwrap();
    assert_eq!(receipt.deleted_records, 1);
    assert!(receipt.artifacts_retained);
    assert_eq!(
        owner
            .store
            .read_artifact(&hash, length, 0, content.len())
            .unwrap(),
        content
    );
    assert!(owner.deletion_preview(conversation).is_err());
    assert!(!owner.store.source_eligible(conversation).unwrap());
    assert!(owner.delete_source(conversation, &preview.review).is_err());
}

#[test]
fn restored_memory_requires_exact_separate_review_without_reenabling_automation() {
    let (_home, owner, _, _) = fixture();
    assert_eq!(
        owner.store.review_restored_memory(None).unwrap()["review_required"],
        false
    );
    owner
        .remember(MemoryScope::User, "I prefer examples".into(), None)
        .unwrap();
    owner
        .store
        .set_document("restore/review-required", b"restore-one", 4096)
        .unwrap();
    assert!(owner.eligible().unwrap().records.is_empty());
    let preview = owner.store.review_restored_memory(None).unwrap();
    assert_eq!(preview["review_required"], true);
    assert!(
        owner
            .store
            .review_restored_memory(Some("wrong-review"))
            .is_err()
    );
    let reviewed = owner
        .store
        .review_restored_memory(Some(preview["review"].as_str().unwrap()))
        .unwrap();
    assert_eq!(reviewed["review_required"], false);
    assert_eq!(owner.eligible().unwrap().records.len(), 1);
    assert!(
        owner
            .store
            .document("restore/review-required", 4096)
            .unwrap()
            .is_some(),
        "automation still has a distinct authority gate"
    );
    owner
        .store
        .set_document("restore/review-required", b"restore-two", 4096)
        .unwrap();
    assert!(owner.eligible().unwrap().records.is_empty());
}

#[test]
fn restore_exclusions_reject_oversized_metadata_before_copy_and_roll_back() {
    let (_source_home, source, _, _) = fixture();
    let (_target_home, target, _, _) = fixture();
    let conversation = source.context.conversation.unwrap();
    source
        .store
        .with_database(|db| {
            db.connection.execute(
                "INSERT INTO excluded_sources VALUES(?1,?2,1)",
                params![conversation.to_string(), "x".repeat(129)],
            )?;
            Ok(())
        })
        .unwrap();
    let error = target
        .store
        .reconcile_exclusions_from(&source.store)
        .unwrap_err();
    assert!(error.to_string().contains("exclusion text exceeds"));
    assert!(target.store.source_eligible(conversation).unwrap());
    source
        .store
        .with_database(|db| {
            db.connection.execute("DELETE FROM excluded_sources", [])?;
            db.connection.execute(
                "INSERT INTO deletion_receipts VALUES(?1,?2,zeroblob(1025))",
                params![Uuid::new_v4().to_string(), conversation.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let error = target
        .store
        .reconcile_exclusions_from(&source.store)
        .unwrap_err();
    assert!(error.to_string().contains("deletion receipt exceeds"));
    assert!(target.store.source_eligible(conversation).unwrap());
}
