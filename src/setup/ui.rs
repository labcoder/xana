//! Rich terminal setup presentation with a permanent line-oriented fallback.

use super::{SetupArgs, SetupBack, SetupCancelled, prompt_default};
use crate::{
    paths::XanaPaths,
    presentation::{
        ColorDepth, DensityChoice, GlyphChoice, MotionChoice, PresentationPreferences,
        ResolvedPresentation, ResolvedTheme, SemanticToken, ThemeChoice, WidthClass,
    },
};
use anyhow::{Result, bail};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::io::{self, BufRead, Write};

#[derive(Debug, Clone, Copy)]
pub(super) struct SetupUi {
    pub(super) profile: ResolvedPresentation,
    pub(super) rich: bool,
}

pub(super) fn preview_preferences(
    mut ui: SetupUi,
    preferences: &PresentationPreferences,
) -> SetupUi {
    match preferences.theme {
        ThemeChoice::Auto => {}
        ThemeChoice::Dark => ui.profile.theme = ResolvedTheme::Dark,
        ThemeChoice::Light => ui.profile.theme = ResolvedTheme::Light,
        ThemeChoice::Monochrome => {
            ui.profile.theme = ResolvedTheme::Monochrome;
            ui.profile.color_depth = ColorDepth::None;
        }
    }
    if ui.profile.theme != ResolvedTheme::Monochrome && ui.profile.color_depth == ColorDepth::None {
        ui.profile.color_depth = ColorDepth::Ansi256;
    }
    match preferences.glyphs {
        GlyphChoice::Auto => {}
        GlyphChoice::Unicode => ui.profile.unicode = true,
        GlyphChoice::Ascii => ui.profile.unicode = false,
    }
    match preferences.motion {
        MotionChoice::Auto => {}
        MotionChoice::Full => ui.profile.reduced_motion = false,
        MotionChoice::Reduced => ui.profile.reduced_motion = true,
    }
    if preferences.density == DensityChoice::Compact {
        ui.profile.width = WidthClass::Compact;
    }
    ui
}

#[derive(Debug, Clone)]
pub(super) struct SelectOption {
    pub(super) label: String,
    pub(super) detail: String,
    pub(super) keywords: String,
    pub(super) swatches: Vec<crate::presentation::PresentationColor>,
}

impl SelectOption {
    pub(super) fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            keywords: String::new(),
            swatches: Vec::new(),
        }
    }

    pub(super) fn with_keywords(mut self, keywords: impl Into<String>) -> Self {
        self.keywords = keywords.into();
        self
    }

    pub(super) fn with_swatches(
        mut self,
        swatches: impl IntoIterator<Item = crate::presentation::PresentationColor>,
    ) -> Self {
        self.swatches.extend(swatches);
        self
    }
}

pub(super) fn enter_rich(output: &mut impl Write) -> Result<()> {
    enable_raw_mode().map_err(anyhow::Error::new)?;
    if let Err(error) = execute!(output, EnterAlternateScreen, EnableBracketedPaste, Hide) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    Ok(())
}

pub(super) fn leave_rich(output: &mut impl Write) -> Result<()> {
    let screen = execute!(output, Show, DisableBracketedPaste, LeaveAlternateScreen);
    let raw = disable_raw_mode();
    screen.map_err(anyhow::Error::new)?;
    raw.map_err(anyhow::Error::new)
}

pub(super) fn play_intro(output: &mut impl Write, profile: ResolvedPresentation) -> Result<()> {
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend).map_err(anyhow::Error::new)?;
    crate::tui::play_intro(&mut terminal, profile).map_err(anyhow::Error::new)
}

pub(super) fn choose_setup_path(
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    ui: SetupUi,
) -> Result<Option<SetupArgs>> {
    let has_explicit_path = args.quick
        || args.full
        || args.section.is_some()
        || args.non_interactive
        || args.kind.is_some()
        || args.connection.is_some()
        || args.model.is_some();
    if has_explicit_path {
        return Ok(Some(args.clone()));
    }

    if ui.rich {
        let options = [
            SelectOption::new(
                "Quick Setup",
                "connection, model, permissions, and optional appearance",
            ),
            SelectOption::new("Full Setup", "Quick Setup plus every advanced section"),
            SelectOption::new(
                "Connection and model",
                "replace or repair the active provider/runtime",
            ),
            SelectOption::new(
                "Permissions and shell",
                "change effect policy and command execution",
            ),
            SelectOption::new("Profiles and routes", "configure child-task execution"),
            SelectOption::new(
                "Appearance",
                "theme, glyphs, motion, composer, and activity",
            ),
            SelectOption::new("Cancel", "leave durable state unchanged"),
        ];
        let Some(choice) = select(output, ui, "Choose a setup path", &options, 0)? else {
            return Ok(None);
        };
        let mut selected = args.clone();
        match choice {
            0 => selected.quick = true,
            1 => selected.full = true,
            2 => selected.section = Some(crate::cli::SetupSectionChoice::Connection),
            3 => selected.section = Some(crate::cli::SetupSectionChoice::PermissionsShell),
            4 => selected.section = Some(crate::cli::SetupSectionChoice::ProfilesRoutes),
            5 => selected.section = Some(crate::cli::SetupSectionChoice::Appearance),
            6 => return Ok(None),
            _ => unreachable!("selector returned an unknown setup path"),
        }
        return Ok(Some(selected));
    }

    write_setup_heading(output, ui.profile, "Xana Setup")?;
    writeln!(
        output,
        "{}",
        ui.profile.paint(
            SemanticToken::Muted,
            "Choose a guided path. Quick Setup is the default and can be rerun safely."
        )
    )?;
    for (number, label, detail, default) in [
        (
            1,
            "Quick Setup",
            "connection, model, permissions, and optional appearance",
            true,
        ),
        (
            2,
            "Full Setup",
            "Quick Setup plus every advanced section",
            false,
        ),
        (
            3,
            "Connection and model",
            "replace or repair the active provider/runtime",
            false,
        ),
        (
            4,
            "Permissions and shell",
            "change effect policy and command execution",
            false,
        ),
        (
            5,
            "Profiles and routes",
            "configure child-task execution",
            false,
        ),
        (
            6,
            "Appearance",
            "theme, glyphs, motion, composer, and activity",
            false,
        ),
    ] {
        write_setup_choice(output, ui.profile, number, label, detail, default)?;
    }
    writeln!(
        output,
        "  {}  {}",
        ui.profile.paint(SemanticToken::Muted, "7."),
        ui.profile.paint(SemanticToken::Muted, "Cancel")
    )?;
    let choice = prompt_default(input, output, "Choice", "1")?;
    let mut selected = args.clone();
    match choice.trim() {
        "" | "1" | "quick" => selected.quick = true,
        "2" | "full" => selected.full = true,
        "3" | "connection" => {
            selected.section = Some(crate::cli::SetupSectionChoice::Connection);
        }
        "4" | "permissions-shell" => {
            selected.section = Some(crate::cli::SetupSectionChoice::PermissionsShell);
        }
        "5" | "profiles-routes" => {
            selected.section = Some(crate::cli::SetupSectionChoice::ProfilesRoutes);
        }
        "6" | "appearance" => {
            selected.section = Some(crate::cli::SetupSectionChoice::Appearance);
        }
        "7" | "cancel" => return Ok(None),
        _ => bail!("unknown setup path; use 1 through 7"),
    }
    Ok(Some(selected))
}

pub(super) fn select(
    output: &mut impl Write,
    ui: SetupUi,
    title: &str,
    options: &[SelectOption],
    default: usize,
) -> Result<Option<usize>> {
    debug_assert!(!options.is_empty());
    if !ui.rich {
        return Ok(None);
    }

    select_inner(output, ui.profile, title, options, default)
}

pub(super) fn prompt_value(
    input: &mut impl BufRead,
    output: &mut impl Write,
    ui: SetupUi,
    label: &str,
    default: &str,
    required: bool,
    secret: bool,
) -> Result<String> {
    if !ui.rich {
        let value = prompt_default(input, output, label, default)?;
        if required && value.trim().is_empty() {
            anyhow::bail!("{label} is required");
        }
        return Ok(value);
    }
    prompt_value_inner(output, ui.profile, label, default, required, secret)?
        .ok_or_else(|| SetupBack.into())
}

fn prompt_value_inner(
    output: &mut impl Write,
    profile: ResolvedPresentation,
    label: &str,
    default: &str,
    required: bool,
    secret: bool,
) -> Result<Option<String>> {
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend).map_err(anyhow::Error::new)?;
    terminal.clear().map_err(anyhow::Error::new)?;
    let mut value = default.to_owned();
    let mut warning = None;
    loop {
        terminal
            .draw(|frame| {
                draw_text_prompt(frame, profile, label, &value, secret, warning.as_deref())
            })
            .map_err(anyhow::Error::new)?;
        match event::read().map_err(anyhow::Error::new)? {
            Event::Paste(text) => {
                let available = 16 * 1024usize.saturating_sub(value.len());
                value.extend(text.chars().take(available));
                warning = None;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(SetupCancelled.into());
                }
                KeyCode::Enter => {
                    let value = value.trim().to_owned();
                    if required && value.is_empty() {
                        warning = Some(format!("{label} is required"));
                    } else {
                        return Ok(Some(value));
                    }
                }
                KeyCode::Backspace => {
                    value.pop();
                    warning = None;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && value.len() < 16 * 1024 =>
                {
                    value.push(character);
                    warning = None;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn draw_text_prompt(
    frame: &mut ratatui::Frame<'_>,
    profile: ResolvedPresentation,
    label: &str,
    value: &str,
    secret: bool,
    warning: Option<&str>,
) {
    let area = frame.area();
    frame.render_widget(Block::default().style(surface_style(profile, false)), area);
    let content = centered(area, area.width.min(78), 12.min(area.height));
    let block = Block::default()
        .title(Line::styled(
            " Xana Setup ",
            semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(semantic_style(profile, SemanticToken::Accent))
        .style(surface_style(profile, true));
    let inner = block.inner(content);
    frame.render_widget(block, content);
    let rendered = if secret {
        if profile.unicode { "•" } else { "*" }.repeat(value.chars().count())
    } else if value.is_empty() {
        "Type a value…".to_owned()
    } else {
        value.to_owned()
    };
    let lines = vec![
        Line::styled(
            label.to_owned(),
            semantic_style(profile, SemanticToken::Focus).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(
            rendered,
            semantic_style(
                profile,
                if value.is_empty() {
                    SemanticToken::Muted
                } else {
                    SemanticToken::Assistant
                },
            ),
        ),
        Line::raw(""),
        warning.map_or_else(
            || {
                Line::styled(
                    "Enter accept · Esc back · Ctrl+C cancel",
                    semantic_style(profile, SemanticToken::Muted),
                )
            },
            |warning| {
                Line::styled(
                    warning.to_owned(),
                    semantic_style(profile, SemanticToken::Warning),
                )
            },
        ),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

pub(super) fn show_status(
    output: &mut impl Write,
    ui: SetupUi,
    title: &str,
    detail: &str,
) -> Result<()> {
    if !ui.rich {
        writeln!(output, "{title}")?;
        writeln!(output, "  {detail}")?;
        return Ok(());
    }
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend).map_err(anyhow::Error::new)?;
    terminal.clear().map_err(anyhow::Error::new)?;
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(
                Block::default().style(surface_style(ui.profile, false)),
                area,
            );
            let popup = centered(area, area.width.min(74), 11.min(area.height));
            let block = Block::default()
                .title(Line::styled(
                    " Xana Setup ",
                    semantic_style(ui.profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(semantic_style(ui.profile, SemanticToken::Accent))
                .style(surface_style(ui.profile, true));
            let inner = block.inner(popup);
            frame.render_widget(block, popup);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        title.to_owned(),
                        semantic_style(ui.profile, SemanticToken::Focus)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(""),
                    Line::styled(
                        detail.to_owned(),
                        semantic_style(ui.profile, SemanticToken::Assistant),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Please wait…",
                        semantic_style(ui.profile, SemanticToken::Muted),
                    ),
                ])
                .wrap(Wrap { trim: false }),
                inner,
            );
        })
        .map_err(anyhow::Error::new)?;
    Ok(())
}

pub(super) fn confirm_review(
    input: &mut impl BufRead,
    output: &mut impl Write,
    ui: SetupUi,
    title: &str,
    lines: &[String],
) -> Result<bool> {
    if !ui.rich {
        return super::confirm(input, output, &format!("{title} [y/N]: "));
    }
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend).map_err(anyhow::Error::new)?;
    terminal.clear().map_err(anyhow::Error::new)?;
    let mut selected = 0usize;
    loop {
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(
                    Block::default().style(surface_style(ui.profile, false)),
                    area,
                );
                let height = u16::try_from(lines.len())
                    .unwrap_or(u16::MAX)
                    .saturating_add(10)
                    .min(area.height);
                let popup = centered(area, area.width.min(88), height);
                let block = Block::default()
                    .title(Line::styled(
                        format!(" Xana Setup · {title} "),
                        semantic_style(ui.profile, SemanticToken::Accent)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(semantic_style(ui.profile, SemanticToken::Accent))
                    .style(surface_style(ui.profile, true));
                let inner = block.inner(popup);
                frame.render_widget(block, popup);
                let chunks =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(4)]).split(inner);
                frame.render_widget(
                    Paragraph::new(lines.iter().cloned().map(Line::raw).collect::<Vec<_>>())
                        .wrap(Wrap { trim: false }),
                    chunks[0],
                );
                let choices = ["Apply changes", "Back", "Cancel setup"];
                frame.render_widget(
                    Paragraph::new(
                        choices
                            .into_iter()
                            .enumerate()
                            .map(|(index, label)| {
                                Line::styled(
                                    format!(
                                        "{} {label}",
                                        if index == selected { "▶" } else { " " }
                                    ),
                                    semantic_style(
                                        ui.profile,
                                        if index == selected {
                                            SemanticToken::Focus
                                        } else {
                                            SemanticToken::Muted
                                        },
                                    ),
                                )
                            })
                            .collect::<Vec<_>>(),
                    ),
                    chunks[1],
                );
            })
            .map_err(anyhow::Error::new)?;
        let Event::Key(key) = event::read().map_err(anyhow::Error::new)? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc => return Err(SetupBack.into()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Err(SetupCancelled.into());
            }
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down => selected = (selected + 1).min(2),
            KeyCode::Enter => {
                return match selected {
                    0 => Ok(true),
                    1 => Err(SetupBack.into()),
                    2 => Err(SetupCancelled.into()),
                    _ => unreachable!(),
                };
            }
            _ => {}
        }
    }
}

fn select_inner(
    output: &mut impl Write,
    profile: ResolvedPresentation,
    title: &str,
    options: &[SelectOption],
    default: usize,
) -> Result<Option<usize>> {
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend).map_err(anyhow::Error::new)?;
    terminal.clear().map_err(anyhow::Error::new)?;
    let mut query = String::new();
    let mut selected = default.min(options.len().saturating_sub(1));
    loop {
        let query_lower = query.to_ascii_lowercase();
        let filtered = options
            .iter()
            .enumerate()
            .filter(|(_, option)| {
                query_lower.is_empty()
                    || option.label.to_ascii_lowercase().contains(&query_lower)
                    || option.detail.to_ascii_lowercase().contains(&query_lower)
                    || option.keywords.to_ascii_lowercase().contains(&query_lower)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if selected >= filtered.len() {
            selected = filtered.len().saturating_sub(1);
        }
        terminal
            .draw(|frame| {
                draw_selector(frame, profile, title, options, &filtered, selected, &query)
            })
            .map_err(anyhow::Error::new)?;
        let Event::Key(key) = event::read().map_err(anyhow::Error::new)? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Err(SetupCancelled.into());
            }
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down => {
                selected = (selected + 1).min(filtered.len().saturating_sub(1));
            }
            KeyCode::PageUp => selected = selected.saturating_sub(10),
            KeyCode::PageDown => {
                selected = (selected + 10).min(filtered.len().saturating_sub(1));
            }
            KeyCode::Home => selected = 0,
            KeyCode::End => selected = filtered.len().saturating_sub(1),
            KeyCode::Backspace => {
                query.pop();
                selected = 0;
            }
            KeyCode::Enter if !filtered.is_empty() => return Ok(Some(filtered[selected])),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                query.push(character);
                selected = 0;
            }
            _ => {}
        }
    }
}

fn draw_selector(
    frame: &mut ratatui::Frame<'_>,
    profile: ResolvedPresentation,
    title: &str,
    options: &[SelectOption],
    filtered: &[usize],
    selected: usize,
    query: &str,
) {
    let area = frame.area();
    frame.render_widget(Block::default().style(surface_style(profile, false)), area);
    let content = centered(area, area.width.min(86), area.height.min(32));
    let visible_rows = usize::from(content.height.saturating_sub(10) / 2).clamp(3, 12);
    let start = selected
        .saturating_sub(visible_rows / 2)
        .min(filtered.len().saturating_sub(visible_rows));
    let end = (start + visible_rows).min(filtered.len());
    let block = Block::default()
        .title(Line::styled(
            format!(" Xana Setup · {title} "),
            semantic_style(profile, SemanticToken::Accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(semantic_style(profile, SemanticToken::Accent))
        .style(surface_style(profile, true));
    let inner = block.inner(content);
    frame.render_widget(block, content);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);
    let search = Line::from(vec![
        Span::styled("Search  ", semantic_style(profile, SemanticToken::Muted)),
        Span::styled(
            if query.is_empty() {
                "all options"
            } else {
                query
            },
            semantic_style(
                profile,
                if query.is_empty() {
                    SemanticToken::Muted
                } else {
                    SemanticToken::Focus
                },
            ),
        ),
    ]);
    frame.render_widget(Paragraph::new(search), chunks[0]);
    let mut lines = Vec::new();
    if filtered.is_empty() {
        lines.push(Line::styled(
            "No matching options",
            semantic_style(profile, SemanticToken::Warning),
        ));
    } else {
        for (position, option_index) in filtered[start..end].iter().enumerate() {
            let option = &options[*option_index];
            let active = start + position == selected;
            let marker = if active {
                if profile.unicode { "▶" } else { ">" }
            } else {
                " "
            };
            let mut title = vec![
                Span::styled(
                    format!(" {marker} "),
                    semantic_style(
                        profile,
                        if active {
                            SemanticToken::Focus
                        } else {
                            SemanticToken::Muted
                        },
                    ),
                ),
                Span::styled(
                    option.label.clone(),
                    semantic_style(
                        profile,
                        if active {
                            SemanticToken::Focus
                        } else {
                            SemanticToken::Assistant
                        },
                    )
                    .add_modifier(if active {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
            ];
            for color in &option.swatches {
                title.push(Span::raw("  "));
                title.push(Span::styled(
                    "██",
                    Style::default()
                        .fg(to_ratatui_color(*color))
                        .bg(to_ratatui_color(*color)),
                ));
            }
            lines.push(Line::from(title));
            lines.push(Line::styled(
                format!("     {}", option.detail),
                semantic_style(profile, SemanticToken::Muted),
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[1]);
    let footer = format!(
        "{} of {} matching · {} total   ↑/↓ move · type to filter · Enter choose · Esc back",
        if filtered.is_empty() { 0 } else { selected + 1 },
        filtered.len(),
        options.len()
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            footer,
            semantic_style(profile, SemanticToken::Muted),
        )),
        chunks[2],
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(height) / 2),
            Constraint::Length(height.min(area.height)),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(area.width.saturating_sub(width) / 2),
            Constraint::Length(width.min(area.width)),
            Constraint::Min(0),
        ])
        .split(vertical[1])[1]
}

fn semantic_style(profile: ResolvedPresentation, token: SemanticToken) -> Style {
    profile
        .color(token)
        .map(to_ratatui_color)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}

fn surface_style(profile: ResolvedPresentation, raised: bool) -> Style {
    profile
        .surface_color(raised)
        .map(to_ratatui_color)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

fn to_ratatui_color(color: crate::presentation::PresentationColor) -> Color {
    use crate::presentation::PresentationColor;
    match color {
        PresentationColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        PresentationColor::Indexed(index) => Color::Indexed(index),
        PresentationColor::Red => Color::Red,
        PresentationColor::Green => Color::Green,
        PresentationColor::Yellow => Color::Yellow,
        PresentationColor::Magenta => Color::Magenta,
        PresentationColor::Cyan => Color::Cyan,
        PresentationColor::DarkGray => Color::DarkGray,
        PresentationColor::Black => Color::Black,
        PresentationColor::White => Color::White,
    }
}

pub(super) fn write_setup_heading(
    output: &mut impl Write,
    profile: ResolvedPresentation,
    title: &str,
) -> io::Result<()> {
    let line = if profile.unicode { "─" } else { "-" }.repeat(57);
    writeln!(output, "{}", profile.paint(SemanticToken::Accent, &line))?;
    writeln!(
        output,
        "{}",
        profile.paint(SemanticToken::Accent, &format!("  {title}"))
    )?;
    writeln!(output, "{}", profile.paint(SemanticToken::Accent, &line))
}

fn write_setup_choice(
    output: &mut impl Write,
    profile: ResolvedPresentation,
    number: usize,
    label: &str,
    detail: &str,
    default: bool,
) -> io::Result<()> {
    let marker = if profile.unicode {
        if default { "●" } else { "○" }
    } else if default {
        "*"
    } else {
        "o"
    };
    let suffix = if default { "  <- default" } else { "" };
    writeln!(
        output,
        "  {} {}. {}{}",
        profile.paint(
            if default {
                SemanticToken::Success
            } else {
                SemanticToken::Muted
            },
            marker
        ),
        profile.paint(SemanticToken::Focus, &number.to_string()),
        label,
        profile.paint(SemanticToken::Muted, suffix)
    )?;
    writeln!(
        output,
        "       {}",
        profile.paint(SemanticToken::Muted, detail)
    )
}

pub(super) fn write_completion_receipt(
    output: &mut impl Write,
    paths: &XanaPaths,
    profile: ResolvedPresentation,
) -> io::Result<()> {
    writeln!(output)?;
    writeln!(
        output,
        "+---------------------------------------------------------+"
    )?;
    writeln!(
        output,
        "{}",
        profile.paint(
            SemanticToken::Success,
            &format!("|{:^57}|", "[OK] Setup Complete!")
        )
    )?;
    writeln!(
        output,
        "+---------------------------------------------------------+"
    )?;
    writeln!(output)?;
    writeln!(
        output,
        "Xana committed the reviewed setup changes atomically."
    )?;
    writeln!(output, "  Config:      {}", paths.config_file().display())?;
    if paths.config_file().with_extension("toml.bak").exists() {
        writeln!(
            output,
            "  Backup:      {}",
            paths.config_file().with_extension("toml.bak").display()
        )?;
    }
    writeln!(output, "  Data:        {}", paths.data_dir().display())?;
    writeln!(output, "  Cache:       {}", paths.cache_dir().display())?;
    writeln!(
        output,
        "  Credentials: operating-system credential store (API keys are not config files)"
    )?;
    writeln!(output)?;
    writeln!(output, "Configuration:")?;
    writeln!(output, "  xana setup          Re-run guided setup")?;
    writeln!(output, "  xana setup --quick  Go directly to Quick Setup")?;
    writeln!(
        output,
        "  xana config check   Validate the active configuration"
    )?;
    writeln!(
        output,
        "  xana config edit    Edit a validated temporary copy"
    )?;
    writeln!(output)?;
    writeln!(output, "Ready:")?;
    writeln!(output, "  xana                Start chatting")?;
    writeln!(output, "  xana doctor         Check this installation")?;
    writeln!(output)?;
    writeln!(
        output,
        "From a source checkout, prefix commands with `cargo run --`, for example `cargo run -- doctor`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_preview_restores_color_after_monochrome() {
        let ui = SetupUi {
            profile: ResolvedPresentation::test_plain(),
            rich: true,
        };
        let mut preferences = PresentationPreferences::default();
        preferences.theme = ThemeChoice::Light;
        preferences.glyphs = GlyphChoice::Unicode;
        preferences.motion = MotionChoice::Full;
        preferences.density = DensityChoice::Compact;

        let preview = preview_preferences(ui, &preferences);

        assert_eq!(preview.profile.theme, ResolvedTheme::Light);
        assert_eq!(preview.profile.color_depth, ColorDepth::Ansi256);
        assert!(preview.profile.unicode);
        assert!(!preview.profile.reduced_motion);
        assert_eq!(preview.profile.width, WidthClass::Compact);
    }
}
