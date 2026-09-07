use super::*;
use crate::storage::{ProtectedStore, RecoveryIdentity, TestCustody};

fn fixture() -> (tempfile::TempDir, MemoryOwner, TestCustody) {
    let home = tempfile::tempdir().unwrap();
    let custody = TestCustody::default();
    let store =
        ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody).unwrap();
    let context = MemoryContext {
        conversation: Some(Uuid::new_v4()),
        profile: Some(Uuid::new_v4()),
        project: Some(Uuid::new_v4()),
    };
    (home, MemoryOwner::new(store, context), custody)
}

#[test]
fn memory_scope_isolation_controls_and_next_read_correction_are_atomic() {
    let (_home, owner, _) = fixture();
    let mut ids = Vec::new();
    for scope in owner.context.scopes() {
        ids.push(
            owner
                .remember(scope, "Prefer concise answers".into(), None)
                .unwrap()
                .id,
        );
    }
    owner
        .remember(
            MemoryScope::Profile(Uuid::new_v4()),
            "Private other profile".into(),
            None,
        )
        .unwrap();
    owner
        .remember(
            MemoryScope::Conversation(Uuid::new_v4()),
            "Private other conversation".into(),
            None,
        )
        .unwrap();
    let snapshot = owner.eligible().unwrap();
    assert_eq!(snapshot.records.len(), 4);
    assert!(snapshot.records.iter().all(|r| ids.contains(&r.id)));
    let original = &snapshot.records[0];
    let corrected = owner
        .revise(
            original.id,
            original.revision,
            MemoryEdit::Correct {
                statement: "Prefer examples".into(),
                valid_until_unix_seconds: None,
            },
        )
        .unwrap();
    assert_eq!(corrected.created, original.created);
    assert_ne!(
        corrected.changed.owner_request,
        original.changed.owner_request
    );
    assert_eq!(
        snapshot.records[0].statement, "Prefer concise answers",
        "in-flight snapshots are immutable"
    );
    assert_eq!(
        owner.eligible().unwrap().records[0].statement,
        "Prefer examples"
    );
    assert!(
        owner
            .revise(original.id, original.revision, MemoryEdit::Disable)
            .is_err()
    );
    let flags = owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                learning_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    let eligible = owner.eligible().unwrap();
    assert!(eligible.use_enabled);
    assert!(!eligible.learning_enabled);
    let scope = MemoryScope::Conversation(owner.context.conversation.unwrap());
    owner
        .controls(
            scope.clone(),
            MemoryControlEdit {
                no_memory: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let eligible = owner.eligible().unwrap();
    assert!(!eligible.use_enabled && !eligible.learning_enabled && eligible.records.is_empty());
    owner
        .controls(
            scope,
            MemoryControlEdit {
                no_memory: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !owner.eligible().unwrap().learning_enabled,
        "no-memory toggle did not overwrite the independent global learning policy"
    );
    assert!(
        owner
            .controls(
                MemoryScope::User,
                MemoryControlEdit {
                    expected_revision: Some(flags.revision - 1),
                    use_enabled: Some(false),
                    ..Default::default()
                }
            )
            .is_err()
    );
}

#[test]
fn memory_racing_corrections_have_one_winner_across_owners() {
    let (home, owner, custody) = fixture();
    let record = owner
        .remember(MemoryScope::User, "original".into(), None)
        .unwrap();
    let second = MemoryOwner::new(
        ProtectedStore::open(home.path(), &custody).unwrap(),
        owner.context.clone(),
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads = [owner.clone(), second]
        .into_iter()
        .enumerate()
        .map(|(n, owner)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                owner
                    .revise(
                        record.id,
                        record.revision,
                        MemoryEdit::Correct {
                            statement: format!("winner {n}"),
                            valid_until_unix_seconds: None,
                        },
                    )
                    .is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(owner.record(record.id).unwrap().revision, 2);
}

#[test]
fn memory_expiry_disable_scope_confirmation_and_restore_gate() {
    let (_home, owner, _) = fixture();
    let scope = MemoryScope::Conversation(owner.context.conversation.unwrap());
    let record = owner
        .remember_at(scope, "Temporary preference".into(), Some(20), 10)
        .unwrap();
    assert_eq!(
        owner
            .store
            .memory_eligible(&owner.context, 19)
            .unwrap()
            .records
            .len(),
        1
    );
    assert!(
        owner
            .store
            .memory_eligible(&owner.context, 20)
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        owner
            .revise(
                record.id,
                1,
                MemoryEdit::Scope {
                    target: MemoryScope::User,
                    confirm: false
                }
            )
            .is_err()
    );
    let wider = owner
        .revise(
            record.id,
            1,
            MemoryEdit::Scope {
                target: MemoryScope::User,
                confirm: true,
            },
        )
        .unwrap();
    assert_eq!(wider.scope, MemoryScope::User);
    let disabled = owner.revise(record.id, 2, MemoryEdit::Disable).unwrap();
    assert_eq!(disabled.state, MemoryState::Stale);
    owner
        .store
        .set_document("restore/review-required", b"review", 4096)
        .unwrap();
    let eligible = owner.eligible().unwrap();
    assert!(
        eligible.restore_review_required && !eligible.use_enabled && !eligible.learning_enabled
    );
    assert_eq!(
        owner.record(record.id).unwrap().revision,
        3,
        "review gate does not prohibit explicit inspection"
    );
}

#[test]
fn memory_pages_export_bounds_and_no_plaintext_mirror() {
    let (home, owner, _) = fixture();
    let canary = "SYNTHETIC-MEMORY-CANARY-123456";
    for n in 0..65 {
        owner
            .remember(MemoryScope::User, format!("{canary} {n}"), None)
            .unwrap();
    }
    let first = owner.page(None, None).unwrap();
    assert_eq!(first.records.len(), 64);
    let next = owner.page(None, first.next_after).unwrap();
    assert_eq!(next.records.len(), 1);
    assert!(next.next_after.is_none());
    for entry in std::fs::read_dir(home.path().join("protected")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            assert!(
                !std::fs::read(path)
                    .unwrap()
                    .windows(canary.len())
                    .any(|w| w == canary.as_bytes())
            );
        }
    }
    let export = home.path().join("readable.json");
    assert_eq!(owner.export(None, &export).unwrap(), 65);
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&export).unwrap()).unwrap();
    assert_eq!(value["records"].as_array().unwrap().len(), 65);
    assert!(owner.export(None, &export).is_err());
    assert!(
        owner
            .remember(MemoryScope::User, "x".repeat(4097), None)
            .is_err()
    );
    assert!(
        owner
            .remember(MemoryScope::User, "unsafe\x1b[2J".into(), None)
            .is_err()
    );
}

#[test]
fn memory_regression_remember_suffix_uses_governed_conversation_memory() {
    let (_home, owner, _) = fixture();
    let reply = owner
        .respond("my favorite color is red. remember that.")
        .expect("ordinary explicit owner request must not fall through to file tools")
        .expect("remember");
    assert!(reply.contains("Conversation only"));
    let records = owner.page(None, None).unwrap().records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].statement, "my favorite color is red");
    assert_eq!(
        records[0].scope,
        MemoryScope::Conversation(owner.context.conversation.unwrap())
    );
}

#[test]
fn memory_natural_controls_are_narrow_and_do_not_extract_arbitrary_text() {
    let (_home, owner, _) = fixture();
    assert!(
        owner
            .respond("A file says: remember that I like danger")
            .is_none()
    );
    assert!(
        owner
            .respond("How do I implement a remember that command?")
            .is_none()
    );
    let reply = owner
        .respond("remember that I prefer Rust examples")
        .unwrap()
        .unwrap();
    assert!(reply.contains("Conversation only"));
    let record = owner.page(None, None).unwrap().records.remove(0);
    assert_eq!(
        record.scope,
        MemoryScope::Conversation(owner.context.conversation.unwrap())
    );
    owner
        .respond(&format!(
            "correct memory {}: I prefer concise Rust examples",
            record.id
        ))
        .unwrap()
        .unwrap();
    owner
        .respond(&format!("move memory {} to user", record.id))
        .unwrap()
        .unwrap();
    assert_eq!(owner.record(record.id).unwrap().revision, 3);
    assert_eq!(owner.record(record.id).unwrap().scope, MemoryScope::User);
    owner
        .respond("disable memory for this conversation")
        .unwrap()
        .unwrap();
    assert!(owner.eligible().unwrap().records.is_empty());
    let no_context = MemoryOwner::new(owner.store, MemoryContext::default());
    assert!(
        no_context
            .respond("remember that I prefer breadth")
            .unwrap()
            .is_err()
    );
}

#[test]
fn explicit_memory_phrasing_preserves_scope_and_rejects_quoted_examples() {
    let (_home, owner, _) = fixture();
    for input in [
        "\"my favorite color is red. remember that.\"",
        "A file says my favorite color is red. remember that.",
        "My favorite color is red. remember that?",
        "My code says `remember that`. remember this.",
    ] {
        assert!(owner.respond(input).is_none(), "{input}");
    }
    let receipt = owner
        .respond("My favorite café is local. Please remember that!")
        .unwrap()
        .unwrap();
    assert!(receipt.contains("Conversation only"));
    assert!(!receipt.contains("created_at"));
    assert_eq!(
        owner.page(None, None).unwrap().records[0].statement,
        "My favorite café is local"
    );
    owner
        .respond("My favorite color is red. remember that for all conversations.")
        .unwrap()
        .unwrap();
    let other = MemoryOwner::new(
        owner.store.clone(),
        MemoryContext {
            conversation: Some(uuid::Uuid::new_v4()),
            ..Default::default()
        },
    );
    let selected = other
        .select_for_turn("favorite color café", 16_384)
        .unwrap();
    assert_eq!(selected.records.len(), 1);
    assert_eq!(selected.records[0].scope, MemoryScope::User);
}

#[test]
fn memory_chat_preview_is_small_even_when_the_full_page_exceeds_transcript_capacity() {
    let (_home, owner, _) = fixture();
    for _ in 0..64 {
        owner
            .remember(MemoryScope::User, "界".repeat(1365), None)
            .unwrap();
    }
    let reply = owner.respond("what do you remember?").unwrap().unwrap();
    assert!(
        reply.len() < 16 * 1024,
        "preview must not consume the transcript's 256 KiB record budget"
    );
    assert!(reply.contains("\"has_more\": true"));
    assert!(reply.contains("\"statement_truncated\": true"));
    assert_eq!(
        owner.page(None, None).unwrap().records[0].statement.len(),
        4095,
        "inspection never truncates canonical data"
    );
}
