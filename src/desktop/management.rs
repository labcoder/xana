//! Typed Desktop setup and connection-management control plane.
//!
//! GPUI owns form state only. All path resolution, config parsing, live
//! discovery, secret handling, validation, and durable mutation stay here.

mod accounting;
mod workers;
pub use workers::{DesktopWorkerCancellation, DesktopWorkerIntent, DesktopWorkerSummary};
mod autonomy;
mod autonomy_observer;
pub use autonomy::{
    DesktopAutonomySnapshot, DesktopGithubCredential, DesktopHostEdit, DesktopScheduleEdit,
    DesktopScheduledTask, DesktopTaskDraft, DesktopTaskPreview, DesktopTaskTrigger,
    DesktopWorkGroup,
};
pub use autonomy_observer::{
    DesktopAutonomyObserver, DesktopBackgroundAttention, DesktopBackgroundAttentionKind,
    DesktopBackgroundUpdate,
};
mod candidates;
mod memory;
pub use candidates::{
    DesktopCandidateCommand, DesktopCandidateKind, DesktopCandidateResult, DesktopCandidateRow,
};
pub use memory::{
    DesktopMemoryMutation, DesktopMemorySnapshot, MemoryClaim, MemoryControlEdit, MemoryControls,
    MemoryEdit, MemoryPage, MemoryRecord, MemoryScope, MemoryState, SourceDeletionPreview,
    SourceDeletionReceipt,
};
mod actions;
mod entities;
mod maintenance;
mod permissions;
mod resources;
mod workbench;

pub use accounting::{
    DesktopBudgetEdit, DesktopBudgetField, DesktopBudgetSetting, DesktopUsagePage,
};

pub use actions::{
    DesktopConnectionMutationReceipt, DesktopConnectionRemovalPlan, DesktopManagedLogin,
};
pub use entities::{
    DesktopCapabilityFact, DesktopCapabilitySnapshot, DesktopEntityMutationReceipt,
    DesktopManagementSnapshot, DesktopProfileDraft, DesktopProfileRetirement,
    DesktopProfileSummary, DesktopProjectDraft, DesktopProjectSummary,
};
pub use maintenance::{
    DesktopDiagnosticEntry, DesktopDiagnosticsSnapshot, DesktopDoctorFinding,
    DesktopDoctorRepairReceipt, DesktopDoctorRepairResult, DesktopDoctorSeverity,
    DesktopDoctorSnapshot, DesktopMigrationReceipt, DesktopMigrationSnapshot,
    DesktopPrivateMigrationRecord, DesktopResetPlan, DesktopResetReceipt, DesktopResetScope,
    DesktopResetTarget,
};
pub use permissions::{
    DesktopPermissionDecision, DesktopPermissionEffect, DesktopPermissionPreview,
    DesktopPermissionRuleDraft, DesktopPermissionRuleSummary, DesktopPermissionSnapshot,
};
pub use resources::{
    DesktopResourceLimit, DesktopResourcePolicyDraft, DesktopResourcePolicyPreview,
    DesktopResourcePolicySnapshot,
};
pub use workbench::DesktopWorkbenchPreferenceSnapshot;

use super::{DesktopError, DesktopErrorCode};
use crate::{
    config::{ConfigReadiness, PermissionMode, ProviderKind},
    connection_management::{
        ConnectionHealth, ConnectionManagement, CredentialSource, CredentialState,
        ManagedAccountState, ReachabilityState, RecoveryAction, SelectedModelState,
    },
    credential::SecretString,
    model_catalog::{DescriptorSource, ExecutionKind, ModelDescriptor},
    paths::XanaPaths,
    setup::{self, DesktopSetupCredential as CoreCredential, DesktopSetupDraft as CoreDraft},
};
use std::{ffi::OsString, fmt, path::PathBuf};

const DESKTOP_MANAGEMENT_VERSION: u16 = 1;
const MAX_MODELS: usize = 2_048;
const MAX_PUBLIC_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopSetupMode {
    StartWithConnection,
    FullCustomize,
    Blank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopProviderKind {
    Ollama,
    OpenAiCompatible,
    OpenAi,
    OpenRouter,
    Anthropic,
    Codex,
}

impl DesktopProviderKind {
    pub const ALL: [Self; 6] = [
        Self::Ollama,
        Self::OpenAiCompatible,
        Self::OpenAi,
        Self::OpenRouter,
        Self::Anthropic,
        Self::Codex,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAiCompatible => "openai_compatible",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::OpenAiCompatible => "OpenAI-compatible",
            Self::OpenAi => "OpenAI API",
            Self::OpenRouter => "OpenRouter",
            Self::Anthropic => "Anthropic",
            Self::Codex => "Codex",
        }
    }

    pub const fn default_connection(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAiCompatible => "compatible",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }

    pub const fn default_endpoint(self) -> Option<&'static str> {
        match self {
            Self::Ollama => Some("http://localhost:11434/v1"),
            Self::OpenAiCompatible => None,
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::Anthropic => Some("https://api.anthropic.com"),
            Self::Codex => None,
        }
    }

    pub const fn uses_managed_account(self) -> bool {
        matches!(self, Self::Codex)
    }

    pub const fn requires_credential(self) -> bool {
        matches!(self, Self::OpenAi | Self::OpenRouter | Self::Anthropic)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPermissionMode {
    Deny,
    Ask,
    Allow,
}

impl DesktopPermissionMode {
    pub const ALL: [Self; 3] = [Self::Ask, Self::Deny, Self::Allow];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Allow => "allow",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopCredentialInput {
    None,
    Environment { variable: String },
    Stored { id: String },
}

/// Write-only secret payload. Debug output is always redacted and Drop zeroizes it.
pub struct DesktopSecret(SecretString);

impl DesktopSecret {
    pub fn new(value: String) -> Result<Self, DesktopError> {
        SecretString::new(value).map(Self).map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("credential input is invalid: {error}"),
            )
        })
    }
}

impl fmt::Debug for DesktopSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DesktopSecret(REDACTED)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSetupDraft {
    pub profile: Option<String>,
    pub make_default: bool,
    pub mode: DesktopSetupMode,
    pub provider: DesktopProviderKind,
    pub connection: String,
    pub endpoint: Option<String>,
    pub codex_program: Option<String>,
    pub codex_home: Option<PathBuf>,
    pub credential: DesktopCredentialInput,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub permission_mode: DesktopPermissionMode,
}

impl DesktopSetupDraft {
    pub fn for_provider(provider: DesktopProviderKind, mode: DesktopSetupMode) -> Self {
        Self {
            mode,
            profile: None,
            make_default: false,
            provider,
            connection: provider.default_connection().to_owned(),
            endpoint: provider.default_endpoint().map(str::to_owned),
            codex_program: provider.uses_managed_account().then(|| "codex".to_owned()),
            codex_home: None,
            credential: if provider.requires_credential() {
                DesktopCredentialInput::Stored {
                    id: provider.default_connection().to_owned(),
                }
            } else {
                DesktopCredentialInput::None
            },
            model: None,
            reasoning_effort: None,
            permission_mode: DesktopPermissionMode::Ask,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSetupSnapshot {
    configuration_revision: setup::ConfigRevision,
    pub version: u16,
    pub configuration_state: String,
    pub intentionally_blank: bool,
    pub modes: Vec<DesktopSetupMode>,
    pub providers: Vec<DesktopProviderKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSetupReceipt {
    pub semantic_code: String,
    pub connection: Option<String>,
    pub model: Option<String>,
    pub discovered_model_count: usize,
    pub replaced_existing_configuration: bool,
    pub backup_created: bool,
    pub requires_new_conversation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopExecutionKind {
    Native,
    Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopCredentialState {
    Available,
    Missing,
    Inaccessible,
    NotRequired,
    ManagedExternally,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopModelOption {
    pub id: String,
    pub display_name: String,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub tools: Option<bool>,
    pub reasoning: Option<bool>,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
    pub context_tokens: Option<usize>,
    pub max_output_tokens: Option<usize>,
    pub pricing: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConnection {
    pub id: String,
    pub provider: DesktopProviderKind,
    pub execution: DesktopExecutionKind,
    pub selected_for_new_conversations: bool,
    pub selected_model: Option<String>,
    pub credential_source: String,
    pub credential: DesktopCredentialState,
    pub managed_account: String,
    pub reachability: String,
    pub catalog_freshness: String,
    pub catalog_fetched_at_unix_seconds: Option<u64>,
    pub cached_model_count: usize,
    pub available_model_count: usize,
    pub selected_model_state: String,
    pub health: String,
    pub recovery: String,
    pub profile_references: Vec<String>,
    pub models: Vec<DesktopModelOption>,
    pub models_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConnectionSnapshot {
    pub version: u16,
    pub connections: Vec<DesktopConnection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConnectionOperationReceipt {
    pub semantic_code: String,
    pub connection: String,
    pub usable: Option<bool>,
    pub reachability: Option<String>,
    pub credential: Option<DesktopCredentialState>,
    pub managed_account: Option<String>,
    pub discovered_model_count: usize,
    pub recovery: Option<String>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DesktopControlPlane {
    paths: XanaPaths,
}

impl DesktopControlPlane {
    /// Lock only after every content-bearing client has been shut down and
    /// dropped. The store refuses to report a lock while another owner is live.
    pub fn lock_storage(&self) -> Result<(), DesktopError> {
        use crate::storage::{ProtectedStore, StorageStatus};
        if matches!(
            ProtectedStore::status(self.paths.data_dir()).map_err(control_error)?,
            StorageStatus::Protected { locked: true, .. }
        ) {
            return Ok(());
        }
        ProtectedStore::configured(self.paths.data_dir())
            .map_err(control_error)?
            .ok_or_else(|| control_error("This home is not protected; no storage was locked"))?
            .lock()
            .map_err(control_error)
    }

    pub(super) fn resolve(xana_home: Option<OsString>) -> Result<Self, DesktopError> {
        XanaPaths::resolve(xana_home)
            .map(|paths| Self { paths })
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::ConfigurationUnavailable,
                    format!("could not resolve Xana paths: {error}"),
                )
            })
    }

    pub fn setup_snapshot(&self) -> Result<DesktopSetupSnapshot, DesktopError> {
        let intentionally_blank = setup::inspect_installation(&self.paths)
            .map_err(control_error)?
            == setup::SetupInstallation::Blank;
        Ok(DesktopSetupSnapshot {
            configuration_revision: setup::ConfigRevision::capture(self.paths.config_file())
                .map_err(control_error)?,
            version: DESKTOP_MANAGEMENT_VERSION,
            configuration_state: ConfigReadiness::inspect(self.paths.config_file())
                .as_str()
                .to_owned(),
            intentionally_blank,
            modes: vec![
                DesktopSetupMode::StartWithConnection,
                DesktopSetupMode::FullCustomize,
                DesktopSetupMode::Blank,
            ],
            providers: DesktopProviderKind::ALL.to_vec(),
        })
    }

    pub fn connections(&self) -> Result<DesktopConnectionSnapshot, DesktopError> {
        let snapshot = ConnectionManagement::open(&self.paths)
            .and_then(|management| management.snapshot())
            .map_err(control_error)?;
        Ok(DesktopConnectionSnapshot {
            version: DESKTOP_MANAGEMENT_VERSION,
            connections: snapshot
                .connections
                .into_iter()
                .map(project_connection)
                .collect(),
        })
    }

    pub async fn discover_setup(
        &self,
        draft: &DesktopSetupDraft,
        secret: Option<&DesktopSecret>,
    ) -> Result<Vec<DesktopModelOption>, DesktopError> {
        setup::discover_for_desktop(&core_draft(draft), &self.paths, secret.map(|s| &s.0))
            .await
            .map(|models| {
                models
                    .into_iter()
                    .take(MAX_MODELS)
                    .map(project_model)
                    .collect()
            })
            .map_err(control_error)
    }

    pub async fn commit_setup(
        &self,
        draft: &DesktopSetupDraft,
        secret: Option<&DesktopSecret>,
        review: &DesktopSetupSnapshot,
    ) -> Result<DesktopSetupReceipt, DesktopError> {
        if draft.mode == DesktopSetupMode::Blank {
            return self.commit_blank();
        }
        setup::commit_for_desktop(
            &core_draft(draft),
            &self.paths,
            secret.map(|s| &s.0),
            review.configuration_revision,
        )
        .await
        .map(|receipt| DesktopSetupReceipt {
            semantic_code: "setup.commit.completed.v1".to_owned(),
            connection: Some(receipt.connection),
            model: Some(receipt.model),
            discovered_model_count: receipt.discovered_model_count,
            replaced_existing_configuration: receipt.replaced_existing_configuration,
            backup_created: receipt.backup_created,
            requires_new_conversation: true,
        })
        .map_err(control_error)
    }

    pub fn commit_blank(&self) -> Result<DesktopSetupReceipt, DesktopError> {
        setup::commit_blank_for_desktop(&self.paths).map_err(control_error)?;
        Ok(DesktopSetupReceipt {
            semantic_code: "setup.blank.completed.v1".to_owned(),
            connection: None,
            model: None,
            discovered_model_count: 0,
            replaced_existing_configuration: false,
            backup_created: false,
            requires_new_conversation: false,
        })
    }

    pub fn select_model(
        &self,
        connection: &str,
        model: &str,
        reasoning_effort: Option<String>,
    ) -> Result<DesktopSetupReceipt, DesktopError> {
        let (_, receipt) = ConnectionManagement::open(&self.paths)
            .and_then(|management| management.select(connection, model, reasoning_effort, None))
            .map_err(control_error)?;
        Ok(DesktopSetupReceipt {
            semantic_code: receipt.semantic_code.to_owned(),
            connection: Some(connection.to_owned()),
            model: Some(model.to_owned()),
            discovered_model_count: 0,
            replaced_existing_configuration: false,
            backup_created: false,
            requires_new_conversation: true,
        })
    }

    pub async fn test_connection(
        &self,
        connection: &str,
    ) -> Result<DesktopConnectionOperationReceipt, DesktopError> {
        crate::app::test_connection(&self.paths, connection)
            .await
            .map(|(receipt, _)| DesktopConnectionOperationReceipt {
                semantic_code: receipt.semantic_code.to_owned(),
                connection: receipt.connection,
                usable: Some(receipt.usable),
                reachability: Some(reachability(receipt.reachability)),
                credential: Some(match receipt.credential {
                    CredentialState::Available => DesktopCredentialState::Available,
                    CredentialState::Missing => DesktopCredentialState::Missing,
                    CredentialState::Inaccessible => DesktopCredentialState::Inaccessible,
                    CredentialState::NotRequired => DesktopCredentialState::NotRequired,
                    CredentialState::ManagedExternally => DesktopCredentialState::ManagedExternally,
                }),
                managed_account: Some(account_state(&receipt.account)),
                discovered_model_count: receipt.discovered_model_count,
                recovery: Some(recovery(receipt.recovery)),
                failure: receipt.failure.map(bounded),
            })
            .map_err(control_error)
    }

    pub async fn refresh_connection(
        &self,
        connection: &str,
    ) -> Result<DesktopConnectionOperationReceipt, DesktopError> {
        crate::app::refresh_connection(&self.paths, connection)
            .await
            .map(|(receipt, count)| DesktopConnectionOperationReceipt {
                semantic_code: receipt.semantic_code.to_owned(),
                connection: receipt.connection,
                usable: None,
                reachability: None,
                credential: None,
                managed_account: None,
                discovered_model_count: count,
                recovery: None,
                failure: None,
            })
            .map_err(control_error)
    }
}

fn core_draft(draft: &DesktopSetupDraft) -> CoreDraft {
    CoreDraft {
        profile: draft.profile.clone(),
        make_default: draft.make_default,
        kind: match draft.provider {
            DesktopProviderKind::Ollama => ProviderKind::Ollama,
            DesktopProviderKind::OpenAiCompatible => ProviderKind::OpenAiCompat,
            DesktopProviderKind::OpenAi => ProviderKind::OpenAi,
            DesktopProviderKind::OpenRouter => ProviderKind::OpenRouter,
            DesktopProviderKind::Anthropic => ProviderKind::Anthropic,
            DesktopProviderKind::Codex => ProviderKind::Codex,
        },
        connection: draft.connection.clone(),
        base_url: draft.endpoint.clone(),
        codex_program: draft.codex_program.clone(),
        codex_home: draft.codex_home.clone(),
        credential: match &draft.credential {
            DesktopCredentialInput::None => CoreCredential::None,
            DesktopCredentialInput::Environment { variable } => {
                CoreCredential::Environment(variable.clone())
            }
            DesktopCredentialInput::Stored { id } => CoreCredential::Stored { id: id.clone() },
        },
        model: draft.model.clone(),
        reasoning_effort: draft.reasoning_effort.clone(),
        permission_mode: match draft.permission_mode {
            DesktopPermissionMode::Deny => PermissionMode::Deny,
            DesktopPermissionMode::Ask => PermissionMode::Ask,
            DesktopPermissionMode::Allow => PermissionMode::Allow,
        },
    }
}

fn project_connection(
    value: crate::connection_management::ConnectionSummaryView,
) -> DesktopConnection {
    let models_truncated = value.models.len() > MAX_MODELS;
    DesktopConnection {
        id: bounded(value.id),
        provider: project_provider(value.provider),
        execution: match value.execution {
            ExecutionKind::Native => DesktopExecutionKind::Native,
            ExecutionKind::Managed => DesktopExecutionKind::Managed,
        },
        selected_for_new_conversations: value.selected_for_new_conversations,
        selected_model: value.selected_model.map(bounded),
        credential_source: credential_source(&value.credential_source),
        credential: match value.facets.credential {
            CredentialState::Available => DesktopCredentialState::Available,
            CredentialState::Missing => DesktopCredentialState::Missing,
            CredentialState::Inaccessible => DesktopCredentialState::Inaccessible,
            CredentialState::NotRequired => DesktopCredentialState::NotRequired,
            CredentialState::ManagedExternally => DesktopCredentialState::ManagedExternally,
        },
        managed_account: account_state(&value.facets.account),
        reachability: reachability(value.facets.reachability),
        catalog_freshness: format!("{:?}", value.facets.catalog.freshness).to_lowercase(),
        catalog_fetched_at_unix_seconds: value.facets.catalog.fetched_at_unix_seconds,
        cached_model_count: value.facets.catalog.cached_model_count,
        available_model_count: value.facets.catalog.available_model_count,
        selected_model_state: selected_model_state(value.facets.selected_model),
        health: health(value.health),
        recovery: recovery(value.recovery),
        profile_references: value.profile_references.into_iter().map(bounded).collect(),
        models: value
            .models
            .into_iter()
            .take(MAX_MODELS)
            .map(project_model)
            .collect(),
        models_truncated,
    }
}

fn project_model(value: ModelDescriptor) -> DesktopModelOption {
    DesktopModelOption {
        id: bounded(value.id),
        display_name: bounded(value.display_name),
        input_modalities: value.input_modalities.into_iter().map(bounded).collect(),
        output_modalities: value.output_modalities.into_iter().map(bounded).collect(),
        tools: value.tools,
        reasoning: value.reasoning,
        reasoning_efforts: value
            .reasoning_efforts
            .into_iter()
            .map(|effort| bounded(effort.id))
            .collect(),
        default_reasoning_effort: value.default_reasoning_effort.map(bounded),
        context_tokens: value.context_tokens,
        max_output_tokens: value.max_output_tokens,
        pricing: value.pricing.summary().map(bounded),
        source: match value.source {
            DescriptorSource::Configured => "configured",
            DescriptorSource::Remote => "remote",
            DescriptorSource::ManagedRuntime => "managed_runtime",
        }
        .to_owned(),
    }
}

fn project_provider(value: ProviderKind) -> DesktopProviderKind {
    match value {
        ProviderKind::OpenAiCompat => DesktopProviderKind::OpenAiCompatible,
        ProviderKind::Ollama => DesktopProviderKind::Ollama,
        ProviderKind::OpenAi => DesktopProviderKind::OpenAi,
        ProviderKind::OpenRouter => DesktopProviderKind::OpenRouter,
        ProviderKind::Anthropic => DesktopProviderKind::Anthropic,
        ProviderKind::Codex => DesktopProviderKind::Codex,
    }
}

fn credential_source(value: &CredentialSource) -> String {
    match value {
        CredentialSource::NotRequired => "not_required".to_owned(),
        CredentialSource::Environment { variable } => format!("environment:{variable}"),
        CredentialSource::Stored { id } => format!("stored:{id}"),
        CredentialSource::ManagedAccount => "managed_account".to_owned(),
    }
}

fn account_state(value: &ManagedAccountState) -> String {
    match value {
        ManagedAccountState::NotApplicable => "not_applicable".to_owned(),
        ManagedAccountState::Unknown => "unknown".to_owned(),
        ManagedAccountState::LoggedOut => "logged_out".to_owned(),
        ManagedAccountState::LoggedIn { kind } => format!("logged_in:{kind}"),
    }
}

fn reachability(value: ReachabilityState) -> String {
    match value {
        ReachabilityState::NotTested => "not_tested",
        ReachabilityState::Reachable => "reachable",
        ReachabilityState::Unreachable => "unreachable",
        ReachabilityState::Unknown => "unknown",
    }
    .to_owned()
}

fn selected_model_state(value: SelectedModelState) -> String {
    match value {
        SelectedModelState::Available => "available",
        SelectedModelState::Unavailable => "unavailable",
        SelectedModelState::NotSelected => "not_selected",
    }
    .to_owned()
}

fn health(value: ConnectionHealth) -> String {
    format!("{value:?}").to_lowercase()
}

fn recovery(value: RecoveryAction) -> String {
    format!("{value:?}").to_lowercase()
}

fn bounded(mut value: String) -> String {
    if value.len() <= MAX_PUBLIC_TEXT_BYTES {
        return value;
    }
    let mut end = MAX_PUBLIC_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push('…');
    value
}

fn control_error(error: impl fmt::Display) -> DesktopError {
    DesktopError::new(DesktopErrorCode::StateInvalid, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, XanaConfig},
        shell::ShellConfig,
    };
    use std::fs;
    use tempfile::tempdir;

    fn control() -> (tempfile::TempDir, DesktopControlPlane) {
        let directory = tempdir().unwrap();
        let control =
            DesktopControlPlane::resolve(Some(directory.path().join("home").into_os_string()))
                .unwrap();
        (directory, control)
    }

    #[test]
    fn setup_snapshot_is_typed_and_side_effect_free() {
        let (_directory, control) = control();
        let snapshot = control.setup_snapshot().unwrap();
        assert_eq!(snapshot.configuration_state, "missing");
        assert!(!snapshot.intentionally_blank);
        assert_eq!(snapshot.providers, DesktopProviderKind::ALL);
        assert!(!control.paths.config_file().exists());
    }

    #[test]
    fn blank_commit_is_explicit_and_never_creates_config() {
        let (_directory, control) = control();
        // Existing-home compatibility path; fresh protection uses injected
        // custody in setup::storage tests, never the developer's key store.
        fs::create_dir_all(control.paths.data_dir()).unwrap();
        fs::write(control.paths.data_dir().join("retained"), b"existing").unwrap();
        let receipt = control.commit_blank().unwrap();
        assert_eq!(receipt.semantic_code, "setup.blank.completed.v1");
        assert!(control.setup_snapshot().unwrap().intentionally_blank);
        assert!(!control.paths.config_file().exists());
    }

    #[test]
    fn connection_snapshot_is_bounded_and_secret_free() {
        let (_directory, control) = control();
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "ollama".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        fs::create_dir_all(control.paths.config_file().parent().unwrap()).unwrap();
        fs::write(control.paths.config_file(), rendered).unwrap();

        let snapshot = control.connections().unwrap();
        assert_eq!(snapshot.connections.len(), 1);
        assert_eq!(snapshot.connections[0].id, "ollama");
        assert_eq!(snapshot.connections[0].credential_source, "not_required");
        assert!(!format!("{snapshot:?}").contains("api_key"));
    }

    #[test]
    fn desktop_secret_never_formats_its_value() {
        let secret = DesktopSecret::new("do-not-print".to_owned()).unwrap();
        assert_eq!(format!("{secret:?}"), "DesktopSecret(REDACTED)");
    }
}
