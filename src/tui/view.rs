//! Pure Ratatui rendering for the adaptive shell.

use super::model::{LayoutClass, MessageKind, TuiState};
use crate::presentation::{PresentationColor, ResolvedPresentation, SemanticToken};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub(super) fn render(frame: &mut Frame<'_>, state: &TuiState, profile: ResolvedPresentation) {
    let area = frame.area();
    match LayoutClass::for_width(area.width) {
        LayoutClass::Wide => render_wide(frame, area, state, profile),
        LayoutClass::Medium => render_medium(frame, area, state, profile),
        LayoutClass::Narrow => render_narrow(frame, area, state, profile),
    }
}

fn render_wide(frame: &mut Frame<'_>, area: Rect, state: &TuiState, profile: ResolvedPresentation) {
    let rows = shell_rows(area);
    render_header(frame, rows[0], state, profile, false);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
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
    render_footer(
        frame,
        rows[3],
        state,
        profile,
        "medium · sessions/activity in drawers",
    );
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
            Constraint::Length(3),
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
            Constraint::Length(3),
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
            "{} / {}  ·  session {}  ·  {}",
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
    Paragraph::new(Text::from(vec![
        Line::styled(
            "Current",
            semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD),
        ),
        Line::raw(state.session.clone()),
        Line::raw(""),
        Line::styled(
            "Workspace conversations",
            semantic_style(profile, SemanticToken::Muted),
        ),
        Line::raw("Session picker lands in P5-08"),
    ]))
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
        .wrap(Wrap { trim: false })
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
        " Working — Ctrl+C exits and interrupts "
    } else {
        " Message — Enter sends "
    };
    frame.render_widget(
        Paragraph::new(state.input.clone())
            .style(semantic_style(profile, SemanticToken::User))
            .block(
                Block::default()
                    .title(title)
                    .border_style(semantic_style(profile, SemanticToken::Focus))
                    .borders(Borders::ALL),
            ),
        area,
    );
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
            "Ctrl+C quit · {layout} · {motion} · {}",
            state.status
        ))
        .style(semantic_style(profile, SemanticToken::Muted)),
        area,
    );
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
    use crate::presentation::ResolvedPresentation;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn wide_medium_and_narrow_buffers_keep_the_conversation_usable() {
        for (width, expected, absent) in [
            (130, "Sessions", "drawers"),
            (90, "drawers", "Session picker lands"),
            (50, "narrow", "Session picker lands"),
        ] {
            let backend = TestBackend::new(width, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut state = TuiState::starting();
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
