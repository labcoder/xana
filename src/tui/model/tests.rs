use super::*;
use crate::{
    identity::{SessionId, StepId},
    runtime::OperationOutcome,
    workspace_host::{ConversationProjection, ConversationState, WorkspaceSnapshot},
};

#[test]
fn composer_edits_unicode_multiline_and_selection_safely() {
    let mut composer = Composer::new();
    composer.insert("one\ntwø").unwrap();
    composer.move_cursor(MoveDirection::Left, true);
    assert!(composer.selection().is_some());
    composer.insert("o").unwrap();
    assert_eq!(composer.text, "one\ntwo");
    composer.move_cursor(MoveDirection::Home, false);
    assert_eq!(composer.cursor, 4);
    composer.move_cursor(MoveDirection::Up, false);
    assert_eq!(composer.cursor, 0);
}

#[test]
fn paste_is_previewed_sanitized_and_never_executed() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.update_input(InputAction::Paste("/quit\u{1b}[31m\r\ntext".to_owned()));
    assert!(matches!(state.overlay, Some(Overlay::PastePreview { .. })));
    state.update_input(InputAction::Confirm);
    assert_eq!(state.composer.text, "/quit[31m\ntext");
    assert!(!matches!(
        state.update_input(InputAction::Cancel),
        UpdateEffect::Quit
    ));
}

#[test]
fn busy_submissions_queue_in_order_and_can_be_edited_or_removed() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = true;
    for input in ["first", "second", "third"] {
        state.update_input(InputAction::Insert(input.to_owned()));
        assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    }
    assert_eq!(state.followups.len(), 3);
    state.composer.replace("/queue remove 2".to_owned());
    state.update_input(InputAction::Submit);
    state.composer.replace("/queue edit 1".to_owned());
    state.update_input(InputAction::Submit);
    assert_eq!(state.composer.text, "first");
    assert_eq!(state.followups.front().unwrap().input, "third");
}

#[test]
fn interrupt_and_steer_are_distinct_and_capability_gated() {
    let operation_id = OperationId::new();
    let mut native = TuiState::starting(ComposerPreset::Submit);
    native.busy = true;
    native.active_operation = Some(operation_id);
    assert_eq!(
        native.update_input(InputAction::CopyOrInterrupt),
        UpdateEffect::Interrupt { operation_id }
    );
    native.composer.replace("/steer focus".to_owned());
    assert_eq!(native.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(native.status.contains("does not support"));

    let mut managed =
        TuiState::starting(ComposerPreset::Submit).with_capabilities(OwnerCapabilities::managed());
    managed.busy = true;
    managed.active_operation = Some(operation_id);
    managed.composer.replace("/steer focus".to_owned());
    assert_eq!(
        managed.update_input(InputAction::Submit),
        UpdateEffect::None
    );
    assert!(managed.status.contains("does not support"));
}

#[test]
fn input_and_runtime_events_follow_one_explicit_update_path() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.update_input(InputAction::Insert("hello".to_owned()));
    let UpdateEffect::Submit {
        operation_id,
        input,
        images,
    } = state.update_input(InputAction::Submit)
    else {
        panic!("submit effect");
    };
    assert_eq!(input, "hello");
    assert!(images.is_empty());
    state.mark_submitted(operation_id, input);
    state.apply_runtime(&AgentEvent::AssistantTextDelta {
        operation_id,
        step_id: StepId::new(),
        text: "hi".to_owned(),
    });
    state.apply_runtime(&AgentEvent::AssistantMessage {
        operation_id,
        message: Message::text(Role::Assistant, "hi there"),
    });
    state.apply_runtime(&AgentEvent::OperationStateChanged {
        operation_id,
        state: OperationState::Finished(OperationOutcome::Completed),
    });
    assert!(!state.busy);
    assert_eq!(state.messages.back().unwrap().text, "hi there");
}

#[test]
fn composer_and_retained_views_are_bounded() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.update_input(InputAction::Insert("x".repeat(MAX_INPUT_BYTES + 1)));
    assert!(state.composer.text.is_empty());
    assert!(state.status.contains("limit"));
    for index in 0..(MAX_ACTIVITY + 20) {
        state.push_activity(format!("event {index}"));
    }
    assert_eq!(state.activity.len(), MAX_ACTIVITY);
}

#[test]
fn hidden_activity_cannot_hide_or_duplicate_a_native_approval() {
    let operation_id = OperationId::new();
    let invocation_id = ToolInvocationId::new();
    let request = crate::permission::PermissionRequest {
        operation_id,
        invocation_id,
        tool_name: "run_command".to_owned(),
        effect_class: crate::tool::EffectClass::Execute,
        final_arguments: serde_json::json!({"command": "cargo test"}),
        scope: crate::permission::PermissionScope::Unscoped,
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.activity_visibility = ActivityVisibility::Hidden;
    state.apply_runtime(&AgentEvent::PermissionRequested {
        request: request.clone(),
    });
    assert!(matches!(state.overlay, Some(Overlay::Approval { .. })));
    assert!(
        state
            .activity
            .back()
            .is_some_and(|card| card.kind == ActivityKind::Approval)
    );
    assert_eq!(
        state.update_input(InputAction::Confirm),
        UpdateEffect::DecideNativeApproval {
            operation_id,
            invocation_id,
            decision: ControllerDecision::AllowOnce,
        }
    );
    assert_eq!(state.update_input(InputAction::Confirm), UpdateEffect::None);
    assert_eq!(state.activity_visibility, ActivityVisibility::Hidden);
}

#[test]
fn fake_codex_transcript_preserves_reasoning_and_managed_ownership() {
    let mut state = TuiState::from_managed(
        "codex".to_owned(),
        "gpt-test".to_owned(),
        "thread-test".to_owned(),
        ComposerPreset::Submit,
        ActivityVisibility::Auto,
        ConversationRef::NewManaged {
            connection: "codex".to_owned(),
        },
    );
    state.apply_managed_event(&ManagedClientEvent::ReasoningSummaryDelta(
        "checking the workspace".to_owned(),
    ));
    state.apply_managed_event(&ManagedClientEvent::ItemStarted(
        crate::frontend::ManagedClientItem {
            id: "command-1".to_owned(),
            kind: "commandExecution".to_owned(),
            status: Some("inProgress".to_owned()),
            label: "cargo test".to_owned(),
            details: "running tests".to_owned(),
        },
    ));
    state.apply_managed_event(&ManagedClientEvent::AssistantDelta(
        "Tests are passing.".to_owned(),
    ));
    assert!(state.auto_activity_open);
    assert!(
        state
            .activity
            .iter()
            .any(|card| { card.kind == ActivityKind::ReasoningSummary && card.owner == "Codex" })
    );
    assert!(
        state
            .activity
            .iter()
            .any(|card| { card.kind == ActivityKind::Tool && card.identity == "command-1" })
    );
    assert_eq!(state.messages.back().unwrap().kind, MessageKind::Assistant);
}

#[test]
fn pointer_actions_preserve_typed_selection_and_activation() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.composer.text = "one\ntwo".to_owned();
    state.composer.cursor = state.composer.text.len();
    assert_eq!(
        state.update_input(InputAction::PlaceCursor {
            line: 1,
            column: 1,
            width: 20,
            scroll: 0,
            select: false,
        }),
        UpdateEffect::None
    );
    assert_eq!(state.composer.cursor, 5);
    assert_eq!(state.composer.selection(), None);

    state.open_model_picker(vec!["first".to_owned(), "second".to_owned()]);
    assert_eq!(
        state.update_input(InputAction::ChooseOverlay(1)),
        UpdateEffect::SelectModel("second".to_owned())
    );

    let conversation = ConversationRef::NewManaged {
        connection: "codex".to_owned(),
    };
    assert_eq!(
        state.update_input(InputAction::ViewSession(conversation.clone())),
        UpdateEffect::ViewSession(conversation)
    );
    let expanded = state.activity[0].expanded;
    assert_eq!(
        state.update_input(InputAction::ToggleActivity(0)),
        UpdateEffect::None
    );
    assert_ne!(state.activity[0].expanded, expanded);
}

#[test]
fn typing_collapses_the_startup_header_and_header_command_reopens_it() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    assert!(state.header_expanded);

    state.update_input(InputAction::Insert("h".to_owned()));
    assert!(!state.header_expanded);
    state.composer.replace("/header".to_owned());
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(state.header_expanded);
}

#[test]
fn session_and_activity_commands_use_consistent_view_verbs_and_exact_archive_ids() {
    let runtime = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let archived = ConversationRef::Managed {
        connection: "codex".to_owned(),
        thread_id: "thread-to-archive".to_owned(),
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.runtime_conversation = runtime.clone();
    state.viewed_conversation = runtime.clone();
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        conversations: vec![
            ConversationProjection {
                conversation: runtime,
                state: ConversationState::Controlled,
                record_count: Some(1),
                modified: None,
                selected: true,
            },
            ConversationProjection {
                conversation: archived.clone(),
                state: ConversationState::Inactive,
                record_count: None,
                modified: None,
                selected: false,
            },
        ],
        active: None,
    });

    state
        .composer
        .replace("/sessions archive thread-to-archive".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::ArchiveConversation(archived)
    );

    state.composer.replace("/sessions view hide".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::PersistRail(false)
    );
    assert!(!state.rail_expanded);

    state.composer.replace("/activity view show".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::PersistActivity(ActivityPaneChoice::Open)
    );
    assert_eq!(state.activity_visibility, ActivityVisibility::Open);

    let mut idle = TuiState::starting(ComposerPreset::Submit);
    idle.busy = false;
    idle.composer.replace("/sessions new".to_owned());
    assert_eq!(
        idle.update_input(InputAction::Submit),
        UpdateEffect::NewConversation
    );

    idle.busy = true;
    idle.composer.replace("/sessions new".to_owned());
    assert_eq!(idle.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(idle.status.contains("active turn"));
}

#[test]
fn clicking_the_sessions_title_persists_the_hidden_state() {
    let mut state = TuiState::starting(ComposerPreset::Submit);

    assert_eq!(
        state.update_input(InputAction::ToggleSessionsView),
        UpdateEffect::PersistRail(false)
    );
    assert!(!state.rail_expanded);
    assert!(state.status.contains("/sessions view show"));
}

#[test]
fn mouse_wheel_moves_the_command_palette_selection() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.update_input(InputAction::OpenPalette);

    state.update_input(InputAction::Scroll(4));
    assert!(matches!(
        state.overlay,
        Some(Overlay::Palette { selected: 4, .. })
    ));

    state.update_input(InputAction::Scroll(-2));
    assert!(matches!(
        state.overlay,
        Some(Overlay::Palette { selected: 2, .. })
    ));
}

#[test]
fn streaming_at_bottom_and_scrolled_history_preserve_the_expected_anchor() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.messages.clear();
    for index in 0..20 {
        state.push_message(MessageKind::Assistant, format!("message {index}"));
    }
    assert_eq!(state.scroll, 0);
    state.update_input(InputAction::Scroll(-6));
    assert_eq!(state.scroll, 6);
    state.push_message(MessageKind::Assistant, "late message");
    assert_eq!(state.scroll, 9, "distance from the newest edge is anchored");
    state.update_input(InputAction::Scroll(9));
    assert_eq!(state.scroll, 0);
    state.push_message(MessageKind::Assistant, "newest message");
    assert_eq!(state.scroll, 0, "the newest edge stays anchored");
}

#[test]
fn session_inspection_keeps_the_runtime_transcript_and_draft_separate() {
    let runtime = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let other = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = true;
    state.runtime_conversation = runtime.clone();
    state.viewed_conversation = runtime.clone();
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        conversations: vec![
            ConversationProjection {
                conversation: runtime.clone(),
                state: ConversationState::Controlled,
                record_count: Some(5),
                modified: None,
                selected: false,
            },
            ConversationProjection {
                conversation: other.clone(),
                state: ConversationState::Inactive,
                record_count: Some(3),
                modified: None,
                selected: false,
            },
        ],
        active: None,
    });

    state.view_session(
        other.clone(),
        Some(vec![Message::text(Role::User, "retained history")]),
    );
    state.update_input(InputAction::Insert("local draft".to_owned()));
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert_eq!(state.composer.text, "local draft");

    let operation_id = OperationId::new();
    state.active_operation = Some(operation_id);
    state.apply_runtime(&AgentEvent::AssistantMessage {
        operation_id,
        message: Message::text(Role::Assistant, "background result"),
    });
    assert_eq!(state.messages.back().unwrap().text, "retained history");
    assert!(
        state
            .sessions
            .iter()
            .find(|row| row.conversation == runtime)
            .unwrap()
            .unread
    );

    state.view_session(runtime, None);
    assert_eq!(state.messages.back().unwrap().text, "background result");
}
