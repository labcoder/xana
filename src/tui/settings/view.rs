//! Responsive Ratatui rendering for the settings workspace.

use super::state::{Overlay, SettingsState, StatusTone};
use crate::{
    presentation::{ColorDepth, ResolvedPresentation, ResolvedTheme, SemanticToken, WidthClass},
    settings::{SettingEffect, SettingEntry, SettingSource, SettingTarget, SettingsSection},
    tui::{rich_text, view as shell_view},
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap},
};

const ASCII_BORDER: ratatui::symbols::border::Set<'static> = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

const WIDE_MIN: u16 = 108;
const MEDIUM_MIN: u16 = 68;

pub(super) fn render(
    frame: &mut Frame<'_>,
    state: &SettingsState,
    base_profile: ResolvedPresentation,
) {
    let area = frame.area();
    let profile = preview_profile(base_profile, state, area.width);
    frame.render_widget(
        Block::default().style(shell_view::surface_style(profile, false)),
        area,
    );
    if area.width < 36 || area.height < 12 {
        render_too_small(frame, area, profile);
        return;
    }

    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, shell[0], state, profile);
    if area.width >= WIDE_MIN {
        render_wide(frame, shell[1], state, profile);
    } else if area.width >= MEDIUM_MIN {
        render_medium(frame, shell[1], state, profile);
    } else {
        render_narrow(frame, shell[1], state, profile);
    }
    render_status(frame, shell[2], state, profile);
    render_footer(frame, shell[3], state, profile);
    render_overlay(frame, area, state, profile);
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let count = state.pending_count();
    let title = Line::from(vec![
        Span::styled(
            " XANA ",
            shell_view::semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "/ SETTINGS",
            shell_view::semantic_style(profile, SemanticToken::Muted),
        ),
    ]);
    let right = if state.search.is_empty() {
        format!(
            "{}  {}  {} staged ",
            state.section().title(),
            separator(profile),
            count
        )
    } else {
        format!(
            "Search: {}  {}  {} staged ",
            safe(&state.search),
            separator(profile),
            count
        )
    };
    let block = panel(profile, false).title(title).title_bottom(
        Line::styled(
            right,
            if count > 0 {
                shell_view::semantic_style(profile, SemanticToken::Warning)
            } else {
                shell_view::semantic_style(profile, SemanticToken::Muted)
            },
        )
        .right_aligned(),
    );
    frame.render_widget(block, area);
}

fn render_wide(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(23),
            Constraint::Min(38),
            Constraint::Length(39),
        ])
        .split(area);
    render_sections(frame, columns[0], state, profile);
    render_settings_list(frame, columns[1], state, profile, false);
    render_detail(frame, columns[2], state, profile);
}

fn render_medium(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(19), Constraint::Min(36)])
        .split(area);
    let content = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(columns[1]);
    render_sections(frame, columns[0], state, profile);
    render_settings_list(frame, content[0], state, profile, true);
    render_detail(frame, content[1], state, profile);
}

fn render_narrow(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let content = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Percentage(57),
            Constraint::Min(4),
        ])
        .split(area);
    let section_text = format!(
        " {}  {}/{}  {} sections",
        state.section().title(),
        state.section_index + 1,
        SettingsSection::all().len(),
        if profile.unicode { "← →" } else { "< >" }
    );
    frame.render_widget(
        Paragraph::new(section_text)
            .style(shell_view::semantic_style(profile, SemanticToken::Accent))
            .block(panel(profile, false).borders(Borders::LEFT | Borders::RIGHT)),
        content[0],
    );
    render_settings_list(frame, content[1], state, profile, true);
    render_detail(frame, content[2], state, profile);
}

fn render_sections(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let items = SettingsSection::all()
        .iter()
        .map(|section| {
            let count = state.snapshot.entries_in(*section).len();
            let marker = if *section == state.section() {
                if profile.unicode { "◆" } else { ">" }
            } else {
                " "
            };
            let style = if *section == state.section() {
                shell_view::semantic_style(profile, SemanticToken::Accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                shell_view::semantic_style(profile, SemanticToken::Muted)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker} "), style),
                Span::styled(section.title(), style),
                Span::styled(
                    format!("  {count}"),
                    shell_view::semantic_style(profile, SemanticToken::Muted),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            panel(profile, false)
                .title(" Browse ")
                .padding(Padding::vertical(1)),
        ),
        area,
    );
}

fn render_settings_list(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
    compact: bool,
) {
    let entries = state.visible_entries();
    let items = entries
        .iter()
        .map(|entry| setting_item(entry, profile, compact))
        .collect::<Vec<_>>();
    let title = if state.search.is_empty() {
        format!(
            " {} {} {} ",
            state.section().title(),
            separator(profile),
            entries.len()
        )
    } else {
        format!(" Search results {} {} ", separator(profile), entries.len())
    };
    let empty = items.is_empty();
    let list = List::new(if empty {
        vec![ListItem::new("  No settings matched")]
    } else {
        items
    })
    .block(
        panel(profile, true)
            .title(title)
            .padding(Padding::vertical(1)),
    )
    .highlight_symbol(if profile.unicode { "  ▸ " } else { "  > " })
    .highlight_style(
        shell_view::semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD),
    );
    let mut list_state = ListState::default().with_selected((!empty).then_some(state.selected));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn setting_item(
    entry: &SettingEntry,
    profile: ResolvedPresentation,
    compact: bool,
) -> ListItem<'static> {
    let marker = if entry.staged {
        if profile.unicode { "● " } else { "* " }
    } else {
        "  "
    };
    let label_style = if entry.staged {
        shell_view::semantic_style(profile, SemanticToken::Warning)
    } else if entry.editable {
        Style::default()
    } else {
        shell_view::semantic_style(profile, SemanticToken::Muted)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(marker, label_style),
        Span::styled(safe(&entry.label), label_style.add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            safe(&entry.value.display),
            shell_view::semantic_style(profile, SemanticToken::Accent),
        ),
    ])];
    if !compact {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                safe(&entry.key),
                shell_view::semantic_style(profile, SemanticToken::Muted),
            ),
            Span::styled(
                format!("  {}  {}", separator(profile), short_effect(entry.effect)),
                shell_view::semantic_style(profile, SemanticToken::Muted),
            ),
        ]));
    }
    ListItem::new(Text::from(lines))
}

fn render_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let Some(entry) = state.selected_entry() else {
        frame.render_widget(
            Paragraph::new("Adjust the search or choose another section.")
                .style(shell_view::semantic_style(profile, SemanticToken::Muted))
                .block(
                    panel(profile, false)
                        .title(" Details ")
                        .padding(Padding::uniform(1)),
                )
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };
    let edit_hint = if entry.editable {
        if profile.unicode {
            "Enter to edit · R reset · U revert staged"
        } else {
            "Enter to edit | R reset | U revert staged"
        }
    } else if entry.action.is_some() {
        "Enter shows the focused manager command"
    } else {
        "Informational"
    };
    let mut lines = vec![
        Line::styled(
            safe(&entry.label),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            safe(&entry.key),
            shell_view::semantic_style(profile, SemanticToken::Muted),
        ),
        Line::default(),
        Line::from(safe(&entry.description)),
        Line::default(),
        detail_line("Current", &entry.value.display, profile),
        detail_line(
            "Default",
            entry
                .default
                .as_ref()
                .map_or("Not applicable", |value| value.display.as_str()),
            profile,
        ),
        detail_line("Source", source_label(entry.source), profile),
        detail_line("Scope", target_label(entry.target), profile),
        detail_line("Effect", effect_label(entry.effect), profile),
    ];
    if !entry.choices.is_empty() {
        lines.push(detail_line("Choices", &entry.choices.join(", "), profile));
    }
    if let Some(action) = &entry.action {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("Open: `{}`", safe(action)),
            shell_view::semantic_style(profile, SemanticToken::Accent),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        edit_hint,
        shell_view::semantic_style(profile, SemanticToken::Muted),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                panel(profile, false)
                    .title(" Details ")
                    .padding(Padding::uniform(1)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn detail_line(label: &'static str, value: &str, profile: ResolvedPresentation) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<8}"),
            shell_view::semantic_style(profile, SemanticToken::Muted),
        ),
        Span::raw(safe(value)),
    ])
}

fn render_status(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let (token, marker) = match state.status.tone {
        StatusTone::Neutral => (
            SemanticToken::Muted,
            if profile.unicode { "●" } else { "-" },
        ),
        StatusTone::Success => (
            SemanticToken::Success,
            if profile.unicode { "✓" } else { "+" },
        ),
        StatusTone::Warning => (SemanticToken::Warning, "!"),
        StatusTone::Error => (SemanticToken::Danger, "!"),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {marker} "),
                shell_view::semantic_style(profile, token),
            ),
            Span::styled(
                safe(&state.status.message),
                shell_view::semantic_style(profile, token),
            ),
        ]))
        .block(panel(profile, false).borders(Borders::LEFT | Borders::RIGHT)),
        area,
    );
}

fn render_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let keys = if state.overlay.is_some() {
        if profile.unicode {
            "↑↓ move  ·  Enter confirm  ·  Esc back"
        } else {
            "Up/Down move | Enter confirm | Esc back"
        }
    } else if area.width < MEDIUM_MIN {
        if profile.unicode {
            "↵ edit  / search  ^S apply  ? help  Esc back"
        } else {
            "Enter edit  / search  ^S apply  ? help  Esc back"
        }
    } else if area.width < WIDE_MIN {
        if profile.unicode {
            "↑↓ move · Tab section · ↵ edit · / search · Ctrl+S review · ? · Esc back"
        } else {
            "J/K move | Tab section | Enter edit | / search | Ctrl+S review | ? | Esc back"
        }
    } else {
        if profile.unicode {
            "↑↓ navigate  ·  ←→ section  ·  Enter edit  ·  / search  ·  Ctrl+S review  ·  ? help  ·  Esc back"
        } else {
            "Up/Down navigate | Left/Right section | Enter edit | / search | Ctrl+S review | ? help | Esc back"
        }
    };
    frame.render_widget(
        Paragraph::new(keys)
            .alignment(Alignment::Center)
            .style(shell_view::semantic_style(profile, SemanticToken::Muted))
            .block(panel(profile, false).borders(Borders::TOP)),
        area,
    );
}

fn render_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let Some(overlay) = &state.overlay else {
        return;
    };
    let popup = match overlay {
        Overlay::Search => centered(area, 72, 7),
        Overlay::Choice { choices, .. } => centered(area, 64, (choices.len() as u16 + 4).min(18)),
        Overlay::Text { .. } => centered(area, 72, 9),
        Overlay::Review { changes } => centered(area, 82, (changes.len() as u16 * 3 + 7).min(22)),
        Overlay::Help => centered(area, 78, 20),
        Overlay::ConfirmDiscard => centered(area, 64, 8),
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::default().style(shell_view::surface_style(profile, true)),
        popup,
    );
    match overlay {
        Overlay::Search => render_search_overlay(frame, popup, state, profile),
        Overlay::Choice {
            label,
            choices,
            selected,
            ..
        } => render_choice_overlay(frame, popup, label, choices, *selected, profile),
        Overlay::Text {
            label, input, hint, ..
        } => render_text_overlay(frame, popup, label, input, hint, profile),
        Overlay::Review { changes } => render_review_overlay(frame, popup, changes, profile),
        Overlay::Help => render_help_overlay(frame, popup, profile),
        Overlay::ConfirmDiscard => render_discard_overlay(frame, popup, state, profile),
    }
}

fn render_search_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    let value = if state.search.is_empty() {
        if profile.unicode {
            "Type to search every setting…".to_owned()
        } else {
            "Type to search every setting...".to_owned()
        }
    } else {
        format!("{}{}", safe(&state.search), cursor(profile))
    };
    frame.render_widget(
        Paragraph::new(value)
            .style(if state.search.is_empty() {
                shell_view::semantic_style(profile, SemanticToken::Muted)
            } else {
                shell_view::semantic_style(profile, SemanticToken::Focus)
            })
            .block(
                modal(profile, " Find a setting ")
                    .padding(Padding::uniform(1))
                    .title_bottom(Line::from(" Enter keep · Esc back ").right_aligned()),
            ),
        area,
    );
}

fn render_choice_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    choices: &[String],
    selected: usize,
    profile: ResolvedPresentation,
) {
    let items = choices
        .iter()
        .map(|choice| ListItem::new(format!("  {}", safe(choice))))
        .collect::<Vec<_>>();
    let mut list_state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol(if profile.unicode { "  ◆ " } else { "  > " })
            .highlight_style(
                shell_view::semantic_style(profile, SemanticToken::Focus)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                modal(profile, &format!(" {} ", safe(label)))
                    .padding(Padding::vertical(1))
                    .title_bottom(Line::from(" Enter stage · Esc cancel ").right_aligned()),
            ),
        area,
        &mut list_state,
    );
}

fn render_text_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    input: &str,
    hint: &str,
    profile: ResolvedPresentation,
) {
    let lines = vec![
        Line::styled(
            safe(hint),
            shell_view::semantic_style(profile, SemanticToken::Muted),
        ),
        Line::default(),
        Line::styled(
            format!("{}{}", safe(input), cursor(profile)),
            shell_view::semantic_style(profile, SemanticToken::Focus),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            modal(profile, &format!(" {} ", safe(label)))
                .padding(Padding::uniform(1))
                .title_bottom(Line::from(" Enter validate & stage · Esc cancel ").right_aligned()),
        ),
        area,
    );
}

fn render_review_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    changes: &[crate::settings::SettingChange],
    profile: ResolvedPresentation,
) {
    let mut lines = Vec::new();
    lines.push(Line::styled(
        "Review scope and timing before Xana writes either durable owner.",
        shell_view::semantic_style(profile, SemanticToken::Muted),
    ));
    lines.push(Line::default());
    for change in changes {
        lines.push(Line::from(vec![
            Span::styled(
                if profile.unicode { "● " } else { "* " },
                shell_view::semantic_style(profile, SemanticToken::Warning),
            ),
            Span::styled(
                safe(&change.label),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  {} {} {}",
                safe(&change.before.display),
                if profile.unicode { "→" } else { "->" },
                safe(&change.after.display)
            )),
        ]));
        lines.push(Line::styled(
            format!(
                "    {} {} {}",
                target_label(change.target),
                separator(profile),
                effect_label(change.effect)
            ),
            shell_view::semantic_style(profile, SemanticToken::Muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            modal(profile, &format!(" Review {} change(s) ", changes.len()))
                .padding(Padding::uniform(1))
                .title_bottom(
                    Line::from(" Enter / Ctrl+S apply · Esc keep editing ").right_aligned(),
                ),
        ),
        area,
    );
}

fn render_help_overlay(frame: &mut Frame<'_>, area: Rect, profile: ResolvedPresentation) {
    let help = [
        (
            if profile.unicode {
                "↑ ↓ / J K"
            } else {
                "Up Down / J K"
            },
            "Move through settings or choices",
        ),
        (
            if profile.unicode {
                "← → / Tab"
            } else {
                "Left Right/Tab"
            },
            "Move between sections",
        ),
        ("Enter", "Edit, stage, or confirm"),
        ("/ or Ctrl+F", "Search across every section"),
        ("R", "Reset the selected setting to its default"),
        ("U", "Revert only the selected staged edit"),
        ("Ctrl+S / A", "Review staged changes, then apply"),
        ("D", "Discard all staged changes after confirmation"),
        ("Esc", "Back one level; staged work is never lost silently"),
        ("Ctrl+Q", "Request exit"),
    ];
    let lines = help
        .into_iter()
        .map(|(key, meaning)| {
            Line::from(vec![
                Span::styled(
                    format!("{key:<14}"),
                    shell_view::semantic_style(profile, SemanticToken::Accent),
                ),
                Span::raw(meaning),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).block(
            modal(profile, " Keyboard map ")
                .padding(Padding::uniform(1))
                .title_bottom(Line::from(" Esc close ").right_aligned()),
        ),
        area,
    );
}

fn render_discard_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &SettingsState,
    profile: ResolvedPresentation,
) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "Discard {} staged change{}?",
                state.pending_count(),
                if state.pending_count() == 1 { "" } else { "s" }
            )),
            Line::default(),
            Line::styled(
                "Durable files have not changed.",
                shell_view::semantic_style(profile, SemanticToken::Success),
            ),
        ])
        .block(
            modal(profile, " Discard staged changes? ")
                .padding(Padding::uniform(1))
                .title_bottom(Line::from(" Enter / D discard · Esc keep ").right_aligned()),
        ),
        area,
    );
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect, profile: ResolvedPresentation) {
    frame.render_widget(
        Paragraph::new("Xana Settings\n\nResize to at least 36x12.\nEsc exits safely.")
            .alignment(Alignment::Center)
            .style(shell_view::semantic_style(profile, SemanticToken::Warning))
            .block(modal(profile, " Settings ").padding(Padding::uniform(1)))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn panel(profile: ResolvedPresentation, raised: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(if profile.unicode {
            ratatui::symbols::border::ROUNDED
        } else {
            ASCII_BORDER
        })
        .border_style(shell_view::semantic_style(profile, SemanticToken::Muted))
        .style(shell_view::surface_style(profile, raised))
}

fn modal(profile: ResolvedPresentation, title: &str) -> Block<'static> {
    panel(profile, true)
        .title(Line::styled(
            title.to_owned(),
            shell_view::semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
        ))
        .border_style(shell_view::semantic_style(profile, SemanticToken::Focus))
}

fn centered(area: Rect, maximum_width: u16, maximum_height: u16) -> Rect {
    let width = maximum_width.min(area.width.saturating_sub(4)).max(1);
    let height = maximum_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn preview_profile(
    mut profile: ResolvedPresentation,
    state: &SettingsState,
    width: u16,
) -> ResolvedPresentation {
    if let Some(value) = state
        .snapshot
        .entry("appearance.theme")
        .and_then(|entry| entry.value.raw.as_deref())
    {
        match value {
            "dark" => profile.theme = ResolvedTheme::Dark,
            "light" => profile.theme = ResolvedTheme::Light,
            "monochrome" => {
                profile.theme = ResolvedTheme::Monochrome;
                profile.color_depth = ColorDepth::None;
            }
            "auto" => {}
            _ => {}
        }
    }
    if let Some(value) = state
        .snapshot
        .entry("appearance.glyphs")
        .and_then(|entry| entry.value.raw.as_deref())
    {
        match value {
            "unicode" => profile.unicode = true,
            "ascii" => profile.unicode = false,
            "auto" => {}
            _ => {}
        }
    }
    if let Some(value) = state
        .snapshot
        .entry("appearance.motion")
        .and_then(|entry| entry.value.raw.as_deref())
    {
        match value {
            "full" => profile.reduced_motion = false,
            "reduced" => profile.reduced_motion = true,
            "auto" => {}
            _ => {}
        }
    }
    profile.width = if width < 50 {
        WidthClass::Narrow
    } else if width < WIDE_MIN {
        WidthClass::Compact
    } else {
        WidthClass::Wide
    };
    profile
}

fn source_label(source: SettingSource) -> &'static str {
    source.label()
}

fn target_label(target: SettingTarget) -> &'static str {
    target.label()
}

fn effect_label(effect: SettingEffect) -> &'static str {
    effect.label()
}

fn short_effect(effect: SettingEffect) -> &'static str {
    match effect {
        SettingEffect::Immediate => "Now",
        SettingEffect::NewConversation => "New conversation",
        SettingEffect::NextLaunch => "Next launch",
        SettingEffect::ManagedElsewhere => "Focused manager",
    }
}

fn separator(profile: ResolvedPresentation) -> &'static str {
    if profile.unicode { "·" } else { "|" }
}

fn cursor(profile: ResolvedPresentation) -> &'static str {
    if profile.unicode { "▌" } else { "_" }
}

fn safe(value: &str) -> String {
    rich_text::sanitize(value).replace(['\n', '\t'], " ")
}

#[cfg(test)]
mod tests;
