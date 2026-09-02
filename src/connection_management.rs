//! Typed, presentation-neutral connection management state and mutations.
//!
//! Frontends consume these read models and receipts. They never inspect the
//! configuration document or credential store directly, and no secret value
//! can be represented by these types.

use crate::{
    config::{ConnectionConfig, ConnectionRegistry, CredentialReference, ProviderKind, XanaConfig},
    credential::CredentialAvailability,
    model_catalog::{
        CatalogCacheState, CatalogMetadata, ExecutionKind, ModelDescriptor, ModelManager,
        ModelSelection,
    },
    paths::XanaPaths,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, path::PathBuf, time::Duration};

pub(crate) const CONNECTION_STATE_VERSION: u16 = 1;
const FRESH_CATALOG_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub(crate) enum CredentialSource {
    NotRequired,
    Environment { variable: String },
    Stored { id: String },
    ManagedAccount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CredentialState {
    Available,
    Missing,
    Inaccessible,
    NotRequired,
    ManagedExternally,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum ManagedAccountState {
    NotApplicable,
    Unknown,
    LoggedOut,
    LoggedIn { kind: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReachabilityState {
    NotTested,
    Reachable,
    Unreachable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogFreshness {
    Fresh,
    Stale,
    ConfiguredOnly,
    NeverFetched,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogFacts {
    pub(crate) freshness: CatalogFreshness,
    pub(crate) fetched_at_unix_seconds: Option<u64>,
    pub(crate) cached_model_count: usize,
    pub(crate) available_model_count: usize,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SelectedModelState {
    Available,
    Unavailable,
    NotSelected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionHealth {
    Ready,
    Configured,
    CredentialMissing,
    CredentialInaccessible,
    NeedsLogin,
    CatalogStale,
    CatalogUnavailable,
    ModelUnavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryAction {
    None,
    AddCredential,
    CheckCredentialStore,
    Login,
    RefreshCatalog,
    SelectModel,
    TestConnection,
    Inspect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionFacets {
    pub(crate) credential: CredentialState,
    pub(crate) account: ManagedAccountState,
    pub(crate) reachability: ReachabilityState,
    pub(crate) catalog: CatalogFacts,
    pub(crate) selected_model: SelectedModelState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionSummaryView {
    pub(crate) id: String,
    pub(crate) provider: ProviderKind,
    pub(crate) execution: ExecutionKind,
    pub(crate) selected_for_new_conversations: bool,
    pub(crate) selected_model: Option<String>,
    pub(crate) default_profile: bool,
    pub(crate) profile_references: Vec<String>,
    pub(crate) credential_source: CredentialSource,
    pub(crate) facets: ConnectionFacets,
    pub(crate) health: ConnectionHealth,
    pub(crate) recovery: RecoveryAction,
    pub(crate) models: Vec<ModelDescriptor>,
}

impl ConnectionSummaryView {
    pub(crate) fn observe_managed_account(&mut self, account: ManagedAccountState) {
        self.facets.account = account;
        self.facets.reachability = ReachabilityState::Reachable;
        (self.health, self.recovery) = derive_health(&self.facets);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionSnapshot {
    pub(crate) version: u16,
    pub(crate) connections: Vec<ConnectionSummaryView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "reference")]
pub(crate) enum RemovalBlocker {
    SelectedForNewConversations,
    DefaultProfile(String),
    Profile(String),
    ActiveConversation(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemovalPlan {
    pub(crate) connection: String,
    pub(crate) blockers: Vec<RemovalBlocker>,
    pub(crate) retains_credential: bool,
    pub(crate) retains_managed_account: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionEffect {
    Validated,
    Tested,
    Repaired,
    Added,
    Removed,
    SelectedForNewConversations,
    CredentialReplaced,
    CredentialDeleted,
    ManagedLoginCompleted,
    ManagedLoginCancelled,
    ManagedLogoutCompleted,
    CatalogRefreshed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionProgressStage {
    Validating,
    CheckingAuthority,
    DiscoveringCatalog,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionProgress {
    pub(crate) semantic_code: &'static str,
    pub(crate) stage: ConnectionProgressStage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionTestReceipt {
    pub(crate) version: u16,
    pub(crate) semantic_code: &'static str,
    pub(crate) effect: ConnectionEffect,
    pub(crate) connection: String,
    pub(crate) execution: ExecutionKind,
    pub(crate) reachability: ReachabilityState,
    pub(crate) credential: CredentialState,
    pub(crate) account: ManagedAccountState,
    pub(crate) discovered_model_count: usize,
    pub(crate) usable: bool,
    pub(crate) recovery: RecoveryAction,
    pub(crate) failure: Option<String>,
    pub(crate) progress: Vec<ConnectionProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionReceipt {
    pub(crate) version: u16,
    pub(crate) semantic_code: &'static str,
    pub(crate) connection: String,
    pub(crate) effect: ConnectionEffect,
    pub(crate) backup: Option<PathBuf>,
    pub(crate) retained_authority: Vec<&'static str>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionErrorCode {
    UnknownConnection,
    DuplicateConnection,
    RemovalBlocked,
    InvalidDraft,
    StateUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectionManagementError {
    pub(crate) code: ConnectionErrorCode,
    pub(crate) connection: Option<String>,
    pub(crate) message: String,
    pub(crate) blockers: Vec<RemovalBlocker>,
}

impl fmt::Display for ConnectionManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ConnectionManagementError {}

pub(crate) struct ConnectionManagement<'a> {
    paths: &'a XanaPaths,
    registry: ConnectionRegistry,
    models: ModelManager,
}

impl<'a> ConnectionManagement<'a> {
    pub(crate) fn open(paths: &'a XanaPaths) -> Result<Self, ConnectionManagementError> {
        let registry = XanaConfig::load_registry_from(paths.config_file())
            .map_err(|error| state_error(error.to_string()))?;
        let models = ModelManager::new(
            registry.clone(),
            paths.cache_dir().to_owned(),
            paths.data_dir().join("selection.toml"),
        );
        Ok(Self {
            paths,
            registry,
            models,
        })
    }

    pub(crate) fn snapshot(&self) -> Result<ConnectionSnapshot, ConnectionManagementError> {
        self.snapshot_at(unix_now())
    }

    fn snapshot_at(&self, now: u64) -> Result<ConnectionSnapshot, ConnectionManagementError> {
        let selected = self
            .models
            .selected()
            .map_err(|error| state_error(error.to_string()))?;
        let connections = self
            .registry
            .connections
            .values()
            .map(|connection| self.summary(connection, &selected, now))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ConnectionSnapshot {
            version: CONNECTION_STATE_VERSION,
            connections,
        })
    }

    pub(crate) fn removal_plan(
        &self,
        id: &str,
        active_conversations: impl IntoIterator<Item = String>,
    ) -> Result<RemovalPlan, ConnectionManagementError> {
        let connection =
            self.registry
                .connections
                .get(id)
                .ok_or_else(|| ConnectionManagementError {
                    code: ConnectionErrorCode::UnknownConnection,
                    connection: Some(id.to_owned()),
                    message: format!("unknown connection {id:?}"),
                    blockers: Vec::new(),
                })?;
        let selected = self
            .models
            .selected()
            .map_err(|error| state_error(error.to_string()))?;
        let mut blockers = Vec::new();
        if selected.connection == id {
            blockers.push(RemovalBlocker::SelectedForNewConversations);
        }
        for profile in self.registry.profiles.values() {
            if profile.connection != id {
                continue;
            }
            if profile.id == self.registry.default_profile {
                blockers.push(RemovalBlocker::DefaultProfile(profile.id.clone()));
            } else {
                blockers.push(RemovalBlocker::Profile(profile.id.clone()));
            }
        }
        blockers.extend(
            active_conversations
                .into_iter()
                .map(RemovalBlocker::ActiveConversation),
        );
        blockers.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
        Ok(RemovalPlan {
            connection: id.to_owned(),
            blockers,
            retains_credential: matches!(
                connection.credential,
                Some(CredentialReference::Stored { .. })
            ),
            retains_managed_account: connection.kind == ProviderKind::Codex,
        })
    }

    pub(crate) fn remove(
        &self,
        plan: &RemovalPlan,
    ) -> Result<ConnectionReceipt, ConnectionManagementError> {
        if !plan.blockers.is_empty() {
            return Err(ConnectionManagementError {
                code: ConnectionErrorCode::RemovalBlocked,
                connection: Some(plan.connection.clone()),
                message: format!(
                    "connection {:?} cannot be removed until {} blocking reference(s) are resolved",
                    plan.connection,
                    plan.blockers.len()
                ),
                blockers: plan.blockers.clone(),
            });
        }
        XanaConfig::remove_connection(self.paths.config_file(), &plan.connection)
            .map_err(|error| mutation_error(&plan.connection, error.to_string()))?;
        let mut retained_authority = Vec::new();
        if plan.retains_credential {
            retained_authority.push("stored_credential");
        }
        if plan.retains_managed_account {
            retained_authority.push("managed_account");
        }
        Ok(ConnectionReceipt {
            version: CONNECTION_STATE_VERSION,
            semantic_code: "connection.remove.completed.v1",
            connection: plan.connection.clone(),
            effect: ConnectionEffect::Removed,
            backup: Some(self.paths.config_file().with_extension("toml.bak")),
            retained_authority,
            warnings: Vec::new(),
        })
    }

    pub(crate) fn select(
        &self,
        connection: &str,
        model: &str,
        reasoning_effort: Option<String>,
        reasoning_summary: Option<crate::model_catalog::ReasoningSummary>,
    ) -> Result<(ModelSelection, ConnectionReceipt), ConnectionManagementError> {
        let selection = self
            .models
            .select_with_options(connection, model, reasoning_effort, reasoning_summary)
            .map_err(|error| mutation_error(connection, error.to_string()))?;
        let receipt = ConnectionReceipt {
            version: CONNECTION_STATE_VERSION,
            semantic_code: "connection.selection.completed.v1",
            connection: connection.to_owned(),
            effect: ConnectionEffect::SelectedForNewConversations,
            backup: None,
            retained_authority: Vec::new(),
            warnings: Vec::new(),
        };
        Ok((selection, receipt))
    }

    fn summary(
        &self,
        connection: &ConnectionConfig,
        selected: &ModelSelection,
        now: u64,
    ) -> Result<ConnectionSummaryView, ConnectionManagementError> {
        let models = self.models.models_for(connection);
        let credential_source = credential_source(connection);
        let credential = if connection.kind == ProviderKind::Codex {
            CredentialState::ManagedExternally
        } else if connection.credential.is_none() {
            CredentialState::NotRequired
        } else {
            match self.models.credential_availability(connection) {
                Ok(CredentialAvailability::Available) => CredentialState::Available,
                Ok(CredentialAvailability::Missing) => CredentialState::Missing,
                Err(_) => CredentialState::Inaccessible,
            }
        };
        let metadata = self
            .models
            .catalog_metadata(&connection.id)
            .map_err(|error| state_error(error.to_string()))?;
        let catalog = catalog_facts(metadata, models.len(), now);
        let selected_for_new_conversations = selected.connection == connection.id;
        let selected_model = selected_for_new_conversations.then(|| selected.model.clone());
        let selected_model_state =
            selected_model
                .as_ref()
                .map_or(SelectedModelState::NotSelected, |selected| {
                    if models.iter().any(|model| model.id == *selected) {
                        SelectedModelState::Available
                    } else {
                        SelectedModelState::Unavailable
                    }
                });
        let profile_references = self
            .registry
            .profiles
            .values()
            .filter(|profile| profile.connection == connection.id)
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        let account = if connection.kind == ProviderKind::Codex {
            ManagedAccountState::Unknown
        } else {
            ManagedAccountState::NotApplicable
        };
        let facets = ConnectionFacets {
            credential,
            account,
            reachability: ReachabilityState::NotTested,
            catalog,
            selected_model: selected_model_state,
        };
        let (health, recovery) = derive_health(&facets);
        Ok(ConnectionSummaryView {
            id: connection.id.clone(),
            provider: connection.kind,
            execution: if connection.kind == ProviderKind::Codex {
                ExecutionKind::Managed
            } else {
                ExecutionKind::Native
            },
            selected_for_new_conversations,
            selected_model,
            default_profile: profile_references
                .iter()
                .any(|profile| profile == &self.registry.default_profile),
            profile_references,
            credential_source,
            facets,
            health,
            recovery,
            models,
        })
    }
}

fn credential_source(connection: &ConnectionConfig) -> CredentialSource {
    if connection.kind == ProviderKind::Codex {
        return CredentialSource::ManagedAccount;
    }
    match &connection.credential {
        None => CredentialSource::NotRequired,
        Some(CredentialReference::Environment { variable }) => CredentialSource::Environment {
            variable: variable.clone(),
        },
        Some(CredentialReference::Stored { id }) => CredentialSource::Stored { id: id.clone() },
    }
}

fn catalog_facts(
    metadata: CatalogMetadata,
    available_model_count: usize,
    now: u64,
) -> CatalogFacts {
    let freshness = match metadata.state {
        CatalogCacheState::NeverFetched if available_model_count > 0 => {
            CatalogFreshness::ConfiguredOnly
        }
        CatalogCacheState::NeverFetched => CatalogFreshness::NeverFetched,
        CatalogCacheState::Unavailable => CatalogFreshness::Unavailable,
        CatalogCacheState::Cached => {
            metadata
                .fetched_at_unix_seconds
                .map_or(CatalogFreshness::Unknown, |fetched_at| {
                    if now.saturating_sub(fetched_at) <= FRESH_CATALOG_AGE.as_secs() {
                        CatalogFreshness::Fresh
                    } else {
                        CatalogFreshness::Stale
                    }
                })
        }
    };
    CatalogFacts {
        freshness,
        fetched_at_unix_seconds: metadata.fetched_at_unix_seconds,
        cached_model_count: metadata.model_count,
        available_model_count,
        error: metadata.error,
    }
}

fn derive_health(facets: &ConnectionFacets) -> (ConnectionHealth, RecoveryAction) {
    match facets.credential {
        CredentialState::Missing => {
            return (
                ConnectionHealth::CredentialMissing,
                RecoveryAction::AddCredential,
            );
        }
        CredentialState::Inaccessible => {
            return (
                ConnectionHealth::CredentialInaccessible,
                RecoveryAction::CheckCredentialStore,
            );
        }
        _ => {}
    }
    if facets.account == ManagedAccountState::LoggedOut {
        return (ConnectionHealth::NeedsLogin, RecoveryAction::Login);
    }
    if facets.credential == CredentialState::ManagedExternally
        && facets.account == ManagedAccountState::Unknown
    {
        return (ConnectionHealth::Unknown, RecoveryAction::Inspect);
    }
    if facets.selected_model == SelectedModelState::Unavailable {
        return (
            ConnectionHealth::ModelUnavailable,
            RecoveryAction::SelectModel,
        );
    }
    match facets.catalog.freshness {
        CatalogFreshness::Unavailable => (
            ConnectionHealth::CatalogUnavailable,
            RecoveryAction::RefreshCatalog,
        ),
        CatalogFreshness::Stale => (
            ConnectionHealth::CatalogStale,
            RecoveryAction::RefreshCatalog,
        ),
        CatalogFreshness::Fresh
            if (matches!(
                facets.credential,
                CredentialState::Available | CredentialState::NotRequired
            ) || (facets.credential == CredentialState::ManagedExternally
                && matches!(facets.account, ManagedAccountState::LoggedIn { .. })))
                && facets.selected_model != SelectedModelState::Unavailable =>
        {
            (ConnectionHealth::Ready, RecoveryAction::None)
        }
        CatalogFreshness::ConfiguredOnly | CatalogFreshness::NeverFetched => {
            (ConnectionHealth::Configured, RecoveryAction::TestConnection)
        }
        CatalogFreshness::Unknown | CatalogFreshness::Fresh => {
            (ConnectionHealth::Unknown, RecoveryAction::Inspect)
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn state_error(message: String) -> ConnectionManagementError {
    ConnectionManagementError {
        code: ConnectionErrorCode::StateUnavailable,
        connection: None,
        message,
        blockers: Vec::new(),
    }
}

fn mutation_error(connection: &str, message: String) -> ConnectionManagementError {
    let code = if message.contains("already exists") {
        ConnectionErrorCode::DuplicateConnection
    } else if message.contains("unknown") {
        ConnectionErrorCode::UnknownConnection
    } else {
        ConnectionErrorCode::InvalidDraft
    };
    ConnectionManagementError {
        code,
        connection: Some(connection.to_owned()),
        message,
        blockers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode},
        shell::ShellConfig,
    };
    use tempfile::tempdir;

    fn facets(
        credential: CredentialState,
        account: ManagedAccountState,
        catalog: CatalogFreshness,
        model: SelectedModelState,
    ) -> ConnectionFacets {
        ConnectionFacets {
            credential,
            account,
            reachability: ReachabilityState::NotTested,
            catalog: CatalogFacts {
                freshness: catalog,
                fetched_at_unix_seconds: None,
                cached_model_count: 0,
                available_model_count: 1,
                error: None,
            },
            selected_model: model,
        }
    }

    #[test]
    fn health_never_turns_unknown_or_missing_facts_into_ready() {
        let cases = [
            (
                facets(
                    CredentialState::Missing,
                    ManagedAccountState::NotApplicable,
                    CatalogFreshness::Fresh,
                    SelectedModelState::Available,
                ),
                ConnectionHealth::CredentialMissing,
            ),
            (
                facets(
                    CredentialState::ManagedExternally,
                    ManagedAccountState::Unknown,
                    CatalogFreshness::Fresh,
                    SelectedModelState::Available,
                ),
                ConnectionHealth::Unknown,
            ),
            (
                facets(
                    CredentialState::Available,
                    ManagedAccountState::NotApplicable,
                    CatalogFreshness::ConfiguredOnly,
                    SelectedModelState::Available,
                ),
                ConnectionHealth::Configured,
            ),
            (
                facets(
                    CredentialState::Available,
                    ManagedAccountState::NotApplicable,
                    CatalogFreshness::Fresh,
                    SelectedModelState::Unavailable,
                ),
                ConnectionHealth::ModelUnavailable,
            ),
        ];
        for (facts, expected) in cases {
            assert_eq!(derive_health(&facts).0, expected);
        }
    }

    #[test]
    fn catalog_age_preserves_legacy_unknown_and_explicit_staleness() {
        let legacy = catalog_facts(
            CatalogMetadata {
                state: CatalogCacheState::Cached,
                fetched_at_unix_seconds: None,
                model_count: 1,
                error: None,
            },
            1,
            100,
        );
        assert_eq!(legacy.freshness, CatalogFreshness::Unknown);
        let stale = catalog_facts(
            CatalogMetadata {
                state: CatalogCacheState::Cached,
                fetched_at_unix_seconds: Some(1),
                model_count: 1,
                error: None,
            },
            1,
            FRESH_CATALOG_AGE.as_secs() + 2,
        );
        assert_eq!(stale.freshness, CatalogFreshness::Stale);
    }

    #[test]
    fn removal_plan_names_selection_profiles_and_retained_authority() {
        let directory = tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        std::fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        let config = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Codex {
                name: "codex".to_owned(),
                program: "codex".to_owned(),
                home: None,
            },
            model: "gpt-test".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
        let manager = ConnectionManagement::open(&paths).unwrap();
        let plan = manager
            .removal_plan("codex", ["conversation-1".to_owned()])
            .unwrap();
        assert!(
            plan.blockers
                .contains(&RemovalBlocker::SelectedForNewConversations)
        );
        assert!(plan.blockers.iter().any(
            |blocker| matches!(blocker, RemovalBlocker::DefaultProfile(profile) if profile == "default")
        ));
        assert!(plan.retains_managed_account);
        assert!(!plan.retains_credential);
    }

    #[test]
    fn typed_selection_receipt_names_future_conversation_scope() {
        let directory = tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        std::fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        let config = XanaConfig::render_initial(InitialConfig {
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
        std::fs::write(paths.config_file(), config).unwrap();
        let manager = ConnectionManagement::open(&paths).unwrap();
        let (_, receipt) = manager.select("ollama", "qwen", None, None).unwrap();
        assert_eq!(
            receipt.effect,
            ConnectionEffect::SelectedForNewConversations
        );
        assert_eq!(receipt.semantic_code, "connection.selection.completed.v1");
    }
}
