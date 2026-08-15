use super::*;
use crate::{
    identity::{SessionId, StepId},
    native_runtime::OperationOutcome,
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
fn pasted_image_path_is_staged_as_a_drop_without_inserting_text() {
    let mut state = TuiState::starting(ComposerPreset::Submit);

    assert_eq!(
        state.update_input(InputAction::Paste(
            "\"screenshots/example image.PNG\"".to_owned()
        )),
        UpdateEffect::AttachDropped("screenshots/example image.PNG".to_owned())
    );
    assert!(state.composer.text.is_empty());
    assert!(state.overlay.is_none());

    assert_eq!(
        state.update_input(InputAction::Paste(
            "screenshots/example.png is relevant".to_owned()
        )),
        UpdateEffect::None
    );
    assert!(matches!(state.overlay, Some(Overlay::PastePreview { .. })));
}

#[test]
fn vision_command_selects_or_clears_the_next_turn_route() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.composer.replace("/vision describe".to_owned());

    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert_eq!(state.pending_vision_route.as_deref(), Some("describe"));
    assert!(state.status.contains("next image turn"));

    state.composer.replace("/vision auto".to_owned());
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(state.pending_vision_route.is_none());
}

#[test]
fn a_message_containing_two_image_paths_requests_both_automatic_attachments() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.composer.replace(
        r#"C:\Users\xana\Downloads\first.png C:\Users\xana\Downloads\second.jpg compare these"#
            .to_owned(),
    );

    let effect = state.update_input(InputAction::Submit);

    assert!(matches!(
        effect,
        UpdateEffect::AttachAndSubmit { input, paths, approved_external: false, .. }
            if input.ends_with("compare these")
                && paths == vec![
                    r#"C:\Users\xana\Downloads\first.png"#.to_owned(),
                    r#"C:\Users\xana\Downloads\second.jpg"#.to_owned(),
                ]
    ));
}

#[test]
fn too_many_implicit_image_paths_restore_the_complete_draft() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    let input = (0..=MAX_IMAGES_PER_TURN)
        .map(|index| format!("image-{index}.png"))
        .collect::<Vec<_>>()
        .join(" ");
    state.composer.replace(input.clone());

    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert_eq!(state.composer.text, input);
    assert!(state.status.contains("At most 8"));
}

#[test]
fn external_image_approval_can_continue_or_restore_the_draft() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.request_external_image_approval(
        OperationId::new(),
        "please inspect it".to_owned(),
        vec!["C:\\outside\\photo.png".to_owned()],
        vec!["C:\\outside\\photo.png".to_owned()],
    );

    let effect = state.update_input(InputAction::Confirm);

    assert!(matches!(
        effect,
        UpdateEffect::AttachAndSubmit {
            paths,
            approved_external: true,
            ..
        } if paths == vec!["C:\\outside\\photo.png".to_owned()]
    ));
}

#[test]
fn work_indicator_advances_only_for_an_active_turn() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    assert!(!state.advance_work_indicator());

    state.busy = true;
    state.active_operation = Some(OperationId::new());
    assert!(state.advance_work_indicator());
    assert_eq!(state.work_indicator_frame, 1);

    state.busy = false;
    assert!(!state.advance_work_indicator());
    assert_eq!(state.work_indicator_frame, 1);
}

#[test]
fn failed_followup_attachment_restores_its_draft_without_detaching_the_active_turn() {
    let operation_id = OperationId::new();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = true;
    state.active_operation = Some(operation_id);

    state.restore_auto_attachment_draft(
        "follow-up with image".to_owned(),
        "image is unavailable".to_owned(),
    );

    assert_eq!(state.composer.text, "follow-up with image");
    assert_eq!(state.active_operation, Some(operation_id));
    assert!(state.busy);
}

#[test]
fn cancelling_external_image_approval_restores_the_draft() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.request_external_image_approval(
        OperationId::new(),
        "please inspect it".to_owned(),
        vec!["C:\\outside\\photo.png".to_owned()],
        vec!["C:\\outside\\photo.png".to_owned()],
    );

    assert_eq!(state.update_input(InputAction::Cancel), UpdateEffect::None);

    assert_eq!(state.composer.text, "please inspect it");
    assert!(state.status.contains("draft restored"));
    assert!(state.overlay.is_none());
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
        vision_route,
    } = state.update_input(InputAction::Submit)
    else {
        panic!("submit effect");
    };
    assert_eq!(input, "hello");
    assert!(images.is_empty());
    assert!(vision_route.is_none());
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
fn committed_tool_requests_and_results_are_visible_without_restart() {
    let operation_id = OperationId::new();
    let invocation_id = ToolInvocationId::new();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.active_operation = Some(operation_id);
    state.apply_runtime(&AgentEvent::AssistantMessage {
        operation_id,
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(crate::message::ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "README.md"}),
            })],
        },
    });
    state.apply_runtime(&AgentEvent::ToolFinished {
        operation_id,
        invocation_id,
        result: Message::tool_result(crate::message::ToolResult::success(
            "call-1",
            "file contents",
        )),
    });

    let visible = state
        .messages
        .iter()
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["[tool call: read_file]", "file contents"]);
    let activity = state.activity.back().expect("tool activity");
    assert_eq!(activity.state, ActivityState::Complete);
    assert!(activity.detail.contains("file contents"));
}

#[test]
fn native_provider_reasoning_is_bounded_and_accumulates_in_activity() {
    let operation_id = OperationId::new();
    let step_id = StepId::new();
    let mut state = TuiState::starting(ComposerPreset::Submit);

    for text in ["checking ", "the image"] {
        state.apply_runtime(&AgentEvent::ProviderReasoningDelta {
            operation_id,
            step_id,
            text: text.to_owned(),
        });
    }

    let card = state
        .activity
        .iter()
        .find(|card| card.kind == ActivityKind::ReasoningRaw)
        .expect("native reasoning card");
    assert_eq!(card.detail, "checking the image");
    assert!(!card.expanded);
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
        outbound_review: None,
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
fn native_approval_uses_user_facing_authority_and_scope_details() {
    let request = crate::permission::PermissionRequest {
        operation_id: OperationId::new(),
        invocation_id: ToolInvocationId::new(),
        tool_name: "run_command".to_owned(),
        effect_class: crate::tool::EffectClass::Execute,
        final_arguments: serde_json::json!({
            "command": "cargo test",
            "cwd": "."
        }),
        scope: crate::permission::PermissionScope::Command {
            shell: "PowerShell (powershell.exe)".to_owned(),
            canonical_cwd: std::path::PathBuf::from("C:\\workspace"),
            command: "cargo test".to_owned(),
        },
        outbound_review: None,
    };

    let prompt = ApprovalPrompt::native(request);

    assert_eq!(prompt.owner, "this Xana conversation");
    assert_eq!(prompt.title, "Run command");
    assert!(
        prompt
            .details
            .iter()
            .any(|line| line == "Command: cargo test")
    );
    assert!(
        prompt
            .details
            .iter()
            .any(|line| line == "Working directory: C:\\workspace")
    );
    assert!(!prompt.details.iter().any(|line| line.contains("Command {")));
    assert!(
        !prompt
            .details
            .iter()
            .any(|line| line.contains("{\"command\""))
    );
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
fn activity_detail_overlay_scrolls_selects_and_copies_without_touching_the_runtime() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.activity.clear();
    state.activity.push_back(ActivityCard::new(
        "Xana",
        "reasoning",
        ActivityKind::ReasoningSummary,
        ActivityState::Complete,
        "checked the current configuration",
        "first detail line\nsecond detail line",
    ));

    assert_eq!(
        state.update_input(InputAction::OpenActivityDetail(0)),
        UpdateEffect::None
    );
    assert!(matches!(
        state.overlay,
        Some(Overlay::ActivityDetail { scroll: 0, .. })
    ));
    assert_eq!(
        state.update_input(InputAction::Scroll(3)),
        UpdateEffect::None
    );
    assert!(matches!(
        state.overlay,
        Some(Overlay::ActivityDetail { scroll: 3, .. })
    ));

    let start = ScreenPoint { column: 2, row: 3 };
    let end = ScreenPoint { column: 8, row: 3 };
    state.update_input(InputAction::BeginActivitySelection(start));
    state.update_input(InputAction::ExtendActivitySelection(end));
    state.update_input(InputAction::FinishActivitySelection {
        end,
        text: Some("selected detail".to_owned()),
    });
    assert_eq!(
        state.update_input(InputAction::CopyOrInterrupt),
        UpdateEffect::CopyText("selected detail".to_owned())
    );
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
                project: Some("Xana".to_owned()),
            },
            ConversationProjection {
                conversation: archived.clone(),
                state: ConversationState::Inactive,
                record_count: None,
                modified: None,
                selected: false,
                project: None,
            },
        ],
        active: None,
    });
    assert_eq!(state.sessions[0].project.as_deref(), Some("Xana"));

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
fn project_and_profile_slash_commands_use_the_shared_control_path() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.composer.replace("/project list --all".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::ControlCommand {
            family: "project".to_owned(),
            arguments: "list --all".to_owned(),
        }
    );
    state
        .composer
        .replace("/profile resolve review --json".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::ControlCommand {
            family: "profile".to_owned(),
            arguments: "resolve review --json".to_owned(),
        }
    );
    state
        .composer
        .replace("/skill activate project/review".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::ControlCommand {
            family: "skill".to_owned(),
            arguments: "activate project/review".to_owned(),
        }
    );
    state.composer.replace("/plugin inspect quality".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::ControlCommand {
            family: "plugin".to_owned(),
            arguments: "inspect quality".to_owned(),
        }
    );
}

#[test]
fn bare_profile_create_opens_a_prefilled_form_and_emits_one_typed_command() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.connection = "ollama".to_owned();
    state.model = "qwen3:8b".to_owned();
    state.composer.replace("/profile create".to_owned());

    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(matches!(
        state.overlay,
        Some(Overlay::ProfileCreate {
            ref fields,
            selected: 0,
            ..
        }) if fields == &["".to_owned(), "ollama".to_owned(), "qwen3:8b".to_owned()]
    ));

    state.update_input(InputAction::Insert("daily review".to_owned()));
    state.update_input(InputAction::Confirm);
    state.update_input(InputAction::Confirm);
    assert_eq!(
        state.update_input(InputAction::Confirm),
        UpdateEffect::ControlCommand {
            family: "profile".to_owned(),
            arguments: "create 'daily review' --connection ollama --model qwen3:8b".to_owned(),
        }
    );
}

#[test]
fn profile_create_keeps_invalid_drafts_in_the_form() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.connection.clear();
    state.model.clear();
    state.composer.replace("/profile create".to_owned());
    state.update_input(InputAction::Submit);

    state.update_input(InputAction::Confirm);
    state.update_input(InputAction::Confirm);
    assert_eq!(state.update_input(InputAction::Confirm), UpdateEffect::None);
    assert!(matches!(
        state.overlay,
        Some(Overlay::ProfileCreate {
            selected: 0,
            error: Some(_),
            ..
        })
    ));
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
                project: Some("Xana".to_owned()),
            },
            ConversationProjection {
                conversation: other.clone(),
                state: ConversationState::Inactive,
                record_count: Some(3),
                modified: None,
                selected: false,
                project: None,
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
