//! Pure Ratatui rendering for the adaptive shell.

use super::{
    activity::{ActivityKind, ActivityState},
    model::{ActivityVisibility, LayoutClass, MessageKind, Overlay, TuiState},
    rich_text::RichLineKind,
};
use crate::presentation::{PresentationColor, ResolvedPresentation, SemanticToken};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
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
        return overlay_choice_at(overlay, content_row)
            .map(super::model::InputAction::ChooseOverlay);
    }

    let layout = shell_layout(area, state);
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
    None
}

fn render_wide(frame: &mut Frame<'_>, area: Rect, state: &TuiState, profile: ResolvedPresentation) {
    let shell = shell_layout(area, state);
    render_header(frame, shell.header, state, profile, false);
    let columns = wide_columns(shell.body, state);
    if state.rail_expanded {
        frame.render_widget(session_rail(state, profile), columns[0]);
    }
    frame.render_widget(conversation(state, profile, columns[1].height), columns[1]);
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
    frame.render_widget(conversation(state, profile, shell.body.height), shell.body);
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
    frame.render_widget(conversation(state, profile, shell.body.height), shell.body);
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

fn overlay_choice_at(overlay: &super::model::Overlay, content_row: u16) -> Option<usize> {
    let first = match overlay {
        super::model::Overlay::Palette { .. } | super::model::Overlay::SessionPicker { .. } => 1,
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
        72.min(area.width.saturating_sub(2)),
        16.min(area.height.saturating_sub(2)),
    )
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
            .title(" Sessions · /sessions collapsed ")
            .borders(Borders::ALL),
    )
}

fn conversation(
    state: &TuiState,
    profile: ResolvedPresentation,
    viewport_height: u16,
) -> Paragraph<'static> {
    let mut lines = Vec::new();
    let render_limit = usize::from(viewport_height)
        .saturating_sub(2)
        .saturating_div(3)
        .clamp(1, 128);
    let end = state
        .messages
        .len()
        .saturating_sub(usize::from(state.scroll));
    let start = end.saturating_sub(render_limit);
    if start > 0 {
        lines.push(Line::styled(
            format!("[{} older message(s) outside this viewport]", start),
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    for message in state.messages.iter().skip(start).take(end - start) {
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
        lines.push(header);
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
                RichLineKind::DiffRemove => {
                    ("- ", semantic_style(profile, SemanticToken::DiffRemove))
                }
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
            "{layout} | Ctrl+P commands | Shift+drag copy | Ctrl+Q quit | {motion} | {} queued | {}",
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
                Line::raw(
                    "Shift+drag selects terminal text for native copy; ordinary clicks control Xana.",
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
        assert_eq!(
            pointer_action(5, 5, false, &state, Rect::new(0, 0, 80, 24)),
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
        assert!(rendered.contains("Shift+drag copy"));
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
