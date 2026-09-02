//! Bounded Desktop projection of Xana's shared settings transaction.
//!
//! This adapter deliberately owns no files and exposes no credentials. The
//! runtime keeps the [`SettingsManager`] and its process-local draft; Desktop
//! clients receive immutable, redacted snapshots and send typed intent.

use crate::settings::{
    SettingChange, SettingEffect, SettingEntry, SettingKind, SettingSource, SettingTarget,
    SettingValue, SettingsDraft, SettingsError, SettingsManager, SettingsReceipt, SettingsSection,
    SettingsSnapshot,
};

use super::{DesktopError, DesktopErrorCode};

const SETTINGS_PROTOCOL_VERSION: u16 = 1;
const MAX_SETTINGS_ENTRIES: usize = 256;
const MAX_SETTING_CHOICES: usize = 512;
const MAX_SETTING_TEXT_BYTES: usize = 16 * 1024;

/// Opaque identity for one process-local settings draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DesktopSettingsDraftId(u64);

impl DesktopSettingsDraftId {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A client-owned semantic message with a safe English fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLocalizedText {
    pub code: String,
    pub fallback: String,
}

/// Stable Desktop settings section identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopSettingsSection {
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

impl DesktopSettingsSection {
    pub const fn id(self) -> &'static str {
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

    pub const fn title(self) -> &'static str {
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
}

/// Presentation kind for one redacted settings row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSettingKind {
    Boolean,
    Choice,
    Integer,
    Bytes,
    DurationDays,
    OptionalPath,
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSettingSource {
    BuiltInDefault,
    ConfigurationFile,
    PresentationFile,
    Derived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSettingTarget {
    MachinePresentation,
    GlobalConfiguration,
    TaskSpecificManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSettingEffect {
    Immediate,
    NewConversation,
    NextLaunch,
    ManagedElsewhere,
}

/// Secret-free scalar value. `raw == None` means automatic/absent, not hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSettingValue {
    pub raw: Option<String>,
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSettingEntry {
    pub key: String,
    pub section: DesktopSettingsSection,
    pub label: DesktopLocalizedText,
    pub description: DesktopLocalizedText,
    pub value: DesktopSettingValue,
    pub default: Option<DesktopSettingValue>,
    pub kind: DesktopSettingKind,
    pub choices: Vec<String>,
    pub source: DesktopSettingSource,
    pub target: DesktopSettingTarget,
    pub effect: DesktopSettingEffect,
    pub editable: bool,
    pub focused_action: Option<String>,
    pub staged: bool,
}

/// Atomic redacted settings catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSettingsSnapshot {
    pub version: u16,
    pub revision: String,
    pub warnings: Vec<String>,
    pub entries: Vec<DesktopSettingEntry>,
    pub truncated: bool,
}

impl DesktopSettingsSnapshot {
    pub fn entries_in(
        &self,
        section: DesktopSettingsSection,
    ) -> impl Iterator<Item = &DesktopSettingEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.section == section)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSettingChange {
    pub key: String,
    pub label: DesktopLocalizedText,
    pub before: DesktopSettingValue,
    pub after: DesktopSettingValue,
    pub target: DesktopSettingTarget,
    pub effect: DesktopSettingEffect,
}

/// Current process-local draft projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSettingsDraftSnapshot {
    pub id: DesktopSettingsDraftId,
    pub base_revision: String,
    pub pending_count: usize,
    pub preview: DesktopSettingsSnapshot,
    pub changes: Vec<DesktopSettingChange>,
}

/// Authoritative redacted commit or dry-run receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSettingsReceipt {
    pub dry_run: bool,
    pub revision_before: String,
    pub revision_after: String,
    pub changes: Vec<DesktopSettingChange>,
    pub requires_new_conversation: bool,
    pub durable_owners: Vec<DesktopSettingsOwner>,
    pub configuration_backup: DesktopSettingsBackup,
    pub rollback_on_failure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSettingsOwner {
    GlobalConfiguration,
    MachinePresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSettingsBackup {
    NotNeeded,
    Planned,
    Created,
}

struct ActiveDraft {
    id: DesktopSettingsDraftId,
    draft: SettingsDraft,
}

pub(super) struct DesktopSettingsState {
    manager: SettingsManager,
    snapshot: DesktopSettingsSnapshot,
    draft: Option<ActiveDraft>,
    next_draft_id: u64,
}

impl DesktopSettingsState {
    pub(super) fn open(manager: SettingsManager) -> Result<Self, DesktopError> {
        let snapshot = project_snapshot(manager.snapshot().map_err(project_error)?);
        Ok(Self {
            manager,
            snapshot,
            draft: None,
            next_draft_id: 1,
        })
    }

    pub(super) fn snapshot(&self) -> &DesktopSettingsSnapshot {
        &self.snapshot
    }

    pub(super) fn begin(&mut self) -> Result<DesktopSettingsDraftSnapshot, DesktopError> {
        if let Some(active) = &self.draft {
            return project_draft(active);
        }
        let id = DesktopSettingsDraftId(self.next_draft_id);
        self.next_draft_id = self.next_draft_id.wrapping_add(1).max(1);
        let draft = self.manager.begin().map_err(project_error)?;
        self.draft = Some(ActiveDraft { id, draft });
        project_draft(self.draft.as_ref().expect("draft was just installed"))
    }

    pub(super) fn set(
        &mut self,
        id: DesktopSettingsDraftId,
        key: &str,
        value: &str,
    ) -> Result<DesktopSettingsDraftSnapshot, DesktopError> {
        let active = self.active_mut(id)?;
        active.draft.set(key, value).map_err(project_error)?;
        project_draft(active)
    }

    pub(super) fn reset(
        &mut self,
        id: DesktopSettingsDraftId,
        key: &str,
    ) -> Result<DesktopSettingsDraftSnapshot, DesktopError> {
        let active = self.active_mut(id)?;
        active.draft.reset(key).map_err(project_error)?;
        project_draft(active)
    }

    pub(super) fn revert(
        &mut self,
        id: DesktopSettingsDraftId,
        key: &str,
    ) -> Result<DesktopSettingsDraftSnapshot, DesktopError> {
        let active = self.active_mut(id)?;
        active.draft.revert(key);
        project_draft(active)
    }

    pub(super) fn validate(
        &mut self,
        id: DesktopSettingsDraftId,
    ) -> Result<DesktopSettingsReceipt, DesktopError> {
        let active = self.active(id)?;
        self.manager
            .commit(&active.draft, true)
            .map(project_receipt)
            .map_err(project_error)
    }

    pub(super) fn commit(
        &mut self,
        id: DesktopSettingsDraftId,
    ) -> Result<(DesktopSettingsReceipt, DesktopSettingsSnapshot), DesktopError> {
        let active = self.active(id)?;
        let receipt = self
            .manager
            .commit(&active.draft, false)
            .map_err(project_error)?;
        self.draft = None;
        self.snapshot = project_snapshot(self.manager.snapshot().map_err(project_error)?);
        Ok((project_receipt(receipt), self.snapshot.clone()))
    }

    pub(super) fn discard(&mut self, id: DesktopSettingsDraftId) -> Result<(), DesktopError> {
        self.active(id)?;
        self.draft = None;
        Ok(())
    }

    pub(super) fn reload(&mut self) -> Result<DesktopSettingsSnapshot, DesktopError> {
        self.draft = None;
        self.snapshot = project_snapshot(self.manager.snapshot().map_err(project_error)?);
        Ok(self.snapshot.clone())
    }

    fn active(&self, id: DesktopSettingsDraftId) -> Result<&ActiveDraft, DesktopError> {
        self.draft
            .as_ref()
            .filter(|draft| draft.id == id)
            .ok_or_else(stale_draft_error)
    }

    fn active_mut(&mut self, id: DesktopSettingsDraftId) -> Result<&mut ActiveDraft, DesktopError> {
        self.draft
            .as_mut()
            .filter(|draft| draft.id == id)
            .ok_or_else(stale_draft_error)
    }
}

fn stale_draft_error() -> DesktopError {
    DesktopError::new(
        DesktopErrorCode::StateInvalid,
        "settings draft is stale, discarded, or belongs to another runtime",
    )
}

fn project_draft(active: &ActiveDraft) -> Result<DesktopSettingsDraftSnapshot, DesktopError> {
    let preview = active.draft.preview().map_err(project_error)?;
    let changes = active
        .draft
        .pending_changes()
        .map_err(project_error)?
        .into_iter()
        .map(project_change)
        .collect::<Vec<_>>();
    Ok(DesktopSettingsDraftSnapshot {
        id: active.id,
        base_revision: active.draft.base_revision(),
        pending_count: changes.len(),
        preview: project_snapshot(preview),
        changes,
    })
}

fn project_snapshot(snapshot: SettingsSnapshot) -> DesktopSettingsSnapshot {
    let truncated = snapshot.entries.len() > MAX_SETTINGS_ENTRIES;
    DesktopSettingsSnapshot {
        version: SETTINGS_PROTOCOL_VERSION,
        revision: bounded(snapshot.revision),
        warnings: snapshot.warnings.into_iter().map(bounded).collect(),
        entries: snapshot
            .entries
            .into_iter()
            .take(MAX_SETTINGS_ENTRIES)
            .map(project_entry)
            .collect(),
        truncated,
    }
}

fn project_entry(entry: SettingEntry) -> DesktopSettingEntry {
    let key = bounded(entry.key);
    DesktopSettingEntry {
        label: semantic_text(format!("settings.{key}.label"), entry.label),
        description: semantic_text(format!("settings.{key}.description"), entry.description),
        key,
        section: project_section(entry.section),
        value: project_value(entry.value),
        default: entry.default.map(project_value),
        kind: project_kind(entry.kind),
        choices: entry
            .choices
            .into_iter()
            .take(MAX_SETTING_CHOICES)
            .map(bounded)
            .collect(),
        source: project_source(entry.source),
        target: project_target(entry.target),
        effect: project_effect(entry.effect),
        editable: entry.editable,
        focused_action: entry.action.map(bounded),
        staged: entry.staged,
    }
}

fn project_receipt(receipt: SettingsReceipt) -> DesktopSettingsReceipt {
    let requires_new_conversation = receipt.requires_new_conversation();
    let configuration_changed = receipt
        .changes
        .iter()
        .any(|change| change.target == SettingTarget::GlobalConfiguration);
    let presentation_changed = receipt
        .changes
        .iter()
        .any(|change| change.target == SettingTarget::MachinePresentation);
    let mut durable_owners = Vec::with_capacity(2);
    if configuration_changed {
        durable_owners.push(DesktopSettingsOwner::GlobalConfiguration);
    }
    if presentation_changed {
        durable_owners.push(DesktopSettingsOwner::MachinePresentation);
    }
    let configuration_backup = if !configuration_changed {
        DesktopSettingsBackup::NotNeeded
    } else if receipt.dry_run {
        DesktopSettingsBackup::Planned
    } else {
        DesktopSettingsBackup::Created
    };
    DesktopSettingsReceipt {
        dry_run: receipt.dry_run,
        revision_before: bounded(receipt.revision_before),
        revision_after: bounded(receipt.revision_after),
        changes: receipt.changes.into_iter().map(project_change).collect(),
        requires_new_conversation,
        durable_owners,
        configuration_backup,
        rollback_on_failure: true,
    }
}

fn project_change(change: SettingChange) -> DesktopSettingChange {
    let key = bounded(change.key);
    DesktopSettingChange {
        label: semantic_text(format!("settings.{key}.label"), change.label),
        key,
        before: project_value(change.before),
        after: project_value(change.after),
        target: project_target(change.target),
        effect: project_effect(change.effect),
    }
}

fn project_value(value: SettingValue) -> DesktopSettingValue {
    DesktopSettingValue {
        raw: value.raw.map(bounded),
        display: bounded(value.display),
    }
}

fn semantic_text(code: String, fallback: String) -> DesktopLocalizedText {
    DesktopLocalizedText {
        code: bounded(code),
        fallback: bounded(fallback),
    }
}

fn project_section(section: SettingsSection) -> DesktopSettingsSection {
    match section {
        SettingsSection::Overview => DesktopSettingsSection::Overview,
        SettingsSection::Appearance => DesktopSettingsSection::Appearance,
        SettingsSection::Connections => DesktopSettingsSection::Connections,
        SettingsSection::Profiles => DesktopSettingsSection::Profiles,
        SettingsSection::Permissions => DesktopSettingsSection::Permissions,
        SettingsSection::Execution => DesktopSettingsSection::Execution,
        SettingsSection::Diagnostics => DesktopSettingsSection::Diagnostics,
        SettingsSection::Integrations => DesktopSettingsSection::Integrations,
        SettingsSection::Advanced => DesktopSettingsSection::Advanced,
    }
}

fn project_kind(kind: SettingKind) -> DesktopSettingKind {
    match kind {
        SettingKind::Boolean => DesktopSettingKind::Boolean,
        SettingKind::Choice => DesktopSettingKind::Choice,
        SettingKind::Integer => DesktopSettingKind::Integer,
        SettingKind::Bytes => DesktopSettingKind::Bytes,
        SettingKind::DurationDays => DesktopSettingKind::DurationDays,
        SettingKind::OptionalPath => DesktopSettingKind::OptionalPath,
        SettingKind::ReadOnly => DesktopSettingKind::ReadOnly,
    }
}

fn project_source(source: SettingSource) -> DesktopSettingSource {
    match source {
        SettingSource::BuiltInDefault => DesktopSettingSource::BuiltInDefault,
        SettingSource::ConfigurationFile => DesktopSettingSource::ConfigurationFile,
        SettingSource::PresentationFile => DesktopSettingSource::PresentationFile,
        SettingSource::Derived => DesktopSettingSource::Derived,
    }
}

fn project_target(target: SettingTarget) -> DesktopSettingTarget {
    match target {
        SettingTarget::MachinePresentation => DesktopSettingTarget::MachinePresentation,
        SettingTarget::GlobalConfiguration => DesktopSettingTarget::GlobalConfiguration,
        SettingTarget::TaskSpecificManager => DesktopSettingTarget::TaskSpecificManager,
    }
}

fn project_effect(effect: SettingEffect) -> DesktopSettingEffect {
    match effect {
        SettingEffect::Immediate => DesktopSettingEffect::Immediate,
        SettingEffect::NewConversation => DesktopSettingEffect::NewConversation,
        SettingEffect::NextLaunch => DesktopSettingEffect::NextLaunch,
        SettingEffect::ManagedElsewhere => DesktopSettingEffect::ManagedElsewhere,
    }
}

fn project_error(error: SettingsError) -> DesktopError {
    let code = match error {
        SettingsError::Config(_) | SettingsError::Io { .. } => {
            DesktopErrorCode::ConfigurationUnavailable
        }
        SettingsError::ConcurrentChange { .. } => DesktopErrorCode::StateInvalid,
        _ => DesktopErrorCode::CommandRejected,
    };
    DesktopError::new(code, error.to_string())
}

fn bounded(mut value: String) -> String {
    if value.len() <= MAX_SETTING_TEXT_BYTES {
        return value;
    }
    let mut end = MAX_SETTING_TEXT_BYTES.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
        paths::XanaPaths,
        shell::ShellConfig,
    };
    use std::{ffi::OsString, fs};

    fn fixture() -> (tempfile::TempDir, DesktopSettingsState) {
        let directory = tempfile::tempdir().expect("temporary Xana home");
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path())))
            .expect("absolute temporary Xana home");
        let config = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "ollama".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen3:1.7b".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .expect("render config");
        fs::write(paths.config_file(), config).expect("write config");
        let state = DesktopSettingsState::open(SettingsManager::new(&paths)).expect("open state");
        (directory, state)
    }

    #[test]
    fn desktop_projection_is_bounded_semantic_and_secret_free() {
        let (_directory, state) = fixture();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.version, SETTINGS_PROTOCOL_VERSION);
        assert!(!snapshot.truncated);
        let theme = snapshot
            .entries
            .iter()
            .find(|entry| entry.key == "appearance.theme")
            .expect("theme entry");
        assert_eq!(theme.label.code, "settings.appearance.theme.label");
        assert_eq!(theme.section, DesktopSettingsSection::Appearance);
        assert!(snapshot.entries.iter().all(|entry| {
            !entry.key.contains("secret") && !entry.label.fallback.contains("credential value")
        }));
    }

    #[test]
    fn stale_draft_cannot_mutate_or_commit_another_transaction() {
        let (_directory, mut state) = fixture();
        let first = state.begin().expect("begin");
        state.discard(first.id).expect("discard");
        let second = state.begin().expect("begin replacement");
        assert_ne!(first.id, second.id);
        assert_eq!(
            state
                .set(first.id, "appearance.theme", "dark")
                .expect_err("stale id")
                .code,
            DesktopErrorCode::StateInvalid
        );
    }

    #[test]
    fn validation_is_dry_and_commit_returns_authoritative_snapshot() {
        let (_directory, mut state) = fixture();
        let draft = state.begin().expect("begin");
        let draft = state
            .set(draft.id, "appearance.theme", "dark")
            .expect("stage");
        assert_eq!(draft.pending_count, 1);
        let validation = state.validate(draft.id).expect("validate");
        assert!(validation.dry_run);
        assert_eq!(
            validation.configuration_backup,
            DesktopSettingsBackup::NotNeeded
        );
        assert_eq!(
            validation.durable_owners,
            vec![DesktopSettingsOwner::MachinePresentation]
        );
        let (receipt, snapshot) = state.commit(draft.id).expect("commit");
        assert!(!receipt.dry_run);
        assert_eq!(receipt.changes.len(), 1);
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.key == "appearance.theme")
                .and_then(|entry| entry.value.raw.as_deref()),
            Some("dark")
        );
    }

    #[test]
    fn concurrent_durable_change_rejects_commit_without_losing_the_draft() {
        let (directory, mut state) = fixture();
        let draft = state.begin().expect("begin");
        let draft = state
            .set(draft.id, "permissions.default", "deny")
            .expect("stage configuration value");
        let paths =
            XanaPaths::resolve(Some(OsString::from(directory.path()))).expect("fixture paths");
        let mut external = fs::read_to_string(paths.config_file()).expect("read config");
        external.push_str("\n# external concurrent edit\n");
        fs::write(paths.config_file(), external).expect("mutate config externally");

        let error = state.commit(draft.id).expect_err("concurrent commit");
        assert_eq!(error.code, DesktopErrorCode::StateInvalid);
        assert!(state.draft.is_some(), "rejected commit must preserve draft");
    }

    #[test]
    fn committed_configuration_receipt_reports_created_backup_and_rollback() {
        let (_directory, mut state) = fixture();
        let draft = state.begin().expect("begin");
        let draft = state
            .set(draft.id, "permissions.default", "deny")
            .expect("stage configuration value");
        let (receipt, _) = state.commit(draft.id).expect("commit");
        assert_eq!(
            receipt.durable_owners,
            vec![DesktopSettingsOwner::GlobalConfiguration]
        );
        assert_eq!(receipt.configuration_backup, DesktopSettingsBackup::Created);
        assert!(receipt.rollback_on_failure);
    }
}
