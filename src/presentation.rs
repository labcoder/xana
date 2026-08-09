//! Frontend-owned terminal capability, preference, and semantic-style policy.
//!
//! Process facts are injected by the application edge. This module is pure
//! apart from bounded preference I/O and never changes runtime authority.

use crate::bounded_file;
use serde::{Deserialize, Serialize};
use std::{io, io::Write, path::Path};

const PREFERENCE_VERSION: u16 = 1;
const MAX_PREFERENCE_BYTES: usize = 32 * 1024;
const PORTRAIT_WIDTH: usize = 79;
const WORDMARK_WIDTH: usize = 27;
const WORDMARK_INDENT: usize = (PORTRAIT_WIDTH - WORDMARK_WIDTH) / 2;

const PORTRAIT: &[&str] = &[
    "",
    "",
    "                                  :-===-*%%%%*:",
    "                         :*#**%%%%%%%%%%%%%%%%%%%%",
    "                       %%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%=",
    "                     %%%-*%%%%%%%%%%%%%%%%%%%%%%%%%%=%%%*",
    "                    @%. :- %%%%**%%%%%%%%%%%%%%%%%%%%++%%-",
    "                   :* :%%%-:%%%% %%%%+%%%%%%%%%#%%%%%%.%%%%",
    "                     *%%%%%:%%%% *%%%*=%%%%%%%%% %%%%%=*%%%%+",
    "                    =%%%%%%=#+%%  *%%% :%%%%%%%%-:%%%%+ %%%*%:",
    "                    %%%%%%%%: %%    %%%#  %%%%%%: %%%%%  %%:%:",
    "                   %%%#**#%%%  %%   ==%%%* +%%%%: *%%%%*  %%%:",
    "                   %* .*##+ *%% %%=   %%%%. %#%%%   %%%%%  %@:",
    "                   =%%%+#%%%%%%%#*%.   =%*+  #-%%#   +%%%% *+%",
    "                   *%#    %%%%%%%+%: #  %%      %%%   %.%%   ##",
    "                  +%%%% +*%%%%%%%## :%: %:  :%%%: %   %-.%   =#",
    "                 %%%%%%%%%%%%%%%-% +%% *.  ** %+%.-   %  -   -:",
    "              .%%%%%%%%%%%%%%%%%#= %%=  +%%% =%%%.    -     +.",
    "       .:::--: %*#%%%%%%%%%%%%%%%% =%*  %%%   %%%  :   .#    .  ------::.",
    "   :--::.       .*#%%%%%%%%%%%**%+*.%%  %%%%%%%-   -    ==  .......        -==-",
    "                               ---                                .:::..",
];

const WORDMARK: &[&str] = &[
    "▄▄▄   ▄▄▄",
    "████▄████",
    " ▀█████▀   ▀▀█▄ ████▄  ▀▀█▄",
    "▄███████▄ ▄█▀██ ██ ██ ▄█▀██",
    "███▀ ▀███ ▀█▄██ ██ ██ ▀█▄██",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ThemeChoice {
    #[default]
    Auto,
    Dark,
    Light,
    Monochrome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GlyphChoice {
    #[default]
    Auto,
    Unicode,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MotionChoice {
    #[default]
    Auto,
    Full,
    Reduced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DensityChoice {
    #[default]
    Auto,
    Comfortable,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComposerPreset {
    #[default]
    Submit,
    Newline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PresentationPreferences {
    version: u16,
    #[serde(default)]
    pub(crate) theme: ThemeChoice,
    #[serde(default)]
    pub(crate) glyphs: GlyphChoice,
    #[serde(default)]
    pub(crate) motion: MotionChoice,
    #[serde(default)]
    pub(crate) density: DensityChoice,
    #[serde(default)]
    pub(crate) composer: ComposerPreset,
}

impl Default for PresentationPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCE_VERSION,
            theme: ThemeChoice::Auto,
            glyphs: GlyphChoice::Auto,
            motion: MotionChoice::Auto,
            density: DensityChoice::Auto,
            composer: ComposerPreset::Submit,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreferenceLoad {
    pub(crate) preferences: PresentationPreferences,
    pub(crate) warning: Option<String>,
}

impl PresentationPreferences {
    pub(crate) fn load(path: &Path) -> PreferenceLoad {
        let bytes = match bounded_file::read(path, MAX_PREFERENCE_BYTES) {
            Ok(bytes) => bytes,
            Err(bounded_file::BoundedReadError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return PreferenceLoad {
                    preferences: Self::default(),
                    warning: None,
                };
            }
            Err(error) => {
                return preference_fallback(path, error.to_string());
            }
        };
        let preferences: Self = match toml::from_slice(&bytes) {
            Ok(preferences) => preferences,
            Err(error) => return preference_fallback(path, error.to_string()),
        };
        if preferences.version != PREFERENCE_VERSION {
            return preference_fallback(
                path,
                format!("unsupported version {}", preferences.version),
            );
        }
        PreferenceLoad {
            preferences,
            warning: None,
        }
    }

    fn store(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rendered = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let mut file = atomic_write_file::AtomicWriteFile::open(path)?;
        file.write_all(rendered.as_bytes())?;
        file.commit()
    }

    pub(crate) fn set_composer(path: &Path, composer: ComposerPreset) -> io::Result<()> {
        let mut preferences = Self::load(path).preferences;
        preferences.composer = composer;
        preferences.store(path)
    }
}

fn preference_fallback(path: &Path, reason: String) -> PreferenceLoad {
    PreferenceLoad {
        preferences: PresentationPreferences::default(),
        warning: Some(format!(
            "could not use presentation preferences at {} ({reason}); using safe defaults",
            path.display()
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Background {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalFacts {
    pub(crate) input_is_terminal: bool,
    pub(crate) output_is_terminal: bool,
    pub(crate) color_depth: ColorDepth,
    pub(crate) background: Option<Background>,
    pub(crate) unicode: bool,
    pub(crate) width: u16,
    pub(crate) reduced_motion: bool,
    pub(crate) dumb: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedTheme {
    Dark,
    Light,
    Monochrome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WidthClass {
    Wide,
    Compact,
    Narrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedPresentation {
    pub(crate) theme: ResolvedTheme,
    pub(crate) color_depth: ColorDepth,
    pub(crate) unicode: bool,
    pub(crate) width: WidthClass,
    pub(crate) reduced_motion: bool,
}

pub(crate) fn resolve(
    facts: TerminalFacts,
    preferences: &PresentationPreferences,
) -> ResolvedPresentation {
    debug_assert_eq!(SemanticToken::all().len(), 15);
    let interactive = facts.input_is_terminal && facts.output_is_terminal && !facts.dumb;
    let theme = match preferences.theme {
        ThemeChoice::Dark => ResolvedTheme::Dark,
        ThemeChoice::Light => ResolvedTheme::Light,
        ThemeChoice::Monochrome => ResolvedTheme::Monochrome,
        ThemeChoice::Auto => match facts.background {
            Some(Background::Light) => ResolvedTheme::Light,
            Some(Background::Dark) | None => ResolvedTheme::Dark,
        },
    };
    let color_depth = if !interactive || theme == ResolvedTheme::Monochrome {
        ColorDepth::None
    } else {
        facts.color_depth
    };
    let unicode = match preferences.glyphs {
        GlyphChoice::Unicode => true,
        GlyphChoice::Ascii => false,
        GlyphChoice::Auto => interactive && facts.unicode,
    };
    let width = if facts.width < 50 {
        WidthClass::Narrow
    } else if facts.width < PORTRAIT_WIDTH as u16 || preferences.density == DensityChoice::Compact {
        WidthClass::Compact
    } else {
        WidthClass::Wide
    };
    let reduced_motion = match preferences.motion {
        MotionChoice::Reduced => true,
        MotionChoice::Full => false,
        MotionChoice::Auto => facts.reduced_motion,
    };
    ResolvedPresentation {
        theme,
        color_depth,
        unicode,
        width,
        reduced_motion,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticToken {
    Accent,
    Muted,
    Success,
    Warning,
    Danger,
    User,
    Assistant,
    Summary,
    Reasoning,
    Tool,
    Child,
    Focus,
    Approval,
    DiffAdd,
    DiffRemove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentationColor {
    Rgb(u8, u8, u8),
    Indexed(u8),
    Red,
    Green,
    Yellow,
    Magenta,
    Cyan,
    DarkGray,
}

impl SemanticToken {
    pub(crate) const fn all() -> [Self; 15] {
        [
            Self::Accent,
            Self::Muted,
            Self::Success,
            Self::Warning,
            Self::Danger,
            Self::User,
            Self::Assistant,
            Self::Summary,
            Self::Reasoning,
            Self::Tool,
            Self::Child,
            Self::Focus,
            Self::Approval,
            Self::DiffAdd,
            Self::DiffRemove,
        ]
    }
}

impl ResolvedPresentation {
    pub(crate) fn color(self, token: SemanticToken) -> Option<PresentationColor> {
        let slot = token_slot(self.theme, token);
        match self.color_depth {
            ColorDepth::None => None,
            ColorDepth::TrueColor => {
                let (red, green, blue) = slot.rgb;
                Some(PresentationColor::Rgb(red, green, blue))
            }
            ColorDepth::Ansi256 => Some(PresentationColor::Indexed(slot.ansi256)),
            ColorDepth::Ansi16 => Some(slot.ansi16),
        }
    }

    pub(crate) fn paint(self, token: SemanticToken, text: &str) -> String {
        let Some(code) = ansi_code(self.color(token)) else {
            return text.to_owned();
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }

    #[cfg(test)]
    pub(crate) const fn test_plain() -> Self {
        Self {
            theme: ResolvedTheme::Monochrome,
            color_depth: ColorDepth::None,
            unicode: false,
            width: WidthClass::Narrow,
            reduced_motion: true,
        }
    }
}

fn ansi_code(color: Option<PresentationColor>) -> Option<String> {
    Some(match color? {
        PresentationColor::Rgb(red, green, blue) => format!("38;2;{red};{green};{blue}"),
        PresentationColor::Indexed(index) => format!("38;5;{index}"),
        PresentationColor::Red => "31".to_owned(),
        PresentationColor::Green => "32".to_owned(),
        PresentationColor::Yellow => "33".to_owned(),
        PresentationColor::Magenta => "35".to_owned(),
        PresentationColor::Cyan => "36".to_owned(),
        PresentationColor::DarkGray => "90".to_owned(),
    })
}

struct ColorSlot {
    rgb: (u8, u8, u8),
    ansi256: u8,
    ansi16: PresentationColor,
}

fn token_slot(theme: ResolvedTheme, token: SemanticToken) -> ColorSlot {
    let light = theme == ResolvedTheme::Light;
    match token {
        SemanticToken::Accent | SemanticToken::Assistant | SemanticToken::Focus => ColorSlot {
            rgb: if light {
                (177, 38, 100)
            } else {
                (255, 99, 125)
            },
            ansi256: if light { 125 } else { 204 },
            ansi16: PresentationColor::Magenta,
        },
        SemanticToken::Muted | SemanticToken::Summary | SemanticToken::Reasoning => ColorSlot {
            rgb: if light {
                (80, 91, 104)
            } else {
                (156, 163, 175)
            },
            ansi256: if light { 59 } else { 247 },
            ansi16: PresentationColor::DarkGray,
        },
        SemanticToken::Success | SemanticToken::DiffAdd => ColorSlot {
            rgb: if light { (20, 120, 83) } else { (83, 208, 158) },
            ansi256: if light { 29 } else { 79 },
            ansi16: PresentationColor::Green,
        },
        SemanticToken::Warning | SemanticToken::Approval => ColorSlot {
            rgb: if light { (155, 91, 0) } else { (246, 193, 91) },
            ansi256: if light { 130 } else { 221 },
            ansi16: PresentationColor::Yellow,
        },
        SemanticToken::Danger | SemanticToken::DiffRemove => ColorSlot {
            rgb: if light {
                (183, 37, 56)
            } else {
                (255, 111, 125)
            },
            ansi256: if light { 124 } else { 203 },
            ansi16: PresentationColor::Red,
        },
        SemanticToken::User | SemanticToken::Tool | SemanticToken::Child => ColorSlot {
            rgb: if light {
                (0, 116, 128)
            } else {
                (101, 212, 222)
            },
            ansi256: if light { 30 } else { 80 },
            ansi16: PresentationColor::Cyan,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BannerMode {
    visible: bool,
    profile: ResolvedPresentation,
}

impl BannerMode {
    pub(crate) const fn hidden(profile: ResolvedPresentation) -> Self {
        Self {
            visible: false,
            profile,
        }
    }

    pub(crate) const fn visible(profile: ResolvedPresentation) -> Self {
        Self {
            visible: true,
            profile,
        }
    }

    pub(crate) const fn profile(self) -> ResolvedPresentation {
        self.profile
    }

    #[cfg(test)]
    pub(crate) const fn test_hidden() -> Self {
        Self::hidden(ResolvedPresentation::test_plain())
    }
}

pub(crate) fn choose_banner_mode(
    selected_surface: bool,
    suppressed: bool,
    profile: ResolvedPresentation,
) -> BannerMode {
    if selected_surface && !suppressed {
        BannerMode::visible(profile)
    } else {
        BannerMode::hidden(profile)
    }
}

pub(crate) fn write_banner<W: Write>(output: &mut W, mode: BannerMode) -> io::Result<()> {
    if !mode.visible {
        return Ok(());
    }
    match mode.profile.width {
        WidthClass::Wide if mode.profile.unicode => {
            write_asset(output, PORTRAIT, 0, PORTRAIT_WIDTH, mode.profile)?;
            writeln!(output)?;
            write_asset(
                output,
                WORDMARK,
                WORDMARK_INDENT,
                WORDMARK_WIDTH,
                mode.profile,
            )?;
        }
        WidthClass::Compact if mode.profile.unicode => {
            write_asset(output, WORDMARK, 0, WORDMARK_WIDTH, mode.profile)?;
        }
        _ => {
            let mark = if mode.profile.unicode {
                "✦ Xana"
            } else {
                "[ Xana ]"
            };
            writeln!(
                output,
                "{}",
                mode.profile.paint(SemanticToken::Accent, mark)
            )?;
        }
    }
    writeln!(output)
}

fn write_asset<W: Write>(
    output: &mut W,
    lines: &[&str],
    indent: usize,
    width: usize,
    profile: ResolvedPresentation,
) -> io::Result<()> {
    for line in lines {
        if line.is_empty() {
            writeln!(output)?;
            continue;
        }
        write!(output, "{:indent$}", "")?;
        for (column, character) in line.chars().enumerate() {
            let token = if column.saturating_mul(2) < width {
                SemanticToken::Accent
            } else {
                SemanticToken::User
            };
            write!(output, "{}", profile.paint(token, &character.to_string()))?;
        }
        writeln!(output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn facts(depth: ColorDepth, width: u16) -> TerminalFacts {
        TerminalFacts {
            input_is_terminal: true,
            output_is_terminal: true,
            color_depth: depth,
            background: None,
            unicode: true,
            width,
            reduced_motion: false,
            dumb: false,
        }
    }

    #[test]
    fn resolution_covers_capability_theme_width_glyph_and_motion_precedence() {
        let defaults = PresentationPreferences::default();
        let auto = resolve(facts(ColorDepth::TrueColor, 100), &defaults);
        assert_eq!(auto.theme, ResolvedTheme::Dark);
        assert_eq!(auto.width, WidthClass::Wide);
        assert!(auto.unicode);

        let preferences = PresentationPreferences {
            theme: ThemeChoice::Light,
            glyphs: GlyphChoice::Ascii,
            motion: MotionChoice::Reduced,
            density: DensityChoice::Compact,
            ..defaults
        };
        let resolved = resolve(facts(ColorDepth::Ansi256, 100), &preferences);
        assert_eq!(resolved.theme, ResolvedTheme::Light);
        assert_eq!(resolved.color_depth, ColorDepth::Ansi256);
        assert_eq!(resolved.width, WidthClass::Compact);
        assert!(!resolved.unicode);
        assert!(resolved.reduced_motion);
    }

    #[test]
    fn redirected_dumb_and_monochrome_profiles_emit_no_controls() {
        let mut cases = Vec::new();
        let mut redirected = facts(ColorDepth::TrueColor, 100);
        redirected.output_is_terminal = false;
        cases.push(resolve(redirected, &PresentationPreferences::default()));
        let mut dumb = facts(ColorDepth::TrueColor, 100);
        dumb.dumb = true;
        cases.push(resolve(dumb, &PresentationPreferences::default()));
        cases.push(resolve(
            facts(ColorDepth::TrueColor, 100),
            &PresentationPreferences {
                theme: ThemeChoice::Monochrome,
                ..PresentationPreferences::default()
            },
        ));

        for profile in cases {
            for token in SemanticToken::all() {
                assert!(!profile.paint(token, "state").contains('\x1b'));
            }
        }
    }

    #[test]
    fn every_depth_and_width_renders_bounded_recognizable_output() {
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::None,
        ] {
            for width in [30, 60, 100] {
                let profile = resolve(facts(depth, width), &PresentationPreferences::default());
                let mut output = Vec::new();
                write_banner(&mut output, BannerMode::visible(profile)).unwrap();
                let output = String::from_utf8(output).unwrap();
                assert!(output.contains("Xana") || output.contains('█') || output.contains('%'));
                if depth == ColorDepth::None {
                    assert!(!output.contains('\x1b'));
                }
                if width < 50 {
                    assert!(
                        output
                            .lines()
                            .all(|line| strip_ansi(line).chars().count() <= 30)
                    );
                }
            }
        }
    }

    #[test]
    fn preference_store_is_versioned_bounded_and_recovers_from_corruption() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("presentation.toml");
        let preferences = PresentationPreferences {
            theme: ThemeChoice::Light,
            ..PresentationPreferences::default()
        };
        preferences.store(&path).unwrap();
        let loaded = PresentationPreferences::load(&path);
        assert_eq!(loaded.preferences, preferences);
        assert!(loaded.warning.is_none());

        std::fs::write(&path, b"version = 99\ntheme = 'dark'\n").unwrap();
        let recovered = PresentationPreferences::load(&path);
        assert_eq!(recovered.preferences, PresentationPreferences::default());
        assert!(recovered.warning.unwrap().contains("safe defaults"));

        std::fs::write(&path, vec![b'x'; MAX_PREFERENCE_BYTES + 1]).unwrap();
        assert!(PresentationPreferences::load(&path).warning.is_some());
    }

    fn strip_ansi(value: &str) -> String {
        let mut output = String::new();
        let mut escape = false;
        for character in value.chars() {
            if character == '\x1b' {
                escape = true;
            } else if escape && character == 'm' {
                escape = false;
            } else if !escape {
                output.push(character);
            }
        }
        output
    }
}
