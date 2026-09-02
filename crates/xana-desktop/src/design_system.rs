//! Xana-owned semantic appearance policy for the native Desktop.
//!
//! `gpui-component` and `gpui-ai` own component mechanics. This module owns
//! the product-level palette, density, scale, and motion choices projected
//! into those libraries. Raw color literals are deliberately confined here.

use gpui::{App, Global, Hsla, SharedString};
use gpui_ai::prelude::{MotionPreference, MotionTokens};
use gpui_component::{
    ActiveTheme as _, SemanticThemeTokens, Theme, ThemeConfig, ThemeConfigColors, ThemeMode,
};
use std::rc::Rc;
use xana::desktop::DesktopSettingsSnapshot;

const BASE_FONT_SIZE: f32 = 16.0;
const BASE_MONO_FONT_SIZE: f32 = 13.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ColorScheme {
    Light,
    #[default]
    Dark,
    HighContrast,
}

impl ColorScheme {
    pub(crate) const ALL: [Self; 3] = [Self::Light, Self::Dark, Self::HighContrast];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::HighContrast => "High contrast",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Density {
    Compact,
    #[default]
    Comfortable,
}

impl Density {
    pub(crate) const ALL: [Self; 2] = [Self::Compact, Self::Comfortable];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Comfortable => "Comfortable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MotionMode {
    #[default]
    Full,
    Reduced,
    None,
}

impl MotionMode {
    pub(crate) const ALL: [Self; 3] = [Self::Full, Self::Reduced, Self::None];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Full => "Full motion",
            Self::Reduced => "Reduced motion",
            Self::None => "No motion",
        }
    }

    const fn preference(self) -> MotionPreference {
        match self {
            Self::Full => MotionPreference::Full,
            Self::Reduced => MotionPreference::Crossfade,
            Self::None => MotionPreference::Snap,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppearancePreferences {
    pub(crate) scheme: ColorScheme,
    pub(crate) density: Density,
    pub(crate) motion: MotionMode,
    pub(crate) text_scale_percent: u16,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            scheme: ColorScheme::Dark,
            density: Density::Comfortable,
            motion: MotionMode::Full,
            text_scale_percent: 100,
        }
    }
}

impl AppearancePreferences {
    pub(crate) const MIN_TEXT_SCALE_PERCENT: u16 = 100;
    pub(crate) const MAX_TEXT_SCALE_PERCENT: u16 = 200;

    pub(crate) fn with_text_scale(mut self, percent: u16) -> Self {
        self.text_scale_percent =
            percent.clamp(Self::MIN_TEXT_SCALE_PERCENT, Self::MAX_TEXT_SCALE_PERCENT);
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VisualSystem {
    preferences: AppearancePreferences,
    tokens: SemanticThemeTokens,
}

impl Global for VisualSystem {}

impl VisualSystem {
    pub(crate) fn read(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub(crate) const fn preferences(&self) -> AppearancePreferences {
        self.preferences
    }

    pub(crate) fn tokens(&self) -> &SemanticThemeTokens {
        &self.tokens
    }
}

#[derive(Debug, Clone, Copy)]
struct Palette {
    background: u32,
    foreground: u32,
    surface: u32,
    surface_foreground: u32,
    primary: u32,
    primary_foreground: u32,
    secondary: u32,
    secondary_foreground: u32,
    muted: u32,
    muted_foreground: u32,
    accent: u32,
    accent_foreground: u32,
    destructive: u32,
    destructive_foreground: u32,
    border: u32,
    input: u32,
    ring: u32,
    success: u32,
    warning: u32,
    info: u32,
    selection: &'static str,
}

const LIGHT: Palette = Palette {
    background: 0xfaf8f5,
    foreground: 0x201b18,
    surface: 0xffffff,
    surface_foreground: 0x201b18,
    primary: 0x76264c,
    primary_foreground: 0xffffff,
    secondary: 0xeee7e1,
    secondary_foreground: 0x332b27,
    muted: 0xf2ece7,
    muted_foreground: 0x675d57,
    accent: 0xf5dce8,
    accent_foreground: 0x4b1830,
    destructive: 0xa91b12,
    destructive_foreground: 0xffffff,
    border: 0xcfc5bd,
    input: 0x8c7f76,
    ring: 0x76264c,
    success: 0x176b45,
    warning: 0x8a4d00,
    info: 0x1c5f8a,
    selection: "#c94f864d",
};

const DARK: Palette = Palette {
    background: 0x181315,
    foreground: 0xf8f2f5,
    surface: 0x211a1d,
    surface_foreground: 0xf8f2f5,
    primary: 0xf28db9,
    primary_foreground: 0x2b0c19,
    secondary: 0x35292f,
    secondary_foreground: 0xf8f2f5,
    muted: 0x2a2225,
    muted_foreground: 0xbeb2b7,
    accent: 0x4a2838,
    accent_foreground: 0xffffff,
    destructive: 0xff9b93,
    destructive_foreground: 0x2b0704,
    border: 0x57464e,
    input: 0x77656d,
    ring: 0xf28db9,
    success: 0x66d69a,
    warning: 0xf6c66d,
    info: 0x79c9ff,
    selection: "#f28db94d",
};

const HIGH_CONTRAST: Palette = Palette {
    background: 0x000000,
    foreground: 0xffffff,
    surface: 0x000000,
    surface_foreground: 0xffffff,
    primary: 0xffdf00,
    primary_foreground: 0x000000,
    secondary: 0xffffff,
    secondary_foreground: 0x000000,
    muted: 0x161616,
    muted_foreground: 0xffffff,
    accent: 0x00ffff,
    accent_foreground: 0x000000,
    destructive: 0xff6b6b,
    destructive_foreground: 0x000000,
    border: 0xffffff,
    input: 0xffffff,
    ring: 0x00ffff,
    success: 0x76ff9f,
    warning: 0xffdf00,
    info: 0x66d9ff,
    selection: "#00ffff66",
};

/// Install the initial product appearance after `gpui_ai::init`.
pub(crate) fn install(cx: &mut App) {
    apply(AppearancePreferences::default(), cx);
}

/// Project one application-owned appearance snapshot into the pinned UI stack.
pub(crate) fn apply(preferences: AppearancePreferences, cx: &mut App) {
    let palette = palette(preferences.scheme);
    let config = Rc::new(theme_config(preferences, palette));
    Theme::global_mut(cx).apply_config(&config);

    let tokens = {
        let mut tokens = cx.theme().semantic_tokens();
        let scale = f32::from(preferences.text_scale_percent) / 100.0;
        tokens.typography.xs.size *= scale;
        tokens.typography.xs.line_height *= scale;
        tokens.typography.sm.size *= scale;
        tokens.typography.sm.line_height *= scale;
        tokens.typography.md.size *= scale;
        tokens.typography.md.line_height *= scale;
        tokens.typography.lg.size *= scale;
        tokens.typography.lg.line_height *= scale;
        tokens.typography.xl.size *= scale;
        tokens.typography.xl.line_height *= scale;
        tokens.typography.mono_md.size *= scale;
        tokens.typography.mono_md.line_height *= scale;
        tokens
    };

    let theme = Theme::global_mut(cx);
    theme.apply_semantic_tokens(&tokens);
    theme.selection = parse_color(palette.selection);
    Theme::sync_base(cx);

    let scale = f32::from(preferences.text_scale_percent) / 100.0;
    let density_scale = match preferences.density {
        Density::Compact => 0.9,
        Density::Comfortable => 1.0,
    };
    let control_scale = scale * density_scale;
    gpui_ai::sizing::SizeTokens::default()
        .with_control_sm(gpui::px(24.0 * control_scale))
        .with_control_md(gpui::px(28.0 * control_scale))
        .with_control_lg(gpui::px(32.0 * control_scale))
        .with_slot_sm(gpui::px(16.0 * scale))
        .with_slot_md(gpui::px(20.0 * scale))
        .set(cx);
    MotionTokens::default()
        .with_preference(preferences.motion.preference())
        .set(cx);
    cx.set_global(VisualSystem {
        preferences,
        tokens,
    });
    cx.refresh_windows();
}

/// Resolve the subset of shared presentation settings that Desktop can
/// preview today. Unknown and automatic values preserve the client-owned
/// fallback instead of guessing operating-system state.
pub(crate) fn appearance_from_settings(
    snapshot: &DesktopSettingsSnapshot,
    fallback: AppearancePreferences,
) -> AppearancePreferences {
    let value = |key: &str| {
        snapshot
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.value.raw.as_deref())
    };
    let scheme = match value("appearance.theme") {
        Some("light") => ColorScheme::Light,
        Some("dark") => ColorScheme::Dark,
        Some("monochrome") => ColorScheme::HighContrast,
        _ => fallback.scheme,
    };
    let density = match value("appearance.density") {
        Some("compact") => Density::Compact,
        Some("comfortable") => Density::Comfortable,
        _ => fallback.density,
    };
    let motion = match value("appearance.motion") {
        Some("reduced") => MotionMode::Reduced,
        Some("full") => MotionMode::Full,
        _ => fallback.motion,
    };
    AppearancePreferences {
        scheme,
        density,
        motion,
        ..fallback
    }
}

fn palette(scheme: ColorScheme) -> Palette {
    match scheme {
        ColorScheme::Light => LIGHT,
        ColorScheme::Dark => DARK,
        ColorScheme::HighContrast => HIGH_CONTRAST,
    }
}

fn theme_config(preferences: AppearancePreferences, palette: Palette) -> ThemeConfig {
    let mode = if preferences.scheme == ColorScheme::Light {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    let mut colors = ThemeConfigColors::default();
    colors.background = Some(hex(palette.background));
    colors.foreground = Some(hex(palette.foreground));
    colors.popover = Some(hex(palette.surface));
    colors.popover_foreground = Some(hex(palette.surface_foreground));
    colors.primary = Some(hex(palette.primary));
    colors.primary_foreground = Some(hex(palette.primary_foreground));
    colors.secondary = Some(hex(palette.secondary));
    colors.secondary_foreground = Some(hex(palette.secondary_foreground));
    colors.muted = Some(hex(palette.muted));
    colors.muted_foreground = Some(hex(palette.muted_foreground));
    colors.accent = Some(hex(palette.accent));
    colors.accent_foreground = Some(hex(palette.accent_foreground));
    colors.danger = Some(hex(palette.destructive));
    colors.danger_foreground = Some(hex(palette.destructive_foreground));
    colors.border = Some(hex(palette.border));
    colors.input = Some(hex(palette.input));
    colors.ring = Some(hex(palette.ring));
    colors.success = Some(hex(palette.success));
    colors.warning = Some(hex(palette.warning));
    colors.info = Some(hex(palette.info));
    colors.selection = Some(palette.selection.into());
    colors.sidebar = Some(hex(palette.surface));
    colors.sidebar_foreground = Some(hex(palette.surface_foreground));
    colors.sidebar_border = Some(hex(palette.border));

    ThemeConfig {
        name: format!("Xana {}", preferences.scheme.label()).into(),
        mode,
        // Semantic typography is scaled once after this base configuration is
        // resolved. Keeping the seed unscaled avoids compounding 200% text.
        font_size: Some(BASE_FONT_SIZE),
        mono_font_size: Some(BASE_MONO_FONT_SIZE),
        radius: Some(if preferences.scheme == ColorScheme::HighContrast {
            3
        } else {
            7
        }),
        radius_lg: Some(if preferences.scheme == ColorScheme::HighContrast {
            4
        } else {
            10
        }),
        shadow: Some(preferences.scheme != ColorScheme::HighContrast),
        colors,
        ..ThemeConfig::default()
    }
}

fn hex(value: u32) -> SharedString {
    format!("#{value:06x}").into()
}

fn parse_color(value: &str) -> Hsla {
    gpui_component::try_parse_color(value).expect("static Xana color must parse")
}

#[cfg(test)]
fn contrast_ratio(first: u32, second: u32) -> f64 {
    let first = luminance(first);
    let second = luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

#[cfg(test)]
fn luminance(value: u32) -> f64 {
    let channel = |shift| {
        let component = f64::from((value >> shift) & 0xff_u32) / 255.0;
        if component <= 0.04045 {
            component / 12.92
        } else {
            ((component + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::rgb;
    use xana::desktop::{
        DesktopLocalizedText, DesktopSettingEffect, DesktopSettingEntry, DesktopSettingKind,
        DesktopSettingSource, DesktopSettingTarget, DesktopSettingValue, DesktopSettingsSection,
    };

    fn appearance_setting(key: &str, value: &str) -> DesktopSettingEntry {
        DesktopSettingEntry {
            key: key.to_owned(),
            section: DesktopSettingsSection::Appearance,
            label: DesktopLocalizedText {
                code: format!("settings.{key}.label"),
                fallback: key.to_owned(),
            },
            description: DesktopLocalizedText {
                code: format!("settings.{key}.description"),
                fallback: key.to_owned(),
            },
            value: DesktopSettingValue {
                raw: Some(value.to_owned()),
                display: value.to_owned(),
            },
            default: None,
            kind: DesktopSettingKind::Choice,
            choices: Vec::new(),
            source: DesktopSettingSource::PresentationFile,
            target: DesktopSettingTarget::MachinePresentation,
            effect: DesktopSettingEffect::Immediate,
            editable: true,
            focused_action: None,
            staged: true,
        }
    }

    #[test]
    fn every_palette_meets_text_contrast_contracts() {
        for palette in [LIGHT, DARK, HIGH_CONTRAST] {
            assert!(contrast_ratio(palette.background, palette.foreground) >= 7.0);
            assert!(contrast_ratio(palette.surface, palette.surface_foreground) >= 7.0);
            assert!(contrast_ratio(palette.primary, palette.primary_foreground) >= 4.5);
            assert!(contrast_ratio(palette.secondary, palette.secondary_foreground) >= 4.5);
            assert!(contrast_ratio(palette.accent, palette.accent_foreground) >= 4.5);
            assert!(contrast_ratio(palette.destructive, palette.destructive_foreground) >= 4.5);
            assert!(contrast_ratio(palette.background, palette.muted_foreground) >= 4.5);
        }
    }

    #[test]
    fn text_scale_is_bounded_and_preferences_have_explicit_modes() {
        assert_eq!(
            AppearancePreferences::default().with_text_scale(20),
            AppearancePreferences::default().with_text_scale(100)
        );
        assert_eq!(
            AppearancePreferences::default()
                .with_text_scale(500)
                .text_scale_percent,
            200
        );
        assert_eq!(ColorScheme::ALL.len(), 3);
        assert_eq!(Density::ALL.len(), 2);
        assert_eq!(MotionMode::ALL.len(), 3);
    }

    #[test]
    fn static_palette_colors_parse_through_the_pinned_theme_parser() {
        for scheme in ColorScheme::ALL {
            let palette = palette(scheme);
            for color in [
                palette.background,
                palette.foreground,
                palette.primary,
                palette.border,
                palette.ring,
            ] {
                let _: Hsla = parse_color(&hex(color));
            }
            let _: Hsla = parse_color(palette.selection);
        }
    }

    #[test]
    fn rgb_helper_matches_the_gpui_color_space() {
        assert_eq!(Hsla::from(rgb(0xffffff)).l, 1.0);
        assert_eq!(Hsla::from(rgb(0x000000)).l, 0.0);
    }

    #[test]
    fn staged_shared_settings_resolve_to_desktop_preview_preferences() {
        let snapshot = DesktopSettingsSnapshot {
            version: 1,
            revision: "preview".to_owned(),
            warnings: Vec::new(),
            entries: vec![
                appearance_setting("appearance.theme", "light"),
                appearance_setting("appearance.density", "compact"),
                appearance_setting("appearance.motion", "reduced"),
            ],
            truncated: false,
        };
        let resolved = appearance_from_settings(&snapshot, AppearancePreferences::default());
        assert_eq!(resolved.scheme, ColorScheme::Light);
        assert_eq!(resolved.density, Density::Compact);
        assert_eq!(resolved.motion, MotionMode::Reduced);
    }
}
