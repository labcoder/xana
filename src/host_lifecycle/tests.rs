use super::*;
use crate::identity::SessionId;
use tempfile::tempdir;

fn conversation() -> ConversationRef {
    ConversationRef::Native {
        session_id: SessionId::new(),
    }
}

#[test]
fn notification_policy_is_focus_aware_redacted_and_deduplicated() {
    let settings = NotificationPolicy::default();
    let signal = AttentionSignal {
        kind: AttentionKind::Approval,
        conversation: Some(conversation()),
        operation_id: Some(OperationId::new()),
    };
    let mut planner = NotificationPlanner::new();

    assert!(
        planner
            .plan(&settings, ClientFocus::Focused, &signal)
            .is_none()
    );
    let candidate = planner
        .plan(&settings, ClientFocus::Minimized, &signal)
        .unwrap();
    assert_eq!(candidate.title, "Xana needs approval");
    assert!(!candidate.body.contains("prompt"));
    assert_eq!(candidate.conversation, signal.conversation);
    assert_eq!(candidate.operation_id, signal.operation_id);
    assert!(
        planner
            .plan(&settings, ClientFocus::Unfocused, &signal)
            .is_none()
    );
}

#[test]
fn notification_policy_honors_each_attention_switch() {
    let settings = NotificationPolicy {
        completions: false,
        ..NotificationPolicy::default()
    };
    let mut planner = NotificationPlanner::new();
    let signal = AttentionSignal {
        kind: AttentionKind::Completed,
        conversation: Some(conversation()),
        operation_id: Some(OperationId::new()),
    };
    assert!(
        planner
            .plan(&settings, ClientFocus::Minimized, &signal)
            .is_none()
    );
}

#[test]
fn last_window_choices_do_not_silently_cancel_work() {
    assert_eq!(
        last_window_effect(LastWindowChoice::KeepXanaOpen),
        LastWindowEffect::KeepOpen
    );
    assert_eq!(
        last_window_effect(LastWindowChoice::CancelAndQuit),
        LastWindowEffect::RequestShutdown
    );
    assert_eq!(
        last_window_effect(LastWindowChoice::Return),
        LastWindowEffect::NoChange
    );
}

#[test]
fn startup_recovery_is_idempotent_and_does_not_touch_published_content() {
    let root = tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(root.path().to_owned().into_os_string())).unwrap();
    let artifacts = paths.data_dir().join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let partial = artifacts.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let published = artifacts.join("a".repeat(64));
    std::fs::write(&partial, b"partial").unwrap();
    std::fs::write(&published, b"published").unwrap();

    let first = recover_startup(&paths, 2).unwrap();
    let second = recover_startup(&paths, 2).unwrap();

    assert_eq!(first.stale_exit_markers, 2);
    assert_eq!(first.artifacts.removed, 1);
    assert_eq!(second.artifacts.removed, 0);
    assert!(published.exists());
}
