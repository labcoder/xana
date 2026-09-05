use super::*;
use crate::{
    identity::OperationId,
    message::{Message, Role},
    session::{CompactionReason, DurableSession},
    storage::{RecoveryIdentity, TestCustody},
};

fn fixture() -> (tempfile::TempDir, ProtectedStore, DurableSession) {
    let directory = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let session =
        DurableSession::create_protected(store.clone(), directory.path().into(), SessionId::new())
            .unwrap();
    (directory, store, session)
}

#[test]
fn subject_lookup_uses_bounded_record_index_work() {
    // Exercise the production SQL and exact schema/index with the linked SQLite
    // engine. This is a query-plan fixture, not a valid execution journal or a
    // substitute for the encrypted end-to-end resource probe.
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(super::super::SCHEMA).unwrap();
    connection
        .execute_batch(super::super::EXECUTION_SCHEMA)
        .unwrap();
    let tx = connection.transaction().unwrap();
    tx.execute(
        "INSERT INTO native_sessions VALUES('fixture','root','workspace',NULL,0,0,0)",
        [],
    )
    .unwrap();
    for sequence in 0..4096i64 {
        tx.execute(
            "INSERT INTO native_records VALUES('fixture',?1,?2,?3)",
            params![sequence, format!("record-{sequence}"), b"{}".as_slice()],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO native_subjects VALUES('fixture','entry',?1,?2)",
            params![format!("entry-{sequence}"), sequence],
        )
        .unwrap();
    }
    let plan = tx
        .prepare(&format!("EXPLAIN QUERY PLAN {SUBJECT_LOOKUP}"))
        .unwrap()
        .query_map(params!["fixture", 4095], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut query = tx.prepare(SUBJECT_LOOKUP).unwrap();
    let actual = query
        .query_map(params!["fixture", 4095], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let steps = query.get_status(rusqlite::StatementStatus::VmStep);
    println!(
        "subject_lookup_plan={plan:?} population=4096 returned={} vm_steps={steps}",
        actual.len()
    );
    assert_eq!(actual, vec![("entry".into(), "entry-4095".into())]);
    assert!(
        steps <= 64,
        "a single-record lookup scanned unrelated history: {steps} VM steps; {plan:?}"
    );
    assert!(
        plan.iter().any(
            |detail| detail.contains("native_subjects_record") && detail.contains("sequence=?")
        )
    );
}

#[test]
fn offline_registration_replay_rejects_duplicate_orphan_artifacts_after_eviction() {
    let (_directory, store, mut session) = fixture();
    let crate::operation::DurableValueRef::Artifact(reference) = session
        .store_json_value(serde_json::json!({"large":"evidence".repeat(10000)}))
        .unwrap()
    else {
        panic!("artifact expected")
    };
    let id = session.session_id();
    let records = store
        .history_records_for(id, super::super::HistorySubject::Artifact(reference.id))
        .unwrap();
    let sequence = store
        .history_record_sequence(id, records[0].record_id)
        .unwrap();
    store
        .with_database(|db| {
            let tx = db.connection.transaction()?;
            super::super::constraints::validate_registration_before(
                &tx,
                id,
                &records[0].record,
                Some(sequence),
            )?;
            assert!(
                super::super::constraints::validate_registration_before(
                    &tx,
                    id,
                    &records[0].record,
                    Some(sequence + 1)
                )
                .is_err()
            );
            Ok(())
        })
        .unwrap();
    store.verify_content().unwrap();
}

#[test]
fn offline_verification_proves_historical_compaction_after_current_path_is_cleared() {
    let (_directory, store, mut session) = fixture();
    for index in 0..260 {
        session
            .append_message(Message::text(Role::User, format!("Original entry {index}")))
            .unwrap();
    }
    let budget = crate::prompt::PromptBudgetPlan::derive(
        &crate::prompt::PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        crate::prompt::ModelBudgetFacts {
            connection: "fixture".into(),
            model: "fixture".into(),
            context_tokens: None,
            max_output_tokens: None,
            reasoning: false,
        },
    )
    .unwrap();
    let checkpoint = session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget)
        .unwrap();
    assert_eq!(checkpoint.source_entry_count, 259);
    session.clear_conversation().unwrap();
    session
        .append_message(Message::text(Role::User, "New independent path"))
        .unwrap();
    store.verify_content().unwrap();
    assert_eq!(
        store
            .history_metadata(session.session_id())
            .unwrap()
            .active_entries,
        1
    );
}

#[test]
fn historical_compaction_rejects_wrong_predecessor_and_inactive_source_even_with_valid_hashes() {
    let (_directory, store, mut session) = fixture();
    for index in 0..8 {
        session
            .append_message(Message::text(Role::User, format!("Source {index}")))
            .unwrap();
    }
    let budget = crate::prompt::PromptBudgetPlan::derive(
        &crate::prompt::PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        crate::prompt::ModelBudgetFacts {
            connection: "fixture".into(),
            model: "fixture".into(),
            context_tokens: None,
            max_output_tokens: None,
            reasoning: false,
        },
    )
    .unwrap();
    let first = session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget)
        .unwrap();
    for index in 8..12 {
        session
            .append_message(Message::text(Role::User, format!("Source {index}")))
            .unwrap();
    }
    let second = session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget)
        .unwrap();
    let id = session.session_id();
    let record = store
        .history_records_for(id, super::super::HistorySubject::Compaction(second.id))
        .unwrap()
        .remove(0);
    let sequence = store.history_record_sequence(id, record.record_id).unwrap();
    let head = store.history_metadata(id).unwrap().head;
    store
        .with_database(|db| {
            let tx = db.connection.transaction()?;
            assert!(
                historical_proof(&tx, id, &second, sequence, None, &mut 0)?.matches(id, &second)
            );
            verify_compaction_position(&tx, id, &second, head, sequence)?;
            let mut wrong = second.clone();
            wrong.previous_checkpoint = None;
            assert!(verify_compaction_position(&tx, id, &wrong, head, sequence).is_err());
            wrong.previous_checkpoint = Some(crate::identity::CompactionId::new());
            assert!(verify_compaction_position(&tx, id, &wrong, head, sequence).is_err());
            // Exact original bytes still exist, but the then-current head no longer
            // contains this checkpoint's retained tail. Hash validity cannot admit it.
            assert!(
                verify_compaction_position(&tx, id, &second, Some(first.source_start), sequence)
                    .is_err()
            );
            assert!(verify_compaction_position(&tx, id, &second, None, sequence).is_err());
            Ok(())
        })
        .unwrap();
    store.verify_content().unwrap();
}

fn compaction_budget() -> crate::prompt::PromptBudgetPlan {
    crate::prompt::PromptBudgetPlan::derive(
        &crate::prompt::PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        crate::prompt::ModelBudgetFacts {
            connection: "fixture".into(),
            model: "fixture".into(),
            context_tokens: None,
            max_output_tokens: None,
            reasoning: false,
        },
    )
    .unwrap()
}

fn checkpoint_sequence(
    store: &ProtectedStore,
    id: SessionId,
    checkpoint: &CompactionCheckpoint,
) -> usize {
    let record = store
        .history_records_for(id, super::super::HistorySubject::Compaction(checkpoint.id))
        .unwrap()
        .remove(0);
    store.history_record_sequence(id, record.record_id).unwrap()
}

#[test]
fn historical_prefix_reuse_reads_only_advancing_sources_and_matches_full_proof() {
    let (_directory, store, mut session) = fixture();
    for index in 0..257 {
        session
            .append_message(Message::text(
                Role::User,
                format!("Original 日本語 🦀 {index}"),
            ))
            .unwrap();
    }
    let first = session
        .compact_conversation(
            OperationId::new(),
            CompactionReason::Manual,
            &compaction_budget(),
        )
        .unwrap();
    for index in 257..386 {
        session
            .append_message(Message::text(Role::User, format!("Later evidence {index}")))
            .unwrap();
    }
    let second = session
        .compact_conversation(
            OperationId::new(),
            CompactionReason::Manual,
            &compaction_budget(),
        )
        .unwrap();
    let id = session.session_id();
    let first_sequence = checkpoint_sequence(&store, id, &first);
    let second_sequence = checkpoint_sequence(&store, id, &second);
    store.with_database(|db| {
        let tx = db.connection.transaction()?;
        let first_proof = historical_proof(&tx, id, &first, first_sequence, None, &mut 0)?;
        ensure!(first_proof.matches(id, &first), "fixture prefix differs");
        let prefix = VerifiedPrefix { checkpoint: first.clone(), accumulator: first_proof.accumulator() };
        let mut full_reads = 0;
        let full = historical_proof(&tx, id, &second, second_sequence, None, &mut full_reads)?;
        let mut incremental_reads = 0;
        let incremental = historical_proof(&tx, id, &second, second_sequence, Some(&prefix), &mut incremental_reads)?;
        assert!(full.matches(id, &second) && incremental.matches(id, &second));
        assert_eq!(full.digest(), incremental.digest());
        assert_eq!((full.start(),full.end(),full.tail()),(incremental.start(),incremental.end(),incremental.tail()));
        assert_eq!(full_reads, second.source_entry_count + 1);
        println!("historical_proof full_source_reads={full_reads} incremental_source_reads={incremental_reads}");
        assert_eq!(incremental_reads, second.source_entry_count - first.source_entry_count + 1, "verified original prefix was unnecessarily re-read");
        Ok(())
    }).unwrap();
    store.verify_content().unwrap();
}

fn compacted_fixture() -> (
    tempfile::TempDir,
    ProtectedStore,
    DurableSession,
    [CompactionCheckpoint; 2],
) {
    let (directory, store, mut session) = fixture();
    let checkpoints = std::array::from_fn(|index| {
        for source in 0..[8, 4][index] {
            session
                .append_message(Message::text(
                    Role::User,
                    format!("Batch {index} source {source}"),
                ))
                .unwrap();
        }
        session
            .compact_conversation(
                OperationId::new(),
                CompactionReason::Manual,
                &compaction_budget(),
            )
            .unwrap()
    });
    (directory, store, session, checkpoints)
}

#[test]
fn historical_prefix_reuse_falls_back_after_clear_or_cache_mismatch() {
    let (_directory, store, mut session, [first, second]) = compacted_fixture();
    let id = session.session_id();
    let first_sequence = checkpoint_sequence(&store, id, &first);
    let second_sequence = checkpoint_sequence(&store, id, &second);
    session.clear_conversation().unwrap();
    for index in 0..16 {
        session
            .append_message(Message::text(Role::User, format!("New path {index}")))
            .unwrap();
    }
    let independent = session
        .compact_conversation(
            OperationId::new(),
            CompactionReason::Manual,
            &compaction_budget(),
        )
        .unwrap();
    assert!(independent.previous_checkpoint.is_none());
    let independent_sequence = checkpoint_sequence(&store, id, &independent);
    store
        .with_database(|db| {
            let tx = db.connection.transaction()?;
            let original = historical_proof(&tx, id, &first, first_sequence, None, &mut 0)?;
            assert!(original.matches(id, &first));
            let mut prefix = VerifiedPrefix {
                checkpoint: first.clone(),
                accumulator: original.accumulator(),
            };
            for mismatch in ["checkpoint", "metadata", "clear"] {
                prefix.checkpoint = first.clone();
                if mismatch == "checkpoint" {
                    prefix.checkpoint.id = crate::identity::CompactionId::new();
                }
                if mismatch == "metadata" {
                    prefix.checkpoint.source_digest = "unverified digest".into();
                }
                let (checkpoint, sequence) = if mismatch == "clear" {
                    (&independent, independent_sequence)
                } else {
                    (&second, second_sequence)
                };
                let mut reads = 0;
                let proof =
                    historical_proof(&tx, id, checkpoint, sequence, Some(&prefix), &mut reads)?;
                assert!(
                    proof.matches(id, checkpoint),
                    "{mismatch} fallback changed source proof"
                );
                assert_eq!(
                    reads,
                    checkpoint.source_entry_count + 1,
                    "{mismatch} unexpectedly reused an ineligible prefix"
                );
            }
            Ok(())
        })
        .unwrap();
    store.verify_content().unwrap();
}

#[test]
fn historical_prefix_reuse_rejects_bad_boundaries_and_source_changes() {
    let (_directory, store, session, [first, second]) = compacted_fixture();
    let id = session.session_id();
    let first_sequence = checkpoint_sequence(&store, id, &first);
    let second_sequence = checkpoint_sequence(&store, id, &second);
    store
        .with_database(|db| {
            let tx = db.connection.transaction()?;
            let original = historical_proof(&tx, id, &first, first_sequence, None, &mut 0)?;
            assert!(original.matches(id, &first));
            let prefix = VerifiedPrefix {
                checkpoint: first.clone(),
                accumulator: original.accumulator(),
            };
            let (_, delta_sequence) = entry_metadata(&tx, id, first.retained_tail_start)?;
            let delta_record = original_record(&tx, id, delta_sequence)?;
            let mut changed = delta_record.clone();
            let SessionRecord::ConversationEntryAppended { entry } = &mut changed.record else {
                panic!("source entry")
            };
            entry.message = Message::text(Role::User, "Changed newly compacted source");
            tx.execute(
                "UPDATE native_records SET body=?1 WHERE session=?2 AND sequence=?3",
                params![
                    serde_json::to_vec(&changed)?,
                    id.to_string(),
                    i64::try_from(delta_sequence)?
                ],
            )?;
            let changed_proof =
                historical_proof(&tx, id, &second, second_sequence, Some(&prefix), &mut 0)?;
            assert!(
                !changed_proof.matches(id, &second),
                "cached prefix hid a changed new source"
            );
            tx.execute(
                "UPDATE native_records SET body=?1 WHERE session=?2 AND sequence=?3",
                params![
                    serde_json::to_vec(&delta_record)?,
                    id.to_string(),
                    i64::try_from(delta_sequence)?
                ],
            )?;
            tx.execute(
                "UPDATE native_entries SET parent=NULL WHERE session=?1 AND id=?2",
                params![id.to_string(), first.retained_tail_start.to_string()],
            )?;
            let error = historical_proof(&tx, id, &second, second_sequence, Some(&prefix), &mut 0)
                .err()
                .expect("broken cache boundary must fail");
            assert!(error.to_string().contains("prefix boundary"));
            tx.execute(
                "UPDATE native_entries SET parent=?1 WHERE session=?2 AND id=?3",
                params![
                    first.source_end.to_string(),
                    id.to_string(),
                    first.retained_tail_start.to_string()
                ],
            )?;
            let (_, old_sequence) = entry_metadata(&tx, id, first.source_start)?;
            let mut old_record = original_record(&tx, id, old_sequence)?;
            let SessionRecord::ConversationEntryAppended { entry } = &mut old_record.record else {
                panic!("original prefix entry")
            };
            entry.message =
                Message::text(Role::User, "Corrupted original prefix before validation");
            tx.execute(
                "UPDATE native_records SET body=?1 WHERE session=?2 AND sequence=?3",
                params![
                    serde_json::to_vec(&old_record)?,
                    id.to_string(),
                    i64::try_from(old_sequence)?
                ],
            )?;
            assert!(
                !historical_proof(&tx, id, &first, first_sequence, None, &mut 0)?
                    .matches(id, &first),
                "unverified original bytes cannot establish a trusted cache"
            );
            Ok(()) // Roll back these intentional corruption fixtures.
        })
        .unwrap();
    store.verify_content().unwrap();
}

#[test]
fn historical_prefix_reuse_enforces_absolute_and_advancing_count_bounds() {
    let (_directory, store, session, [first, second]) = compacted_fixture();
    let id = session.session_id();
    let first_sequence = checkpoint_sequence(&store, id, &first);
    let second_sequence = checkpoint_sequence(&store, id, &second);
    store
        .with_database(|db| {
            let tx = db.connection.transaction()?;
            let proof = historical_proof(&tx, id, &first, first_sequence, None, &mut 0)?;
            assert!(proof.matches(id, &first));
            let prefix = VerifiedPrefix {
                checkpoint: first.clone(),
                accumulator: proof.accumulator(),
            };
            for invalid_count in [
                0,
                first.source_entry_count,
                MAX_PROTECTED_RECORDS,
                usize::MAX,
            ] {
                let mut invalid = second.clone();
                invalid.source_entry_count = invalid_count;
                let mut reads = 0;
                assert!(
                    historical_proof(
                        &tx,
                        id,
                        &invalid,
                        second_sequence,
                        Some(&prefix),
                        &mut reads
                    )
                    .is_err()
                );
                assert_eq!(
                    reads, 0,
                    "invalid absolute/count range must fail before original-body reads"
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn offline_verification_detects_each_derived_index_mismatch() {
    for kind in ["subjects", "subject_overflow", "digests", "path"] {
        let (_directory, store, mut session) = fixture();
        session
            .append_message(Message::text(Role::User, "An immutable entry"))
            .unwrap();
        store.verify_content().unwrap();
        store
            .with_database(|db| {
                match kind {
                    "subjects" => {
                        db.connection.execute(
                            "UPDATE native_subjects SET subject='wrong' WHERE kind='entry'",
                            [],
                        )?;
                    }
                    "subject_overflow" => {
                        // The lookup's extra row remains an overflow sentinel;
                        // a fast plan must not turn extra index rows into success.
                        for extra in 0..130 {
                            db.connection.execute(
                                "INSERT INTO native_subjects VALUES(?1,'unexpected',?2,1)",
                                params![session.session_id().to_string(), format!("extra-{extra}")],
                            )?;
                        }
                    }
                    "digests" => {
                        db.connection.execute(
                            "UPDATE native_record_digests SET digest='wrong' WHERE sequence=0",
                            [],
                        )?;
                    }
                    "path" => {
                        db.connection
                            .execute("UPDATE native_path SET position=position+1", [])?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            })
            .unwrap();
        assert!(
            store.verify_content().is_err(),
            "{kind} mismatch was not detected"
        );
    }
}

#[test]
fn source_body_change_is_detected_even_when_sqlcipher_pages_remain_valid() {
    let (_directory, store, mut session) = fixture();
    session
        .append_message(Message::text(Role::User, "Original content"))
        .unwrap();
    store
        .with_database(|db| {
            let body: Vec<u8> = db.connection.query_row(
                "SELECT body FROM native_records WHERE sequence=1",
                [],
                |r| r.get(0),
            )?;
            let mut record: RecordEnvelope = serde_json::from_slice(&body)?;
            let SessionRecord::ConversationEntryAppended { entry } = &mut record.record else {
                panic!("entry fixture")
            };
            entry.message = Message::text(Role::User, "Changed content");
            db.connection.execute(
                "UPDATE native_records SET body=?1 WHERE sequence=1",
                [serde_json::to_vec(&record)?],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(store.verify_content().is_err());
}

#[test]
fn historical_invalid_transition_cannot_hide_behind_a_recomputed_snapshot_digest() {
    let (_directory, store, session) = fixture();
    let id = session.session_id();
    let revision = store.history_metadata(id).unwrap().revision;
    let state = DurableSession::inspect_execution_protected(&store, id).unwrap();
    store
        .append_history(
            id,
            revision,
            &RecordEnvelope::new(
                id,
                SessionRecord::OperationStateChanged {
                    operation_id: OperationId::new(),
                    state: crate::native_runtime::OperationState::Finished(
                        crate::native_runtime::OperationOutcome::Completed,
                    ),
                },
            ),
        )
        .unwrap();
    // A syntactically valid snapshot/chain is not proof that this historical
    // operation ever started. The independent exact transition replay rejects it.
    store
        .save_execution_checkpoint(id, revision + 1, &state)
        .unwrap();
    assert!(store.verify_content().is_err());
}
