//! Bounded terminal-native work and attention perspective.

use super::{
    activity::{ActivityKind, ActivityState},
    state::{LayoutClass, Overlay, TuiState},
    view::{semantic_style, surface_style},
};
use crate::{
    command_catalog::ConversationDisplayState,
    presentation::{ResolvedPresentation, SemanticToken},
    workspace_host::{ConversationRef, ConversationState},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

const MAX_ROWS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EspejoScope {
    Global,
    Project(Option<String>),
}

impl EspejoScope {
    pub(super) fn label(&self) -> String {
        match self {
            Self::Global => "Global · current local workspace".to_owned(),
            Self::Project(Some(project)) => format!("Project · {project}"),
            Self::Project(None) => "Project · Ungrouped".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EspejoViewState {
    pub(super) scope: EspejoScope,
    pub(super) selected: usize,
}

impl EspejoViewState {
    pub(super) const fn global() -> Self {
        Self {
            scope: EspejoScope::Global,
            selected: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AttentionBand {
    NeedsYou,
    Working,
    Blocked,
    Failed,
    Idle,
}

impl AttentionBand {
    const fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "In motion",
            Self::Blocked => "Blocked",
            Self::Failed => "Failed",
            Self::Idle => "Idle",
        }
    }

    const fn marker(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (Self::NeedsYou, true) => "?",
            (Self::Working, true) => "▶",
            (Self::Blocked, true) => "‖",
            (Self::Failed, true) => "!",
            (Self::Idle, true) => "·",
            (Self::NeedsYou, false) => "?",
            (Self::Working, false) => ">",
            (Self::Blocked, false) => "#",
            (Self::Failed, false) => "!",
            (Self::Idle, false) => ".",
        }
    }

    const fn token(self) -> SemanticToken {
        match self {
            Self::NeedsYou => SemanticToken::Approval,
            Self::Working => SemanticToken::Accent,
            Self::Blocked | Self::Failed => SemanticToken::Warning,
            Self::Idle => SemanticToken::Muted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EspejoRow {
    pub(super) conversation: ConversationRef,
    pub(super) title: String,
    pub(super) project: Option<String>,
    pub(super) owner: &'static str,
    pub(super) connection: String,
    pub(super) model: String,
    pub(super) display_state: ConversationDisplayState,
    pub(super) attention: AttentionBand,
    pub(super) record_count: Option<usize>,
}

pub(super) fn rows(state: &TuiState) -> Vec<EspejoRow> {
    let scope = state
        .espejo
        .as_ref()
        .map(|espejo| &espejo.scope)
        .unwrap_or(&EspejoScope::Global);
    let mut rows = state
        .sessions
        .iter()
        .filter(|row| match scope {
            EspejoScope::Global => true,
            EspejoScope::Project(project) => &row.project == project,
        })
        .take(MAX_ROWS)
        .map(|row| {
            let current = row.conversation == state.runtime_conversation;
            let awaiting_approval = current
                && matches!(
                    state.overlay,
                    Some(
                        Overlay::Approval { .. }
                            | Overlay::ExternalImageApproval { .. }
                            | Overlay::ExternalResourceApproval { .. }
                            | Overlay::VisionApproval { .. }
                    )
                );
            let attention = if row.error {
                AttentionBand::Failed
            } else if awaiting_approval || row.unread {
                AttentionBand::NeedsYou
            } else if current && state.pending_round_budget.is_some() {
                AttentionBand::Blocked
            } else if (current && state.busy)
                || matches!(
                    row.state,
                    ConversationState::Active
                        | ConversationState::Controlled
                        | ConversationState::Observable
                )
            {
                AttentionBand::Working
            } else if row.state == ConversationState::Unavailable {
                AttentionBand::Blocked
            } else {
                AttentionBand::Idle
            };
            EspejoRow {
                conversation: row.conversation.clone(),
                title: row.title.clone(),
                project: row.project.clone(),
                owner: row.execution_owner,
                connection: row.connection.clone(),
                model: row.model.clone(),
                display_state: row.display_state(&state.runtime_conversation),
                attention,
                record_count: row.record_count,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.attention
            .cmp(&right.attention)
            .then_with(|| left.title.cmp(&right.title))
    });
    rows
}

pub(super) fn selected_conversation(state: &TuiState) -> Option<ConversationRef> {
    let selected = state.espejo.as_ref()?.selected;
    rows(state)
        .get(selected)
        .map(|row| row.conversation.clone())
}

pub(super) fn row_at(area: Rect, column: u16, row: u16, state: &TuiState) -> Option<usize> {
    let layout = espejo_layout(area);
    let list = list_area(layout.body, area.width);
    if column <= list.x
        || column >= list.right().saturating_sub(1)
        || row <= list.y
        || row >= list.bottom().saturating_sub(1)
    {
        return None;
    }
    let projected = rows(state);
    let selected = state.espejo.as_ref().map_or(0, |espejo| espejo.selected);
    let index = visible_row_start(selected, projected.len(), list.height)
        .saturating_add(usize::from(row.saturating_sub(list.y.saturating_add(1))) / 2);
    (index < projected.len()).then_some(index)
}

pub(super) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
) {
    frame.render_widget(Block::default().style(surface_style(profile, false)), area);
    if area.width < 42 || area.height < 12 {
        frame.render_widget(
            Paragraph::new("Espejo needs at least 42 columns × 12 rows. Resize or press Esc.")
                .style(semantic_style(profile, SemanticToken::Warning))
                .wrap(Wrap { trim: false })
                .block(Block::default().title(" Espejo ").borders(Borders::ALL)),
            area,
        );
        return;
    }

    let layout = espejo_layout(area);
    let scope = state
        .espejo
        .as_ref()
        .map(|espejo| espejo.scope.label())
        .unwrap_or_else(|| EspejoScope::Global.label());
    let projected = rows(state);
    render_heading(frame, layout.heading, state, profile, &scope);
    render_summary(frame, layout.summary, state, profile, &projected);

    match LayoutClass::for_width(area.width) {
        LayoutClass::Wide => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
                .split(layout.body);
            render_rows(frame, columns[0], state, profile, &projected);
            render_details(frame, columns[1], state, profile, &projected);
        }
        LayoutClass::Medium | LayoutClass::Narrow => {
            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(layout.body);
            render_rows(frame, sections[0], state, profile, &projected);
            render_details(frame, sections[1], state, profile, &projected);
        }
    }
    frame.render_widget(
        Paragraph::new(
            "↑/↓ navigate · Enter preview · G global · P selected Project · A Activity · D Diagnostics · Ctrl+P commands · Esc conversation",
        )
        .style(semantic_style(profile, SemanticToken::Muted)),
        layout.footer,
    );
}

struct EspejoLayout {
    heading: Rect,
    summary: Rect,
    body: Rect,
    footer: Rect,
}

fn espejo_layout(area: Rect) -> EspejoLayout {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);
    EspejoLayout {
        heading: rows[0],
        summary: rows[1],
        body: rows[2],
        footer: rows[3],
    }
}

fn list_area(body: Rect, width: u16) -> Rect {
    if LayoutClass::for_width(width) == LayoutClass::Wide {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(body)[0]
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(body)[0]
    }
}

fn render_heading(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
    scope: &str,
) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Espejo",
                semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" · {scope} · {}", state.workspace.display())),
        ]))
        .block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn render_summary(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
    rows: &[EspejoRow],
) {
    let count = |band| rows.iter().filter(|row| row.attention == band).count();
    let completed = state
        .activity
        .iter()
        .filter(|card| card.state == ActivityState::Complete)
        .count();
    let lines = vec![
        Line::raw(format!(
            "Needs you {} · In motion {} · Blocked {} · Failed {} · Idle {} · Recently completed {}",
            count(AttentionBand::NeedsYou),
            count(AttentionBand::Working),
            count(AttentionBand::Blocked),
            count(AttentionBand::Failed),
            count(AttentionBand::Idle),
            completed,
        )),
        Line::styled(
            host_summary(state),
            semantic_style(
                profile,
                if host_collision(state) {
                    SemanticToken::Warning
                } else {
                    SemanticToken::Muted
                },
            ),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(" Attention ").borders(Borders::ALL)),
        area,
    );
}

fn render_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
    rows: &[EspejoRow],
) {
    let selected = state.espejo.as_ref().map_or(0, |espejo| espejo.selected);
    let viewport_rows = (usize::from(area.height.saturating_sub(2)) / 2).max(1);
    let start = visible_row_start(selected, rows.len(), area.height);
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(start).take(viewport_rows) {
        let focus = index == selected;
        lines.push(Line::styled(
            format!(
                "{} {} {} · {}",
                if focus { ">" } else { " " },
                row.attention.marker(profile.unicode),
                row.attention.label(),
                row.title,
            ),
            if focus {
                semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD)
            } else {
                semantic_style(profile, row.attention.token())
            },
        ));
        lines.push(Line::styled(
            format!(
                "    {} · {} · {}",
                row.display_state.label(),
                row.project.as_deref().unwrap_or("Ungrouped"),
                row.conversation
            ),
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "No Conversations match this scope.",
            semantic_style(profile, SemanticToken::Muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(Block::default().title(" Work ").borders(Borders::ALL)),
        area,
    );
}

fn visible_row_start(selected: usize, row_count: usize, area_height: u16) -> usize {
    let viewport_rows = (usize::from(area_height.saturating_sub(2)) / 2).max(1);
    selected
        .saturating_add(1)
        .saturating_sub(viewport_rows)
        .min(row_count.saturating_sub(viewport_rows))
}

fn render_details(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    profile: ResolvedPresentation,
    rows: &[EspejoRow],
) {
    let selected = state.espejo.as_ref().map_or(0, |espejo| espejo.selected);
    let mut lines = if let Some(row) = rows.get(selected) {
        vec![
            Line::styled(
                row.title.clone(),
                semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD),
            ),
            Line::raw(format!(
                "State: {} · {}",
                row.attention.label(),
                row.display_state.label()
            )),
            Line::raw(format!(
                "Owner: {} · {}/{}",
                row.owner, row.connection, row.model
            )),
            Line::raw(format!(
                "Project: {} · Records: {}",
                row.project.as_deref().unwrap_or("Ungrouped"),
                row.record_count
                    .map_or_else(|| "unavailable".to_owned(), |count| count.to_string())
            )),
            Line::raw(format!("Conversation: {}", row.conversation)),
        ]
    } else {
        vec![Line::raw("No selected Conversation")]
    };
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Execution",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(format!(
        "Run: {} · queued input: {} · approvals: {}",
        state
            .active_operation
            .map_or_else(|| "none".to_owned(), |id| id.to_string()),
        state.followups.len(),
        state
            .activity
            .iter()
            .filter(
                |card| card.kind == ActivityKind::Approval && card.state == ActivityState::Waiting
            )
            .count(),
    )));
    let tools = state
        .activity
        .iter()
        .filter(|card| card.kind == ActivityKind::Tool)
        .count();
    let agents = state
        .activity
        .iter()
        .filter(|card| card.kind == ActivityKind::Child)
        .count();
    let artifacts = state
        .messages
        .iter()
        .map(|message| message.document.artifacts.len())
        .sum::<usize>();
    lines.push(Line::raw(format!(
        "Observed activity: {tools} tool event(s) · {agents} child event(s) · {artifacts} artifact reference(s)"
    )));
    lines.push(Line::raw(usage_summary(state)));
    lines.push(Line::raw(
        "Performance: unavailable; this owner reports no measured performance facts",
    ));
    lines.push(Line::raw(
        "Completion receipts: unavailable; recent completed Activity is shown only as Activity",
    ));
    if let Some(card) = state
        .activity
        .iter()
        .rev()
        .find(|card| card.state == ActivityState::Complete)
    {
        lines.push(Line::raw(format!(
            "Recent Activity: {} · {}",
            card.owner, card.summary
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(" Evidence ").borders(Borders::ALL)),
        area,
    );
}

fn usage_summary(state: &TuiState) -> String {
    state.managed_usage.map_or_else(
        || format!("Usage: {}", state.native_usage.render()),
        |(input, output, total)| {
            format!("Usage: managed input {input} · output {output} · total {total}")
        },
    )
}

fn host_collision(state: &TuiState) -> bool {
    state
        .active_host_root
        .as_ref()
        .is_some_and(|(conversation, _)| conversation != &state.runtime_conversation)
}

fn host_summary(state: &TuiState) -> String {
    match &state.active_host_root {
        Some((conversation, process)) if conversation == &state.runtime_conversation => format!(
            "Host healthy · attached Run lease in process {process} · workspace {}",
            state.workspace_id
        ),
        Some((conversation, process)) => format!(
            "Workspace collision · {conversation} is active in process {process} · current TUI remains observer-only for it"
        ),
        None => format!(
            "Host healthy · no active Run lease · workspace {}",
            state.workspace_id
        ),
    }
}
