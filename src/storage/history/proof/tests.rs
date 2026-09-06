use super::*;
use crate::{
    identity::{AgentId, ThreadId},
    message::{Message, Role},
    session::ConversationEntry,
    storage::{RecoveryIdentity, TestCustody},
};

fn fixture(count: usize, text_bytes: usize) -> (tempfile::TempDir, ProtectedStore, SessionId) {
    let directory = tempfile::tempdir().unwrap();
    let home = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let session = SessionId::new();
    let thread = ThreadId::new();
    let mut records = vec![RecordEnvelope::new(
        session,
        SessionRecord::SessionCreated {
            thread_id: thread,
            workspace_root: directory.path().into(),
        },
    )];
    let mut parent = None;
    for index in 0..count {
        let entry = ConversationEntry {
            id: ConversationEntryId::new(),
            parent,
            agent_id: AgentId::for_session(session),
            message: Message::text(
                Role::User,
                format!("Original {index} {}", "x".repeat(text_bytes)),
            ),
        };
        parent = Some(entry.id);
        records.push(RecordEnvelope::new(
            session,
            SessionRecord::ConversationEntryAppended { entry },
        ));
        records.push(RecordEnvelope::new(
            session,
            SessionRecord::ThreadHeadMoved {
                thread_id: thread,
                head: parent,
            },
        ));
    }
    home.create_history(&records).unwrap();
    (directory, home, session)
}

fn begin(home: &ProtectedStore, session: SessionId, count: usize) -> ActivePrefixProofCursor {
    let metadata = home.history_metadata(session).unwrap();
    home.begin_active_prefix_proof(
        session,
        count,
        metadata.revision,
        metadata.head,
        home.privacy_generation().unwrap(),
    )
    .unwrap()
}

#[test]
fn original_proof_pages_bound_reads_release_database_and_equal_full_proof() {
    let (_directory, home, session) = fixture(387, 8);
    let full = home.active_prefix_proof(session, 386).unwrap();
    let mut cursor = begin(&home, session, 386);
    assert_eq!(cursor.progress(), (0, 387));
    assert!(!cursor.is_ready());
    let mut page_reads = Vec::new();
    while !cursor.is_ready() {
        let before = cursor.progress().0;
        cursor = cursor.advance().unwrap();
        page_reads.push(cursor.progress().0 - before);
        // This would deadlock if the continuation retained the store mutex.
        assert_eq!(
            home.history_page(session, None, None, 1).unwrap().total,
            387
        );
    }
    assert_eq!(page_reads, [128, 128, 128, 3]);
    let paged = cursor.finish().unwrap();
    assert_eq!(
        (paged.start(), paged.end(), paged.tail(), paged.digest()),
        (full.start(), full.end(), full.tail(), full.digest())
    );
}

#[test]
fn proof_pages_honor_aggregate_bytes_and_drop_without_writing() {
    let (_directory, home, session) = fixture(40, 128 * 1024);
    let revision = home.history_metadata(session).unwrap().revision;
    let cursor = begin(&home, session, 39).advance().unwrap();
    assert_eq!(
        cursor.progress(),
        (15, 40),
        "encoded record overhead prevents a sixteenth128KiB body fitting2MiB"
    );
    assert!(
        cursor.finish().is_err(),
        "partial original hashing is never a candidate"
    );
    assert_eq!(home.history_metadata(session).unwrap().revision, revision);
    drop(begin(&home, session, 39).advance().unwrap());
    assert_eq!(home.history_metadata(session).unwrap().revision, revision);
}

#[test]
fn proof_pages_reject_history_and_privacy_changes_before_more_reads() {
    let (_directory, home, session) = fixture(387, 8);
    let cursor = begin(&home, session, 386).advance().unwrap();
    let metadata = home.history_metadata(session).unwrap();
    home.append_history(
        session,
        metadata.revision,
        &RecordEnvelope::new(
            session,
            SessionRecord::ThreadHeadMoved {
                thread_id: metadata.thread_id,
                head: None,
            },
        ),
    )
    .unwrap();
    assert!(
        cursor
            .advance()
            .err()
            .unwrap()
            .to_string()
            .contains("history changed")
    );

    let (_directory, home, session) = fixture(387, 8);
    let cursor = begin(&home, session, 386).advance().unwrap();
    home.with_database(|db| {
        db.connection.execute(
            "UPDATE privacy_generation SET revision=revision+1 WHERE singleton=1",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        cursor
            .advance()
            .err()
            .unwrap()
            .to_string()
            .contains("eligibility changed")
    );
}

#[test]
fn proof_pages_reject_corrupt_boundary_originals_and_oversized_records() {
    for oversized in [false, true] {
        let (_directory, home, session) = fixture(387, 8);
        let cursor = begin(&home, session, 386).advance().unwrap();
        home.with_database(|db| {
            let (sequence, body): (i64, Vec<u8>) = db.connection.query_row(
                "SELECT r.sequence,r.body FROM native_path p JOIN native_records r ON r.session=p.session AND r.sequence=p.sequence WHERE p.session=?1 AND p.position=128", [session.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let mut record: RecordEnvelope = serde_json::from_slice(&body)?;
            let SessionRecord::ConversationEntryAppended { entry } = &mut record.record else { panic!("entry fixture") };
            if oversized {
                entry.message = Message::text(Role::User, "x".repeat(MAX_RECORD_BYTES));
            } else {
                entry.parent = None;
            }
            db.connection.execute("UPDATE native_records SET body=?3 WHERE session=?1 AND sequence=?2", params![session.to_string(), sequence, serde_json::to_vec(&record)?])?;
            Ok(())
        }).unwrap();
        let error = cursor.advance().err().unwrap().to_string();
        assert!(
            error.contains(if oversized {
                "record bound"
            } else {
                "ancestry differs"
            }),
            "{error}"
        );
    }
}

#[test]
fn proof_finalization_rechecks_quarantine_and_begin_rejects_invalid_snapshot() {
    let (_directory, home, session) = fixture(3, 8);
    let metadata = home.history_metadata(session).unwrap();
    let generation = home.privacy_generation().unwrap();
    for (count, revision, head) in [
        (0, metadata.revision, metadata.head),
        (3, metadata.revision, metadata.head),
        (2, metadata.revision - 1, metadata.head),
        (2, metadata.revision, None),
    ] {
        assert!(
            home.begin_active_prefix_proof(session, count, revision, head, generation)
                .is_err()
        );
    }
    let cursor = begin(&home, session, 2).advance().unwrap();
    assert!(cursor.is_ready());
    home.with_database(|db| {
        // Hold generation unchanged deliberately: the final source-eligibility
        // predicate must still reject quarantine, not merely compare a counter.
        db.connection.execute(
            "INSERT INTO excluded_sources(conversation,reason,at) VALUES(?1,'fixture',0)",
            [session.to_string()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        cursor
            .finish()
            .err()
            .unwrap()
            .to_string()
            .contains("eligibility changed")
    );
}

#[test]
fn cloned_disclosure_guard_rechecks_live_custody_without_retaining_a_lock() {
    let (_directory, home, session) = fixture(3, 8);
    let metadata = home.history_metadata(session).unwrap();
    let guard = home
        .compaction_disclosure_guard(
            session,
            2,
            metadata.revision,
            metadata.head,
            home.privacy_generation().unwrap(),
        )
        .unwrap();
    let cloned = guard.clone();
    guard.recheck().unwrap();
    cloned.recheck().unwrap();
    assert_eq!(home.history_page(session, None, None, 1).unwrap().total, 3);
    home.lock().unwrap();
    assert!(guard.recheck().is_err());
    assert!(
        cloned.recheck().is_err(),
        "a cloned guard is not independently authorized custody"
    );
}
