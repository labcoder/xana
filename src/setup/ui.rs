//! Line-oriented setup presentation and guided-path selection.

use super::{SetupArgs, prompt_default};
use crate::{
    paths::XanaPaths,
    presentation::{ResolvedPresentation, SemanticToken},
};
use anyhow::{Result, bail};
use std::io::{self, BufRead, Write};

pub(super) fn choose_setup_path(
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    profile: ResolvedPresentation,
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

    write_setup_heading(output, profile, "Xana Setup")?;
    writeln!(
        output,
        "{}",
        profile.paint(
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
        write_setup_choice(output, profile, number, label, detail, default)?;
    }
    writeln!(
        output,
        "  {}  {}",
        profile.paint(SemanticToken::Muted, "7."),
        profile.paint(SemanticToken::Muted, "Cancel")
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
