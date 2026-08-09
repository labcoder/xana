//! Pure Ratatui rendering for the adaptive shell.

use super::model::{ActivityVisibility, LayoutClass, MessageKind, Overlay, TuiState};
use crate::presentation::{PresentationColor, ResolvedPresentation, SemanticToken};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

pub(super) fn render(frame: &mut Frame<'_>, state: &TuiState, profile: ResolvedPresentation) {
    let area = frame.area();
    match LayoutClass::for_width(area.width) {
        LayoutClass::Wide => render_wide(frame, area, state, profile),
        LayoutClass::Medium => render_medium(frame, area, state, profile),
        LayoutClass::Narrow => render_narrow(frame, area, state, profile),
    }
    render_overlay(frame, area, state, profile);
}

fn render_wide(frame: &mut Frame<'_>, area: Rect, state: &TuiState, profile: ResolvedPresentation) {
    let rows = shell_rows(area);
    render_header(frame, rows[0], state, profile, false);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(if state.rail_expanded { 32 } else { 9 }),
            Constraint::Min(36),
            Constraint::Length(30),
        ])
        .split(rows[1]);
    frame.render_widget(session_rail(state, profile), columns[0]);
    frame.render_widget(conversation(state, profile), columns[1]);
    frame.render_widget(activity(state, profile), columns[2]);
    render_composer(frame, rows[2], state, profile);
    render_footer(frame, rows[3], state, profile, "wide");
}

fn render_medium(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let rows = shell_rows(area);
    render_header(frame, rows[0], state, profile, false);
    frame.render_widget(conversation(state, profile), rows[1]);
    render_composer(frame, rows[2], state, profile);
    render_footer(frame, rows[3], state, profile, "medium");
}

fn render_narrow(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);
    render_header(frame, rows[0], state, profile, true);
    frame.render_widget(conversation(state, profile), rows[1]);
    render_composer(frame, rows[2], state, profile);
    render_footer(frame, rows[3], state, profile, "narrow");
}

fn shell_rows(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area)
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
    narrow: bool,
) {
    let title = Span::styled(
        if profile.unicode {
            "✦ Xana"
        } else {
            "[ Xana ]"
        },
        semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
    );
    let details = if narrow {
        format!("{} · {}", state.connection, state.status)
    } else {
        format!(
            "{} / {} · session {} · {}",
            state.connection, state.model, state.session, state.status
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![title, Span::raw("  "), Span::raw(details)]))
            .block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn session_rail(state: &TuiState, profile: ResolvedPresentation) -> Paragraph<'static> {
    let mut lines = Vec::new();
    for row in &state.sessions {
        let focused = row.conversation == state.viewed_conversation;
        let marker = session_marker(row.state, row.unread, row.error);
        if state.rail_expanded {
            lines.push(Line::styled(
                format!("{} {marker} {}", if focused { ">" } else { " " }, row.title),
                if focused {
                    semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ));
            lines.push(Line::styled(
                format!(
                    "  {} · {}/{} · {}",
                    row.execution_owner, row.connection, row.model, row.state
                ),
                semantic_style(profile, SemanticToken::Muted),
            ));
        } else {
            lines.push(Line::styled(
                format!("{} {marker}", if focused { ">" } else { " " }),
                semantic_style(profile, SemanticToken::Focus),
            ));
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "No sessions",
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    Paragraph::new(Text::from(lines))
        .block(Block::default().title(" Sessions ").borders(Borders::ALL))
        .wrap(Wrap { trim: false })
}

fn conversation(state: &TuiState, profile: ResolvedPresentation) -> Paragraph<'static> {
    let mut lines = Vec::new();
    for message in &state.messages {
        let (label, token) = match message.kind {
            MessageKind::User => ("you", SemanticToken::User),
            MessageKind::Assistant => ("xana", SemanticToken::Assistant),
            MessageKind::Tool => ("tool", SemanticToken::Tool),
            MessageKind::System => ("status", SemanticToken::Muted),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label}> "),
                semantic_style(profile, token).add_modifier(Modifier::BOLD),
            ),
            Span::raw(message.text.clone()),
        ]));
        lines.push(Line::raw(""));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "Start a conversation below.",
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title(" Conversation ")
                .borders(Borders::ALL),
        )
        .scroll((state.scroll, 0))
        .wrap(Wrap { trim: false })
}

fn activity(state: &TuiState, profile: ResolvedPresentation) -> Paragraph<'static> {
    let lines = if state.activity_visibility == ActivityVisibility::Quiet {
        vec![Line::styled(
            "Activity hidden (/activity normal)",
            semantic_style(profile, SemanticToken::Muted),
        )]
    } else if state.activity.is_empty() {
        vec![Line::styled(
            "No activity yet",
            semantic_style(profile, SemanticToken::Muted),
        )]
    } else {
        state
            .activity
            .iter()
            .map(|value| {
                Line::from(vec![
                    Span::styled("• ", semantic_style(profile, SemanticToken::Child)),
                    Span::raw(value.clone()),
                ])
            })
            .collect()
    };
    Paragraph::new(Text::from(lines))
        .block(Block::default().title(" Activity ").borders(Borders::ALL))
        .wrap(Wrap { trim: false })
}

fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let title = if state.busy {
        " Working - submit queues; Ctrl+C interrupts "
    } else {
        " Message - Enter/Ctrl+J use composer preset "
    };
    let attachment_note = if state.pending_image_count() == 0 {
        String::new()
    } else {
        format!("\n[{} image(s) staged]", state.pending_image_count())
    };
    frame.render_widget(
        Paragraph::new(format!("{}{}", state.composer.text, attachment_note))
            .style(semantic_style(profile, SemanticToken::User))
            .block(
                Block::default()
                    .title(title)
                    .border_style(semantic_style(profile, SemanticToken::Focus))
                    .borders(Borders::ALL),
            ),
        area,
    );

    let before_cursor = &state.composer.text[..state.composer.cursor];
    let row = before_cursor
        .chars()
        .filter(|character| *character == '\n')
        .count() as u16;
    let column = before_cursor
        .rsplit('\n')
        .next()
        .map_or(0, |line| line.chars().count()) as u16;
    frame.set_cursor_position((
        area.x
            .saturating_add(1)
            .saturating_add(column)
            .min(area.right().saturating_sub(2)),
        area.y
            .saturating_add(1)
            .saturating_add(row)
            .min(area.bottom().saturating_sub(2)),
    ));
}

fn render_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
    layout: &str,
) {
    let motion = if profile.reduced_motion {
        "reduced motion"
    } else {
        "motion allowed"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "Ctrl+P commands | Ctrl+Q quit | {layout} | {motion} | {} queued | {}",
            state.followups.len(),
            state.status
        ))
        .style(semantic_style(profile, SemanticToken::Muted)),
        area,
    );
}

fn render_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let Some(overlay) = &state.overlay else {
        return;
    };
    let popup = centered(
        area,
        72.min(area.width.saturating_sub(2)),
        16.min(area.height.saturating_sub(2)),
    );
    let (title, lines) = match overlay {
        Overlay::Palette { query, selected } => {
            let mut lines = vec![Line::from(vec![
                Span::styled("> ", semantic_style(profile, SemanticToken::Focus)),
                Span::raw(query.clone()),
            ])];
            for (index, command) in state.palette_entries().into_iter().enumerate() {
                let marker = if index == *selected { ">" } else { " " };
                let style = if index == *selected {
                    semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::styled(
                    format!("{marker} {:<13} {}", command.usage, command.summary),
                    style,
                ));
            }
            (" Commands ", lines)
        }
        Overlay::PastePreview { text } => (
            " Confirm pasted draft ",
            vec![
                Line::styled(
                    "Paste is untrusted text. Enter inserts it; Esc discards it.",
                    semantic_style(profile, SemanticToken::Warning),
                ),
                Line::raw(""),
                Line::raw(text.clone()),
            ],
        ),
        Overlay::Help => (
            " Keyboard help ",
            vec![
                Line::raw("Ctrl+P commands   Ctrl+Q quit   Ctrl+C interrupt"),
                Line::raw("Enter primary     Ctrl+J alternate     Shift+Enter newline"),
                Line::raw("Ctrl+Enter submit   arrows move/select   mouse wheel scrolls"),
                Line::raw("Slash commands and palette entries share one registry."),
            ],
        ),
        Overlay::Queue => {
            let mut lines = vec![Line::raw(
                "Follow-ups run in order. /queue edit N or /queue remove N.",
            )];
            lines.extend(state.followups.iter().enumerate().map(|(index, turn)| {
                Line::raw(format!("{}. {}", index + 1, turn.input.replace('\n', " ")))
            }));
            (" Follow-up queue ", lines)
        }
        Overlay::ModelPicker { choices, selected } => (
            " Select model (starts a new conversation) ",
            choice_lines(choices, *selected, profile),
        ),
        Overlay::ReasoningPicker { choices, selected } => (
            " Select reasoning effort ",
            choice_lines(choices, *selected, profile),
        ),
        Overlay::SessionPicker {
            query,
            choices,
            selected,
        } => {
            let filtered = choices
                .iter()
                .filter(|row| session_row_matches(row, query))
                .collect::<Vec<_>>();
            let mut lines = vec![Line::from(vec![
                Span::styled("> ", semantic_style(profile, SemanticToken::Focus)),
                Span::raw(query.clone()),
            ])];
            lines.extend(filtered.into_iter().enumerate().map(|(index, row)| {
                let marker = if index == *selected { ">" } else { " " };
                let recency = row.modified_unix.map_or_else(
                    || "recency unknown".to_owned(),
                    |value| format!("updated {value}"),
                );
                Line::styled(
                    format!(
                        "{marker} {} [{} · {}/{} · {} · {recency}{}]",
                        row.title,
                        row.execution_owner,
                        row.connection,
                        row.model,
                        row.state,
                        row.record_count
                            .map_or_else(String::new, |count| format!(" · {count} records")),
                    ),
                    if index == *selected {
                        semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                )
            }));
            (" Conversations ", lines)
        }
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(title)
                    .border_style(semantic_style(profile, SemanticToken::Focus))
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn session_marker(
    state: crate::workspace_host::ConversationState,
    unread: bool,
    error: bool,
) -> String {
    let state = match state {
        crate::workspace_host::ConversationState::Inactive => "idle",
        crate::workspace_host::ConversationState::Active => "active",
        crate::workspace_host::ConversationState::Controlled => "control",
        crate::workspace_host::ConversationState::Observable => "observe",
        crate::workspace_host::ConversationState::Unavailable => "unavail",
    };
    format!(
        "[{state}{}{}]",
        if unread { " unread" } else { "" },
        if error { " error" } else { "" }
    )
}

fn session_row_matches(row: &super::session::SessionRow, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty()
        || row.title.to_ascii_lowercase().contains(&query)
        || row.connection.to_ascii_lowercase().contains(&query)
        || row.model.to_ascii_lowercase().contains(&query)
        || row.execution_owner.contains(&query)
        || row.state.to_string().contains(&query)
}

fn choice_lines(
    choices: &[String],
    selected: usize,
    profile: ResolvedPresentation,
) -> Vec<Line<'static>> {
    choices
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let marker = if index == selected { ">" } else { " " };
            let style = if index == selected {
                semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::styled(format!("{marker} {choice}"), style)
        })
        .collect()
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width.max(1),
        height.max(1),
    )
}

fn semantic_style(profile: ResolvedPresentation, token: SemanticToken) -> Style {
    profile
        .color(token)
        .map_or_else(Style::default, |color| Style::default().fg(to_color(color)))
}

fn to_color(color: PresentationColor) -> Color {
    match color {
        PresentationColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        PresentationColor::Indexed(index) => Color::Indexed(index),
        PresentationColor::Red => Color::Red,
        PresentationColor::Green => Color::Green,
        PresentationColor::Yellow => Color::Yellow,
        PresentationColor::Magenta => Color::Magenta,
        PresentationColor::Cyan => Color::Cyan,
        PresentationColor::DarkGray => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
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
    fn command_queue_and_model_overlays_have_bounded_readable_snapshots() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.busy = false;
        state.update_input(super::super::model::InputAction::OpenPalette);
        state.update_input(super::super::model::InputAction::Insert("mod".to_owned()));
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
            }],
            active: None,
        });
        state.open_session_picker();
        terminal
            .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
            .unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("Conversations"));
        assert!(rendered.contains("12 records"));
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
}
