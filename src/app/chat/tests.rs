use super::*;
use crate::workspace_host::{ConversationProjection, ConversationState, WorkspaceSnapshot};

#[test]
fn automatic_native_selection_preserves_latest_identity_and_busy_gate() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().canonicalize().unwrap();
    let data = directory.path().join("data");
    let host = WorkspaceHost::open(&data, &workspace).unwrap();
    assert!(
        automatic_conversation(
            &host.snapshot().unwrap(),
            "local",
            ProviderKind::Ollama,
            true
        )
        .is_err()
    );
    let session = DurableSession::create(&data, workspace).unwrap();
    let conversation = ConversationRef::Native {
        session_id: session.session_id(),
    };
    drop(session);
    assert_eq!(
        automatic_conversation(
            &host.snapshot().unwrap(),
            "local",
            ProviderKind::Ollama,
            false
        )
        .unwrap(),
        Some(conversation.clone())
    );
    let _lease = host.acquire_root(conversation).unwrap();
    assert!(
        automatic_conversation(
            &host.snapshot().unwrap(),
            "local",
            ProviderKind::Ollama,
            true
        )
        .is_err()
    );
    assert_eq!(
        automatic_conversation(
            &host.snapshot().unwrap(),
            "local",
            ProviderKind::Ollama,
            false
        )
        .unwrap(),
        None
    );
}

#[test]
fn automatic_managed_selection_uses_only_current_handle_on_exact_connection() {
    let managed = |connection: &str, selected| ConversationProjection {
        conversation: ConversationRef::Managed {
            conversation_id: ConversationId::new(),
            connection: connection.into(),
            thread_id: uuid::Uuid::new_v4().to_string(),
        },
        selected,
        state: ConversationState::Inactive,
        record_count: None,
        modified: None,
        project: None,
    };
    let retained = managed("codex", true);
    let snapshot = WorkspaceSnapshot {
        workspace: std::path::PathBuf::new(),
        workspace_id: "fixture".into(),
        active: None,
        conversations: vec![
            managed("other", true),
            managed("codex", false),
            retained.clone(),
        ],
    };
    assert_eq!(
        automatic_conversation(&snapshot, "codex", ProviderKind::Codex, true).unwrap(),
        Some(retained.conversation)
    );
    assert_eq!(
        automatic_conversation(&snapshot, "absent", ProviderKind::Codex, false).unwrap(),
        None
    );
}
