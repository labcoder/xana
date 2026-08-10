//! Presentation-preference editing for advanced setup.

use super::super::prompt_default;
use crate::{
    cli::{
        ActivityChoice, ComposerChoice, DensityChoice, GlyphChoice, MotionChoice, SetupArgs,
        ThemeChoice,
    },
    paths::XanaPaths,
    presentation::{
        ActivityPaneChoice, ComposerPreset, DensityChoice as PresentationDensity,
        GlyphChoice as PresentationGlyphs, MotionChoice as PresentationMotion,
        PresentationPreferences, ThemeChoice as PresentationTheme,
    },
};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, Write};

pub(super) fn edit(
    args: &SetupArgs,
    paths: &XanaPaths,
    input: &mut impl BufRead,
    output: &mut impl Write,
    full: bool,
) -> Result<PresentationPreferences> {
    let mut preferences = PresentationPreferences::load(&paths.presentation_file()).preferences;
    if args.non_interactive && !full && flags_empty(args) {
        bail!("noninteractive appearance setup requires at least one appearance option");
    }
    preferences.theme = match args.theme {
        Some(value) => map_theme(value),
        None if full && !args.non_interactive => parse_theme(&prompt_default(
            input,
            output,
            "Theme",
            theme_name(preferences.theme),
        )?)
        .context("theme must be auto, dark, light, or monochrome")?,
        None => preferences.theme,
    };
    if let Some(value) = args.glyphs {
        preferences.glyphs = map_glyphs(value);
    } else if full && !args.non_interactive {
        preferences.glyphs = parse_glyphs(&prompt_default(
            input,
            output,
            "Glyphs",
            glyph_name(preferences.glyphs),
        )?)
        .context("glyphs must be auto, unicode, or ascii")?;
    }
    if let Some(value) = args.motion {
        preferences.motion = map_motion(value);
    } else if full && !args.non_interactive {
        preferences.motion = parse_motion(&prompt_default(
            input,
            output,
            "Motion",
            motion_name(preferences.motion),
        )?)
        .context("motion must be auto, full, or reduced")?;
    }
    if let Some(value) = args.density {
        preferences.density = map_density(value);
    } else if full && !args.non_interactive {
        preferences.density = parse_density(&prompt_default(
            input,
            output,
            "Density",
            density_name(preferences.density),
        )?)
        .context("density must be auto, comfortable, or compact")?;
    }
    if let Some(value) = args.composer {
        preferences.composer = match value {
            ComposerChoice::Submit => ComposerPreset::Submit,
            ComposerChoice::Newline => ComposerPreset::Newline,
        };
    } else if full && !args.non_interactive {
        preferences.composer = parse_composer(&prompt_default(
            input,
            output,
            "Enter key",
            composer_name(preferences.composer),
        )?)
        .context("composer must be submit or newline")?;
    }
    if let Some(value) = args.activity {
        preferences.activity = match value {
            ActivityChoice::Auto => ActivityPaneChoice::Auto,
            ActivityChoice::Open => ActivityPaneChoice::Open,
            ActivityChoice::Hidden => ActivityPaneChoice::Hidden,
        };
    } else if full && !args.non_interactive {
        preferences.activity = parse_activity(&prompt_default(
            input,
            output,
            "Activity pane",
            activity_name(preferences.activity),
        )?)
        .context("activity must be auto, open, or hidden")?;
    }
    Ok(preferences)
}

pub(super) fn flags_empty(args: &SetupArgs) -> bool {
    args.theme.is_none()
        && args.glyphs.is_none()
        && args.motion.is_none()
        && args.density.is_none()
        && args.composer.is_none()
        && args.activity.is_none()
}

fn map_theme(value: ThemeChoice) -> PresentationTheme {
    match value {
        ThemeChoice::Auto => PresentationTheme::Auto,
        ThemeChoice::Dark => PresentationTheme::Dark,
        ThemeChoice::Light => PresentationTheme::Light,
        ThemeChoice::Monochrome => PresentationTheme::Monochrome,
    }
}

fn map_glyphs(value: GlyphChoice) -> PresentationGlyphs {
    match value {
        GlyphChoice::Auto => PresentationGlyphs::Auto,
        GlyphChoice::Unicode => PresentationGlyphs::Unicode,
        GlyphChoice::Ascii => PresentationGlyphs::Ascii,
    }
}

fn map_motion(value: MotionChoice) -> PresentationMotion {
    match value {
        MotionChoice::Auto => PresentationMotion::Auto,
        MotionChoice::Full => PresentationMotion::Full,
        MotionChoice::Reduced => PresentationMotion::Reduced,
    }
}

fn map_density(value: DensityChoice) -> PresentationDensity {
    match value {
        DensityChoice::Auto => PresentationDensity::Auto,
        DensityChoice::Comfortable => PresentationDensity::Comfortable,
        DensityChoice::Compact => PresentationDensity::Compact,
    }
}

fn theme_name(value: PresentationTheme) -> &'static str {
    match value {
        PresentationTheme::Auto => "auto",
        PresentationTheme::Dark => "dark",
        PresentationTheme::Light => "light",
        PresentationTheme::Monochrome => "monochrome",
    }
}

fn parse_theme(value: &str) -> Option<PresentationTheme> {
    match value {
        "auto" => Some(PresentationTheme::Auto),
        "dark" => Some(PresentationTheme::Dark),
        "light" => Some(PresentationTheme::Light),
        "monochrome" => Some(PresentationTheme::Monochrome),
        _ => None,
    }
}

fn glyph_name(value: PresentationGlyphs) -> &'static str {
    match value {
        PresentationGlyphs::Auto => "auto",
        PresentationGlyphs::Unicode => "unicode",
        PresentationGlyphs::Ascii => "ascii",
    }
}

fn parse_glyphs(value: &str) -> Option<PresentationGlyphs> {
    match value {
        "auto" => Some(PresentationGlyphs::Auto),
        "unicode" => Some(PresentationGlyphs::Unicode),
        "ascii" => Some(PresentationGlyphs::Ascii),
        _ => None,
    }
}

fn motion_name(value: PresentationMotion) -> &'static str {
    match value {
        PresentationMotion::Auto => "auto",
        PresentationMotion::Full => "full",
        PresentationMotion::Reduced => "reduced",
    }
}

fn parse_motion(value: &str) -> Option<PresentationMotion> {
    match value {
        "auto" => Some(PresentationMotion::Auto),
        "full" => Some(PresentationMotion::Full),
        "reduced" => Some(PresentationMotion::Reduced),
        _ => None,
    }
}

fn density_name(value: PresentationDensity) -> &'static str {
    match value {
        PresentationDensity::Auto => "auto",
        PresentationDensity::Comfortable => "comfortable",
        PresentationDensity::Compact => "compact",
    }
}

fn parse_density(value: &str) -> Option<PresentationDensity> {
    match value {
        "auto" => Some(PresentationDensity::Auto),
        "comfortable" => Some(PresentationDensity::Comfortable),
        "compact" => Some(PresentationDensity::Compact),
        _ => None,
    }
}

fn composer_name(value: ComposerPreset) -> &'static str {
    match value {
        ComposerPreset::Submit => "submit",
        ComposerPreset::Newline => "newline",
    }
}

fn parse_composer(value: &str) -> Option<ComposerPreset> {
    match value {
        "submit" => Some(ComposerPreset::Submit),
        "newline" => Some(ComposerPreset::Newline),
        _ => None,
    }
}

fn activity_name(value: ActivityPaneChoice) -> &'static str {
    match value {
        ActivityPaneChoice::Auto => "auto",
        ActivityPaneChoice::Open => "open",
        ActivityPaneChoice::Hidden => "hidden",
    }
}

fn parse_activity(value: &str) -> Option<ActivityPaneChoice> {
    match value {
        "auto" => Some(ActivityPaneChoice::Auto),
        "open" => Some(ActivityPaneChoice::Open),
        "hidden" => Some(ActivityPaneChoice::Hidden),
        _ => None,
    }
}
