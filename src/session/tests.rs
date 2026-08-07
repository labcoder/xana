use super::*;
use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ContentHash},
    context::persisted::{
        ContextKind, ContextRecord, ContextViewRecord, MaterializationBudget, Provenance,
        TrustClass, ViewSelector,
    },
    identity::*,
    message::{Message, Role},
    permission::{PermissionAuditFact, PermissionRequest, PermissionScope, PolicyDecision},
    runtime::{OperationOutcome, OperationState},
    tool::EffectClass,
};
use std::{fs, io::Write, path::PathBuf};
use tempfile::tempdir;

fn created(session_id: SessionId, thread_id: ThreadId) -> RecordEnvelope {
    RecordEnvelope::new(
        session_id,
        SessionRecord::SessionCreated {
            thread_id,
            workspace_root: PathBuf::from("/workspace"),
        },
    )
}

fn audit(operation_id: OperationId) -> PermissionAuditFact {
    PermissionAuditFact {
        request: PermissionRequest {
            operation_id,
            invocation_id: ToolInvocationId::new(),
            tool_name: "read_file".to_owned(),
            effect_class: EffectClass::Read,
            final_arguments: serde_json::json!({"path": "README.md"}),
            scope: PermissionScope::Unscoped,
        },
        policy_evaluation: PolicyDecision::Allow,
        controller_decision: None,
        effective: PolicyDecision::Allow,
    }
}

#[test]
fn every_v1_record_kind_round_trips_without_collapsing_audit_into_conversation() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let operation_id = OperationId::new();
    let artifact = ArtifactRecord {
        reference: ArtifactRef {
            id: ArtifactId::new(),
            content_hash: ContentHash::for_bytes(b"context"),
        },
        media_type: "text/plain".to_owned(),
        byte_len: 7,
        owner: PrincipalId::new(),
    };
    let context = ContextRecord {
        id: ContextId::new(),
        version: 1,
        artifact: artifact.reference.clone(),
        kind: ContextKind::ProjectInstructions,
        content_hash: artifact.reference.content_hash.clone(),
        logical_size: artifact.byte_len,
        provenance: Provenance::ProjectFile {
            relative_path: PathBuf::from("AGENTS.md"),
        },
        trust: TrustClass::Project,
        owner: artifact.owner,
    };
    let view = ContextViewRecord {
        id: ContextViewId::new(),
        source: context.id,
        source_version: 1,
        selector: ViewSelector::Full,
        content_hash: context.content_hash.clone(),
        budget: MaterializationBudget {
            max_bytes: 100,
            max_estimated_tokens: 25,
        },
    };
    let entry = ConversationEntry {
        id: ConversationEntryId::new(),
        parent: None,
        agent_id: AgentId::new(),
        message: Message::text(Role::User, "hello"),
    };
    let records = vec![
        SessionRecord::SessionCreated {
            thread_id,
            workspace_root: PathBuf::from("/workspace"),
        },
        SessionRecord::ConversationEntryAppended {
            entry: entry.clone(),
        },
        SessionRecord::ThreadHeadMoved {
            thread_id,
            head: Some(entry.id),
        },
        SessionRecord::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        },
        SessionRecord::PermissionAudited {
            fact: audit(operation_id),
        },
        SessionRecord::ArtifactRegistered {
            artifact: artifact.clone(),
        },
        SessionRecord::ContextRegistered {
            context: context.clone(),
        },
        SessionRecord::ContextViewRegistered { view },
        SessionRecord::NamedContextSet {
            name: "project:AGENTS.md".to_owned(),
            context_id: context.id,
            version: 1,
        },
    ];

    for record in records {
        let envelope = RecordEnvelope::new(session_id, record);
        let encoded = serde_json::to_vec(&envelope).expect("encode record");
        let decoded: RecordEnvelope = serde_json::from_slice(&encoded).expect("decode record");
        assert_eq!(decoded, envelope);
    }

    let audit_json = serde_json::to_string(&RecordEnvelope::new(
        session_id,
        SessionRecord::PermissionAudited {
            fact: audit(operation_id),
        },
    ))
    .expect("encode audit");
    assert!(audit_json.contains("permission_audited"));
    assert!(!audit_json.contains("conversation_entry_appended"));
}

#[test]
fn checked_in_v1_fixture_stays_decodable() {
    let line = include_str!("fixtures/v1-session.jsonl").trim_end();
    let envelope: RecordEnvelope = serde_json::from_str(line).expect("decode v1 fixture");

    assert_eq!(envelope.version, 1);
    assert!(matches!(
        envelope.record,
        SessionRecord::SessionCreated { .. }
    ));
}

#[test]
fn inspect_distinguishes_torn_tail_interior_corruption_and_future_version() {
    let directory = tempdir().expect("session tempdir");
    let sessions = directory.path().join("sessions");
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let store =
        SessionStore::create(&sessions, created(session_id, thread_id)).expect("create session");
    let path = store.path().to_owned();
    drop(store);
    let committed_len = fs::metadata(&path).expect("session metadata").len();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open tail")
        .write_all(b"{\"version\":1")
        .expect("write torn tail");

    let loaded = SessionStore::inspect(&path).expect("inspect torn session");
    assert_eq!(
        loaded.repair,
        Some(TornTailRepair {
            truncate_to: committed_len
        })
    );

    fs::write(&path, b"not-json\nmore\n").expect("write interior corruption");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::CorruptRecord { offset: 0, .. })
    ));

    let future =
        include_str!("fixtures/v1-session.jsonl").replacen("\"version\":1", "\"version\":2", 1);
    fs::write(&path, future).expect("write future record");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::UnsupportedVersion { version: 2 })
    ));
}

#[test]
fn inspect_is_read_only_and_resume_rechecks_before_repair() {
    let directory = tempdir().expect("session tempdir");
    let sessions = directory.path().join("sessions");
    let session_id = SessionId::new();
    let store = SessionStore::create(&sessions, created(session_id, ThreadId::new()))
        .expect("create session");
    let path = store.path().to_owned();
    drop(store);
    let before = fs::read(&path).expect("session bytes");
    let loaded = SessionStore::inspect(&path).expect("inspect session");
    assert_eq!(fs::read(&path).expect("unchanged bytes"), before);

    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open changed session")
        .write_all(b"x")
        .expect("change session");
    assert!(matches!(
        SessionStore::open_for_resume(&path, loaded),
        Err(store::SessionError::ChangedAfterInspection { .. })
    ));
}

#[test]
fn resume_restores_exact_history_and_exposes_unfinished_work_without_replay() {
    let directory = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    let mut session = DurableSession::create(
        directory.path(),
        workspace
            .path()
            .canonicalize()
            .expect("canonical workspace"),
    )
    .expect("create durable session");
    let session_id = session.session_id();
    let operation_id = OperationId::new();
    session
        .append_message(Message::text(Role::User, "persist me"))
        .expect("append conversation");
    session
        .append_operation_state(operation_id, OperationState::Running)
        .expect("append unfinished state");
    drop(session);

    let (resumed, summary) =
        DurableSession::resume(directory.path(), session_id).expect("resume durable session");

    assert_eq!(
        resumed.conversation().expect("restored conversation"),
        vec![Message::text(Role::User, "persist me")]
    );
    assert_eq!(
        summary.unfinished,
        vec![(operation_id, OperationState::Running)]
    );
}

#[test]
fn append_rejection_does_not_change_the_committed_file() {
    let directory = tempdir().expect("session tempdir");
    let session_id = SessionId::new();
    let mut store = SessionStore::create(
        &directory.path().join("sessions"),
        created(session_id, ThreadId::new()),
    )
    .expect("create session");
    let before = fs::read(store.path()).expect("committed bytes");
    let oversized = RecordEnvelope::new(
        session_id,
        SessionRecord::ConversationEntryAppended {
            entry: ConversationEntry {
                id: ConversationEntryId::new(),
                parent: None,
                agent_id: AgentId::new(),
                message: Message::text(Role::User, "x".repeat(store::MAX_RECORD_BYTES)),
            },
        },
    );

    assert!(matches!(
        store.append(&oversized),
        Err(store::SessionError::RecordTooLarge { .. })
    ));
    assert_eq!(fs::read(store.path()).expect("unchanged bytes"), before);
}

#[test]
fn empty_and_oversized_sessions_are_rejected() {
    let directory = tempdir().expect("session tempdir");
    let path = directory.path().join("empty.jsonl");
    fs::write(&path, []).expect("empty session");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::MissingCreationRecord)
    ));

    fs::write(&path, vec![b'x'; store::MAX_SESSION_BYTES + 1]).expect("oversized session");
    assert!(matches!(
        SessionStore::inspect(&path),
        Err(store::SessionError::SessionTooLarge { .. })
    ));
}

#[test]
fn reduction_derives_only_the_head_path_and_keeps_bookkeeping_separate() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let agent_id = AgentId::new();
    let operation_id = OperationId::new();
    let first = ConversationEntryId::new();
    let branch = ConversationEntryId::new();
    let records = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: ConversationEntry {
                    id: first,
                    parent: None,
                    agent_id,
                    message: Message::text(Role::User, "root"),
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ConversationEntryAppended {
                entry: ConversationEntry {
                    id: branch,
                    parent: Some(first),
                    agent_id,
                    message: Message::text(Role::Assistant, "branch"),
                },
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ThreadHeadMoved {
                thread_id,
                head: Some(branch),
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationStateChanged {
                operation_id,
                state: OperationState::Running,
            },
        ),
        RecordEnvelope::new(
            session_id,
            SessionRecord::PermissionAudited {
                fact: audit(operation_id),
            },
        ),
    ];
    let restored = reduce(&records).expect("reduce records");

    assert_eq!(
        restored.conversation_path().expect("conversation path"),
        vec![
            Message::text(Role::User, "root"),
            Message::text(Role::Assistant, "branch")
        ]
    );
    assert_eq!(restored.audits.len(), 1);
    assert_eq!(restored.operations.len(), 1);
    assert_eq!(restored.conversation_path().expect("path again").len(), 2);
}

#[test]
fn reduction_rejects_unknown_heads_and_invalid_operation_transitions() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let unknown_head = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::ThreadHeadMoved {
                thread_id,
                head: Some(ConversationEntryId::new()),
            },
        ),
    ];
    assert!(matches!(
        reduce(&unknown_head),
        Err(reduce::ReductionError::UnknownHead { .. })
    ));

    let operation_id = OperationId::new();
    let invalid = vec![
        created(session_id, thread_id),
        RecordEnvelope::new(
            session_id,
            SessionRecord::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(OperationOutcome::Completed),
            },
        ),
    ];
    assert!(matches!(
        reduce(&invalid),
        Err(reduce::ReductionError::InvalidOperationTransition { .. })
    ));
}

#[test]
fn project_context_versions_refresh_only_on_change_and_old_bytes_remain_materializable() {
    let directory = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    fs::write(workspace.path().join("AGENTS.md"), "first\nneedle one\n")
        .expect("first project context");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let mut session =
        DurableSession::create(directory.path(), workspace_root).expect("create durable session");

    session.refresh_project_context().expect("first refresh");
    let first_key = session.restored().named_context["project:AGENTS.md"];
    let first_context = session.restored().contexts[&first_key].clone();
    session
        .refresh_project_context()
        .expect("unchanged refresh");
    assert_eq!(session.restored().contexts.len(), 1);

    fs::write(workspace.path().join("AGENTS.md"), "second\nneedle two\n")
        .expect("changed project context");
    let (old_line, _) = session
        .materialize(
            &first_context,
            &ViewSelector::Lines { start: 1, end: 1 },
            MaterializationBudget {
                max_bytes: 32,
                max_estimated_tokens: 8,
            },
        )
        .expect("materialize old version");
    assert_eq!(old_line, "first");

    session.refresh_project_context().expect("changed refresh");
    let second_key = session.restored().named_context["project:AGENTS.md"];
    assert_eq!(second_key.0, first_key.0);
    assert_eq!(second_key.1, 2);

    fs::remove_file(workspace.path().join("AGENTS.md")).expect("remove live source");
    assert!(
        session
            .refresh_project_context()
            .expect("missing refresh")
            .is_empty()
    );
    assert_eq!(
        session.restored().named_context["project:AGENTS.md"],
        second_key
    );
}

#[test]
fn materialization_enforces_byte_and_token_bounds_for_search() {
    let directory = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    fs::write(
        workspace.path().join("AGENTS.md"),
        "match alpha\nignore\nmatch beta\nmatch gamma\n",
    )
    .expect("project context");
    let mut session = DurableSession::create(
        directory.path(),
        workspace
            .path()
            .canonicalize()
            .expect("canonical workspace"),
    )
    .expect("create session");
    session.refresh_project_context().expect("refresh context");
    let key = session.restored().named_context["project:AGENTS.md"];
    let context = session.restored().contexts[&key].clone();

    let (text, _) = session
        .materialize(
            &context,
            &ViewSelector::LiteralSearch {
                query: "match".to_owned(),
                max_matches: 3,
            },
            MaterializationBudget {
                max_bytes: 10,
                max_estimated_tokens: 2,
            },
        )
        .expect("bounded search");
    assert!(text.len() <= 10);
    assert!(crate::context::estimate_tokens(&text) <= 2);
}
