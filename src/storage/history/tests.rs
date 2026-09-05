use super::*;
use crate::{
    identity::{AgentId, ConversationEntryId, ThreadId},
    message::{ContentBlock, Message, Role},
    session::{ConversationEntry, DurableSession},
    storage::{RecoveryIdentity, TestCustody},
};

fn fixture() -> (
    tempfile::TempDir,
    ProtectedStore,
    TestCustody,
    SessionId,
    ThreadId,
) {
    let directory = tempfile::tempdir().unwrap();
    let custody = TestCustody::default();
    let store =
        ProtectedStore::initialize(directory.path(), &RecoveryIdentity::generate(), &custody)
            .unwrap();
    let session = SessionId::new();
    let thread = ThreadId::new();
    store
        .create_history(&[RecordEnvelope::new(
            session,
            SessionRecord::SessionCreated {
                thread_id: thread,
                workspace_root: directory.path().to_owned(),
            },
        )])
        .unwrap();
    (directory, store, custody, session, thread)
}

fn append_entry(
    store: &ProtectedStore,
    session: SessionId,
    parent: Option<ConversationEntryId>,
    text: &str,
    revision: &mut usize,
) -> ConversationEntryId {
    let id = ConversationEntryId::new();
    store
        .append_history(
            session,
            *revision,
            &RecordEnvelope::new(
                session,
                SessionRecord::ConversationEntryAppended {
                    entry: ConversationEntry {
                        id,
                        parent,
                        agent_id: AgentId::for_session(session),
                        message: Message::text(Role::User, text),
                    },
                },
            ),
        )
        .unwrap();
    *revision += 1;
    id
}

fn move_head(
    store: &ProtectedStore,
    session: SessionId,
    thread: ThreadId,
    head: Option<ConversationEntryId>,
    revision: &mut usize,
) {
    store
        .append_history(
            session,
            *revision,
            &RecordEnvelope::new(
                session,
                SessionRecord::ThreadHeadMoved {
                    thread_id: thread,
                    head,
                },
            ),
        )
        .unwrap();
    *revision += 1;
}

fn text(page: &crate::session::ConversationPage) -> Vec<&str> {
    page.messages
        .iter()
        .map(|message| {
            let ContentBlock::Text(text) = &message.content[0] else {
                panic!("text fixture")
            };
            text.as_str()
        })
        .collect()
}

#[test]
fn indexed_pages_preserve_rewind_branch_clear_and_reopen() {
    let (directory, store, custody, session, thread) = fixture();
    let mut revision = 1;
    let first = append_entry(&store, session, None, "first 水", &mut revision);
    move_head(&store, session, thread, Some(first), &mut revision);
    let second = append_entry(&store, session, Some(first), "second", &mut revision);
    move_head(&store, session, thread, Some(second), &mut revision);
    let third = append_entry(&store, session, Some(second), "third", &mut revision);
    move_head(&store, session, thread, Some(third), &mut revision);
    let tail = store.history_page(session, None, None, 2).unwrap();
    assert_eq!((tail.start, tail.total, tail.has_older), (1, 3, true));
    assert_eq!(text(&tail), ["second", "third"]);
    let previous = store
        .history_page(session, Some(tail.start), None, 2)
        .unwrap();
    assert_eq!(text(&previous), ["first 水"]);
    assert_eq!(
        text(&store.history_page(session, None, Some(1), 2).unwrap()),
        ["second", "third"]
    );

    move_head(&store, session, thread, Some(first), &mut revision);
    let branch = append_entry(&store, session, Some(first), "branch", &mut revision);
    move_head(&store, session, thread, Some(branch), &mut revision);
    assert_eq!(
        text(&store.history_page(session, None, None, 128).unwrap()),
        ["first 水", "branch"]
    );
    // Returning to the old suffix requires rebuilding positions, not sorting all entries.
    move_head(&store, session, thread, Some(third), &mut revision);
    assert_eq!(
        text(&store.history_page(session, None, None, 128).unwrap()),
        ["first 水", "second", "third"]
    );
    move_head(&store, session, thread, None, &mut revision);
    assert_eq!(store.history_page(session, None, None, 0).unwrap().total, 0);
    move_head(&store, session, thread, Some(branch), &mut revision);
    drop(store);
    let reopened = ProtectedStore::open(directory.path(), &custody).unwrap();
    assert_eq!(
        text(&reopened.history_page(session, None, None, 128).unwrap()),
        ["first 水", "branch"]
    );
}

#[test]
fn index_upgrade_rebuilds_only_active_ancestry_and_rejects_cycles_transactionally() {
    let (_directory, store, _custody, session, thread) = fixture();
    let mut revision = 1;
    let first = append_entry(&store, session, None, "first", &mut revision);
    let second = append_entry(&store, session, Some(first), "second", &mut revision);
    move_head(&store, session, thread, Some(second), &mut revision);
    store
        .with_database(|db| {
            db.connection.execute_batch("DROP TABLE native_path")?;
            let tx = db.connection.transaction()?;
            migrate_path_index(&tx)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        text(&store.history_page(session, None, None, 128).unwrap()),
        ["first", "second"]
    );
    store
        .with_database(|db| {
            db.connection.execute_batch("DROP TABLE native_path")?;
            db.connection.execute(
                "UPDATE native_entries SET parent=?1 WHERE session=?2 AND id=?3",
                params![second.to_string(), session.to_string(), first.to_string()],
            )?;
            let tx = db.connection.transaction()?;
            assert!(migrate_path_index(&tx).is_err());
            tx.rollback()?;
            let found: bool = db.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='native_path')",
                [],
                |row| row.get(0),
            )?;
            assert!(
                !found,
                "failed migration must not publish an incomplete index"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn pages_bound_bytes_and_reject_index_corruption_before_exposure() {
    let (_directory, store, _custody, session, thread) = fixture();
    let mut revision = 1;
    let mut parent = None;
    for _ in 0..16 {
        parent = Some(append_entry(
            &store,
            session,
            parent,
            &"水".repeat(70_000),
            &mut revision,
        ));
    }
    move_head(&store, session, thread, parent, &mut revision);
    let tail = store.history_page(session, None, None, usize::MAX).unwrap();
    assert!(tail.messages.len() < 16 && !tail.messages.is_empty());
    assert!(serde_json::to_vec(&tail.messages).unwrap().len() <= 2 * 1024 * 1024);
    let earlier = store
        .history_page(session, Some(tail.start), None, 128)
        .unwrap();
    assert_eq!(earlier.start + earlier.messages.len(), tail.start);
    store
        .with_database(|db| {
            db.connection.execute(
                "DELETE FROM native_path WHERE session=?1 AND position=14",
                [session.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(store.history_page(session, None, None, 128).is_err());
}

#[test]
fn indexed_pages_match_normal_durable_projection() {
    let directory = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = SessionId::new();
    let mut durable =
        DurableSession::create_protected(store.clone(), directory.path().to_owned(), id).unwrap();
    for index in 0..256 {
        durable
            .append_message(Message::text(
                if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                format!("message {index} 水"),
            ))
            .unwrap();
    }
    let (_, restored) = DurableSession::inspect_protected(&store, id).unwrap();
    let whole = restored.conversation_path().unwrap();
    for start in [0, 1, 128, 200, 256, usize::MAX] {
        let page = store.history_page(id, None, Some(start), 128).unwrap();
        let start = start.min(whole.len());
        assert_eq!(page.messages, whole[start..(start + 128).min(whole.len())]);
    }
}

#[test]
fn historical_queries_are_exact_bounded_and_revision_coherent() {
    let (_directory, store, _custody, session, thread) = fixture();
    let mut revision = 1;
    let entry = append_entry(&store, session, None, "one retained entry", &mut revision);
    move_head(&store, session, thread, Some(entry), &mut revision);
    let metadata = store.history_metadata(session).unwrap();
    assert_eq!(metadata.revision, revision);
    assert_eq!(metadata.head, Some(entry));
    assert_eq!(metadata.active_entries, 1);
    assert!(store.history_exists(session).unwrap());
    assert!(!store.history_exists(SessionId::new()).unwrap());
    let records = store
        .history_records_for(session, HistorySubject::Entry(entry))
        .unwrap();
    assert_eq!(records.len(), 1);
    assert!(
        store
            .history_records_for(session, HistorySubject::Entry(ConversationEntryId::new()))
            .unwrap()
            .is_empty()
    );
    let mut seen = 0;
    let observed = store
        .visit_history(session, |sequence, record| {
            assert_eq!(sequence, seen);
            assert_eq!(record.session_id, session);
            seen += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!((observed.revision, seen), (revision, revision));
    store
        .with_database(|db| {
            db.connection.execute(
                "UPDATE native_subjects SET subject=?1 WHERE session=?2",
                params![ConversationEntryId::new().to_string(), session.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    // Metadata-only lookup cannot manufacture a selected historical object;
    // whole verification separately checks the complete derived index.
    assert!(
        store
            .history_records_for(session, HistorySubject::Entry(entry))
            .unwrap()
            .is_empty()
    );
}

/// Deliberately stays inside the production journal cap; this does not claim
/// that a synthetic oversized SQLite table is a supported 100k Conversation.
#[test]
#[ignore = "opt-in release-profile protected history paging measurement"]
fn protected_history_paging_probe() {
    use std::time::Instant;
    for count in [128, 2_048, 8_192] {
        let directory = tempfile::tempdir().unwrap();
        let custody = TestCustody::default();
        let store =
            ProtectedStore::initialize(directory.path(), &RecoveryIdentity::generate(), &custody)
                .unwrap();
        let session = SessionId::new();
        let thread = ThreadId::new();
        let mut records = vec![RecordEnvelope::new(
            session,
            SessionRecord::SessionCreated {
                thread_id: thread,
                workspace_root: directory.path().to_owned(),
            },
        )];
        let mut parent = None;
        for index in 0..count {
            let id = ConversationEntryId::new();
            records.push(RecordEnvelope::new(
                session,
                SessionRecord::ConversationEntryAppended {
                    entry: ConversationEntry {
                        id,
                        parent,
                        agent_id: AgentId::for_session(session),
                        message: Message::text(
                            Role::User,
                            format!(
                                "{index}: {}",
                                "🦀水 text ".repeat(if index % 251 == 0 {
                                    512
                                } else {
                                    4 + index % 32
                                })
                            ),
                        ),
                    },
                },
            ));
            parent = Some(id);
        }
        records.push(RecordEnvelope::new(
            session,
            SessionRecord::ThreadHeadMoved {
                thread_id: thread,
                head: parent,
            },
        ));
        store.create_history(&records).unwrap();
        drop(records);
        drop(store);
        for trial in 0..5 {
            let opened = Instant::now();
            let store = ProtectedStore::open(directory.path(), &custody).unwrap();
            let open_us = opened.elapsed().as_micros();
            let cold = Instant::now();
            let page = store.history_page(session, None, None, 128).unwrap();
            let first_page_us = cold.elapsed().as_micros();
            assert_eq!(page.total, count);
            assert!(page.messages.len() <= 128);
            let mut samples = Vec::new();
            for index in 0..25 {
                let start = [0, count / 2, count.saturating_sub(128)][index % 3];
                let timer = Instant::now();
                let page = store.history_page(session, None, Some(start), 128).unwrap();
                samples.push(timer.elapsed().as_micros());
                assert_eq!(page.start, start);
                assert_eq!(page.messages.len(), (count - start).min(128));
            }
            samples.sort_unstable();
            println!(
                "protected_history messages={count} trial={trial} open_us={open_us} first_page_us={first_page_us} warm_median_us={} warm_p95_us={} retained_page_rows={} retained_page_json_bytes={}",
                samples[samples.len() / 2],
                samples[23],
                page.messages.len(),
                serde_json::to_vec(&page.messages).unwrap().len()
            );
        }
    }
}
