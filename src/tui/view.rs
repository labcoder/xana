//! Pure Ratatui rendering for the adaptive shell.

use super::{
    activity::{ActivityKind, ActivityState},
    command,
    model::{ActivityVisibility, LayoutClass, MessageKind, Overlay, ScreenPoint, TuiState},
    rich_text::RichLineKind,
};
use crate::presentation::{PresentationColor, ResolvedPresentation, SemanticToken};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState, Widget,
        Wrap,
    },
};

const MAX_COPY_CELLS: usize = 256 * 1024;

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
    render_overlay(frame, area, state, profile);
}

pub(super) fn pointer_action(
    column: u16,
    row: u16,
    selecting: bool,
    state: &TuiState,
    area: Rect,
) -> Option<super::model::InputAction> {
    if let Some(overlay) = &state.overlay {
        let popup = overlay_area(area);
        let content_row = row.checked_sub(popup.y.saturating_add(1))?;
        if column <= popup.x || column >= popup.right().saturating_sub(1) {
            return None;
        }
        return overlay_choice_at(overlay, content_row, popup.height.saturating_sub(2))
            .map(super::model::InputAction::ChooseOverlay);
    }

    let layout = shell_layout(area, state);
    let conversation_area = conversation_area(layout, state, area.width);
    let conversation_content = panel_content(conversation_area);
    if selecting && state.conversation_selection.is_some() {
        return Some(super::model::InputAction::ExtendConversationSelection(
            clamp_point(conversation_content, column, row),
        ));
    }
    if contains(layout.header, column, row) {
        return Some(super::model::InputAction::ToggleHeader);
    }
    if contains(layout.composer, column, row) {
        let width = layout.composer.width.saturating_sub(2).max(1);
        let maximum_rows = layout.composer.height.saturating_sub(2).clamp(1, 6);
        let viewport = state.composer.viewport(width, maximum_rows);
        return Some(super::model::InputAction::PlaceCursor {
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
                return Some(super::model::InputAction::ToggleSessionsView);
            }
            let line = usize::from(row.saturating_sub(columns[0].y.saturating_add(1)));
            let index = line / 2;
            return state.sessions.get(index).map(|session| {
                super::model::InputAction::ViewSession(session.conversation.clone())
            });
        }
        if activity_visible(state) && contains(columns[2], column, row) {
            return activity_at(state, row.saturating_sub(columns[2].y.saturating_add(1)))
                .map(super::model::InputAction::ToggleActivity);
        }
    } else if activity_visible(state) {
        let drawer = activity_drawer_area(area);
        if contains(drawer, column, row) {
            return activity_at(state, row.saturating_sub(drawer.y.saturating_add(1)))
                .map(super::model::InputAction::ToggleActivity);
        }
    }
    if contains(conversation_content, column, row) {
        return Some(super::model::InputAction::BeginConversationSelection(
            ScreenPoint { column, row },
        ));
    }
    None
}

pub(super) fn pointer_release_action(
    column: u16,
    row: u16,
    state: &TuiState,
    area: Rect,
) -> Option<super::model::InputAction> {
    let selection = state.conversation_selection?;
    let layout = shell_layout(area, state);
    let conversation_area = conversation_area(layout, state, area.width);
    let end = clamp_point(panel_content(conversation_area), column, row);
    let text = (selection.dragged || end != selection.start)
        .then(|| selected_conversation_text(state, conversation_area, selection.start, end))
        .filter(|text| !text.is_empty());
    Some(super::model::InputAction::FinishConversationSelection { end, text })
}

fn render_wide(frame: &mut Frame<'_>, area: Rect, state: &TuiState, profile: ResolvedPresentation) {
    let shell = shell_layout(area, state);
    render_header(frame, shell.header, state, profile, false);
    let columns = wide_columns(shell.body, state);
    if state.rail_expanded {
        frame.render_widget(session_rail(state, profile), columns[0]);
    }
    render_conversation(frame, columns[1], state, profile);
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
    render_conversation(frame, shell.body, state, profile);
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
    render_conversation(frame, shell.body, state, profile);
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
    overlay: &super::model::Overlay,
    content_row: u16,
    content_height: u16,
) -> Option<usize> {
    let first = match overlay {
        super::model::Overlay::Palette { query, selected } => {
            let choices = command::search(query).len();
            let visible = usize::from(content_height.saturating_sub(3));
            let start = palette_window_start(*selected, choices, visible);
            return usize::from(content_row)
                .checked_sub(3)
                .map(|row| start.saturating_add(row))
                .filter(|index| *index < choices);
        }
        super::model::Overlay::SessionPicker { .. } => 1,
        super::model::Overlay::ModelPicker { .. }
        | super::model::Overlay::ReasoningPicker { .. } => 0,
        super::model::Overlay::Approval { prompt, .. } => 3 + prompt.details.len(),
        super::model::Overlay::Artifact { preview, .. } => {
            if preview.is_some() {
                6
            } else {
                4
            }
        }
        super::model::Overlay::PastePreview { .. }
        | super::model::Overlay::Help
        | super::model::Overlay::Queue => return None,
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
        let marker = session_marker(row.state, row.unread, row.error);
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

fn render_conversation(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    frame.render_widget(conversation(state, profile, area), area);
    if let Some(selection) = state.conversation_selection {
        highlight_selection(
            frame.buffer_mut(),
            panel_content(area),
            selection.start,
            selection.end,
        );
    }
}

fn conversation(state: &TuiState, profile: ResolvedPresentation, area: Rect) -> Paragraph<'static> {
    let width = area.width.saturating_sub(2).max(1);
    let visible_rows = usize::from(area.height.saturating_sub(2).max(1));
    let target_rows = visible_rows.saturating_add(usize::from(state.scroll));
    let mut batches = Vec::new();
    let mut selected_rows = 0usize;
    let mut start = state.messages.len();
    for (index, message) in state.messages.iter().enumerate().rev() {
        let batch = conversation_message_lines(message, profile);
        selected_rows = selected_rows.saturating_add(wrapped_height(&batch, width));
        batches.push(batch);
        start = index;
        if selected_rows >= target_rows {
            break;
        }
    }

    let mut lines = Vec::new();
    for batch in batches.into_iter().rev() {
        lines.extend(batch);
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "Start a conversation below.",
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    let total_rows = wrapped_height(&lines, width);
    let title = if start > 0 {
        format!(" Conversation · {start} older message(s) outside this viewport ")
    } else {
        " Conversation ".to_owned()
    };
    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().title(title).borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    let bottom = total_rows.saturating_sub(visible_rows);
    let offset = bottom.saturating_sub(usize::from(state.scroll));
    paragraph.scroll((offset.min(usize::from(u16::MAX)) as u16, 0))
}

fn selected_conversation_text(
    state: &TuiState,
    area: Rect,
    start: ScreenPoint,
    end: ScreenPoint,
) -> String {
    let mut buffer = Buffer::empty(area);
    conversation(state, ResolvedPresentation::plain(), area).render(area, &mut buffer);
    selection_text(&buffer, panel_content(area), start, end)
}

fn highlight_selection(buffer: &mut Buffer, area: Rect, start: ScreenPoint, end: ScreenPoint) {
    for point in selected_points(area, start, end) {
        if let Some(cell) = buffer.cell_mut((point.column, point.row)) {
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}

fn selection_text(buffer: &Buffer, area: Rect, start: ScreenPoint, end: ScreenPoint) -> String {
    let (start, end) = ordered_points(
        clamp_point(area, start.column, start.row),
        clamp_point(area, end.column, end.row),
    );
    let mut output = String::new();
    let mut cells = 0usize;
    let mut wrote_row = false;
    for row in start.row..=end.row {
        let first = if row == start.row {
            start.column
        } else {
            area.x
        };
        let last = if row == end.row {
            end.column
        } else {
            area.right().saturating_sub(1)
        };
        let mut line = String::new();
        let mut reached_limit = false;
        for column in first..=last {
            if cells >= MAX_COPY_CELLS {
                reached_limit = true;
                break;
            }
            if let Some(cell) = buffer.cell((column, row)) {
                line.push_str(cell.symbol());
            }
            cells += 1;
        }
        if wrote_row {
            output.push('\n');
        }
        output.push_str(line.trim_end());
        wrote_row = true;
        if reached_limit {
            break;
        }
    }
    output.trim_end().to_owned()
}

fn selected_points(
    area: Rect,
    start: ScreenPoint,
    end: ScreenPoint,
) -> impl Iterator<Item = ScreenPoint> {
    let (start, end) = ordered_points(
        clamp_point(area, start.column, start.row),
        clamp_point(area, end.column, end.row),
    );
    (start.row..=end.row)
        .flat_map(move |row| {
            let first = if row == start.row {
                start.column
            } else {
                area.x
            };
            let last = if row == end.row {
                end.column
            } else {
                area.right().saturating_sub(1)
            };
            (first..=last).map(move |column| ScreenPoint { column, row })
        })
        .take(MAX_COPY_CELLS)
}

fn ordered_points(left: ScreenPoint, right: ScreenPoint) -> (ScreenPoint, ScreenPoint) {
    if (left.row, left.column) <= (right.row, right.column) {
        (left, right)
    } else {
        (right, left)
    }
}

fn conversation_message_lines(
    message: &super::model::VisibleMessage,
    profile: ResolvedPresentation,
) -> Vec<Line<'static>> {
    let (label, token) = match message.kind {
        MessageKind::User => ("you", SemanticToken::User),
        MessageKind::Assistant => ("xana", SemanticToken::Assistant),
        MessageKind::Tool => ("tool", SemanticToken::Tool),
        MessageKind::System => ("status", SemanticToken::Muted),
    };
    let header = match message.kind {
        MessageKind::User => Line::styled(
            "-------------------< you",
            semantic_style(profile, token).add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Right),
        MessageKind::Assistant => Line::styled(
            "xana >-------------------",
            semantic_style(profile, token).add_modifier(Modifier::BOLD),
        ),
        MessageKind::Tool | MessageKind::System => Line::styled(
            format!("{label}>"),
            semantic_style(profile, token).add_modifier(Modifier::BOLD),
        ),
    };
    let mut lines = vec![header];
    for rich in &message.document.lines {
        let (prefix, style) = match rich.kind {
            RichLineKind::Heading => (
                "# ",
                semantic_style(profile, token).add_modifier(Modifier::BOLD),
            ),
            RichLineKind::List => ("  ", Style::default()),
            RichLineKind::Quote => ("> ", semantic_style(profile, SemanticToken::Muted)),
            RichLineKind::Table => ("  ", semantic_style(profile, SemanticToken::Tool)),
            RichLineKind::Code => ("  ", semantic_style(profile, SemanticToken::Tool)),
            RichLineKind::DiffAdd => ("+ ", semantic_style(profile, SemanticToken::DiffAdd)),
            RichLineKind::DiffRemove => ("- ", semantic_style(profile, SemanticToken::DiffRemove)),
            RichLineKind::Warning => ("! ", semantic_style(profile, SemanticToken::Warning)),
            RichLineKind::Paragraph => ("  ", Style::default()),
        };
        let mut style = style;
        if rich.emphasized {
            style = style.add_modifier(Modifier::BOLD);
        }
        if rich.inline_code {
            style = style.add_modifier(Modifier::DIM);
        }
        let line = Line::styled(format!("{prefix}{}", rich.text), style);
        lines.push(if message.kind == MessageKind::User {
            line.alignment(Alignment::Right)
        } else {
            line
        });
    }
    for (index, link) in message.document.links.iter().enumerate() {
        lines.push(Line::styled(
            format!("  link {}: {} -> {}", index + 1, link.label, link.target),
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    for artifact in &message.document.artifacts {
        lines.push(Line::styled(
            format!(
                "  artifact {}: {} (/artifact {})",
                artifact.record.reference.id, artifact.label, artifact.record.reference.id
            ),
            semantic_style(profile, SemanticToken::Accent),
        ));
    }
    if message.document.truncated {
        lines.push(Line::styled(
            "  [rich preview truncated at its safety bound]",
            semantic_style(profile, SemanticToken::Warning),
        ));
    }
    lines.push(Line::raw(""));
    lines
}

fn wrapped_height(lines: &[Line<'static>], width: u16) -> usize {
    let width = usize::from(width.max(1));
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
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
    let mut title = if state.busy {
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
    frame.render_widget(
        Paragraph::new(format!(
            "{layout} | Ctrl+P commands | drag conversation to copy | Ctrl+Q quit | {motion} | {} queued | {}",
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
    let popup = overlay_area(area);
    if let Overlay::Palette { query, selected } = overlay {
        render_command_palette(frame, popup, state, query, *selected, profile);
        return;
    }
    let (title, lines) = match overlay {
        Overlay::Palette { .. } => unreachable!("palette is rendered as a stateful table"),
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
                Line::raw(
                    "Drag conversation text to copy; Shift+drag uses terminal-native selection anywhere.",
                ),
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
                let identifier = match &row.conversation {
                    crate::workspace_host::ConversationRef::Native { session_id } => {
                        session_id.to_string()
                    }
                    crate::workspace_host::ConversationRef::Managed {
                        connection,
                        thread_id,
                    } => format!("{connection}/{thread_id}"),
                    crate::workspace_host::ConversationRef::NewNative => "new-native".to_owned(),
                    crate::workspace_host::ConversationRef::NewManaged { connection } => {
                        format!("{connection}/new")
                    }
                };
                let recency = row.modified_unix.map_or_else(
                    || "recency unknown".to_owned(),
                    |value| format!("updated {value}"),
                );
                Line::styled(
                    format!(
                        "{marker} {} · {identifier} [{} · {}/{} · {} · {recency}{}]",
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
            (" Sessions ", lines)
        }
        Overlay::Approval { prompt, selected } => {
            let mut lines = vec![
                Line::styled(
                    format!("Authority requested by {}", prompt.owner),
                    semantic_style(profile, SemanticToken::Approval).add_modifier(Modifier::BOLD),
                ),
                Line::raw(prompt.title.clone()),
            ];
            lines.extend(
                prompt
                    .details
                    .iter()
                    .map(|detail| Line::raw(detail.clone())),
            );
            lines.push(Line::raw(""));
            let mut index = 0usize;
            for (label, enabled) in [
                ("Allow once", prompt.allow_once),
                ("Allow exact scope for this session", prompt.allow_session),
                ("Deny", prompt.deny),
            ] {
                if !enabled {
                    continue;
                }
                lines.push(Line::styled(
                    format!("{} {label}", if index == *selected { ">" } else { " " }),
                    if index == *selected {
                        semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ));
                index += 1;
            }
            (" Approval required ", lines)
        }
        Overlay::Artifact {
            artifact,
            selected,
            preview,
        } => {
            let mut lines = vec![
                Line::styled(
                    format!("Immutable artifact {}", artifact.record.reference.id),
                    semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
                ),
                Line::raw(format!(
                    "{} · {} · {} bytes",
                    artifact.label, artifact.record.media_type, artifact.record.byte_len
                )),
                Line::styled(
                    "Nothing opens automatically. Enter runs only the highlighted action.",
                    semantic_style(profile, SemanticToken::Warning),
                ),
            ];
            if let Some(preview) = preview {
                lines.push(Line::raw(""));
                lines.push(Line::raw(preview.clone()));
            }
            lines.push(Line::raw(""));
            for (index, action) in [
                "Preview bounded bytes",
                "Insert immutable reference into draft",
                "Reveal in the OS file manager",
                "Open with the OS default application",
            ]
            .into_iter()
            .enumerate()
            {
                lines.push(Line::styled(
                    format!("{} {action}", if index == *selected { ">" } else { " " }),
                    if index == *selected {
                        semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ));
            }
            (" Artifact actions ", lines)
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

fn render_command_palette(
    frame: &mut Frame<'_>,
    popup: Rect,
    state: &TuiState,
    query: &str,
    selected: usize,
    profile: ResolvedPresentation,
) {
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Commands ")
        .border_style(semantic_style(profile, SemanticToken::Focus))
        .borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let sections = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Filter: ", semantic_style(profile, SemanticToken::Muted)),
                Span::styled(
                    query.to_owned(),
                    semantic_style(profile, SemanticToken::Focus),
                ),
            ]),
            Line::styled(
                "Type a command, mode, parameter, or description; a leading / is optional.",
                semantic_style(profile, SemanticToken::Muted),
            ),
        ]),
        sections[0],
    );

    let entries = state.palette_entries();
    let rows = entries.iter().map(|command| {
        Row::new(vec![
            Cell::from(if command.id == super::command::CommandId::Reset {
                "Reset Xana state…".to_owned()
            } else {
                format!("/{}", command.name)
            }),
            Cell::from(command.mode),
            Cell::from(command.summary),
        ])
    });
    let header = Row::new(vec!["COMMAND", "MODE OR PARAMETERS", "DESCRIPTION"])
        .style(semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD));
    let visible = usize::from(sections[1].height.saturating_sub(1));
    let offset = palette_window_start(selected, entries.len(), visible);
    let mut table_state = TableState::new()
        .with_offset(offset)
        .with_selected((!entries.is_empty()).then_some(selected));
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(27),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD))
    .highlight_symbol("> ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, sections[1], &mut table_state);
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
        assert!(rendered.contains("Sessions"));
        assert!(rendered.contains("12 records"));
    }

    #[test]
    fn command_palette_keeps_the_keyboard_selection_inside_its_viewport() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.update_input(super::super::model::InputAction::OpenPalette);
        for _ in 0..64 {
            state.update_input(super::super::model::InputAction::PaletteDown);
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
        state.update_input(super::super::model::InputAction::OpenPalette);
        state.update_input(super::super::model::InputAction::Insert(
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
                .push_back(super::super::model::VisibleMessage {
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
            .push_back(super::super::model::VisibleMessage {
                kind: MessageKind::Assistant,
                document: super::super::rich_text::RichDocument::plain(&text),
                text,
            });

        terminal
            .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
            .unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("answer-line-39"));

        state.update_input(super::super::model::InputAction::Scroll(-3));
        terminal
            .draw(|frame| render(frame, &state, ResolvedPresentation::test_plain()))
            .unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("answer-line-36"));
        assert!(!rendered.contains("Start a conversation below"));
    }

    #[test]
    fn ordinary_conversation_drag_highlights_and_copies_rendered_text() {
        let area = Rect::new(0, 0, 80, 24);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.header_expanded = false;
        state.messages.clear();
        state
            .messages
            .push_back(super::super::model::VisibleMessage {
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
            super::super::model::UpdateEffect::None
        );
        let extend = pointer_action(end.column, end.row, true, &state, area).unwrap();
        assert_eq!(
            state.update_input(extend),
            super::super::model::UpdateEffect::None
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
            super::super::model::UpdateEffect::CopyText("copy me".to_owned())
        );
        assert!(state.conversation_selection.is_none());

        let begin = pointer_action(start.column, start.row, false, &state, area).unwrap();
        state.update_input(begin);
        let finish = pointer_release_action(start.column, start.row, &state, area).unwrap();
        assert_eq!(
            state.update_input(finish),
            super::super::model::UpdateEffect::None,
            "an ordinary click must not copy one cell"
        );
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
                .push_back(super::super::model::VisibleMessage {
                    kind: MessageKind::Assistant,
                    document: super::super::rich_text::RichDocument::plain(&text),
                    text,
                });
        }
        let mut samples = Vec::with_capacity(256);
        for _ in 0..256 {
            let started = std::time::Instant::now();
            state.update_input(super::super::model::InputAction::Insert("x".to_owned()));
            state.update_input(super::super::model::InputAction::Backspace);
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
            }],
            active: None,
        });
        let shell = shell_layout(area, &state);
        let columns = wide_columns(shell.body, &state);

        assert_eq!(
            pointer_action(columns[0].x + 1, columns[0].y, false, &state, area),
            Some(super::super::model::InputAction::ToggleSessionsView)
        );

        assert_eq!(
            pointer_action(
                columns[0].x.saturating_add(1),
                columns[0].y.saturating_add(1),
                false,
                &state,
                area,
            ),
            Some(super::super::model::InputAction::ViewSession(conversation))
        );
        assert_eq!(
            pointer_action(1, 1, false, &state, area),
            Some(super::super::model::InputAction::ToggleHeader)
        );
        assert_eq!(
            pointer_action(
                columns[2].x.saturating_add(1),
                columns[2].y.saturating_add(1),
                false,
                &state,
                area,
            ),
            Some(super::super::model::InputAction::ToggleActivity(0))
        );
        assert_eq!(
            pointer_action(
                shell.composer.x.saturating_add(3),
                shell.composer.y.saturating_add(1),
                true,
                &state,
                area,
            ),
            Some(super::super::model::InputAction::PlaceCursor {
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
            Some(super::super::model::InputAction::ChooseOverlay(0))
        );
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
            Some(super::super::model::InputAction::ViewSession(expected))
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
        assert!(rendered.contains("drag conversation to copy"));
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
