//! Pure Ratatui rendering for the adaptive shell.

mod conversation;
mod popup;

use super::{
    activity::{ActivityKind, ActivityState},
    command,
    state::{ActivityVisibility, LayoutClass, ScreenPoint, TuiState},
};
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
    if LayoutClass::for_width(area.width) != LayoutClass::Wide && activity_visible(state) {
        render_activity_drawer(frame, area, state, profile);
    }
    popup::render(frame, area, state, profile);
}

pub(super) fn pointer_action(
    column: u16,
    row: u16,
    selecting: bool,
    state: &TuiState,
    area: Rect,
) -> Option<super::state::InputAction> {
    if let Some(overlay) = &state.overlay {
        let popup = overlay_area(area);
        let Some(content_row) = row.checked_sub(popup.y.saturating_add(1)) else {
            return state
                .conversation_selection
                .is_some()
                .then_some(super::state::InputAction::ClearConversationSelection);
        };
        if column <= popup.x || column >= popup.right().saturating_sub(1) {
            return state
                .conversation_selection
                .is_some()
                .then_some(super::state::InputAction::ClearConversationSelection);
        }
        return overlay_choice_at(overlay, content_row, popup.height.saturating_sub(2))
            .map(super::state::InputAction::ChooseOverlay)
            .or_else(|| {
                state
                    .conversation_selection
                    .is_some()
                    .then_some(super::state::InputAction::ClearConversationSelection)
            });
    }

    let layout = shell_layout(area, state);
    let conversation_area = conversation_area(layout, state, area.width);
    let conversation_content = panel_content(conversation_area);
    if selecting && state.conversation_selection.is_some() {
        return Some(super::state::InputAction::ExtendConversationSelection(
            clamp_point(conversation_content, column, row),
        ));
    }
    if contains(layout.header, column, row) {
        return Some(super::state::InputAction::ToggleHeader);
    }
    if contains(layout.composer, column, row) {
        let width = layout.composer.width.saturating_sub(2).max(1);
        let maximum_rows = layout.composer.height.saturating_sub(2).clamp(1, 6);
        let viewport = state.composer.viewport(width, maximum_rows);
        return Some(super::state::InputAction::PlaceCursor {
            line: usize::from(row.saturating_sub(layout.composer.y.saturating_add(1))),
            column: usize::from(column.saturating_sub(layout.composer.x.saturating_add(1))),
            width,
            scroll: viewport.scroll,
            select: selecting,
        });
    }
    if LayoutClass::for_width(area.width) == LayoutClass::Wide {
        let columns = wide_columns(layout.body, state);
        if state.rail_expanded && contains(columns[0], column, row) {
            if row == columns[0].y {
                return Some(super::state::InputAction::ToggleSessionsView);
            }
            let line = usize::from(row.saturating_sub(columns[0].y.saturating_add(1)));
            let index = line / 2;
            return state.sessions.get(index).map(|session| {
                super::state::InputAction::ViewSession(session.conversation.clone())
            });
        }
        if activity_visible(state) && contains(columns[2], column, row) {
            return activity_at(state, row.saturating_sub(columns[2].y.saturating_add(1)))
                .map(super::state::InputAction::ToggleActivity);
        }
    } else if activity_visible(state) {
        let drawer = activity_drawer_area(area);
        if contains(drawer, column, row) {
            return activity_at(state, row.saturating_sub(drawer.y.saturating_add(1)))
                .map(super::state::InputAction::ToggleActivity);
        }
    }
    if contains(conversation_content, column, row) {
        return Some(super::state::InputAction::BeginConversationSelection(
            ScreenPoint { column, row },
        ));
    }
    state
        .conversation_selection
        .is_some()
        .then_some(super::state::InputAction::ClearConversationSelection)
}

pub(super) fn pointer_release_action(
    column: u16,
    row: u16,
    state: &TuiState,
    area: Rect,
) -> Option<super::state::InputAction> {
    let selection = state.conversation_selection.as_ref()?;
    let layout = shell_layout(area, state);
    let conversation_area = conversation_area(layout, state, area.width);
    let end = clamp_point(panel_content(conversation_area), column, row);
    let text = (selection.dragged || end != selection.start)
        .then(|| conversation::selected_text(state, conversation_area, selection.start, end))
        .filter(|text| !text.is_empty());
    Some(super::state::InputAction::FinishConversationSelection { end, text })
}

fn render_wide(frame: &mut Frame<'_>, area: Rect, state: &TuiState, profile: ResolvedPresentation) {
    let shell = shell_layout(area, state);
    render_header(frame, shell.header, state, profile, false);
    let columns = wide_columns(shell.body, state);
    if state.rail_expanded {
        frame.render_widget(session_rail(state, profile), columns[0]);
    }
    conversation::render(frame, columns[1], state, profile);
    if activity_visible(state) {
        frame.render_widget(activity(state, profile), columns[2]);
    }
    render_composer(frame, shell.composer, state, profile);
    render_footer(frame, shell.footer, state, profile, "wide");
}

fn render_medium(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let shell = shell_layout(area, state);
    render_header(frame, shell.header, state, profile, false);
    conversation::render(frame, shell.body, state, profile);
    render_composer(frame, shell.composer, state, profile);
    render_footer(frame, shell.footer, state, profile, "medium");
}

fn render_narrow(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let shell = shell_layout(area, state);
    render_header(frame, shell.header, state, profile, true);
    conversation::render(frame, shell.body, state, profile);
    render_composer(frame, shell.composer, state, profile);
    render_footer(frame, shell.footer, state, profile, "narrow");
}

#[derive(Debug, Clone, Copy)]
struct ShellLayout {
    header: Rect,
    body: Rect,
    composer: Rect,
    footer: Rect,
}

fn shell_layout(area: Rect, state: &TuiState) -> ShellLayout {
    let class = LayoutClass::for_width(area.width);
    let expanded = state.header_expanded && area.height >= 20;
    let header_height = if expanded {
        8
    } else if class == LayoutClass::Narrow {
        2
    } else {
        3
    };
    let composer_width = area.width.saturating_sub(2).max(1);
    let maximum_composer_rows = area
        .height
        .saturating_sub(header_height)
        .saturating_sub(1)
        .saturating_sub(4)
        .saturating_sub(2)
        .clamp(1, 6);
    let composer_height = state
        .composer
        .viewport(composer_width, maximum_composer_rows)
        .rows
        .saturating_add(2);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(4),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
    ShellLayout {
        header: rows[0],
        body: rows[1],
        composer: rows[2],
        footer: rows[3],
    }
}

fn wide_columns(area: Rect, state: &TuiState) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(if state.rail_expanded { 32 } else { 0 }),
            Constraint::Min(36),
            Constraint::Length(if activity_visible(state) { 30 } else { 0 }),
        ])
        .split(area)
}

fn conversation_area(layout: ShellLayout, state: &TuiState, width: u16) -> Rect {
    if LayoutClass::for_width(width) == LayoutClass::Wide {
        wide_columns(layout.body, state)[1]
    } else {
        layout.body
    }
}

fn panel_content(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn clamp_point(area: Rect, column: u16, row: u16) -> ScreenPoint {
    ScreenPoint {
        column: column.min(area.right().saturating_sub(1)).max(area.x),
        row: row.min(area.bottom().saturating_sub(1)).max(area.y),
    }
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn activity_at(state: &TuiState, content_row: u16) -> Option<usize> {
    let mut start = 0u16;
    for (index, card) in state.activity.iter().enumerate() {
        let height = if card.expanded && !card.detail.is_empty() {
            2
        } else {
            1
        };
        if content_row >= start && content_row < start.saturating_add(height) {
            return Some(index);
        }
        start = start.saturating_add(height);
    }
    None
}

fn overlay_choice_at(
    overlay: &super::state::Overlay,
    content_row: u16,
    content_height: u16,
) -> Option<usize> {
    let first = match overlay {
        super::state::Overlay::Palette { query, selected } => {
            let choices = command::search(query).len();
            let visible = usize::from(content_height.saturating_sub(3));
            let start = palette_window_start(*selected, choices, visible);
            return usize::from(content_row)
                .checked_sub(3)
                .map(|row| start.saturating_add(row))
                .filter(|index| *index < choices);
        }
        super::state::Overlay::SessionPicker { .. } => 1,
        super::state::Overlay::ModelPicker { .. }
        | super::state::Overlay::ReasoningPicker { .. } => 0,
        super::state::Overlay::Approval { prompt, .. } => 3 + prompt.details.len(),
        super::state::Overlay::Artifact { preview, .. } => {
            if preview.is_some() {
                6
            } else {
                4
            }
        }
        super::state::Overlay::PastePreview { .. }
        | super::state::Overlay::Help
        | super::state::Overlay::Queue => return None,
    };
    usize::from(content_row).checked_sub(first)
}

fn overlay_area(area: Rect) -> Rect {
    centered(
        area,
        100.min(area.width.saturating_sub(2)),
        18.min(area.height.saturating_sub(2)),
    )
}

fn palette_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len <= visible {
        0
    } else {
        selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(len - visible)
    }
}

fn activity_drawer_area(area: Rect) -> Rect {
    centered(
        area,
        area.width.saturating_sub(4).min(72),
        area.height.saturating_sub(6).min(18),
    )
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
    narrow: bool,
) {
    if state.header_expanded && area.height >= 8 {
        let block = Block::default()
            .title(format!(" Xana v{} ", env!("CARGO_PKG_VERSION")))
            .title_style(
                semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(semantic_style(profile, SemanticToken::Accent));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let details = vec![
            Line::from(vec![
                Span::styled(
                    "Connection  ",
                    semantic_style(profile, SemanticToken::Muted),
                ),
                Span::raw(state.connection.clone()),
            ]),
            Line::from(vec![
                Span::styled(
                    "Model       ",
                    semantic_style(profile, SemanticToken::Muted),
                ),
                Span::raw(state.model.clone()),
            ]),
            Line::from(vec![
                Span::styled(
                    "Session     ",
                    semantic_style(profile, SemanticToken::Muted),
                ),
                Span::raw(state.session.clone()),
            ]),
            Line::from(vec![
                Span::styled(
                    "Status      ",
                    semantic_style(profile, SemanticToken::Muted),
                ),
                Span::raw(state.status.clone()),
            ]),
            Line::styled(
                if profile.unicode {
                    "Type to collapse · click this panel or use /header to reopen"
                } else {
                    "Type to collapse - click this panel or use /header to reopen"
                },
                semantic_style(profile, SemanticToken::Muted),
            ),
        ];
        if narrow {
            frame.render_widget(Paragraph::new(details).wrap(Wrap { trim: false }), inner);
        } else {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(36), Constraint::Min(20)])
                .split(inner);
            let mark = crate::presentation::tui_wordmark(profile.unicode)
                .iter()
                .map(|line| {
                    Line::styled(
                        (*line).to_owned(),
                        semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
                    )
                })
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(mark), columns[0]);
            frame.render_widget(
                Paragraph::new(details).wrap(Wrap { trim: false }),
                columns[1],
            );
        }
        return;
    }
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
        Paragraph::new(Line::from(vec![
            title,
            Span::raw("  "),
            Span::raw(details),
            Span::styled("  [/header]", semantic_style(profile, SemanticToken::Muted)),
        ]))
        .block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn session_rail(state: &TuiState, profile: ResolvedPresentation) -> Paragraph<'static> {
    let mut lines = Vec::new();
    for row in &state.sessions {
        let focused = row.conversation == state.viewed_conversation;
        let marker = popup::session_marker(row.state, row.unread, row.error);
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
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "No sessions",
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    Paragraph::new(Text::from(lines)).block(
        Block::default()
            .title(" Sessions (click to hide) ")
            .borders(Borders::ALL),
    )
}

fn activity(state: &TuiState, profile: ResolvedPresentation) -> Paragraph<'static> {
    let lines = if state.activity.is_empty() {
        vec![Line::styled(
            "No activity yet",
            semantic_style(profile, SemanticToken::Muted),
        )]
    } else {
        state
            .activity
            .iter()
            .flat_map(|card| {
                let marker = match card.state {
                    ActivityState::Running => ">",
                    ActivityState::Waiting => "?",
                    ActivityState::Complete => "+",
                    ActivityState::Failed => "!",
                };
                let token = match card.kind {
                    ActivityKind::Approval => SemanticToken::Approval,
                    ActivityKind::Warning | ActivityKind::Error => SemanticToken::Warning,
                    ActivityKind::Child => SemanticToken::Child,
                    ActivityKind::ReasoningSummary | ActivityKind::ReasoningRaw => {
                        SemanticToken::Reasoning
                    }
                    ActivityKind::Tool | ActivityKind::Diff => SemanticToken::Tool,
                    _ => SemanticToken::Muted,
                };
                let mut lines = vec![Line::from(vec![
                    Span::styled(format!("{marker} "), semantic_style(profile, token)),
                    Span::styled(
                        format!("{}: ", card.owner),
                        semantic_style(profile, SemanticToken::Muted),
                    ),
                    Span::raw(card.summary.clone()),
                ])];
                if card.expanded && !card.detail.is_empty() {
                    lines.push(Line::styled(
                        format!("  {}", card.detail),
                        semantic_style(profile, SemanticToken::Muted),
                    ));
                }
                lines
            })
            .collect()
    };
    Paragraph::new(Text::from(lines))
        .block(Block::default().title(" Activity ").borders(Borders::ALL))
        .wrap(Wrap { trim: false })
}

fn activity_visible(state: &TuiState) -> bool {
    match state.activity_visibility {
        ActivityVisibility::Open => true,
        ActivityVisibility::Hidden => false,
        ActivityVisibility::Auto => state.auto_activity_open,
    }
}

fn render_activity_drawer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let popup = activity_drawer_area(area);
    frame.render_widget(Clear, popup);
    frame.render_widget(activity(state, profile), popup);
}

fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    let selection_ready = state
        .conversation_selection
        .as_ref()
        .is_some_and(|selection| selection.text.is_some());
    let mut title = if selection_ready {
        " Message - Ctrl+C copies selection / click away to clear ".to_owned()
    } else if state.busy {
        " Message - Enter queues / Ctrl+C interrupts ".to_owned()
    } else {
        match state.composer_preset {
            crate::presentation::ComposerPreset::Submit => {
                " Message - Enter to send / Shift+Enter or Ctrl+J for newline ".to_owned()
            }
            crate::presentation::ComposerPreset::Newline => {
                " Message - Enter for newline / Ctrl+Enter or Ctrl+J to send ".to_owned()
            }
        }
    };
    if state.pending_image_count() > 0 {
        title.push_str(&format!("[{} image(s)] ", state.pending_image_count()));
    }
    let width = area.width.saturating_sub(2).max(1);
    let maximum_rows = area.height.saturating_sub(2).clamp(1, 6);
    let viewport = state.composer.viewport(width, maximum_rows);
    let text = if state.composer.text.is_empty() {
        Text::from(Line::styled(
            if profile.unicode {
                "Type a message…"
            } else {
                "Type a message..."
            },
            semantic_style(profile, SemanticToken::Muted),
        ))
    } else {
        Text::from(
            state
                .composer
                .visible_lines(width, viewport)
                .into_iter()
                .map(Line::raw)
                .collect::<Vec<_>>(),
        )
    };
    frame.render_widget(
        Paragraph::new(text)
            .style(semantic_style(profile, SemanticToken::User))
            .block(
                Block::default()
                    .title(title)
                    .border_style(semantic_style(profile, SemanticToken::Focus))
                    .borders(Borders::ALL),
            ),
        area,
    );

    frame.set_cursor_position((
        area.x
            .saturating_add(1)
            .saturating_add(viewport.cursor_column)
            .min(area.right().saturating_sub(2)),
        area.y
            .saturating_add(1)
            .saturating_add(viewport.cursor_row)
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
    let selection_help = if state
        .conversation_selection
        .as_ref()
        .is_some_and(|selection| selection.text.is_some())
    {
        "Ctrl+C copy selection"
    } else {
        "drag conversation to select"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{layout} | Ctrl+P commands | {selection_help} | Ctrl+Q quit | {motion} | {} queued | {}",
            state.followups.len(),
            state.status
        ))
        .style(semantic_style(profile, SemanticToken::Muted)),
        area,
    );
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
mod tests;
