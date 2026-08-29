//! Shared typed settings catalog and transactional mutation.
//!
//! This module is the seam used by terminal clients and the future Desktop
//! adapter. It hides configuration/presentation file structure, validates
//! complete owner records, and commits staged changes without exposing secret
//! values. Renderers remain adapters over this interface.

use crate::{
    bounded_file,
    config::{ConfigError, ConfigTransactionLock, ConnectionRegistry, XanaConfig},
    paths::XanaPaths,
    presentation::{
        ActivityPaneChoice, ComposerPreset, DensityChoice, GlyphChoice, MotionChoice,
        PresentationPreferences, ThemeChoice,
    },
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_PREFERENCE_BYTES: usize = 32 * 1024;
const MAX_SETTING_VALUE_BYTES: usize = 4 * 1024;

const APPEARANCE_THEME: &str = "appearance.theme";
const APPEARANCE_GLYPHS: &str = "appearance.glyphs";
const APPEARANCE_MOTION: &str = "appearance.motion";
const APPEARANCE_DENSITY: &str = "appearance.density";
const APPEARANCE_COMPOSER: &str = "appearance.composer";
const APPEARANCE_ACTIVITY: &str = "appearance.activity";
const PROFILES_DEFAULT: &str = "profiles.default";
const PERMISSIONS_DEFAULT: &str = "permissions.default";
const EXECUTION_SHELL: &str = "execution.shell";
const EXECUTION_SHELL_PROGRAM: &str = "execution.shell_program";
const EXECUTION_DEFAULT_CHILD_ROUTE: &str = "execution.default_child_route";
const DIAGNOSTICS_ENABLED: &str = "diagnostics.enabled";
const DIAGNOSTICS_LEVEL: &str = "diagnostics.level";
const DIAGNOSTICS_RETENTION_DAYS: &str = "diagnostics.retention_days";
const DIAGNOSTICS_MAX_FILE_BYTES: &str = "diagnostics.max_file_bytes";
const DIAGNOSTICS_MAX_TOTAL_BYTES: &str = "diagnostics.max_total_bytes";
const DIAGNOSTICS_MAX_FILES: &str = "diagnostics.max_files";
const DIAGNOSTICS_QUEUE_CAPACITY: &str = "diagnostics.queue_capacity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingsSection {
    Overview,
    Appearance,
    Connections,
    Profiles,
    Permissions,
    Execution,
    Diagnostics,
    Integrations,
    Advanced,
}

impl SettingsSection {
    pub(crate) const fn all() -> [Self; 9] {
        [
            Self::Overview,
            Self::Appearance,
            Self::Connections,
            Self::Profiles,
            Self::Permissions,
            Self::Execution,
            Self::Diagnostics,
            Self::Integrations,
            Self::Advanced,
        ]
    }

    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Appearance => "appearance",
            Self::Connections => "connections",
            Self::Profiles => "profiles",
            Self::Permissions => "permissions",
            Self::Execution => "execution",
            Self::Diagnostics => "diagnostics",
            Self::Integrations => "integrations",
            Self::Advanced => "advanced",
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Appearance => "Appearance",
            Self::Connections => "Connections & Models",
            Self::Profiles => "Profiles & Routes",
            Self::Permissions => "Permissions",
            Self::Execution => "Execution",
            Self::Diagnostics => "Diagnostics",
            Self::Integrations => "Integrations",
            Self::Advanced => "Advanced",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Self::all().into_iter().find(|section| {
            section.id() == normalized
                || matches!(
                    (section, normalized.as_str()),
                    (Self::Connections, "models")
                        | (Self::Profiles, "routes")
                        | (Self::Permissions, "safety")
                        | (Self::Execution, "shell")
                )
        })
    }
}

impl fmt::Display for SettingsSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingKind {
    Boolean,
    Choice,
    Integer,
    Bytes,
    DurationDays,
    OptionalPath,
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingSource {
    BuiltInDefault,
    ConfigurationFile,
    PresentationFile,
    Derived,
}

impl SettingSource {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::BuiltInDefault => "Built-in default",
            Self::ConfigurationFile => "Configuration file",
            Self::PresentationFile => "Presentation preferences",
            Self::Derived => "Derived status",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingTarget {
    MachinePresentation,
    GlobalConfiguration,
    TaskSpecificManager,
}

impl SettingTarget {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::MachinePresentation => "This terminal frontend",
            Self::GlobalConfiguration => "Global Xana configuration",
            Self::TaskSpecificManager => "Task-specific manager",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingEffect {
    Immediate,
    NewConversation,
    NextLaunch,
    ManagedElsewhere,
}

impl SettingEffect {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Immediate => "Applies immediately",
            Self::NewConversation => "Applies to new conversations",
            Self::NextLaunch => "Applies on the next Xana launch",
            Self::ManagedElsewhere => "Managed by a focused workflow",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SettingValue {
    /// Canonical machine value. `None` means an explicit automatic/absent
    /// value, not a redacted secret.
    pub(crate) raw: Option<String>,
    pub(crate) display: String,
}

impl SettingValue {
    fn scalar(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            raw: Some(value.clone()),
            display: value,
        }
    }

    fn formatted(value: impl Into<String>, display: impl Into<String>) -> Self {
        Self {
            raw: Some(value.into()),
            display: display.into(),
        }
    }

    fn automatic(display: impl Into<String>) -> Self {
        Self {
            raw: None,
            display: display.into(),
        }
    }

    fn summary(display: impl Into<String>) -> Self {
        Self::automatic(display)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SettingEntry {
    pub(crate) key: String,
    pub(crate) section: SettingsSection,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) value: SettingValue,
    pub(crate) default: Option<SettingValue>,
    pub(crate) kind: SettingKind,
    pub(crate) choices: Vec<String>,
    pub(crate) source: SettingSource,
    pub(crate) target: SettingTarget,
    pub(crate) effect: SettingEffect,
    pub(crate) editable: bool,
    pub(crate) action: Option<String>,
    pub(crate) staged: bool,
}

impl SettingEntry {
    fn new(
        key: &str,
        section: SettingsSection,
        label: &str,
        description: &str,
        value: SettingValue,
    ) -> Self {
        Self {
            key: key.to_owned(),
            section,
            label: label.to_owned(),
            description: description.to_owned(),
            value,
            default: None,
            kind: SettingKind::ReadOnly,
            choices: Vec::new(),
            source: SettingSource::Derived,
            target: SettingTarget::TaskSpecificManager,
            effect: SettingEffect::ManagedElsewhere,
            editable: false,
            action: None,
            staged: false,
        }
    }

    fn editable(
        mut self,
        kind: SettingKind,
        default: SettingValue,
        source: SettingSource,
        target: SettingTarget,
        effect: SettingEffect,
    ) -> Self {
        self.kind = kind;
        self.default = Some(default);
        self.source = source;
        self.target = target;
        self.effect = effect;
        self.editable = true;
        self
    }

    fn choices(mut self, choices: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.choices = choices.into_iter().map(Into::into).collect();
        self
    }

    fn action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SettingsSnapshot {
    pub(crate) revision: String,
    pub(crate) warnings: Vec<String>,
    pub(crate) entries: Vec<SettingEntry>,
}

impl SettingsSnapshot {
    pub(crate) fn entry(&self, key: &str) -> Option<&SettingEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    pub(crate) fn entries_in(&self, section: SettingsSection) -> Vec<&SettingEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.section == section)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SettingChange {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) before: SettingValue,
    pub(crate) after: SettingValue,
    pub(crate) target: SettingTarget,
    pub(crate) effect: SettingEffect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SettingsReceipt {
    pub(crate) dry_run: bool,
    pub(crate) revision_before: String,
    pub(crate) revision_after: String,
    pub(crate) changes: Vec<SettingChange>,
}

impl SettingsReceipt {
    pub(crate) fn requires_new_conversation(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.effect == SettingEffect::NewConversation)
    }
}

#[derive(Debug)]
pub(crate) enum SettingsError {
    Config(ConfigError),
    Io { path: PathBuf, source: io::Error },
    InvalidPresentation(String),
    UnknownSection(String),
    UnknownKey(String),
    ReadOnly { key: String, action: Option<String> },
    InvalidValue { key: String, reason: String },
    ConcurrentChange { owner: &'static str },
    Commit(String),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(f, "{error}"),
            Self::Io { path, source } => write!(f, "could not access {}: {source}", path.display()),
            Self::InvalidPresentation(reason) => {
                write!(f, "invalid presentation preferences: {reason}")
            }
            Self::UnknownSection(section) => write!(
                f,
                "unknown settings section {section:?}; expected {}",
                SettingsSection::all().map(SettingsSection::id).join(", ")
            ),
            Self::UnknownKey(key) => write!(
                f,
                "unknown setting {key:?}; run `xana config list` to inspect stable keys"
            ),
            Self::ReadOnly { key, action } => {
                write!(f, "setting {key:?} is managed by a focused workflow")?;
                if let Some(action) = action {
                    write!(f, "; use `{action}`")?;
                }
                Ok(())
            }
            Self::InvalidValue { key, reason } => {
                write!(f, "invalid value for {key:?}: {reason}")
            }
            Self::ConcurrentChange { owner } => write!(
                f,
                "{owner} changed while settings were open; reload before applying"
            ),
            Self::Commit(reason) => write!(f, "could not commit settings: {reason}"),
        }
    }
}

impl Error for SettingsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ConfigError> for SettingsError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

#[derive(Clone)]
pub(crate) struct SettingsManager {
    paths: XanaPaths,
}

impl SettingsManager {
    pub(crate) fn new(paths: &XanaPaths) -> Self {
        Self {
            paths: paths.clone(),
        }
    }

    pub(crate) fn snapshot(&self) -> Result<SettingsSnapshot, SettingsError> {
        let sources = SourceState::load(&self.paths)?;
        build_snapshot(&self.paths, &sources, &BTreeSet::new())
    }

    pub(crate) fn begin(&self) -> Result<SettingsDraft, SettingsError> {
        let base = SourceState::load(&self.paths)?;
        build_snapshot(&self.paths, &base, &BTreeSet::new())?;
        Ok(SettingsDraft {
            paths: self.paths.clone(),
            base,
            changes: BTreeMap::new(),
        })
    }

    pub(crate) fn commit(
        &self,
        draft: &SettingsDraft,
        dry_run: bool,
    ) -> Result<SettingsReceipt, SettingsError> {
        if draft.paths != self.paths {
            return Err(SettingsError::Commit(
                "draft belongs to a different Xana home".to_owned(),
            ));
        }
        let rendered = draft.rendered()?;
        let changes = changes_between(&self.paths, &draft.base, &rendered, &draft.staged_keys())?;
        let receipt = SettingsReceipt {
            dry_run,
            revision_before: revision(&draft.base),
            revision_after: revision(&rendered),
            changes,
        };
        if dry_run || receipt.changes.is_empty() {
            return Ok(receipt);
        }

        let _lock = ConfigTransactionLock::acquire(self.paths.config_file())?;
        let current = SourceState::load(&self.paths)?;
        if current.config != draft.base.config {
            return Err(SettingsError::ConcurrentChange {
                owner: "config.toml",
            });
        }
        if current.presentation != draft.base.presentation {
            return Err(SettingsError::ConcurrentChange {
                owner: "presentation preferences",
            });
        }

        install_sources(&self.paths, &draft.base, &rendered)?;
        Ok(receipt)
    }
}

pub(crate) struct SettingsDraft {
    paths: XanaPaths,
    base: SourceState,
    changes: BTreeMap<String, DraftChange>,
}

impl SettingsDraft {
    pub(crate) fn set(&mut self, key: &str, value: &str) -> Result<(), SettingsError> {
        if value.len() > MAX_SETTING_VALUE_BYTES {
            return Err(SettingsError::InvalidValue {
                key: key.to_owned(),
                reason: format!("value exceeds {MAX_SETTING_VALUE_BYTES} bytes"),
            });
        }
        let snapshot = self.preview()?;
        let entry = editable_entry(&snapshot, key)?;
        let value = canonicalize(entry, value)?;
        let key = entry.key.clone();
        let previous = self.changes.insert(key.clone(), DraftChange::Set(value));
        if let Err(error) = self.rendered() {
            restore_staged_change(&mut self.changes, key, previous);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn reset(&mut self, key: &str) -> Result<(), SettingsError> {
        let snapshot = self.preview()?;
        let entry = editable_entry(&snapshot, key)?;
        if entry.default.is_none() {
            return Err(SettingsError::ReadOnly {
                key: entry.key.clone(),
                action: entry.action.clone(),
            });
        }
        let key = entry.key.clone();
        let previous = self.changes.insert(key.clone(), DraftChange::Reset);
        if let Err(error) = self.rendered() {
            restore_staged_change(&mut self.changes, key, previous);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn revert(&mut self, key: &str) -> bool {
        self.changes.remove(key).is_some()
    }

    pub(crate) fn pending_count(&self) -> Result<usize, SettingsError> {
        Ok(self.pending_changes()?.len())
    }

    pub(crate) fn pending_changes(&self) -> Result<Vec<SettingChange>, SettingsError> {
        let rendered = self.rendered()?;
        changes_between(&self.paths, &self.base, &rendered, &self.staged_keys())
    }

    pub(crate) fn preview(&self) -> Result<SettingsSnapshot, SettingsError> {
        let rendered = self.rendered()?;
        build_snapshot(&self.paths, &rendered, &self.staged_keys())
    }

    fn rendered(&self) -> Result<SourceState, SettingsError> {
        render_sources(&self.base, &self.changes)
    }

    fn staged_keys(&self) -> BTreeSet<String> {
        self.changes.keys().cloned().collect()
    }
}

#[derive(Clone)]
enum DraftChange {
    Set(String),
    Reset,
}

fn restore_staged_change(
    changes: &mut BTreeMap<String, DraftChange>,
    key: String,
    previous: Option<DraftChange>,
) {
    match previous {
        Some(previous) => {
            changes.insert(key, previous);
        }
        None => {
            changes.remove(&key);
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SourceState {
    config: Vec<u8>,
    presentation: Option<Vec<u8>>,
}

impl SourceState {
    fn load(paths: &XanaPaths) -> Result<Self, SettingsError> {
        Ok(Self {
            config: read_required(paths.config_file(), MAX_CONFIG_BYTES)?,
            presentation: read_optional(&paths.presentation_file(), MAX_PREFERENCE_BYTES)?,
        })
    }
}

fn read_required(path: &Path, limit: usize) -> Result<Vec<u8>, SettingsError> {
    bounded_file::read(path, limit).map_err(|error| map_bounded_error(path, error))
}

fn read_optional(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, SettingsError> {
    match bounded_file::read(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(bounded_file::BoundedReadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(map_bounded_error(path, error)),
    }
}

fn map_bounded_error(path: &Path, error: bounded_file::BoundedReadError) -> SettingsError {
    match error {
        bounded_file::BoundedReadError::Io { source, .. } => SettingsError::Io {
            path: path.to_owned(),
            source,
        },
        bounded_file::BoundedReadError::TooLarge { actual, limit, .. } => {
            SettingsError::InvalidValue {
                key: path.display().to_string(),
                reason: format!("record is {actual} bytes; limit is {limit}"),
            }
        }
    }
}

fn build_snapshot(
    paths: &XanaPaths,
    sources: &SourceState,
    staged: &BTreeSet<String>,
) -> Result<SettingsSnapshot, SettingsError> {
    let config_text =
        std::str::from_utf8(&sources.config).map_err(|error| SettingsError::InvalidValue {
            key: "config.toml".to_owned(),
            reason: error.to_string(),
        })?;
    let registry = XanaConfig::parse_registry(config_text)?;
    let resolved = XanaConfig::parse(config_text)?;
    let document = config_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| SettingsError::Config(ConfigError::Edit(error.to_string())))?;
    let (preferences, preference_warning, valid_preference_file) =
        parse_preferences(sources.presentation.as_deref());
    let preference_source = if valid_preference_file {
        SettingSource::PresentationFile
    } else {
        SettingSource::BuiltInDefault
    };

    let mut entries = Vec::new();
    entries.push(SettingEntry::new(
        "overview.health",
        SettingsSection::Overview,
        "Configuration health",
        "Both durable owners were bounded, decoded, and completely validated.",
        SettingValue::summary("Ready"),
    ));
    let default_profile = registry
        .profiles
        .get(&registry.default_profile)
        .expect("configuration validation requires a default profile");
    entries.push(SettingEntry::new(
        "overview.active_default",
        SettingsSection::Overview,
        "Default conversation",
        "The global default used when no frozen conversation profile or model selection overrides it.",
        SettingValue::summary(format!(
            "{} · {}/{}",
            registry.default_profile, default_profile.connection, default_profile.model
        )),
    ));
    entries.push(
        SettingEntry::new(
            "overview.managers",
            SettingsSection::Overview,
            "Configuration managers",
            "Complex configuration remains in typed focused workflows.",
            SettingValue::summary(format!(
                "{} connections · {} profiles · {} routes",
                registry.connections.len(),
                registry.profiles.len(),
                registry.routes.len()
            )),
        )
        .action("xana connect"),
    );

    entries.extend(appearance_entries(&preferences, preference_source));
    entries.extend(connection_entries(&registry));
    entries.extend(profile_entries(&registry, &document));
    entries.extend(permission_entries(&registry));
    entries.extend(execution_entries(&registry, &resolved, &document));
    entries.extend(diagnostic_entries(&registry, &document));
    entries.extend(integration_entries(&registry));
    entries.extend(advanced_entries(paths));

    for entry in &mut entries {
        entry.staged = staged.contains(&entry.key);
    }

    let mut warnings = Vec::new();
    if let Some(warning) = preference_warning {
        warnings.push(warning);
    }
    Ok(SettingsSnapshot {
        revision: revision(sources),
        warnings,
        entries,
    })
}

fn appearance_entries(
    preferences: &PresentationPreferences,
    source: SettingSource,
) -> Vec<SettingEntry> {
    let target = SettingTarget::MachinePresentation;
    let effect = SettingEffect::Immediate;
    vec![
        SettingEntry::new(
            APPEARANCE_THEME,
            SettingsSection::Appearance,
            "Theme",
            "Select terminal colors or follow the detected background.",
            SettingValue::scalar(theme_name(preferences.theme)),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("auto"),
            source,
            target,
            effect,
        )
        .choices(["auto", "dark", "light", "monochrome"]),
        SettingEntry::new(
            APPEARANCE_GLYPHS,
            SettingsSection::Appearance,
            "Glyphs",
            "Choose Unicode decoration, ASCII fallbacks, or terminal detection.",
            SettingValue::scalar(glyph_name(preferences.glyphs)),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("auto"),
            source,
            target,
            effect,
        )
        .choices(["auto", "unicode", "ascii"]),
        SettingEntry::new(
            APPEARANCE_MOTION,
            SettingsSection::Appearance,
            "Motion",
            "Use full motion, reduced motion, or the operating-system preference.",
            SettingValue::scalar(motion_name(preferences.motion)),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("auto"),
            source,
            target,
            effect,
        )
        .choices(["auto", "full", "reduced"]),
        SettingEntry::new(
            APPEARANCE_DENSITY,
            SettingsSection::Appearance,
            "Density",
            "Choose comfortable or compact spacing, or let Xana adapt.",
            SettingValue::scalar(density_name(preferences.density)),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("auto"),
            source,
            target,
            effect,
        )
        .choices(["auto", "comfortable", "compact"]),
        SettingEntry::new(
            APPEARANCE_COMPOSER,
            SettingsSection::Appearance,
            "Enter key",
            "Choose whether Enter submits or inserts a newline in the TUI composer.",
            SettingValue::scalar(composer_name(preferences.composer)),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("submit"),
            source,
            target,
            effect,
        )
        .choices(["submit", "newline"]),
        SettingEntry::new(
            APPEARANCE_ACTIVITY,
            SettingsSection::Appearance,
            "Activity pane",
            "Open, hide, or automatically reveal runtime activity and approvals.",
            SettingValue::scalar(activity_name(preferences.activity)),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("auto"),
            source,
            target,
            effect,
        )
        .choices(["auto", "open", "hidden"]),
    ]
}

fn connection_entries(registry: &ConnectionRegistry) -> Vec<SettingEntry> {
    let kinds = registry
        .connections
        .values()
        .map(|connection| connection.kind.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    vec![
        SettingEntry::new(
            "connections.manage",
            SettingsSection::Connections,
            "Connections",
            "Add, remove, authenticate, test, and inspect provider or managed-runtime connections.",
            SettingValue::summary(format!(
                "{} configured{}",
                registry.connections.len(),
                if kinds.is_empty() {
                    String::new()
                } else {
                    format!(" · {kinds}")
                }
            )),
        )
        .action("xana connection list"),
        SettingEntry::new(
            "connections.models",
            SettingsSection::Connections,
            "Models",
            "Refresh connection-owned catalogs and select the next-conversation model.",
            SettingValue::summary("Managed separately from config.toml"),
        )
        .action("xana model list"),
    ]
}

fn profile_entries(
    registry: &ConnectionRegistry,
    document: &toml_edit::DocumentMut,
) -> Vec<SettingEntry> {
    let profiles = registry.profiles.keys().cloned().collect::<Vec<_>>();
    vec![
        SettingEntry::new(
            PROFILES_DEFAULT,
            SettingsSection::Profiles,
            "Default profile",
            "Select the global profile resolved for new conversations.",
            SettingValue::scalar(&registry.default_profile),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("default"),
            source_for_top_level(document, "default_profile"),
            SettingTarget::GlobalConfiguration,
            SettingEffect::NewConversation,
        )
        .choices(profiles),
        SettingEntry::new(
            "profiles.manage",
            SettingsSection::Profiles,
            "Profiles",
            "Create, duplicate, compare, archive, resolve, and freeze complete profiles.",
            SettingValue::summary(format!("{} configured", registry.profiles.len())),
        )
        .action("xana profile list"),
        SettingEntry::new(
            "profiles.routes",
            SettingsSection::Profiles,
            "Task routes",
            "Inspect exact child profile routing and readiness without starting work.",
            SettingValue::summary(format!("{} configured", registry.routes.len())),
        )
        .action("xana route list"),
    ]
}

fn permission_entries(registry: &ConnectionRegistry) -> Vec<SettingEntry> {
    vec![
        SettingEntry::new(
            PERMISSIONS_DEFAULT,
            SettingsSection::Permissions,
            "Default decision",
            "The global fallback when no narrower permission rule matches.",
            SettingValue::scalar(registry.permission_mode.as_str()),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("ask"),
            SettingSource::ConfigurationFile,
            SettingTarget::GlobalConfiguration,
            SettingEffect::NewConversation,
        )
        .choices(["deny", "ask", "allow"]),
        SettingEntry::new(
            "permissions.rules",
            SettingsSection::Permissions,
            "Permission rules",
            "Ordered typed rules require a focused builder because tool, effect, and workspace scopes interact.",
            SettingValue::summary(format!("{} configured", registry.permission_rules.len())),
        )
        .action("xana setup --section permissions-shell"),
    ]
}

fn execution_entries(
    registry: &ConnectionRegistry,
    resolved: &XanaConfig,
    document: &toml_edit::DocumentMut,
) -> Vec<SettingEntry> {
    let shell_choices = supported_shell_choices();
    let program = resolved.shell.program.as_ref().map_or_else(
        || SettingValue::automatic("Automatic"),
        |path| SettingValue::formatted(path.display().to_string(), path.display().to_string()),
    );
    let route = registry
        .default_child_route
        .as_ref()
        .map_or_else(|| SettingValue::automatic("None"), SettingValue::scalar);
    let mut route_choices = vec!["none".to_owned()];
    route_choices.extend(registry.routes.keys().cloned());
    vec![
        SettingEntry::new(
            EXECUTION_SHELL,
            SettingsSection::Execution,
            "Command shell",
            "Select the explicit platform-aware shell used by run_command.",
            SettingValue::scalar(resolved.shell.kind.config_name()),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("platform"),
            source_for_nested(document, "shell", "kind"),
            SettingTarget::GlobalConfiguration,
            SettingEffect::NewConversation,
        )
        .choices(shell_choices),
        SettingEntry::new(
            EXECUTION_SHELL_PROGRAM,
            SettingsSection::Execution,
            "Shell executable",
            "Optionally replace the selected shell's normal executable without invoking a shell to find it.",
            program,
        )
        .editable(
            SettingKind::OptionalPath,
            SettingValue::automatic("Automatic"),
            source_for_nested(document, "shell", "program"),
            SettingTarget::GlobalConfiguration,
            SettingEffect::NewConversation,
        ),
        SettingEntry::new(
            EXECUTION_DEFAULT_CHILD_ROUTE,
            SettingsSection::Execution,
            "Default child route",
            "Select the exact task route used when orchestration does not name one.",
            route,
        )
        .editable(
            SettingKind::Choice,
            SettingValue::automatic("None"),
            source_for_top_level(document, "default_child_route"),
            SettingTarget::GlobalConfiguration,
            SettingEffect::NewConversation,
        )
        .choices(route_choices),
    ]
}

fn diagnostic_entries(
    registry: &ConnectionRegistry,
    document: &toml_edit::DocumentMut,
) -> Vec<SettingEntry> {
    let diagnostics = &registry.diagnostics;
    let target = SettingTarget::GlobalConfiguration;
    let effect = SettingEffect::NextLaunch;
    vec![
        SettingEntry::new(
            DIAGNOSTICS_ENABLED,
            SettingsSection::Diagnostics,
            "Diagnostic logging",
            "Record bounded metadata-only diagnostic facts for future launches.",
            SettingValue::scalar(diagnostics.enabled.to_string()),
        )
        .editable(
            SettingKind::Boolean,
            SettingValue::scalar("true"),
            source_for_nested(document, "diagnostics", "enabled"),
            target,
            effect,
        ),
        SettingEntry::new(
            DIAGNOSTICS_LEVEL,
            SettingsSection::Diagnostics,
            "Minimum level",
            "Retain events at this severity and above.",
            SettingValue::scalar(diagnostic_level_name(diagnostics.level)),
        )
        .editable(
            SettingKind::Choice,
            SettingValue::scalar("info"),
            source_for_nested(document, "diagnostics", "level"),
            target,
            effect,
        )
        .choices(["error", "warn", "info", "debug", "trace"]),
        SettingEntry::new(
            DIAGNOSTICS_RETENTION_DAYS,
            SettingsSection::Diagnostics,
            "Retention",
            "Remove recognized diagnostic files older than this many days.",
            SettingValue::formatted(
                diagnostics.retention_days.to_string(),
                format!("{} days", diagnostics.retention_days),
            ),
        )
        .editable(
            SettingKind::DurationDays,
            SettingValue::formatted("7", "7 days"),
            source_for_nested(document, "diagnostics", "retention_days"),
            target,
            effect,
        ),
        SettingEntry::new(
            DIAGNOSTICS_MAX_FILE_BYTES,
            SettingsSection::Diagnostics,
            "File limit",
            "Rotate one diagnostic log before it exceeds this bound.",
            SettingValue::formatted(
                diagnostics.max_file_bytes.to_string(),
                format_bytes(diagnostics.max_file_bytes),
            ),
        )
        .editable(
            SettingKind::Bytes,
            SettingValue::formatted((4 * 1024 * 1024).to_string(), "4 MiB"),
            source_for_nested(document, "diagnostics", "max_file_bytes"),
            target,
            effect,
        ),
        SettingEntry::new(
            DIAGNOSTICS_MAX_TOTAL_BYTES,
            SettingsSection::Diagnostics,
            "Total storage limit",
            "Bound aggregate retained diagnostic storage.",
            SettingValue::formatted(
                diagnostics.max_total_bytes.to_string(),
                format_bytes(diagnostics.max_total_bytes),
            ),
        )
        .editable(
            SettingKind::Bytes,
            SettingValue::formatted((32 * 1024 * 1024).to_string(), "32 MiB"),
            source_for_nested(document, "diagnostics", "max_total_bytes"),
            target,
            effect,
        ),
        SettingEntry::new(
            DIAGNOSTICS_MAX_FILES,
            SettingsSection::Diagnostics,
            "File count",
            "Bound the number of recognized diagnostic files retained.",
            SettingValue::scalar(diagnostics.max_files.to_string()),
        )
        .editable(
            SettingKind::Integer,
            SettingValue::scalar("32"),
            source_for_nested(document, "diagnostics", "max_files"),
            target,
            effect,
        ),
        SettingEntry::new(
            DIAGNOSTICS_QUEUE_CAPACITY,
            SettingsSection::Diagnostics,
            "Queue capacity",
            "Bound diagnostic observations waiting for the background writer.",
            SettingValue::scalar(diagnostics.queue_capacity.to_string()),
        )
        .editable(
            SettingKind::Integer,
            SettingValue::scalar("1024"),
            source_for_nested(document, "diagnostics", "queue_capacity"),
            target,
            effect,
        ),
        SettingEntry::new(
            "diagnostics.manage",
            SettingsSection::Diagnostics,
            "Health and support",
            "Inspect local health, probe connections explicitly, or export a redacted support bundle.",
            SettingValue::summary("Read-only by default"),
        )
        .action("xana doctor"),
    ]
}

fn integration_entries(registry: &ConnectionRegistry) -> Vec<SettingEntry> {
    vec![
        SettingEntry::new(
            "integrations.plugins",
            SettingsSection::Integrations,
            "Agent Plugins",
            "Review, install, enable, update, roll back, and remove exact package revisions.",
            SettingValue::summary(format!("{} declared", registry.plugins.len())),
        )
        .action("xana plugin list"),
        SettingEntry::new(
            "integrations.mcp",
            SettingsSection::Integrations,
            "MCP servers",
            "Configure and inspect profile-allowlisted MCP servers and primitives.",
            SettingValue::summary(format!("{} configured", registry.mcp_servers.len())),
        )
        .action("xana mcp list"),
        SettingEntry::new(
            "integrations.external_agents",
            SettingsSection::Integrations,
            "External agents",
            "Refresh, review, trust, and delegate to exact A2A identities.",
            SettingValue::summary(format!("{} configured", registry.external_agents.len())),
        )
        .action("xana external-agent list"),
        SettingEntry::new(
            "integrations.focused_routes",
            SettingsSection::Integrations,
            "Image and vision routes",
            "Manage focused service connections separately from conversational providers.",
            SettingValue::summary(format!("{} configured", registry.service_routes.len())),
        )
        .action("xana image list"),
    ]
}

fn advanced_entries(paths: &XanaPaths) -> Vec<SettingEntry> {
    vec![
        SettingEntry::new(
            "advanced.config_path",
            SettingsSection::Advanced,
            "Configuration file",
            "The human-authored global configuration owner.",
            SettingValue::summary(paths.config_file().display().to_string()),
        )
        .action("xana config edit"),
        SettingEntry::new(
            "advanced.presentation_path",
            SettingsSection::Advanced,
            "Presentation preferences",
            "The machine-local terminal presentation owner.",
            SettingValue::summary(paths.presentation_file().display().to_string()),
        )
        .action("xana setup --section appearance"),
        SettingEntry::new(
            "advanced.migration",
            SettingsSection::Advanced,
            "Configuration migration",
            "Preview schema and private-state migration before applying it explicitly.",
            SettingValue::summary("Review required"),
        )
        .action("xana config migrate"),
    ]
}

fn parse_preferences(source: Option<&[u8]>) -> (PresentationPreferences, Option<String>, bool) {
    let Some(source) = source else {
        return (PresentationPreferences::default(), None, false);
    };
    let parsed = std::str::from_utf8(source)
        .map_err(|error| error.to_string())
        .and_then(|text| PresentationPreferences::parse(text).map_err(|error| error.to_string()));
    match parsed {
        Ok(preferences) => (preferences, None, true),
        Err(reason) => (
            PresentationPreferences::default(),
            Some(format!(
                "presentation preferences are invalid ({reason}); staged appearance changes will replace them with safe defaults"
            )),
            false,
        ),
    }
}

fn source_for_top_level(document: &toml_edit::DocumentMut, key: &str) -> SettingSource {
    if document.get(key).is_some() {
        SettingSource::ConfigurationFile
    } else {
        SettingSource::BuiltInDefault
    }
}

fn source_for_nested(document: &toml_edit::DocumentMut, table: &str, key: &str) -> SettingSource {
    if document
        .get(table)
        .and_then(toml_edit::Item::as_table)
        .is_some_and(|table| table.contains_key(key))
    {
        SettingSource::ConfigurationFile
    } else {
        SettingSource::BuiltInDefault
    }
}

fn editable_entry<'a>(
    snapshot: &'a SettingsSnapshot,
    key: &str,
) -> Result<&'a SettingEntry, SettingsError> {
    let entry = snapshot
        .entry(key.trim())
        .ok_or_else(|| SettingsError::UnknownKey(key.trim().to_owned()))?;
    if !entry.editable {
        return Err(SettingsError::ReadOnly {
            key: entry.key.clone(),
            action: entry.action.clone(),
        });
    }
    Ok(entry)
}

fn canonicalize(entry: &SettingEntry, value: &str) -> Result<String, SettingsError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_value(
            entry,
            "value cannot be blank; use reset for an automatic value",
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_value(
            entry,
            "value cannot contain control characters",
        ));
    }
    let parsed = match entry.kind {
        SettingKind::Boolean => parse_boolean(value).map(str::to_owned),
        SettingKind::Choice => parse_choice(value, &entry.choices),
        SettingKind::Integer => value
            .parse::<u64>()
            .map(|value| value.to_string())
            .map_err(|_| "expected a non-negative integer".to_owned()),
        SettingKind::Bytes => parse_bytes(value).map(|value| value.to_string()),
        SettingKind::DurationDays => parse_days(value).map(|value| value.to_string()),
        SettingKind::OptionalPath => Ok(value.to_owned()),
        SettingKind::ReadOnly => Err("setting is read-only".to_owned()),
    };
    parsed.map_err(|reason| invalid_value(entry, reason))
}

fn invalid_value(entry: &SettingEntry, reason: impl Into<String>) -> SettingsError {
    SettingsError::InvalidValue {
        key: entry.key.clone(),
        reason: reason.into(),
    }
}

fn parse_boolean(value: &str) -> Result<&'static str, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok("true"),
        "false" | "no" | "off" | "0" => Ok("false"),
        _ => Err("expected true/false, yes/no, on/off, or 1/0".to_owned()),
    }
}

fn parse_choice(value: &str, choices: &[String]) -> Result<String, String> {
    if let Some(exact) = choices.iter().find(|choice| choice.as_str() == value) {
        return Ok(exact.clone());
    }
    let folded = value.to_ascii_lowercase();
    let matches = choices
        .iter()
        .filter(|choice| choice.to_ascii_lowercase() == folded)
        .collect::<Vec<_>>();
    if let [choice] = matches.as_slice() {
        return Ok((*choice).clone());
    }
    Err(format!("expected one of {}", choices.join(", ")))
}

fn parse_days(value: &str) -> Result<u64, String> {
    let folded = value.trim().to_ascii_lowercase();
    let number = folded
        .strip_suffix("days")
        .or_else(|| folded.strip_suffix("day"))
        .or_else(|| folded.strip_suffix('d'))
        .unwrap_or(&folded)
        .trim();
    number
        .parse::<u64>()
        .map_err(|_| "expected a whole number of days such as 7 or 30d".to_owned())
}

fn parse_bytes(value: &str) -> Result<u64, String> {
    let normalized = value.trim().to_ascii_lowercase().replace([' ', '_'], "");
    let suffixes = [
        ("gib", 1024_u64.pow(3)),
        ("mib", 1024_u64.pow(2)),
        ("kib", 1024_u64),
        ("gb", 1000_u64.pow(3)),
        ("mb", 1000_u64.pow(2)),
        ("kb", 1000_u64),
        ("b", 1),
    ];
    for (suffix, multiplier) in suffixes {
        if let Some(number) = normalized.strip_suffix(suffix) {
            return number
                .parse::<u64>()
                .ok()
                .and_then(|number| number.checked_mul(multiplier))
                .ok_or_else(|| "byte value is invalid or too large".to_owned());
        }
    }
    normalized
        .parse::<u64>()
        .map_err(|_| "expected bytes such as 65536, 4MiB, or 32MB".to_owned())
}

fn render_sources(
    base: &SourceState,
    changes: &BTreeMap<String, DraftChange>,
) -> Result<SourceState, SettingsError> {
    let config_changed = changes.keys().any(|key| is_config_key(key));
    let presentation_changed = changes.keys().any(|key| is_presentation_key(key));
    let mut config = base.config.clone();
    let mut presentation = base.presentation.clone();

    if config_changed {
        let source =
            std::str::from_utf8(&base.config).map_err(|error| SettingsError::InvalidValue {
                key: "config.toml".to_owned(),
                reason: error.to_string(),
            })?;
        let migrated = XanaConfig::migrate_to_current(source)?;
        let mut document = migrated
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| SettingsError::Config(ConfigError::Edit(error.to_string())))?;
        for (key, change) in changes {
            if is_config_key(key) {
                apply_config_change(&mut document, key, change)?;
            }
        }
        let rendered = document.to_string();
        XanaConfig::parse_registry(&rendered)?;
        config = rendered.into_bytes();
    }

    if presentation_changed {
        let (mut preferences, _, _) = parse_preferences(base.presentation.as_deref());
        for (key, change) in changes {
            if is_presentation_key(key) {
                apply_presentation_change(&mut preferences, key, change)?;
            }
        }
        let rendered = preferences
            .render()
            .map_err(|error| SettingsError::InvalidPresentation(error.to_string()))?;
        PresentationPreferences::parse(&rendered)
            .map_err(|error| SettingsError::InvalidPresentation(error.to_string()))?;
        presentation = Some(rendered.into_bytes());
    }

    Ok(SourceState {
        config,
        presentation,
    })
}

fn is_presentation_key(key: &str) -> bool {
    key.starts_with("appearance.")
}

fn is_config_key(key: &str) -> bool {
    matches!(
        key,
        PROFILES_DEFAULT
            | PERMISSIONS_DEFAULT
            | EXECUTION_SHELL
            | EXECUTION_SHELL_PROGRAM
            | EXECUTION_DEFAULT_CHILD_ROUTE
            | DIAGNOSTICS_ENABLED
            | DIAGNOSTICS_LEVEL
            | DIAGNOSTICS_RETENTION_DAYS
            | DIAGNOSTICS_MAX_FILE_BYTES
            | DIAGNOSTICS_MAX_TOTAL_BYTES
            | DIAGNOSTICS_MAX_FILES
            | DIAGNOSTICS_QUEUE_CAPACITY
    )
}

fn apply_presentation_change(
    preferences: &mut PresentationPreferences,
    key: &str,
    change: &DraftChange,
) -> Result<(), SettingsError> {
    let value = change_value(change);
    match key {
        APPEARANCE_THEME => {
            preferences.theme = match value.unwrap_or("auto") {
                "auto" => ThemeChoice::Auto,
                "dark" => ThemeChoice::Dark,
                "light" => ThemeChoice::Light,
                "monochrome" => ThemeChoice::Monochrome,
                other => return Err(internal_invalid(key, other)),
            };
        }
        APPEARANCE_GLYPHS => {
            preferences.glyphs = match value.unwrap_or("auto") {
                "auto" => GlyphChoice::Auto,
                "unicode" => GlyphChoice::Unicode,
                "ascii" => GlyphChoice::Ascii,
                other => return Err(internal_invalid(key, other)),
            };
        }
        APPEARANCE_MOTION => {
            preferences.motion = match value.unwrap_or("auto") {
                "auto" => MotionChoice::Auto,
                "full" => MotionChoice::Full,
                "reduced" => MotionChoice::Reduced,
                other => return Err(internal_invalid(key, other)),
            };
        }
        APPEARANCE_DENSITY => {
            preferences.density = match value.unwrap_or("auto") {
                "auto" => DensityChoice::Auto,
                "comfortable" => DensityChoice::Comfortable,
                "compact" => DensityChoice::Compact,
                other => return Err(internal_invalid(key, other)),
            };
        }
        APPEARANCE_COMPOSER => {
            preferences.composer = match value.unwrap_or("submit") {
                "submit" => ComposerPreset::Submit,
                "newline" => ComposerPreset::Newline,
                other => return Err(internal_invalid(key, other)),
            };
        }
        APPEARANCE_ACTIVITY => {
            preferences.activity = match value.unwrap_or("auto") {
                "auto" => ActivityPaneChoice::Auto,
                "open" => ActivityPaneChoice::Open,
                "hidden" => ActivityPaneChoice::Hidden,
                other => return Err(internal_invalid(key, other)),
            };
        }
        _ => return Err(SettingsError::UnknownKey(key.to_owned())),
    }
    Ok(())
}

fn apply_config_change(
    document: &mut toml_edit::DocumentMut,
    key: &str,
    change: &DraftChange,
) -> Result<(), SettingsError> {
    let value = change_value(change);
    match key {
        PROFILES_DEFAULT => {
            document["default_profile"] = toml_edit::value(value.unwrap_or("default"));
        }
        PERMISSIONS_DEFAULT => {
            document["permission_mode"] = toml_edit::value(value.unwrap_or("ask"));
        }
        EXECUTION_SHELL => {
            let shell = ensure_table(document, "shell")?;
            match value {
                Some(value) => shell["kind"] = toml_edit::value(value),
                None => {
                    shell.remove("kind");
                }
            }
        }
        EXECUTION_SHELL_PROGRAM => {
            let shell = ensure_table(document, "shell")?;
            match value {
                Some(value) => shell["program"] = toml_edit::value(value),
                None => {
                    shell.remove("program");
                }
            }
        }
        EXECUTION_DEFAULT_CHILD_ROUTE => match value {
            Some("none") | None => {
                document.remove("default_child_route");
            }
            Some(value) => document["default_child_route"] = toml_edit::value(value),
        },
        DIAGNOSTICS_ENABLED => {
            ensure_table(document, "diagnostics")?["enabled"] =
                toml_edit::value(value.unwrap_or("true") == "true");
        }
        DIAGNOSTICS_LEVEL => {
            let diagnostics = ensure_table(document, "diagnostics")?;
            match value {
                Some(value) => diagnostics["level"] = toml_edit::value(value),
                None => {
                    diagnostics.remove("level");
                }
            }
        }
        DIAGNOSTICS_RETENTION_DAYS => set_integer(
            ensure_table(document, "diagnostics")?,
            "retention_days",
            value,
            7,
            key,
        )?,
        DIAGNOSTICS_MAX_FILE_BYTES => set_integer(
            ensure_table(document, "diagnostics")?,
            "max_file_bytes",
            value,
            4 * 1024 * 1024,
            key,
        )?,
        DIAGNOSTICS_MAX_TOTAL_BYTES => set_integer(
            ensure_table(document, "diagnostics")?,
            "max_total_bytes",
            value,
            32 * 1024 * 1024,
            key,
        )?,
        DIAGNOSTICS_MAX_FILES => set_integer(
            ensure_table(document, "diagnostics")?,
            "max_files",
            value,
            32,
            key,
        )?,
        DIAGNOSTICS_QUEUE_CAPACITY => set_integer(
            ensure_table(document, "diagnostics")?,
            "queue_capacity",
            value,
            1024,
            key,
        )?,
        _ => return Err(SettingsError::UnknownKey(key.to_owned())),
    }
    Ok(())
}

fn ensure_table<'a>(
    document: &'a mut toml_edit::DocumentMut,
    key: &str,
) -> Result<&'a mut toml_edit::Table, SettingsError> {
    if document.get(key).is_none() {
        document[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    document
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| SettingsError::Config(ConfigError::Edit(format!("{key} must be a table"))))
}

fn set_integer(
    table: &mut toml_edit::Table,
    field: &str,
    value: Option<&str>,
    default: u64,
    key: &str,
) -> Result<(), SettingsError> {
    let value = value
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| SettingsError::InvalidValue {
            key: key.to_owned(),
            reason: "staged integer is invalid".to_owned(),
        })?
        .unwrap_or(default);
    let value = i64::try_from(value).map_err(|_| SettingsError::InvalidValue {
        key: key.to_owned(),
        reason: "value exceeds TOML's signed integer range".to_owned(),
    })?;
    table[field] = toml_edit::value(value);
    Ok(())
}

fn change_value(change: &DraftChange) -> Option<&str> {
    match change {
        DraftChange::Set(value) => Some(value),
        DraftChange::Reset => None,
    }
}

fn internal_invalid(key: &str, value: &str) -> SettingsError {
    SettingsError::InvalidValue {
        key: key.to_owned(),
        reason: format!("staged canonical value {value:?} is invalid"),
    }
}

fn changes_between(
    paths: &XanaPaths,
    before: &SourceState,
    after: &SourceState,
    staged: &BTreeSet<String>,
) -> Result<Vec<SettingChange>, SettingsError> {
    let before = build_snapshot(paths, before, &BTreeSet::new())?;
    let after = build_snapshot(paths, after, staged)?;
    Ok(staged
        .iter()
        .filter_map(|key| {
            let before = before.entry(key)?;
            let after = after.entry(key)?;
            (before.value != after.value).then(|| SettingChange {
                key: key.clone(),
                label: after.label.clone(),
                before: before.value.clone(),
                after: after.value.clone(),
                target: after.target,
                effect: after.effect,
            })
        })
        .collect())
}

fn install_sources(
    paths: &XanaPaths,
    before: &SourceState,
    after: &SourceState,
) -> Result<(), SettingsError> {
    let config_changed = before.config != after.config;
    let presentation_changed = before.presentation != after.presentation;
    let backup_path = paths.config_file().with_extension("toml.bak");
    let previous_backup = if config_changed {
        read_optional(&backup_path, MAX_CONFIG_BYTES)?
    } else {
        None
    };

    let result = (|| -> Result<(), SettingsError> {
        if config_changed {
            atomic_write(&backup_path, &before.config)?;
            atomic_write(paths.config_file(), &after.config)?;
        }
        if presentation_changed {
            restore_optional(&paths.presentation_file(), after.presentation.as_deref())?;
        }
        Ok(())
    })();

    if let Err(error) = result {
        let mut rollback_failures = Vec::new();
        if config_changed {
            if let Err(rollback) = atomic_write(paths.config_file(), &before.config) {
                rollback_failures.push(rollback.to_string());
            }
            if let Err(rollback) = restore_optional(&backup_path, previous_backup.as_deref()) {
                rollback_failures.push(rollback.to_string());
            }
        }
        if presentation_changed
            && let Err(rollback) =
                restore_optional(&paths.presentation_file(), before.presentation.as_deref())
        {
            rollback_failures.push(rollback.to_string());
        }
        return if rollback_failures.is_empty() {
            Err(SettingsError::Commit(format!(
                "{error}; durable owners were rolled back"
            )))
        } else {
            Err(SettingsError::Commit(format!(
                "{error}; rollback was incomplete: {}",
                rollback_failures.join("; ")
            )))
        };
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let parent = path.parent().ok_or_else(|| {
        SettingsError::Commit(format!("{} has no parent directory", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
        path: parent.to_owned(),
        source,
    })?;
    let mut file =
        atomic_write_file::AtomicWriteFile::open(path).map_err(|source| SettingsError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| SettingsError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.commit().map_err(|source| SettingsError::Io {
        path: path.to_owned(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            SettingsError::Io {
                path: path.to_owned(),
                source,
            }
        })?;
    }
    Ok(())
}

fn restore_optional(path: &Path, value: Option<&[u8]>) -> Result<(), SettingsError> {
    match value {
        Some(value) => atomic_write(path, value),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(SettingsError::Io {
                path: path.to_owned(),
                source,
            }),
        },
    }
}

fn revision(sources: &SourceState) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"xana-settings-v1\0config\0");
    hasher.update(&(sources.config.len() as u64).to_le_bytes());
    hasher.update(&sources.config);
    hasher.update(b"\0presentation\0");
    match &sources.presentation {
        Some(presentation) => {
            hasher.update(&[1]);
            hasher.update(&(presentation.len() as u64).to_le_bytes());
            hasher.update(presentation);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.finalize().to_hex()[..16].to_owned()
}

fn theme_name(value: ThemeChoice) -> &'static str {
    match value {
        ThemeChoice::Auto => "auto",
        ThemeChoice::Dark => "dark",
        ThemeChoice::Light => "light",
        ThemeChoice::Monochrome => "monochrome",
    }
}

fn glyph_name(value: GlyphChoice) -> &'static str {
    match value {
        GlyphChoice::Auto => "auto",
        GlyphChoice::Unicode => "unicode",
        GlyphChoice::Ascii => "ascii",
    }
}

fn motion_name(value: MotionChoice) -> &'static str {
    match value {
        MotionChoice::Auto => "auto",
        MotionChoice::Full => "full",
        MotionChoice::Reduced => "reduced",
    }
}

fn density_name(value: DensityChoice) -> &'static str {
    match value {
        DensityChoice::Auto => "auto",
        DensityChoice::Comfortable => "comfortable",
        DensityChoice::Compact => "compact",
    }
}

fn composer_name(value: ComposerPreset) -> &'static str {
    match value {
        ComposerPreset::Submit => "submit",
        ComposerPreset::Newline => "newline",
    }
}

fn activity_name(value: ActivityPaneChoice) -> &'static str {
    match value {
        ActivityPaneChoice::Auto => "auto",
        ActivityPaneChoice::Open => "open",
        ActivityPaneChoice::Hidden => "hidden",
    }
}

fn diagnostic_level_name(value: crate::config::DiagnosticLevel) -> &'static str {
    match value {
        crate::config::DiagnosticLevel::Error => "error",
        crate::config::DiagnosticLevel::Warn => "warn",
        crate::config::DiagnosticLevel::Info => "info",
        crate::config::DiagnosticLevel::Debug => "debug",
        crate::config::DiagnosticLevel::Trace => "trace",
    }
}

fn supported_shell_choices() -> Vec<&'static str> {
    #[cfg(unix)]
    {
        vec!["platform", "posix"]
    }
    #[cfg(windows)]
    {
        vec!["platform", "git_bash", "powershell", "cmd"]
    }
}

fn format_bytes(value: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if value.is_multiple_of(GIB) {
        format!("{} GiB", value / GIB)
    } else if value.is_multiple_of(MIB) {
        format!("{} MiB", value / MIB)
    } else if value.is_multiple_of(KIB) {
        format!("{} KiB", value / KIB)
    } else {
        format!("{value} bytes")
    }
}

#[cfg(test)]
mod tests;
