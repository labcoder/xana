use super::*;
use crate::{
    identity::{AgentId, ConversationEntryId, SessionId, ThreadId},
    session::{ConversationEntry, RecordEnvelope, SessionRecord, reduce},
};

#[test]
fn active_declarations_are_not_evicted_and_terminal_receipts_make_room() {
    let session = SessionId::new();
    let thread = ThreadId::new();
    let entry = ConversationEntryId::new();
    let mut records = vec![
        RecordEnvelope::new(
            session,
            SessionRecord::SessionCreated {
                thread_id: thread,
                workspace_root: "/fixture".into(),
            },
        ),
        RecordEnvelope::new(
            session,
            SessionRecord::ConversationEntryAppended {
                entry: ConversationEntry {
                    id: entry,
                    parent: None,
                    agent_id: AgentId::for_session(session),
                    message: Message::text(Role::User, "task"),
                },
            },
        ),
    ];
    let generations = (0..129).map(|_| OperationId::new()).collect::<Vec<_>>();
    let accepted = |operation| {
        let mut completion = CompletionEvidence::new(
            operation,
            WorkKind::Root,
            EvidenceOwner::Native,
            CompletionClaim::Interrupted,
            Default::default(),
        )
        .unwrap();
        completion.evaluate();
        RecordEnvelope::new(
            session,
            SessionRecord::FiniteOperationAccepted {
                operation_id: operation,
                thread_id: thread,
                input_entry_id: entry,
                completion,
            },
        )
    };
    records.extend(generations.iter().take(128).copied().map(accepted));
    let restored = reduce(&records).unwrap();
    assert_eq!(restored.completion_evidence.len(), 128);
    records.push(accepted(generations[128]));
    assert!(reduce(&records).is_err());
    records.pop();
    records.push(RecordEnvelope::new(
        session,
        SessionRecord::OperationFinished {
            operation_id: generations[0],
            outcome: crate::native_runtime::OperationOutcome::Failed,
        },
    ));
    records.push(accepted(generations[128]));
    let restored = reduce(&records).unwrap();
    assert_eq!(restored.completion_evidence.len(), 128);
    assert!(
        !restored
            .completion_evidence
            .iter()
            .any(|receipt| receipt.generation == generations[0])
    );
    assert!(
        restored
            .completion_evidence
            .iter()
            .any(|receipt| receipt.generation == generations[1])
    );
}
