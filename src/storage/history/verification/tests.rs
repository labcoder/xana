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
            assert!(historical_proof(&tx, id, &second, sequence)?.matches(id, &second));
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

#[test]
fn offline_verification_detects_each_derived_index_mismatch() {
    for kind in ["subjects", "digests", "path"] {
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
