//! Bounded conversation projection, viewport rendering, and cell selection.

use super::super::{
    rich_text::RichLineKind,
    state::{MessageKind, ScreenPoint, TuiState},
};
use super::{clamp_point, panel_content, semantic_style};
use crate::presentation::{ResolvedPresentation, SemanticToken};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

const MAX_COPY_CELLS: usize = 256 * 1024;

pub(super) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    frame.render_widget(conversation(state, profile, area), area);
    if let Some(selection) = &state.conversation_selection {
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
    let working = state.busy && state.active_operation.is_some();
    if lines.is_empty() && !working {
        lines.push(Line::styled(
            "Start a conversation below.",
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    if working {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        let dots = if profile.reduced_motion {
            "..."
        } else {
            match state.work_indicator_frame {
                0 => ".",
                1 => "..",
                2 => "...",
                _ => "..",
            }
        };
        lines.push(Line::from(vec![
            Span::styled(
                "Xana ",
                semantic_style(profile, SemanticToken::Assistant).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("is working{dots}"),
                semantic_style(profile, SemanticToken::Muted),
            ),
        ]));
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

pub(super) fn selected_text(
    state: &TuiState,
    area: Rect,
    start: ScreenPoint,
    end: ScreenPoint,
) -> String {
    let mut buffer = Buffer::empty(area);
    conversation(state, ResolvedPresentation::plain(), area).render(area, &mut buffer);
    selection_text(&buffer, panel_content(area), start, end)
}

pub(super) fn highlight_selection(
    buffer: &mut Buffer,
    area: Rect,
    start: ScreenPoint,
    end: ScreenPoint,
) {
    for point in selected_points(area, start, end) {
        if let Some(cell) = buffer.cell_mut((point.column, point.row)) {
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}

pub(super) fn selection_text(
    buffer: &Buffer,
    area: Rect,
    start: ScreenPoint,
    end: ScreenPoint,
) -> String {
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
    message: &super::super::state::VisibleMessage,
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
