use super::*;
use crate::{
    identity::{AgentId, SessionId, ThreadId},
    session::{
        ConversationEntry, DurableSession, RecordEnvelope, SessionRecord, apply_validated,
        validate_envelope, validate_envelope_with_compaction_proof,
    },
};
use std::collections::HashSet;

fn budget() -> PromptBudgetPlan {
    PromptBudgetPlan::derive(
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

#[test]
fn archived_prefix_compaction_requires_original_source_proof_and_absolute_positions() {
    let temporary = tempfile::tempdir().unwrap();
    let mut session = DurableSession::create(temporary.path(), temporary.path().into()).unwrap();
    for text in ["Original scope", "First correction", "Retained request"] {
        session
            .append_message(Message::text(Role::User, text))
            .unwrap();
    }
    let previous = session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    assert_eq!(previous.source_entry_count, 2);
    session
        .append_message(Message::text(Role::User, "Second correction"))
        .unwrap();
    session
        .append_message(Message::text(Role::User, "Newest request"))
        .unwrap();
    let candidate = session
        .prepare_compaction(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    let mut state = session.restored().clone();
    let mut builder = CompactionSourceProofBuilder::new(
        state.session_id,
        candidate.checkpoint.source_entry_count,
    );
    for entry in state.conversation_entry_path().unwrap() {
        builder.push(entry.id, &entry.message).unwrap();
    }
    let proof = builder.finish().unwrap();
    let removed: HashSet<_> = state
        .conversation_entry_path()
        .unwrap()
        .iter()
        .take(previous.source_entry_count)
        .map(|entry| entry.id)
        .collect();
    state.entries.retain(|id, _| !removed.contains(id));
    state.archived_prefix = Some(previous.clone());
    assert_eq!(state.retained_offset(), 2);
    assert_eq!(state.conversation_entry_path().unwrap().len(), 3);
    assert_eq!(state.active_compaction().unwrap(), Some(&previous));

    let envelope = RecordEnvelope::new(
        state.session_id,
        SessionRecord::ConversationCompacted {
            checkpoint: candidate.checkpoint.clone(),
        },
    );
    assert!(
        validate_envelope(&state, &HashSet::new(), &envelope, 20).is_err(),
        "a summary is not proof of evicted originals"
    );
    validate_envelope_with_compaction_proof(&state, &HashSet::new(), &envelope, 20, Some(&proof))
        .unwrap();
    let mut forged = candidate.checkpoint.clone();
    forged.source_digest = source_digest([(
        previous.source_start,
        &Message::text(Role::User, previous.summary.render()),
    )]);
    let forged = RecordEnvelope::new(
        state.session_id,
        SessionRecord::ConversationCompacted { checkpoint: forged },
    );
    assert!(
        validate_envelope_with_compaction_proof(&state, &HashSet::new(), &forged, 20, Some(&proof))
            .is_err()
    );
    apply_validated(&mut state, &envelope.record);
    assert_eq!(
        state
            .active_compaction()
            .unwrap()
            .unwrap()
            .source_entry_count,
        4
    );
    assert_eq!(
        state.retained_offset(),
        2,
        "the next summary does not implicitly evict more history"
    );
    crate::session::hydration::archive_compacted_prefix(&mut state).unwrap();
    assert_eq!(state.retained_offset(), 4);
    assert_eq!(state.entries.len(), 1);
    assert_eq!(state.compactions.len(), 1);

    state.head = None;
    assert!(state.active_compaction().unwrap().is_none());
    assert_eq!(state.retained_offset(), 0);
    crate::session::hydration::archive_compacted_prefix(&mut state).unwrap();
    assert!(state.archived_prefix.is_none());
    assert!(state.entries.is_empty());
    let entry = ConversationEntry {
        id: ConversationEntryId::new(),
        parent: None,
        agent_id: AgentId::new(),
        message: Message::text(Role::User, "Independent conversation after clear"),
    };
    state.head = Some(entry.id);
    state.entries.insert(entry.id, entry);
    assert!(
        state.active_compaction().unwrap().is_none(),
        "clearing cannot resurrect the old summary"
    );
}

#[test]
fn compaction_source_proof_requires_exact_count_and_original_bytes() {
    let session = SessionId::new();
    let ids = [
        ConversationEntryId::new(),
        ConversationEntryId::new(),
        ConversationEntryId::new(),
    ];
    let messages = [
        Message::text(Role::User, "original"),
        Message::text(Role::Assistant, "answer"),
        Message::text(Role::User, "retained"),
    ];
    let mut partial = CompactionSourceProofBuilder::new(session, 2);
    partial.push(ids[0], &messages[0]).unwrap();
    assert!(partial.finish().is_err());
    let mut full = CompactionSourceProofBuilder::new(session, 2);
    for (id, message) in ids.into_iter().zip(&messages) {
        full.push(id, message).unwrap();
    }
    assert!(full.push(ConversationEntryId::new(), &messages[0]).is_err());
    let proof = full.finish().unwrap();
    assert_eq!(
        proof.digest(),
        source_digest(ids[..2].iter().copied().zip(&messages[..2]))
    );
    let checkpoint = CompactionCheckpoint {
        version: COMPACTION_CHECKPOINT_VERSION,
        id: CompactionId::new(),
        operation_id: OperationId::new(),
        previous_checkpoint: None,
        reason: CompactionReason::Manual,
        source_start: ids[0],
        source_end: ids[1],
        source_entry_count: 2,
        source_digest: proof.digest().into(),
        retained_tail_start: ids[2],
        summary: CompactionSummary::default(),
        budget: budget(),
        semantic: None,
    };
    assert!(proof.matches(session, &checkpoint));
    assert!(!proof.matches(SessionId::new(), &checkpoint));
}

#[test]
fn cached_prefix_advances_to_the_same_original_byte_digest_as_full_rehash() {
    let session = SessionId::new();
    let entries = (0..7)
        .map(|index| {
            (
                ConversationEntryId::new(),
                Message::text(Role::User, format!("original {index}")),
            )
        })
        .collect::<Vec<_>>();
    let mut first = CompactionSourceProofBuilder::new(session, 2);
    for (id, message) in &entries[..3] {
        first.push(*id, message).unwrap();
    }
    let prefix = first.finish().unwrap().accumulator();
    let mut continued = CompactionSourceProofBuilder::from_prefix(&prefix, 6).unwrap();
    for (id, message) in &entries[2..] {
        continued.push(*id, message).unwrap();
    }
    let continued = continued.finish().unwrap();
    assert_eq!(
        continued.digest(),
        source_digest(entries[..6].iter().map(|(id, message)| (*id, message)))
    );
    assert!(CompactionSourceProofBuilder::from_prefix(&prefix, 2).is_err());
}

#[test]
fn archived_boundary_does_not_hide_missing_suffix_ancestry() {
    let session = SessionId::new();
    let mut state = crate::session::reduce(&[RecordEnvelope::new(
        session,
        SessionRecord::SessionCreated {
            thread_id: ThreadId::new(),
            workspace_root: "/workspace".into(),
        },
    )])
    .unwrap();
    let entry = ConversationEntry {
        id: ConversationEntryId::new(),
        parent: Some(ConversationEntryId::new()),
        agent_id: AgentId::new(),
        message: Message::text(Role::User, "broken path"),
    };
    state.head = Some(entry.id);
    state.entries.insert(entry.id, entry);
    assert!(state.conversation_entry_path().is_err());
}

#[test]
fn archive_preserves_unfinished_operation_evidence_until_it_finishes() {
    let temporary = tempfile::tempdir().unwrap();
    let mut session = DurableSession::create(temporary.path(), temporary.path().into()).unwrap();
    let input = session
        .append_message(Message::text(Role::User, "Original unresolved operation"))
        .unwrap();
    let operation = OperationId::new();
    session
        .append_record(SessionRecord::OperationAccepted {
            operation_id: operation,
            thread_id: session.thread_id(),
            input_entry_id: input,
        })
        .unwrap();
    for text in ["Intermediate task", "Newest task"] {
        session
            .append_message(Message::text(Role::User, text))
            .unwrap();
    }
    session
        .compact_conversation(OperationId::new(), CompactionReason::Manual, &budget())
        .unwrap();
    let mut state = session.restored().clone();
    crate::session::hydration::archive_compacted_prefix(&mut state).unwrap();
    assert!(state.entries.contains_key(&input));
    assert!(state.operation_details.contains_key(&operation));
    assert_eq!(state.conversation_entry_path().unwrap().len(), 1);
    state
        .operation_details
        .get_mut(&operation)
        .unwrap()
        .finished = Some(crate::native_runtime::OperationOutcome::Completed);
    state.operations.insert(
        operation,
        crate::native_runtime::OperationState::Finished(
            crate::native_runtime::OperationOutcome::Completed,
        ),
    );
    crate::session::hydration::trim_completed(&mut state);
    crate::session::hydration::archive_compacted_prefix(&mut state).unwrap();
    assert!(!state.entries.contains_key(&input));
    assert_eq!(state.entries.len(), 1);
}
