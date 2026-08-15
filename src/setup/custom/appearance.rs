//! Presentation-preference editing for advanced setup.

use super::super::{
    SetupBack, prompt_default,
    ui::{SelectOption, SetupUi},
};
use crate::{
    cli::{
        ActivityChoice, ComposerChoice, DensityChoice, GlyphChoice, MotionChoice, SetupArgs,
        ThemeChoice,
    },
    paths::XanaPaths,
    presentation::{
        ActivityPaneChoice, ComposerPreset, DensityChoice as PresentationDensity,
        GlyphChoice as PresentationGlyphs, MotionChoice as PresentationMotion,
        PresentationPreferences, ResolvedTheme, SemanticToken, ThemeChoice as PresentationTheme,
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
    mut ui: SetupUi,
) -> Result<PresentationPreferences> {
    let mut preferences = PresentationPreferences::load(&paths.presentation_file()).preferences;
    if args.non_interactive && !full && flags_empty(args) {
        bail!("noninteractive appearance setup requires at least one appearance option");
    }
    preferences.theme = match args.theme {
        Some(value) => map_theme(value),
        None if full && !args.non_interactive && ui.rich => {
            let options = [
                theme_option(
                    ui,
                    "auto",
                    "terminal background · rose, aqua, amber, slate",
                    ui.profile.theme,
                ),
                theme_option(
                    ui,
                    "dark",
                    "deep water · coral rose, seafoam, amber, pearl",
                    ResolvedTheme::Dark,
                ),
                theme_option(
                    ui,
                    "light",
                    "mist · mulberry, teal, ochre, charcoal",
                    ResolvedTheme::Light,
                ),
                SelectOption::new("monochrome", "no color controls; maximum compatibility"),
            ];
            let current = theme_name(preferences.theme);
            let default = options
                .iter()
                .position(|option| option.label == current)
                .unwrap_or(0);
            let selected = super::super::ui::select(
                output,
                ui,
                "Choose a theme — palette previews",
                &options,
                default,
            )?
            .ok_or(SetupBack)?;
            parse_theme(&options[selected].label).expect("selector contains valid theme values")
        }
        None if full && !args.non_interactive => {
            writeln!(output)?;
            writeln!(output, "Themes:")?;
            writeln!(
                output,
                "  auto        terminal background · rose, aqua, amber, slate"
            )?;
            writeln!(
                output,
                "  dark        deep water · coral rose, seafoam, amber, pearl"
            )?;
            writeln!(
                output,
                "  light       mist · mulberry, teal, ochre, charcoal"
            )?;
            writeln!(output, "  monochrome  no color controls")?;
            parse_theme(&prompt_default(
                input,
                output,
                "Theme",
                theme_name(preferences.theme),
            )?)
            .context("theme must be auto, dark, light, or monochrome")?
        }
        None => preferences.theme,
    };
    match preferences.theme {
        PresentationTheme::Dark => {
            ui.profile.theme = ResolvedTheme::Dark;
            ensure_preview_color(&mut ui);
        }
        PresentationTheme::Light => {
            ui.profile.theme = ResolvedTheme::Light;
            ensure_preview_color(&mut ui);
        }
        PresentationTheme::Monochrome => {
            ui.profile.theme = ResolvedTheme::Monochrome;
            ui.profile.color_depth = crate::presentation::ColorDepth::None;
        }
        PresentationTheme::Auto => {}
    }
    if let Some(value) = args.glyphs {
        preferences.glyphs = map_glyphs(value);
    } else if full && !args.non_interactive && ui.rich {
        let options = [
            SelectOption::new("auto", "use terminal and operating-system capabilities"),
            SelectOption::new(
                "unicode",
                "use symbols, box drawing, and the Unicode portrait",
            ),
            SelectOption::new("ascii", "maximum compatibility with simple terminals"),
        ];
        let default = options
            .iter()
            .position(|option| option.label == glyph_name(preferences.glyphs))
            .unwrap_or(0);
        preferences.glyphs = parse_glyphs(
            &options[super::super::ui::select(output, ui, "Choose glyphs", &options, default)?
                .ok_or(SetupBack)?]
            .label,
        )
        .expect("valid glyph option");
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
    } else if full && !args.non_interactive && ui.rich {
        let options = [
            SelectOption::new("auto", "honor terminal and XANA_REDUCED_MOTION facts"),
            SelectOption::new("full", "enable bounded transitions and activity animation"),
            SelectOption::new("reduced", "disable nonessential animation"),
        ];
        let default = options
            .iter()
            .position(|option| option.label == motion_name(preferences.motion))
            .unwrap_or(0);
        preferences.motion = parse_motion(
            &options[super::super::ui::select(output, ui, "Choose motion", &options, default)?
                .ok_or(SetupBack)?]
            .label,
        )
        .expect("valid motion option");
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
    } else if full && !args.non_interactive && ui.rich {
        let options = [
            SelectOption::new("auto", "adapt to the available terminal width"),
            SelectOption::new("comfortable", "more space between content and controls"),
            SelectOption::new("compact", "prioritize information density"),
        ];
        let default = options
            .iter()
            .position(|option| option.label == density_name(preferences.density))
            .unwrap_or(0);
        preferences.density = parse_density(
            &options[super::super::ui::select(output, ui, "Choose density", &options, default)?
                .ok_or(SetupBack)?]
            .label,
        )
        .expect("valid density option");
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
    } else if full && !args.non_interactive && ui.rich {
        let options = [
            SelectOption::new(
                "submit",
                "Enter sends; Shift+Enter or Ctrl+J inserts a newline",
            ),
            SelectOption::new("newline", "Enter inserts a newline; use /send to submit"),
        ];
        let default = options
            .iter()
            .position(|option| option.label == composer_name(preferences.composer))
            .unwrap_or(0);
        preferences.composer = parse_composer(
            &options[super::super::ui::select(
                output,
                ui,
                "Choose Enter-key behavior",
                &options,
                default,
            )?
            .ok_or(SetupBack)?]
            .label,
        )
        .expect("valid composer option");
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
    } else if full && !args.non_interactive && ui.rich {
        let options = [
            SelectOption::new("auto", "show activity when the current layout has room"),
            SelectOption::new("open", "start with the activity pane visible"),
            SelectOption::new("hidden", "start with the activity pane hidden"),
        ];
        let default = options
            .iter()
            .position(|option| option.label == activity_name(preferences.activity))
            .unwrap_or(0);
        preferences.activity = parse_activity(
            &options[super::super::ui::select(
                output,
                ui,
                "Choose activity visibility",
                &options,
                default,
            )?
            .ok_or(SetupBack)?]
            .label,
        )
        .expect("valid activity option");
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

fn theme_option(ui: SetupUi, label: &str, detail: &str, theme: ResolvedTheme) -> SelectOption {
    let mut preview = crate::presentation::ResolvedPresentation {
        theme,
        ..ui.profile
    };
    if theme != ResolvedTheme::Monochrome
        && preview.color_depth == crate::presentation::ColorDepth::None
    {
        preview.color_depth = crate::presentation::ColorDepth::Ansi256;
    }
    SelectOption::new(label, detail).with_swatches(
        [
            SemanticToken::Accent,
            SemanticToken::User,
            SemanticToken::Success,
            SemanticToken::Warning,
            SemanticToken::Muted,
        ]
        .into_iter()
        .filter_map(|token| preview.color(token)),
    )
}

fn ensure_preview_color(ui: &mut SetupUi) {
    if ui.profile.color_depth == crate::presentation::ColorDepth::None {
        ui.profile.color_depth = crate::presentation::ColorDepth::Ansi256;
    }
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
