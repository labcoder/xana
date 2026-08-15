use super::super::{
    activity::{ActivityCard, ActivityKind, ActivityState},
    state::MessageKind,
};
use super::*;
use crate::presentation::{ComposerPreset, ResolvedPresentation};
use crate::{
    identity::SessionId,
    workspace_host::{
        ConversationProjection, ConversationRef, ConversationState, WorkspaceSnapshot,
    },
};
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn wide_medium_and_narrow_buffers_keep_the_conversation_usable() {
    for (width, expected, absent) in [
        (130, "Session", "medium"),
        (90, "medium", "Conversation controls"),
        (50, "narrow", "Conversation controls"),
    ] {
        let backend = TestBackend::new(width, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.busy = false;
        terminal
            .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
            .unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("Conversation"));
        assert!(rendered.contains("Message"));
        assert!(rendered.contains(expected));
        assert!(!rendered.contains(absent));
    }
}

#[test]
fn active_turn_has_a_transient_conversation_work_indicator() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.header_expanded = false;
    state.messages.clear();
    state
        .messages
        .push_back(super::super::state::VisibleMessage {
            kind: MessageKind::User,
            text: "compare these images".to_owned(),
            document: super::super::rich_text::RichDocument::plain("compare these images"),
        });
    state.busy = true;
    state.active_operation = Some(crate::identity::OperationId::new());

    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();

    assert!(buffer_text(terminal.backend().buffer()).contains("Xana is working"));
}

#[test]
fn reduced_motion_keeps_the_work_indicator_static() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.header_expanded = false;
    state.messages.clear();
    state.busy = true;
    state.active_operation = Some(crate::identity::OperationId::new());
    state.work_indicator_frame = 3;

    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();

    assert!(buffer_text(terminal.backend().buffer()).contains("Xana is working..."));
}

#[test]
fn command_queue_and_model_overlays_have_bounded_readable_snapshots() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.busy = false;
    state.update_input(super::super::state::InputAction::OpenPalette);
    state.update_input(super::super::state::InputAction::Insert("mod".to_owned()));
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    assert!(buffer_text(terminal.backend().buffer()).contains("/model"));

    state.open_model_picker(vec!["openai/gpt-test".to_owned()]);
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    assert!(buffer_text(terminal.backend().buffer()).contains("openai/gpt-test"));

    let conversation = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    state.runtime_conversation = conversation.clone();
    state.viewed_conversation = conversation.clone();
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        conversations: vec![ConversationProjection {
            conversation,
            state: ConversationState::Controlled,
            record_count: Some(12),
            modified: None,
            selected: false,
            project: None,
        }],
        active: None,
    });
    state.open_session_picker();
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let rendered = buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("Sessions"));
    assert!(rendered.contains("12 records"));
}

#[test]
fn external_image_review_lists_the_complete_batch() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    let paths = vec![
        "C:\\outside\\first.png".to_owned(),
        "C:\\outside\\second.png".to_owned(),
    ];
    state.request_external_image_approval(
        crate::identity::OperationId::new(),
        "compare".to_owned(),
        paths.clone(),
        paths,
    );

    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let rendered = buffer_text(terminal.backend().buffer());

    assert!(rendered.contains("C:\\outside\\first.png"));
    assert!(rendered.contains("C:\\outside\\second.png"));
    assert!(rendered.contains("Allow images once"));
}

#[test]
fn command_palette_keeps_the_keyboard_selection_inside_its_viewport() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.update_input(super::super::state::InputAction::OpenPalette);
    for _ in 0..64 {
        state.update_input(super::super::state::InputAction::PaletteDown);
    }

    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();

    let expected = format!(
        "> /{}",
        state.palette_entries().last().expect("palette entry").name
    );
    assert!(buffer_text(terminal.backend().buffer()).contains(&expected));
}

#[test]
fn command_palette_renders_session_modes_as_a_fixed_header_table() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.update_input(super::super::state::InputAction::OpenPalette);
    state.update_input(super::super::state::InputAction::Insert(
        "/sessions".to_owned(),
    ));

    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let rendered = buffer_text(terminal.backend().buffer());

    assert!(rendered.contains("COMMAND"));
    assert!(rendered.contains("MODE OR PARAMETERS"));
    assert!(rendered.contains("archive [ID]"));
    assert!(rendered.contains("view hide"));
    assert!(rendered.contains("view show"));
}

#[test]
fn ten_thousand_message_fixture_renders_only_the_bounded_viewport_window() {
    let backend = TestBackend::new(130, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.messages.clear();
    for index in 0..10_000 {
        let text = format!("message {index}");
        state
            .messages
            .push_back(super::super::state::VisibleMessage {
                kind: MessageKind::Assistant,
                document: super::super::rich_text::RichDocument::plain(&text),
                text,
            });
    }
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let rendered = buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("message 9999"));
    assert!(!rendered.contains("message 0"));
    assert!(rendered.contains("older message(s) outside this viewport"));
}

#[test]
fn conversation_follows_the_visual_bottom_and_scrolls_within_a_long_message() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.header_expanded = false;
    state.messages.clear();
    let text = (0..40)
        .map(|index| format!("answer-line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    state
        .messages
        .push_back(super::super::state::VisibleMessage {
            kind: MessageKind::Assistant,
            document: super::super::rich_text::RichDocument::plain(&text),
            text,
        });

    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    assert!(buffer_text(terminal.backend().buffer()).contains("answer-line-39"));

    state.update_input(super::super::state::InputAction::Scroll(-3));
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let rendered = buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("answer-line-36"));
    assert!(!rendered.contains("Start a conversation below"));
}

#[test]
fn ordinary_conversation_drag_keeps_highlight_until_explicit_copy_or_click() {
    let area = Rect::new(0, 0, 80, 24);
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.header_expanded = false;
    state.messages.clear();
    state
        .messages
        .push_back(super::super::state::VisibleMessage {
            kind: MessageKind::Assistant,
            text: "copy me".to_owned(),
            document: super::super::rich_text::RichDocument::plain("copy me"),
        });
    let conversation = conversation_area(shell_layout(area, &state), &state, area.width);
    let start = ScreenPoint {
        column: conversation.x + 3,
        row: conversation.y + 2,
    };
    let end = ScreenPoint {
        column: start.column + 6,
        row: start.row,
    };

    let begin = pointer_action(start.column, start.row, false, &state, area).unwrap();
    assert_eq!(
        state.update_input(begin),
        super::super::state::UpdateEffect::None
    );
    let extend = pointer_action(end.column, end.row, true, &state, area).unwrap();
    assert_eq!(
        state.update_input(extend),
        super::super::state::UpdateEffect::None
    );
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .cell((start.column, start.row))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED)
    );

    let finish = pointer_release_action(end.column, end.row, &state, area).unwrap();
    assert_eq!(
        state.update_input(finish),
        super::super::state::UpdateEffect::None
    );
    assert!(state.conversation_selection.is_some());
    assert_eq!(
        state.update_input(super::super::state::InputAction::CopyOrInterrupt),
        super::super::state::UpdateEffect::CopyText("copy me".to_owned())
    );
    assert!(state.conversation_selection.is_some());

    let begin = pointer_action(start.column, start.row, false, &state, area).unwrap();
    state.update_input(begin);
    let finish = pointer_release_action(start.column, start.row, &state, area).unwrap();
    assert_eq!(
        state.update_input(finish),
        super::super::state::UpdateEffect::None,
        "an ordinary click must not copy one cell"
    );
    assert!(state.conversation_selection.is_none());
}

#[test]
#[ignore = "manual release-profile timing evidence; not a wall-clock CI gate"]
fn phase5_reference_first_frame_and_input_render_probe() {
    let backend = TestBackend::new(130, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    let started = std::time::Instant::now();
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let first_frame = started.elapsed();

    state.messages.clear();
    for index in 0..10_000 {
        let text = format!("message {index}");
        state
            .messages
            .push_back(super::super::state::VisibleMessage {
                kind: MessageKind::Assistant,
                document: super::super::rich_text::RichDocument::plain(&text),
                text,
            });
    }
    let mut samples = Vec::with_capacity(256);
    for _ in 0..256 {
        let started = std::time::Instant::now();
        state.update_input(super::super::state::InputAction::Insert("x".to_owned()));
        state.update_input(super::super::state::InputAction::Backspace);
        terminal
            .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
            .unwrap();
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
    eprintln!(
        "phase5_reference first_frame_us={} input_render_p95_us={} fixture_messages={} visible_rows={}",
        first_frame.as_micros(),
        p95.as_micros(),
        state.messages.len(),
        terminal.backend().buffer().area.height,
    );
    assert!(buffer_text(terminal.backend().buffer()).contains("message 9999"));
}

#[test]
fn pointer_hit_testing_activates_sessions_overlays_activity_and_composer() {
    let area = Rect::new(0, 0, 130, 24);
    let conversation = ConversationRef::Native {
        session_id: SessionId::new(),
    };
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.activity_visibility = ActivityVisibility::Open;
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        conversations: vec![ConversationProjection {
            conversation: conversation.clone(),
            state: ConversationState::Inactive,
            record_count: Some(2),
            modified: None,
            selected: false,
            project: None,
        }],
        active: None,
    });
    let shell = shell_layout(area, &state);
    let columns = wide_columns(shell.body, &state);

    assert_eq!(
        pointer_action(columns[0].x + 1, columns[0].y, false, &state, area),
        Some(super::super::state::InputAction::ToggleSessionsView)
    );

    assert_eq!(
        pointer_action(
            columns[0].x.saturating_add(1),
            columns[0].y.saturating_add(1),
            false,
            &state,
            area,
        ),
        Some(super::super::state::InputAction::ViewSession(conversation))
    );
    assert_eq!(
        pointer_action(1, 1, false, &state, area),
        Some(super::super::state::InputAction::ToggleHeader)
    );
    assert_eq!(
        pointer_action(
            columns[2].x.saturating_add(1),
            columns[2].y.saturating_add(1),
            false,
            &state,
            area,
        ),
        Some(super::super::state::InputAction::ToggleActivity(0))
    );
    assert_eq!(
        pointer_action(
            shell.composer.x.saturating_add(3),
            shell.composer.y.saturating_add(1),
            true,
            &state,
            area,
        ),
        Some(super::super::state::InputAction::PlaceCursor {
            line: 0,
            column: 2,
            width: 128,
            scroll: 0,
            select: true,
        })
    );

    state.open_model_picker(vec!["openai/gpt-test".to_owned()]);
    let popup = overlay_area(Rect::new(0, 0, 80, 24));
    assert_eq!(
        pointer_action(
            popup.x + 1,
            popup.y + 1,
            false,
            &state,
            Rect::new(0, 0, 80, 24),
        ),
        Some(super::super::state::InputAction::ChooseOverlay(0))
    );
}

#[test]
fn expanded_activity_detail_opens_a_scrollable_modal() {
    let area = Rect::new(0, 0, 130, 24);
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.activity_visibility = ActivityVisibility::Open;
    state.activity.clear();
    let mut card = ActivityCard::new(
        "Xana",
        "reasoning",
        ActivityKind::ReasoningSummary,
        ActivityState::Complete,
        "summary",
        "detailed reasoning event",
    );
    card.expanded = true;
    state.activity.push_back(card);
    let shell = shell_layout(area, &state);
    let columns = wide_columns(shell.body, &state);

    assert_eq!(
        pointer_action(
            columns[2].x.saturating_add(1),
            columns[2].y.saturating_add(2),
            false,
            &state,
            area,
        ),
        Some(super::super::state::InputAction::OpenActivityDetail(0))
    );
    state.update_input(super::super::state::InputAction::OpenActivityDetail(0));

    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let rendered = buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("Activity details"));
    assert!(rendered.contains("detailed reasoning event"));
    assert!(rendered.contains("Ctrl+C copy"));
}

#[test]
fn hidden_session_rail_releases_all_horizontal_space() {
    let area = Rect::new(0, 0, 130, 24);
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.header_expanded = false;
    state.rail_expanded = false;
    let shell = shell_layout(area, &state);
    let columns = wide_columns(shell.body, &state);

    assert_eq!(columns[0].width, 0);
    assert_eq!(columns[1].x, area.x);
    assert_eq!(columns[1].width, area.width);
}

#[test]
fn every_fixed_height_session_row_has_the_same_click_target() {
    let area = Rect::new(0, 0, 130, 24);
    let conversations = (0..3)
        .map(|_| ConversationProjection {
            conversation: ConversationRef::Native {
                session_id: SessionId::new(),
            },
            state: ConversationState::Inactive,
            record_count: Some(2),
            modified: None,
            selected: false,
            project: None,
        })
        .collect::<Vec<_>>();
    let expected = conversations[2].conversation.clone();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.refresh_sessions(WorkspaceSnapshot {
        workspace: std::env::current_dir().unwrap(),
        conversations,
        active: None,
    });
    let shell = shell_layout(area, &state);
    let columns = wide_columns(shell.body, &state);

    assert_eq!(
        pointer_action(
            columns[0].x.saturating_add(1),
            columns[0].y.saturating_add(5),
            false,
            &state,
            area,
        ),
        Some(super::super::state::InputAction::ViewSession(expected))
    );
}

#[test]
fn composer_expands_then_scrolls_without_rendering_over_the_footer() {
    let backend = TestBackend::new(130, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::starting(ComposerPreset::Submit);
    state.header_expanded = false;
    state.busy = false;
    state.composer.replace(
        (0..8)
            .map(|index| format!("draft-line-{index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    terminal
        .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
        .unwrap();
    let rendered = buffer_text(terminal.backend().buffer());
    let shell = shell_layout(Rect::new(0, 0, 130, 24), &state);

    assert_eq!(shell.composer.height, 8);
    assert!(!rendered.contains("draft-line-0"));
    assert!(rendered.contains("draft-line-7"));
    assert!(rendered.contains("drag conversation to select"));
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}
