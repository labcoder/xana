use super::*;
use crate::storage::{RecoveryIdentity, TestCustody};

fn fixture() -> (tempfile::TempDir, MemoryOwner) {
    let home = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        home.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let owner = MemoryOwner::new(
        store,
        MemoryContext {
            conversation: Some(Uuid::new_v4()),
            profile: Some(Uuid::new_v4()),
            project: Some(Uuid::new_v4()),
        },
    );
    (home, owner)
}
#[test]
fn selection_is_bounded_current_and_scope_filtered_without_helper_calls() {
    let (_home, owner) = fixture();
    let row = owner
        .remember(MemoryScope::User, "Prefer concise responses".into(), None)
        .unwrap();
    owner
        .remember(
            MemoryScope::Project(Uuid::new_v4()),
            "PRIVATE_SCOPE_CANARY".into(),
            None,
        )
        .unwrap();
    let first = owner.select_for_turn("responses", 16_384).unwrap();
    assert_eq!(first.records.len(), 1);
    let (text, ids) = first.managed_text("Current request", 16_384).unwrap();
    assert!(text.contains(&row.statement));
    assert!(!text.contains("PRIVATE_SCOPE_CANARY"));
    assert!(estimate_tokens(text.strip_suffix("Current request").unwrap()) <= 16_384 / 20);
    owner.record_selection(&first, &ids).unwrap();
    owner
        .revise(
            row.id,
            row.revision,
            MemoryEdit::Correct {
                statement: "Prefer detailed responses".into(),
                valid_until_unix_seconds: None,
            },
        )
        .unwrap();
    assert!(owner.record_selection(&first, &ids).is_err());
    let updated = owner.select_for_turn("responses", 16_384).unwrap();
    assert_eq!(updated.records[0].statement, "Prefer detailed responses");
    let small = owner.select_for_turn("responses", 128).unwrap();
    assert!(small.records.is_empty());
}
#[test]
fn forget_after_handoff_requires_fresh_conversation_even_if_fact_origin_was_elsewhere() {
    let (_home, owner) = fixture();
    let global = MemoryOwner::new(owner.store.clone(), MemoryContext::default());
    let row = global
        .remember(MemoryScope::User, "FORGET_HANDOFF_CANARY".into(), None)
        .unwrap();
    let selection = owner.select_for_turn("CANARY", 16_384).unwrap();
    owner.record_selection(&selection, &[row.id]).unwrap();
    global
        .revise(row.id, row.revision, MemoryEdit::Forget)
        .unwrap();
    assert!(owner.select_for_turn("CANARY", 16_384).is_err());
    let mut fresh = owner.clone();
    fresh.context.conversation = Some(Uuid::new_v4());
    assert!(
        fresh
            .select_for_turn("CANARY", 16_384)
            .unwrap()
            .records
            .is_empty()
    );
}
#[test]
fn no_memory_never_silently_reuses_a_previous_vendor_handoff() {
    let (_home, owner) = fixture();
    let row = owner
        .remember(MemoryScope::User, "Use examples".into(), None)
        .unwrap();
    let selection = owner.select_for_turn("examples", 16_384).unwrap();
    owner.record_selection(&selection, &[row.id]).unwrap();
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                no_memory: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let disabled = owner.select_for_turn("examples", 16_384).unwrap();
    assert!(disabled.records.is_empty());
    assert!(owner.record_selection(&disabled, &[]).is_err());
}
