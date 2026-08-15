//! Rich terminal setup presentation with a permanent line-oriented fallback.

use super::{SetupArgs, SetupCancelled, prompt_default};
use crate::{
    paths::XanaPaths,
    presentation::{ResolvedPresentation, SemanticToken},
};
use anyhow::{Result, bail};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use std::io::{self, BufRead, Write};

#[derive(Debug, Clone, Copy)]
pub(super) struct SetupUi {
    pub(super) profile: ResolvedPresentation,
    pub(super) rich: bool,
}

#[derive(Debug, Clone)]
pub(super) struct SelectOption {
    pub(super) label: String,
    pub(super) detail: String,
    pub(super) keywords: String,
}

impl SelectOption {
    pub(super) fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            keywords: String::new(),
        }
    }

    pub(super) fn with_keywords(mut self, keywords: impl Into<String>) -> Self {
        self.keywords = keywords.into();
        self
    }
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

    enable_raw_mode().map_err(anyhow::Error::new)?;
    if let Err(error) = execute!(output, EnterAlternateScreen, Hide) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    let result = select_inner(output, ui.profile, title, options, default);
    let cleanup = execute!(output, Show, LeaveAlternateScreen)
        .map_err(anyhow::Error::new)
        .and_then(|()| disable_raw_mode().map_err(anyhow::Error::new));
    match (result, cleanup) {
        (Ok(selection), Ok(())) => Ok(selection),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn select_inner(
    output: &mut impl Write,
    profile: ResolvedPresentation,
    title: &str,
    options: &[SelectOption],
    default: usize,
) -> Result<Option<usize>> {
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
        draw_selector(output, profile, title, options, &filtered, selected, &query)?;
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
    output: &mut impl Write,
    profile: ResolvedPresentation,
    title: &str,
    options: &[SelectOption],
    filtered: &[usize],
    selected: usize,
    query: &str,
) -> io::Result<()> {
    let (_, height) = size().unwrap_or((80, 24));
    let visible_rows = usize::from(height.saturating_sub(12)).clamp(4, 14);
    let start = selected
        .saturating_sub(visible_rows / 2)
        .min(filtered.len().saturating_sub(visible_rows));
    let end = (start + visible_rows).min(filtered.len());
    execute!(output, MoveTo(0, 0), Clear(ClearType::All))?;
    write_setup_logo(output, profile)?;
    writeln!(output, "{}", profile.paint(SemanticToken::Accent, title))?;
    writeln!(
        output,
        "{}",
        profile.paint(
            SemanticToken::Muted,
            "↑/↓ move · PgUp/PgDn page · type to filter · Enter choose · Esc back"
        )
    )?;
    writeln!(output)?;
    writeln!(
        output,
        "  {} {}",
        profile.paint(SemanticToken::Muted, "Search:"),
        if query.is_empty() {
            profile.paint(SemanticToken::Muted, "all options")
        } else {
            profile.paint(SemanticToken::Focus, query)
        }
    )?;
    writeln!(output)?;
    if filtered.is_empty() {
        writeln!(
            output,
            "  {}",
            profile.paint(SemanticToken::Warning, "No matching options")
        )?;
    } else {
        for (position, option_index) in filtered[start..end].iter().enumerate() {
            let option = &options[*option_index];
            let active = start + position == selected;
            let marker = if active {
                if profile.unicode { "▶" } else { ">" }
            } else {
                " "
            };
            writeln!(
                output,
                "  {} {}",
                profile.paint(
                    if active {
                        SemanticToken::Focus
                    } else {
                        SemanticToken::Muted
                    },
                    marker
                ),
                profile.paint(
                    if active {
                        SemanticToken::Focus
                    } else {
                        SemanticToken::Assistant
                    },
                    &option.label
                )
            )?;
            writeln!(
                output,
                "      {}",
                profile.paint(SemanticToken::Muted, &option.detail)
            )?;
        }
    }
    writeln!(output)?;
    writeln!(
        output,
        "  {} of {} matching · {} total",
        if filtered.is_empty() { 0 } else { selected + 1 },
        filtered.len(),
        options.len()
    )?;
    output.flush()
}

pub(super) fn write_setup_logo(
    output: &mut impl Write,
    profile: ResolvedPresentation,
) -> io::Result<()> {
    let lines = if profile.unicode {
        [
            "             ╭────────╮",
            "          .─´  ◕    ◕  `─.",
            "    ≋≋≋≋╱       ╰─╯       ╲≋≋≋≋",
            "        ╰─.____________.─╯",
            "              X A N A",
        ]
    } else {
        [
            "             .--------.",
            "          .-'  o    o  '-.",
            "    ~~~~/       .--.       \\~~~~",
            "        '-.____________.-'",
            "              X A N A",
        ]
    };
    for line in lines {
        writeln!(output, "{}", profile.paint(SemanticToken::Accent, line))?;
    }
    writeln!(output)
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
    writeln!(output, "Xana installed the configuration atomically.")?;
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
