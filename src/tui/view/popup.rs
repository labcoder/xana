//! Overlay and command-palette rendering for the adaptive terminal shell.

use super::super::{
    command, session,
    state::{Overlay, TuiState},
};
use super::{overlay_area, palette_window_start, semantic_style};
use crate::presentation::{ResolvedPresentation, SemanticToken};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap,
    },
};

pub(super) fn render(
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
        Overlay::ExternalImageApproval {
            external_paths,
            selected,
            ..
        } => {
            let mut lines = vec![
                Line::styled(
                    "These images are outside the launch workspace. Xana imports immutable bounded copies only if you approve.",
                    semantic_style(profile, SemanticToken::Warning),
                ),
                Line::raw(""),
            ];
            lines.extend(external_paths.iter().map(|path| Line::raw(path.clone())));
            lines.extend([
                Line::raw(""),
                Line::styled(
                    format!(
                        "{} Allow {} once",
                        if *selected == 0 { ">" } else { " " },
                        if external_paths.len() == 1 {
                            "image"
                        } else {
                            "images"
                        }
                    ),
                    if *selected == 0 {
                        semantic_style(profile, SemanticToken::Focus)
                    } else {
                        Style::default()
                    },
                ),
                Line::styled(
                    format!(
                        "{} Deny and restore draft",
                        if *selected == 1 { ">" } else { " " }
                    ),
                    if *selected == 1 {
                        semantic_style(profile, SemanticToken::Focus)
                    } else {
                        Style::default()
                    },
                ),
            ]);
            (" Read external images? ", lines)
        }
        Overlay::Help => (
            " Keyboard help ",
            vec![
                Line::raw(
                    "Ctrl+P commands   Ctrl+Q quit   Ctrl+C copies a selection or interrupts",
                ),
                Line::raw("Enter primary     Ctrl+J alternate     Shift+Enter newline"),
                Line::raw("Ctrl+Enter submit   arrows move/select   mouse wheel scrolls"),
                Line::raw(
                    "Drag conversation text to select; Ctrl+C copies it; click away clears it.",
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
            Cell::from(if command.id == command::CommandId::Reset {
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

pub(super) fn session_marker(
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

fn session_row_matches(row: &session::SessionRow, query: &str) -> bool {
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
