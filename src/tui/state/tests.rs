use super::*;
use crate::{
    frontend::semantic::{UsageAccountingV1, UsageScopeV1},
    identity::{ConversationId, SessionId, StepId},
    native_runtime::OperationOutcome,
    workspace_host::{ConversationProjection, ConversationState, WorkspaceSnapshot},
};
use uuid::Uuid;

#[test]
fn learning_upgrade_disclosure_is_local_and_does_not_change_saved_cursors_or_draft() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.history_start = 40;
    state.history_end = 80;
    state.composer.insert("my unsent draft").unwrap();
    state.disclose_learning();
    state.disclose_learning();
    assert_eq!(
        state.messages.len(),
        1,
        "a continuation does not duplicate the notice"
    );
    let notice = state.messages.front().unwrap();
    assert_eq!(notice.kind, MessageKind::System);
    assert!(notice.text.contains("explicitly authorized native helper"));
    assert!(notice.text.contains("--learn off"));
    assert_eq!((state.history_start, state.history_end), (40, 80));
    assert!(state.pending_history_entries.is_empty());
    assert!(state.followups.is_empty());
    assert_eq!(state.composer.text, "my unsent draft");
}

#[test]
fn committed_user_echo_deduplicates_local_input_but_renders_passive_input() {
    let mut local = TuiState::starting(ComposerPreset::Submit);
    local.messages.clear();
    let operation_id = OperationId::new();
    local.mark_submitted(operation_id, "same question".into());
    local.apply_runtime(&AgentEvent::UserMessageCommitted {
        operation_id,
        message: Message::text(Role::User, "same question"),
    });
    assert_eq!(local.messages.len(), 1);
    let mut observer = TuiState::starting(ComposerPreset::Submit);
    observer.messages.clear();
    observer.apply_runtime(&AgentEvent::UserMessageCommitted {
        operation_id,
        message: Message::text(Role::User, "same question"),
    });
    assert_eq!(observer.messages.len(), 1);
    assert_eq!(observer.messages.front().unwrap().kind, MessageKind::User);
    observer.apply_runtime(&AgentEvent::UserMessageCommitted {
        operation_id: OperationId::new(),
        message: Message::text(Role::User, "same question"),
    });
    assert_eq!(
        observer.messages.len(),
        2,
        "identical content in another turn is not a duplicate"
    );
}

#[test]
fn storage_lock_can_stop_active_work_instead_of_waiting_for_idle() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = true;
    state.composer.insert("/storage lock").unwrap();
    let effect = state.update_input(InputAction::Submit);
    assert!(
        matches!(effect, UpdateEffect::ControlCommand { family, arguments } if family == "storage" && arguments == "lock")
    );
    assert!(state.composer.text.is_empty());
    state.composer.insert("/storage verify").unwrap();
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
}

#[test]
fn paging_older_history_keeps_the_requested_page_instead_of_evicting_it() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.messages.clear();
    state.composer.insert("draft stays here").unwrap();
    for index in 128..640 {
        state.push_message(MessageKind::User, format!("message {index}"));
    }
    state.seed_saved_history_cursor(640);
    assert_eq!(state.history_before(), Some(128));
    state.prepend_history_page(crate::session::ConversationPage {
        messages: (0..128)
            .map(|index| Message::text(Role::User, format!("message {index}")))
            .collect(),
        start: 0,
        total: 640,
        has_older: false,
    });
    assert_eq!(state.messages.front().unwrap().text, "message 0");
    assert_eq!(state.messages.len(), 512);
    assert_eq!(state.composer.text, "draft stays here");
    assert_eq!(state.history_before(), None);
    assert_eq!(state.history_newer_start(), Some(512));
    state.replace_newer_page(crate::session::ConversationPage {
        messages: (512..640)
            .map(|index| Message::text(Role::User, format!("message {index}")))
            .collect(),
        start: 512,
        total: 640,
        has_older: true,
    });
    assert_eq!(state.messages.back().unwrap().text, "message 639");
    assert_eq!(state.history_newer_start(), None);
    assert_eq!(state.composer.text, "draft stays here");
}

#[test]
fn conversation_window_caps_aggregate_text_not_only_the_number_of_messages() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.messages.clear();
    for index in 0..600 {
        state.push_message(
            MessageKind::User,
            format!("{index}:{}", "x".repeat(16 * 1024)),
        );
    }
    assert!(
        state
            .messages
            .iter()
            .map(|message| message.text.len())
            .sum::<usize>()
            <= 2 * 1024 * 1024
    );
    assert!(state.messages.back().unwrap().text.starts_with("599:"));
}

#[test]
fn saved_history_stays_contiguous_while_live_output_and_local_status_remain_separate() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.messages.clear();
    state.push_message(MessageKind::User, "live tail");
    state.begin_saved_history_page(crate::session::ConversationPage {
        messages: (20..22)
            .map(|n| Message::text(Role::User, format!("saved {n}")))
            .collect(),
        start: 20,
        total: 100,
        has_older: true,
    });
    state.push_message(MessageKind::System, "local usage report");
    assert_eq!(state.messages.len(), 2);
    assert!(state.overlay.is_some());
    assert_eq!(state.history_newer_start(), Some(22));
    let operation_id = OperationId::new();
    state.apply_runtime(&AgentEvent::AssistantMessage {
        operation_id,
        message: Message::text(Role::Assistant, "live answer"),
    });
    assert_eq!(state.messages.front().unwrap().text, "saved 20");
    assert_eq!(state.history_newer_start(), Some(22));
    state.mark_submitted(OperationId::new(), "next question".to_owned());
    assert_eq!(state.messages.front().unwrap().text, "live tail");
    assert!(
        state
            .messages
            .iter()
            .any(|message| message.text == "live answer")
    );
    assert_eq!(state.messages.back().unwrap().text, "next question");
    assert!(state.needs_history_snapshot());
    state.seed_saved_history_cursor(100);
    state.apply_runtime(&AgentEvent::ConversationCleared);
    state.push_message(MessageKind::User, "new conversation");
    assert_eq!(state.history_before(), None);
    assert_eq!(state.history_newer_start(), None);
}

#[test]
fn forward_page_keeps_its_first_unseen_row_when_client_bytes_are_tighter() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.history_preview = true;
    state.replace_newer_page(crate::session::ConversationPage {
        messages: (50..150)
            .map(|n| Message::text(Role::User, format!("{n}:{}", "x".repeat(32 * 1024))))
            .collect(),
        start: 50,
        total: 200,
        has_older: true,
    });
    assert!(state.messages.front().unwrap().text.starts_with("50:"));
    assert!(state.messages.len() < 100);
    assert_eq!(state.history_newer_start(), Some(50 + state.messages.len()));
}

#[test]
fn append_respects_even_a_sub_marker_remaining_byte_budget() {
    let mut text = "1234".to_owned();
    append_bounded(&mut text, "🦀", 5);
    assert!(text.len() <= 5);
}

#[test]
fn managed_usage_replaces_cumulative_snapshots_without_double_counting() {
    let conversation_id = ConversationId::new();
    let mut state = TuiState::from_managed(
        "codex".to_owned(),
        "model".to_owned(),
        "thread-1".to_owned(),
        ComposerPreset::Submit,
        ActivityVisibility::Auto,
        ConversationRef::Managed {
            conversation_id,
            connection: "codex".to_owned(),
            thread_id: "thread-1".to_owned(),
        },
    );
    let event = |input_tokens| ManagedClientEvent::TokenUsageUpdated {
        input_tokens,
        cached_input_tokens: Some(2),
        output_tokens: 5,
        reasoning_tokens: Some(1),
        total_tokens: input_tokens + 5,
        context_input_tokens: Some(input_tokens),
        context_window_tokens: Some(10_000),
    };

    state.apply_managed_event(&event(10));
    state.apply_managed_event(&event(20));

    assert_eq!(state.semantic.usage.len(), 1);
    let observation = &state.semantic.usage[0];
    assert_eq!(
        observation.scope,
        UsageScopeV1::Conversation { conversation_id }
    );
    assert_eq!(observation.amounts.input_tokens, Some(20));
    assert_eq!(
        observation.accounting,
        UsageAccountingV1::CumulativeSnapshot { sequence: 2 }
    );
}

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
fn composer_history_recall_is_workspace_local_and_restores_the_draft() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.install_composer_history(["first".to_owned(), "second".to_owned()], None);
    state.composer.insert("current draft").unwrap();

    state.update_input(InputAction::HistoryPrevious);
    assert_eq!(state.composer.text, "second");
    state.update_input(InputAction::HistoryPrevious);
    assert_eq!(state.composer.text, "first");
    state.update_input(InputAction::HistoryNext);
    assert_eq!(state.composer.text, "second");
    state.update_input(InputAction::HistoryNext);
    assert_eq!(state.composer.text, "current draft");
    assert!(!state.composer_history_active());
}

#[test]
fn submitted_composer_history_is_bounded_and_secret_filtered_before_persistence() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.composer.insert("api_key=never-retain-this").unwrap();

    assert!(matches!(
        state.update_input(InputAction::Submit),
        UpdateEffect::Submit { .. }
    ));
    assert_eq!(
        state.take_pending_history_entry().as_deref(),
        Some("[redacted secret-like composer entry]")
    );
}

#[test]
fn file_completion_replaces_only_the_active_at_token() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.composer.insert("inspect @sr after").unwrap();
    state.composer.move_cursor(MoveDirection::Left, false);
    state.composer.move_cursor(MoveDirection::Left, false);
    state.composer.move_cursor(MoveDirection::Left, false);
    state.composer.move_cursor(MoveDirection::Left, false);
    state.composer.move_cursor(MoveDirection::Left, false);
    state.composer.move_cursor(MoveDirection::Left, false);

    let UpdateEffect::CompleteFile { query, replacement } =
        state.update_input(InputAction::CompleteFile)
    else {
        panic!("expected a file-completion request")
    };
    assert_eq!(query, "sr");
    state.show_file_completions(
        query,
        replacement,
        vec!["src/lib.rs".to_owned(), "src/main.rs".to_owned()],
    );
    state.update_input(InputAction::PaletteDown);
    state.update_input(InputAction::Confirm);
    assert_eq!(state.composer.text, "inspect @src/main.rs after");
}

#[test]
fn stale_file_completion_does_not_replace_a_changed_draft() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.composer.insert("@src").unwrap();
    let UpdateEffect::CompleteFile { query, replacement } =
        state.update_input(InputAction::CompleteFile)
    else {
        panic!("expected a file-completion request")
    };
    state.update_input(InputAction::Insert("x".to_owned()));
    state.show_file_completions(query, replacement, vec!["src/lib.rs".to_owned()]);
    assert!(!matches!(
        state.overlay,
        Some(Overlay::FileCompletion { .. })
    ));
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
fn pasted_media_path_uses_the_same_typed_drop_path() {
    let mut state = TuiState::starting(ComposerPreset::Submit);

    assert_eq!(
        state.update_input(InputAction::Paste("recordings/meeting.webm".to_owned())),
        UpdateEffect::AttachDropped("recordings/meeting.webm".to_owned())
    );
}

#[test]
fn unsupported_typed_resource_stays_staged_instead_of_being_silently_disclosed() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    let artifact = crate::artifact::ArtifactRecord {
        reference: crate::artifact::ArtifactRef {
            id: crate::identity::ArtifactId::new(),
            content_hash: crate::artifact::ContentHash::for_bytes(b"ID3fixture"),
        },
        media_type: "audio/mpeg".to_owned(),
        byte_len: 10,
        owner: crate::identity::PrincipalId::new(),
    };
    state.stage_resource(crate::frontend::semantic::AttachmentV1 {
        id: Uuid::new_v4(),
        resource: crate::resource::ResourceRefV1 {
            version: crate::resource::RESOURCE_SCHEMA_VERSION,
            artifact,
            kind: crate::resource::ResourceKindV1::Audio,
            media_type: crate::resource::MediaTypeFactsV1 {
                declared: Some("audio/mpeg".to_owned()),
                detected: Some("audio/mpeg".to_owned()),
            },
            metadata: crate::resource::ResourceMetadataV1::default(),
            accessibility: None,
            validation: crate::resource::ResourceValidationV1::Accepted,
            lineage: None,
        },
        provenance: crate::frontend::semantic::AttachmentProvenanceV1::UserSelected,
        source_label: Some("meeting.mp3".to_owned()),
        capabilities: Vec::new(),
    });
    state.composer.replace("summarize this".to_owned());

    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert_eq!(state.composer.text, "summarize this");
    assert_eq!(state.pending_resource_count(), 1);
    assert!(state.status.contains("does not advertise provider input"));
}

#[test]
fn external_resource_requires_one_explicit_read_decision() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.request_external_resource_approval("C:\\outside\\clip.webm".to_owned());

    assert_eq!(
        state.update_input(InputAction::Confirm),
        UpdateEffect::AttachApproved("C:\\outside\\clip.webm".to_owned())
    );
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
fn compact_is_available_only_when_xana_owns_native_context() {
    let mut native = TuiState::starting(ComposerPreset::Submit);
    native.busy = false;
    native.composer.replace("/compact".to_owned());
    assert!(matches!(
        native.update_input(InputAction::Submit),
        UpdateEffect::CompactConversation { .. }
    ));

    let mut managed =
        TuiState::starting(ComposerPreset::Submit).with_capabilities(OwnerCapabilities::managed());
    managed.busy = false;
    managed.composer.replace("/compact".to_owned());
    assert_eq!(
        managed.update_input(InputAction::Submit),
        UpdateEffect::None
    );
    assert!(managed.status.contains("owns its context"));
}

#[test]
fn round_budget_requires_the_exact_visible_continue_or_stop_decision() {
    let operation_id = OperationId::new();
    let suspension = crate::native_runtime::RoundBudgetSuspension {
        id: crate::identity::RoundBudgetId::new(),
        operation_id,
        soft_round_limit: 8,
        last_tranche_rounds: 8,
        rounds_consumed: 8,
        hard_round_limit: 256,
        remaining_rounds: 248,
        continuations_used: 0,
        committed: crate::native_runtime::RoundBudgetCommitFacts {
            steps: 8,
            invocations: 8,
            results: 8,
        },
        repeated_tool_patterns: 0,
        usage: crate::agent::AgentTurnUsage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            total_tokens: Some(12),
            requests: 8,
            ..crate::agent::AgentTurnUsage::default()
        },
        allowed_actions: vec![RoundBudgetAction::Continue, RoundBudgetAction::Stop],
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.apply_runtime(&AgentEvent::RoundBudgetReached {
        suspension: suspension.clone(),
    });
    state.apply_runtime(&AgentEvent::OperationStateChanged {
        operation_id,
        state: OperationState::Suspended,
    });

    assert_eq!(state.pending_round_budget, Some(suspension.clone()));
    assert!(state.status.contains("/continue"));
    state.composer.replace("queued accidentally".to_owned());
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert_eq!(state.composer.text, "queued accidentally");

    state.composer.replace("/continue".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::DecideRoundBudget {
            suspension,
            action: RoundBudgetAction::Continue,
        }
    );
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
            conversation_id: crate::identity::ConversationId::new(),
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

    let cwd = format!("C:/workspace/{}important-tail", "nested/".repeat(90));
    let approval =
        crate::tui::activity::ApprovalPrompt::managed(crate::managed::codex::ApprovalRequest {
            item_id: Some("command-1".into()),
            method: "item/commandExecution/requestApproval".into(),
            available_decisions: ["accept".into(), "decline".into()].into_iter().collect(),
            reason: None,
            command: Some("cargo test".into()),
            cwd: Some(cwd.clone()),
        });
    assert!(
        approval
            .details
            .iter()
            .any(|detail| detail == &format!("cwd: {cwd}"))
    );
}

#[test]
fn managed_turns_publish_deterministic_execution_facts_and_completion_receipts() {
    let conversation_id = crate::identity::ConversationId::new();
    let operation_id = OperationId::new();
    let mut state = TuiState::from_managed(
        "codex".to_owned(),
        "gpt-test".to_owned(),
        "thread-test".to_owned(),
        ComposerPreset::Submit,
        ActivityVisibility::Auto,
        ConversationRef::Managed {
            conversation_id,
            connection: "codex".to_owned(),
            thread_id: "thread-test".to_owned(),
        },
    );

    state.mark_submitted(operation_id, "test the workspace".to_owned());
    state.apply_managed_event(&ManagedClientEvent::TokenUsageUpdated {
        input_tokens: 20,
        cached_input_tokens: Some(5),
        output_tokens: 7,
        reasoning_tokens: Some(3),
        total_tokens: 27,
        context_input_tokens: Some(20),
        context_window_tokens: Some(100_000),
    });
    state.finish_managed_turn(operation_id, None);

    assert_eq!(state.semantic.execution_facts.len(), 1);
    let facts = &state.semantic.execution_facts[0];
    assert_eq!(facts.conversation_id, conversation_id);
    assert_eq!(facts.run_id, operation_id);
    assert_eq!(facts.owner, ExecutionOwnerV1::Managed);
    assert_eq!(
        facts.workspace_authority,
        WorkspaceAuthorityV1::WorkspaceWrite
    );
    assert_eq!(facts.connection.as_deref(), Some("codex"));
    assert_eq!(facts.model.as_deref(), Some("gpt-test"));

    assert_eq!(state.semantic.completion_receipts.len(), 1);
    let receipt = &state.semantic.completion_receipts[0];
    assert_eq!(receipt.status, CompletionStatusV1::Completed);
    assert_eq!(receipt.usage.amounts.input_tokens, Some(20));
    assert_eq!(receipt.usage.amounts.output_tokens, Some(7));
    assert_eq!(receipt.usage.observation_count, 1);
    let receipt_id = receipt.id;
    state.semantic.validate().unwrap();

    state.finish_managed_turn(operation_id, None);

    assert_eq!(state.semantic.execution_facts.len(), 1);
    assert_eq!(state.semantic.completion_receipts.len(), 1);
    assert_eq!(state.semantic.completion_receipts[0].id, receipt_id);
    state.semantic.validate().unwrap();
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
        conversation_id: crate::identity::ConversationId::new(),
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
        conversation_id: crate::identity::ConversationId::new(),
        connection: "codex".to_owned(),
        thread_id: "thread-to-archive".to_owned(),
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.runtime_conversation = runtime.clone();
    state.viewed_conversation = runtime.clone();
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        workspace_id: "workspace".into(),
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
fn management_slash_commands_suspend_through_the_shared_cli_path() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    for (input, family, arguments) in [
        ("/connection", "connection", "list"),
        ("/logs path", "logs", "path"),
        ("/outbound", "outbound", "list"),
        ("/route check review", "route", "check review"),
        ("/connect", "connect", ""),
    ] {
        state.composer.replace(input.to_owned());
        assert_eq!(
            state.update_input(InputAction::Submit),
            UpdateEffect::ControlCommand {
                family: family.to_owned(),
                arguments: arguments.to_owned(),
            },
            "{input}"
        );
    }

    state.busy = true;
    state.composer.replace("/connection list".to_owned());
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(state.status.contains("active turn"));
}

#[test]
fn settings_slash_command_validates_section_and_requires_an_idle_owner() {
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.composer.replace("/settings appearance".to_owned());
    assert_eq!(
        state.update_input(InputAction::Submit),
        UpdateEffect::Settings("appearance".to_owned())
    );

    state.composer.replace("/settings colours".to_owned());
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(state.status.contains("unknown settings section"));

    state.busy = true;
    state.composer.replace("/settings".to_owned());
    assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
    assert!(state.status.contains("active turn"));
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
        workspace_id: "workspace".into(),
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
    state.seed_saved_history_cursor(400);
    assert_eq!(state.history_before(), Some(399));
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
    assert_eq!(state.history_before(), Some(399));
    assert_eq!(state.history_newer_start(), None);
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

#[test]
fn conversation_picker_attaches_with_enter_and_previews_with_space() {
    let runtime = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let idle = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.runtime_conversation = runtime.clone();
    state.viewed_conversation = runtime.clone();
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        workspace_id: "workspace".into(),
        conversations: vec![
            ConversationProjection {
                conversation: runtime,
                state: ConversationState::Controlled,
                record_count: Some(1),
                modified: None,
                selected: true,
                project: None,
            },
            ConversationProjection {
                conversation: idle.clone(),
                state: ConversationState::Inactive,
                record_count: Some(1),
                modified: None,
                selected: false,
                project: None,
            },
        ],
        active: None,
    });

    state.open_session_picker();
    state.update_input(InputAction::PaletteDown);
    assert_eq!(
        state.update_input(InputAction::PreviewSelected),
        UpdateEffect::ViewSession(idle.clone())
    );

    state.open_session_picker();
    state.update_input(InputAction::PaletteDown);
    assert_eq!(
        state.update_input(InputAction::Confirm),
        UpdateEffect::SwitchConversation(idle)
    );
}

#[test]
fn conversation_switch_refuses_an_active_target_and_an_active_source_run() {
    let runtime = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let other = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.runtime_conversation = runtime.clone();
    state.viewed_conversation = runtime;
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        workspace_id: "workspace".into(),
        conversations: vec![ConversationProjection {
            conversation: other.clone(),
            state: ConversationState::Active,
            record_count: Some(1),
            modified: None,
            selected: false,
            project: None,
        }],
        active: None,
    });

    assert_eq!(state.attach_conversation(other.clone()), UpdateEffect::None);
    assert!(state.status.contains("another active root"));

    state.sessions[0].state = ConversationState::Inactive;
    state.busy = true;
    assert_eq!(state.attach_conversation(other), UpdateEffect::None);
    assert!(state.status.contains("Finish or interrupt"));
}

#[test]
fn conversation_drafts_keep_text_cursor_selection_and_queues_isolated() {
    let runtime = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let other = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.runtime_conversation = runtime.clone();
    state.viewed_conversation = runtime.clone();
    state.composer.insert("runtime draft").unwrap();
    state.composer.move_cursor(MoveDirection::Left, true);
    state.followups.push_back(QueuedTurn {
        input: "runtime queue".to_owned(),
        images: Vec::new(),
        vision_route: None,
    });

    state.view_session(other.clone(), Some(Vec::new()));
    assert!(state.composer.text.is_empty());
    state.composer.insert("other draft").unwrap();
    state.view_session(runtime.clone(), None);
    assert_eq!(state.composer.text, "runtime draft");
    assert!(state.composer.selection().is_some());
    assert_eq!(state.followups.front().unwrap().input, "runtime queue");

    let continuation = state.into_continuation();
    let mut restored = TuiState::starting(ComposerPreset::Submit);
    restored.busy = false;
    restored.runtime_conversation = other.clone();
    restored.viewed_conversation = other;
    restored.restore_continuation(continuation);
    assert_eq!(restored.composer.text, "other draft");
    assert!(restored.followups.is_empty());
}
