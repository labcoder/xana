use super::*;

fn add_files(root: &KnowledgeRoot) {
    for index in 0..1000 {
        fs::write(
            root.path.join(format!("batch-{index:04}.md")),
            format!("Bounded batch document {index}"),
        )
        .unwrap();
    }
}

#[test]
fn recall_notes_continuation_reopens_after_cancel_without_pruning_unprocessed_sources() {
    let mut fixture = Fixture::new();
    let root = fixture.notes("Original sapphire");
    fixture
        .owner
        .refresh_root(root.id, &CancellationToken::new())
        .unwrap();
    add_files(&root);
    let first = fixture
        .owner
        .refresh_root(root.id, &CancellationToken::new())
        .unwrap();
    assert_eq!(first.inspected, 1000);
    assert!(first.pending);
    assert_eq!(
        fixture.owner.search("sapphire", None).unwrap().len(),
        1,
        "incomplete scan must not delete unprocessed source evidence"
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(fixture.owner.refresh_root(root.id, &cancel).is_err());
    assert_eq!(fixture.owner.search("sapphire", None).unwrap().len(), 1);
    fixture.reopen();
    fs::write(root.path.join("source.md"), "Updated sapphire").unwrap();
    let final_batch = fixture
        .owner
        .refresh_root(root.id, &CancellationToken::new())
        .unwrap();
    assert_eq!(
        final_batch.inspected, 1,
        "reopen must continue instead of rereading the first thousand sources"
    );
    assert!(!final_batch.pending);
    assert_eq!(
        fixture
            .owner
            .search("Updated sapphire", None)
            .unwrap()
            .len(),
        1
    );
    assert!(
        fixture
            .owner
            .search("Original sapphire", None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .owner
            .store
            .recall_root_sources(root.id)
            .unwrap()
            .len(),
        1001
    );
}

#[test]
fn recall_notes_policy_change_requires_restart_and_revocation_discards_pending_scan() {
    let mut fixture = Fixture::new();
    let root = fixture.notes("Selected nebula");
    add_files(&root);
    assert!(
        fixture
            .owner
            .refresh_root(root.id, &CancellationToken::new())
            .unwrap()
            .pending
    );
    fixture
        .owner
        .disclose_root(root.id, "new-route".into(), true)
        .unwrap();
    fixture.reopen();
    assert!(
        fixture
            .owner
            .refresh_root(root.id, &CancellationToken::new())
            .is_err()
    );
    assert!(
        fixture
            .owner
            .refresh_root_with_restart(root.id, &CancellationToken::new(), true)
            .unwrap()
            .pending
    );
    fixture.owner.revoke_root(root.id).unwrap();
    assert!(
        fixture
            .owner
            .refresh_root(root.id, &CancellationToken::new())
            .is_err()
    );
    assert!(
        fixture
            .owner
            .store
            .document_names("recall/refresh/", 32)
            .unwrap()
            .is_empty()
    );
    assert!(root.path.join("source.md").exists());
}

#[test]
fn recall_notes_removed_source_is_pruned_only_after_resumed_inventory_completes() {
    let mut fixture = Fixture::new();
    let root = fixture.notes("Selected original");
    fixture
        .owner
        .refresh_root(root.id, &CancellationToken::new())
        .unwrap();
    add_files(&root);
    assert!(
        fixture
            .owner
            .refresh_root(root.id, &CancellationToken::new())
            .unwrap()
            .pending
    );
    fs::remove_file(root.path.join("source.md")).unwrap();
    fs::write(root.path.join("newly-added.txt"), "Added later corpus").unwrap();
    fixture.reopen();
    let resumed = fixture
        .owner
        .refresh_root(root.id, &CancellationToken::new())
        .unwrap();
    assert!(!resumed.pending);
    assert_eq!(resumed.skipped, 1);
    assert_eq!(
        fixture
            .owner
            .store
            .recall_root_sources(root.id)
            .unwrap()
            .len(),
        1000
    );
    assert!(
        fixture
            .owner
            .search("Added later", None)
            .unwrap()
            .is_empty(),
        "a captured scan does not silently extend its inventory"
    );
    assert!(
        fixture
            .owner
            .refresh_root(root.id, &CancellationToken::new())
            .unwrap()
            .pending
    );
    assert!(
        !fixture
            .owner
            .refresh_root(root.id, &CancellationToken::new())
            .unwrap()
            .pending
    );
    assert_eq!(fixture.owner.search("Added later", None).unwrap().len(), 1);
}

#[test]
fn recall_notes_stale_cursor_cannot_clean_up_a_newer_scan() {
    let fixture = Fixture::new();
    let root = fixture.notes("Retain this source");
    fixture
        .owner
        .refresh_root(root.id, &CancellationToken::new())
        .unwrap();
    let generation = fixture.owner.store.privacy_generation().unwrap();
    let first = b"first bounded cursor";
    let newer = b"newer bounded cursor";
    fixture
        .owner
        .store
        .recall_refresh_checkpoint(root.id, None, Some(first), generation, None)
        .unwrap();
    fixture
        .owner
        .store
        .recall_refresh_checkpoint(root.id, Some(first), Some(newer), generation, None)
        .unwrap();
    assert!(
        fixture
            .owner
            .store
            .recall_refresh_checkpoint(root.id, Some(first), None, generation, Some(&[]))
            .is_err()
    );
    assert_eq!(fixture.owner.search("Retain", None).unwrap().len(), 1);
    assert_eq!(
        fixture
            .owner
            .store
            .document(&format!("recall/refresh/{}", root.id), 2 * 1024 * 1024)
            .unwrap()
            .as_deref(),
        Some(newer.as_slice())
    );
}
