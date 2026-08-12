//! Deterministic profile lifecycle, resolution, and immutable conversation snapshots.
//!
//! Profiles select guidance and capability ceilings. They never own credentials,
//! provider availability, or execution authority, and project profiles can only
//! narrow the named user-global authority profile.

use crate::{
    config::{
        ConnectionRegistry, McpPrimitiveSelection, OrchestrationLimits, OutboundDataClass,
        PermissionMode, ProfileConfig, ProfileUpdate, ProfileUse, XanaConfig,
    },
    identity::{ProjectId, SessionId},
    paths::XanaPaths,
    plugin::{PluginError, PluginManager, PluginScope},
    portable_project::{PortableProjectError, PortableProjectStore},
    private_state::{
        FrozenProfileSnapshot, PrivateStateError, ProjectLifecycle, ProjectRegistryDocument,
        UpdateDocumentError, read_document, update_document,
    },
    project::ProjectError,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProfileScope {
    Global,
    Project(ProjectId),
}

impl fmt::Display for ProfileScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => formatter.write_str("global"),
            Self::Project(project) => write!(formatter, "project:{project}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub(crate) enum ProfileProvenance {
    CoreDefault,
    UserPolicy,
    GlobalProfile { profile: String },
    ProjectProfile { project: ProjectId, profile: String },
    LocalBinding { logical: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvedField<T> {
    pub(crate) value: T,
    pub(crate) provenance: ProfileProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvedProfile {
    pub(crate) profile_id: uuid::Uuid,
    pub(crate) name: String,
    pub(crate) scope: ProfileScope,
    pub(crate) applies_to: ResolvedField<Vec<ProfileUse>>,
    pub(crate) connection: ResolvedField<String>,
    pub(crate) model: ResolvedField<String>,
    pub(crate) reasoning_effort: ResolvedField<Option<String>>,
    pub(crate) reasoning_summary: ResolvedField<Option<String>>,
    pub(crate) identity: ResolvedField<Option<String>>,
    pub(crate) capabilities: ResolvedField<Option<Vec<String>>>,
    pub(crate) permission_mode: ResolvedField<PermissionMode>,
    pub(crate) max_tool_rounds: ResolvedField<usize>,
    pub(crate) orchestration: ResolvedField<OrchestrationLimits>,
    pub(crate) skills: ResolvedField<Vec<String>>,
    pub(crate) plugins: ResolvedField<Vec<String>>,
    /// Exact locally reviewed package revisions selected when this profile was resolved.
    #[serde(default)]
    pub(crate) plugin_revisions: BTreeMap<String, String>,
    pub(crate) mcp_servers: ResolvedField<Vec<String>>,
    pub(crate) mcp_allowlists: ResolvedField<BTreeMap<String, McpPrimitiveSelection>>,
    pub(crate) external_agents: ResolvedField<Vec<String>>,
    pub(crate) service_routes: ResolvedField<Vec<String>>,
    pub(crate) egress: ResolvedField<Vec<OutboundDataClass>>,
    pub(crate) readiness: Vec<String>,
}

impl ResolvedProfile {
    pub(crate) fn is_ready(&self) -> bool {
        self.readiness.is_empty()
    }

    pub(crate) fn redacted_json(&self) -> Result<serde_json::Value, ProfileError> {
        serde_json::to_value(self).map_err(ProfileError::Encode)
    }

    fn digest(&self) -> Result<String, ProfileError> {
        let bytes = serde_json::to_vec(self).map_err(ProfileError::Encode)?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProfileStore {
    paths: XanaPaths,
    config_file: PathBuf,
    projects_file: PathBuf,
}

impl ProfileStore {
    pub(crate) fn open(paths: &XanaPaths) -> Self {
        Self {
            paths: paths.clone(),
            config_file: paths.config_file().to_owned(),
            projects_file: paths.projects_file(),
        }
    }

    pub(crate) fn list_global(
        &self,
        include_archived: bool,
    ) -> Result<Vec<ProfileConfig>, ProfileError> {
        let registry = self.registry()?;
        Ok(registry
            .profiles
            .into_values()
            .filter(|profile| include_archived || !profile.archived)
            .collect())
    }

    pub(crate) fn inspect_global(&self, name: &str) -> Result<ProfileConfig, ProfileError> {
        self.registry()?
            .profiles
            .remove(name)
            .ok_or_else(|| ProfileError::Unknown(name.to_owned()))
    }

    pub(crate) fn create_global(
        &self,
        name: String,
        connection: String,
        model: String,
    ) -> Result<ProfileConfig, ProfileError> {
        XanaConfig::add_profile(
            &self.config_file,
            crate::config::NewProfile {
                id: name.clone(),
                connection,
                model,
            },
        )?;
        self.inspect_global(&name)
    }

    pub(crate) fn edit_global(
        &self,
        name: &str,
        update: ProfileUpdate,
    ) -> Result<ProfileConfig, ProfileError> {
        XanaConfig::update_profile(&self.config_file, name, update)?;
        self.inspect_global(name)
    }

    pub(crate) fn duplicate_global(
        &self,
        source: &str,
        name: &str,
    ) -> Result<ProfileConfig, ProfileError> {
        XanaConfig::duplicate_profile(&self.config_file, source, name)?;
        self.inspect_global(name)
    }

    pub(crate) fn rename_global(
        &self,
        old: &str,
        new: &str,
    ) -> Result<ProfileConfig, ProfileError> {
        XanaConfig::rename_profile(&self.config_file, old, new)?;
        self.inspect_global(new)
    }

    pub(crate) fn set_global_archived(
        &self,
        name: &str,
        archived: bool,
    ) -> Result<ProfileConfig, ProfileError> {
        XanaConfig::set_profile_archived(&self.config_file, name, archived)?;
        self.inspect_global(name)
    }

    pub(crate) fn delete_global(&self, name: &str) -> Result<(), ProfileError> {
        XanaConfig::delete_profile(&self.config_file, name).map_err(ProfileError::Config)
    }

    pub(crate) fn resolve_global(&self, name: &str) -> Result<ResolvedProfile, ProfileError> {
        let registry = self.registry()?;
        let profile = registry
            .profiles
            .get(name)
            .ok_or_else(|| ProfileError::Unknown(name.to_owned()))?;
        if profile.archived {
            return Err(ProfileError::Archived(name.to_owned()));
        }
        let mut resolved = resolve_global_profile(name, profile, &registry);
        let (revisions, readiness) = PluginManager::open(&self.paths).resolve_profile_plugins(
            &resolved.plugins.value,
            &PluginScope::Profile {
                project: None,
                profile: name.to_owned(),
            },
        )?;
        resolved.plugin_revisions = revisions;
        resolved.readiness.extend(readiness);
        Ok(resolved)
    }

    pub(crate) fn resolve_project(
        &self,
        paths: &XanaPaths,
        project: ProjectId,
        name: &str,
    ) -> Result<ResolvedProfile, ProfileError> {
        let registry = self.registry()?;
        let portable = PortableProjectStore::open(paths).resolve(paths, project)?;
        let profile = portable
            .manifest
            .profiles
            .get(name)
            .ok_or_else(|| ProfileError::Unknown(name.to_owned()))?;
        if profile.archived {
            return Err(ProfileError::Archived(name.to_owned()));
        }
        let ceiling = registry
            .profiles
            .get(&profile.authority_profile)
            .ok_or_else(|| ProfileError::Unknown(profile.authority_profile.clone()))?;
        let provenance = ProfileProvenance::ProjectProfile {
            project,
            profile: name.to_owned(),
        };
        let connection = portable.bindings.get(&profile.connection).cloned();
        let mut readiness = portable
            .missing
            .iter()
            .map(|value| format!("bind project requirement {value:?} with `xana project bind {project} {value} LOCAL_NAME`"))
            .collect::<Vec<_>>();
        if portable.stale {
            readiness.push(format!(
                "portable project changed; review and run `xana project refresh {project}`"
            ));
        }
        let connection = match connection {
            Some(connection) if registry.connections.contains_key(&connection) => connection,
            Some(connection) => {
                readiness.push(format!("local connection {connection:?} no longer exists"));
                connection
            }
            None => profile.connection.clone(),
        };
        readiness.extend(integration_readiness(
            &profile.mcp_servers,
            &profile.external_agents,
            &profile.service_routes,
            &registry,
        ));
        let (plugin_revisions, plugin_readiness) = PluginManager::open(paths)
            .resolve_profile_plugins(
                &profile.plugins,
                &PluginScope::Profile {
                    project: Some(project),
                    profile: name.to_owned(),
                },
            )?;
        readiness.extend(plugin_readiness);
        let effective_permission = profile
            .permission_mode
            .unwrap_or_else(|| ceiling.permission_mode.unwrap_or(registry.permission_mode));
        let effective_rounds = profile.max_tool_rounds.unwrap_or(ceiling.max_tool_rounds);
        let effective_limits = profile
            .orchestration
            .clone()
            .unwrap_or_else(|| ceiling.orchestration.clone());
        Ok(ResolvedProfile {
            profile_id: PortableProjectStore::profile_id(&portable.manifest, name, profile),
            name: name.to_owned(),
            scope: ProfileScope::Project(project),
            applies_to: field(profile.applies_to.clone(), provenance.clone()),
            connection: field(
                connection,
                ProfileProvenance::LocalBinding {
                    logical: profile.connection.clone(),
                },
            ),
            model: field(profile.model.clone(), provenance.clone()),
            reasoning_effort: field(profile.reasoning_effort.clone(), provenance.clone()),
            reasoning_summary: field(profile.reasoning_summary.clone(), provenance.clone()),
            identity: field(profile.identity.clone(), provenance.clone()),
            capabilities: field(Some(profile.capabilities.clone()), provenance.clone()),
            permission_mode: field(effective_permission, provenance.clone()),
            max_tool_rounds: field(effective_rounds, provenance.clone()),
            orchestration: field(effective_limits, provenance.clone()),
            skills: field(profile.skills.clone(), provenance.clone()),
            plugins: field(profile.plugins.clone(), provenance.clone()),
            plugin_revisions,
            mcp_servers: field(profile.mcp_servers.clone(), provenance.clone()),
            mcp_allowlists: field(profile.mcp_allowlists.clone(), provenance.clone()),
            external_agents: field(profile.external_agents.clone(), provenance.clone()),
            service_routes: field(profile.service_routes.clone(), provenance.clone()),
            egress: field(profile.egress.clone(), provenance),
            readiness,
        })
    }

    pub(crate) fn freeze(
        &self,
        conversation: &str,
        profile: &ResolvedProfile,
    ) -> Result<FrozenProfileSnapshot, ProfileError> {
        validate_conversation(conversation)?;
        let snapshot = FrozenProfileSnapshot {
            profile_id: profile.profile_id.to_string(),
            profile_name: profile.name.clone(),
            scope: profile.scope.to_string(),
            digest: profile.digest()?,
            resolved: profile.redacted_json()?,
        };
        self.update_projects(|document| {
            if let Some(existing) = document.conversation_profiles.get(conversation) {
                if existing == &snapshot {
                    return Ok(existing.clone());
                }
                return Err(ProfileError::Immutable(conversation.to_owned()));
            }
            document
                .conversation_profiles
                .insert(conversation.to_owned(), snapshot.clone());
            Ok(snapshot)
        })
    }

    pub(crate) fn snapshot(
        &self,
        conversation: &str,
    ) -> Result<Option<FrozenProfileSnapshot>, ProfileError> {
        validate_conversation(conversation)?;
        Ok(
            read_document::<ProjectRegistryDocument>(&self.projects_file)?
                .conversation_profiles
                .remove(conversation),
        )
    }

    pub(crate) fn continue_with(
        &self,
        source: &str,
        resolved: &ResolvedProfile,
    ) -> Result<SessionId, ProfileError> {
        validate_conversation(source)?;
        let next = SessionId::new();
        let next_key = next.to_string();
        let snapshot = FrozenProfileSnapshot {
            profile_id: resolved.profile_id.to_string(),
            profile_name: resolved.name.clone(),
            scope: resolved.scope.to_string(),
            digest: resolved.digest()?,
            resolved: resolved.redacted_json()?,
        };
        self.update_projects(|document| {
            if !document.conversation_profiles.contains_key(source) {
                return Err(ProfileError::MissingSnapshot(source.to_owned()));
            }
            if let Some(project) = document.conversation_memberships.get(source).copied() {
                document
                    .conversation_memberships
                    .insert(next_key.clone(), project);
            }
            document
                .conversation_profiles
                .insert(next_key.clone(), snapshot);
            document
                .conversation_predecessors
                .insert(next_key.clone(), source.to_owned());
            Ok(())
        })?;
        Ok(next)
    }

    pub(crate) fn commit_project_continuation(
        &self,
        source: &str,
        target: &str,
        project: ProjectId,
        resolved: &ResolvedProfile,
    ) -> Result<(), ProfileError> {
        validate_conversation(source)?;
        validate_conversation(target)?;
        let snapshot = FrozenProfileSnapshot {
            profile_id: resolved.profile_id.to_string(),
            profile_name: resolved.name.clone(),
            scope: resolved.scope.to_string(),
            digest: resolved.digest()?,
            resolved: resolved.redacted_json()?,
        };
        self.update_projects(|document| {
            let record = document
                .projects
                .get(&project)
                .ok_or(ProjectError::Unknown(project))?;
            if record.lifecycle != ProjectLifecycle::Active {
                return Err(ProjectError::Archived(project).into());
            }
            if document.conversation_profiles.contains_key(target) {
                return Err(ProfileError::Immutable(target.to_owned()));
            }
            document
                .conversation_memberships
                .insert(target.to_owned(), project);
            document
                .conversation_profiles
                .insert(target.to_owned(), snapshot);
            document
                .conversation_predecessors
                .insert(target.to_owned(), source.to_owned());
            Ok(())
        })
    }

    pub(crate) fn predecessor(&self, conversation: &str) -> Result<Option<String>, ProfileError> {
        Ok(
            read_document::<ProjectRegistryDocument>(&self.projects_file)?
                .conversation_predecessors
                .remove(conversation),
        )
    }

    fn registry(&self) -> Result<ConnectionRegistry, ProfileError> {
        XanaConfig::load_registry_from(&self.config_file).map_err(ProfileError::Config)
    }

    fn update_projects<T>(
        &self,
        update: impl FnOnce(&mut ProjectRegistryDocument) -> Result<T, ProfileError>,
    ) -> Result<T, ProfileError> {
        match update_document(&self.projects_file, update) {
            Ok(output) => Ok(output),
            Err(UpdateDocumentError::State(error)) => Err(ProfileError::State(error)),
            Err(UpdateDocumentError::Update(error)) => Err(error),
        }
    }
}

fn resolve_global_profile(
    name: &str,
    profile: &ProfileConfig,
    registry: &ConnectionRegistry,
) -> ResolvedProfile {
    let provenance = ProfileProvenance::GlobalProfile {
        profile: name.to_owned(),
    };
    let permission = match profile.permission_mode {
        Some(value) => field(value, provenance.clone()),
        None => field(registry.permission_mode, ProfileProvenance::UserPolicy),
    };
    let egress = profile
        .egress_policy
        .as_ref()
        .and_then(|name| registry.egress_policies.get(name))
        .map(|policy| policy.allowed.clone())
        .unwrap_or_default();
    ResolvedProfile {
        profile_id: profile.profile_id,
        name: name.to_owned(),
        scope: ProfileScope::Global,
        applies_to: field(profile.applies_to.clone(), provenance.clone()),
        connection: field(profile.connection.clone(), provenance.clone()),
        model: field(profile.model.clone(), provenance.clone()),
        reasoning_effort: field(profile.reasoning_effort.clone(), provenance.clone()),
        reasoning_summary: field(profile.reasoning_summary.clone(), provenance.clone()),
        identity: field(profile.identity.clone(), provenance.clone()),
        capabilities: field(profile.capabilities.clone(), provenance.clone()),
        permission_mode: permission,
        max_tool_rounds: field(profile.max_tool_rounds, provenance.clone()),
        orchestration: field(profile.orchestration.clone(), provenance.clone()),
        skills: field(profile.skills.clone(), provenance.clone()),
        plugins: field(profile.plugins.clone(), provenance.clone()),
        plugin_revisions: BTreeMap::new(),
        mcp_servers: field(profile.mcp_servers.clone(), provenance.clone()),
        mcp_allowlists: field(profile.mcp_allowlists.clone(), provenance.clone()),
        external_agents: field(profile.external_agents.clone(), provenance.clone()),
        service_routes: field(profile.service_routes.clone(), provenance.clone()),
        egress: field(egress, provenance),
        readiness: integration_readiness(
            &profile.mcp_servers,
            &profile.external_agents,
            &profile.service_routes,
            registry,
        ),
    }
}

fn integration_readiness(
    mcp_servers: &[String],
    external_agents: &[String],
    service_routes: &[String],
    registry: &ConnectionRegistry,
) -> Vec<String> {
    let mut missing = Vec::new();
    for server in mcp_servers {
        if !registry.mcp_servers.contains_key(server) {
            missing.push(format!("configure MCP server {server:?}"));
        }
    }
    for agent in external_agents {
        if !registry.external_agents.contains_key(agent) {
            missing.push(format!("configure external agent {agent:?}"));
        }
    }
    for route in service_routes {
        if !registry.service_routes.contains_key(route) {
            missing.push(format!("configure focused-service route {route:?}"));
        }
    }
    missing
}

fn field<T>(value: T, provenance: ProfileProvenance) -> ResolvedField<T> {
    ResolvedField { value, provenance }
}

fn validate_conversation(value: &str) -> Result<(), ProfileError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ProfileError::Invalid(
            "conversation identity must contain 1..=256 bytes without control characters".into(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ProfileError {
    Config(crate::config::ConfigError),
    Portable(PortableProjectError),
    Project(ProjectError),
    State(PrivateStateError),
    Plugin(PluginError),
    Encode(serde_json::Error),
    Unknown(String),
    Archived(String),
    Immutable(String),
    MissingSnapshot(String),
    Invalid(String),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Portable(error) => error.fmt(formatter),
            Self::Project(error) => error.fmt(formatter),
            Self::State(error) => error.fmt(formatter),
            Self::Plugin(error) => error.fmt(formatter),
            Self::Encode(error) => write!(formatter, "could not encode redacted profile: {error}"),
            Self::Unknown(name) => write!(formatter, "unknown profile {name:?}"),
            Self::Archived(name) => write!(formatter, "profile {name:?} is archived"),
            Self::Immutable(conversation) => write!(
                formatter,
                "conversation {conversation} already has a different immutable profile snapshot; create a linked continuation"
            ),
            Self::MissingSnapshot(conversation) => write!(
                formatter,
                "conversation {conversation} has no frozen profile snapshot"
            ),
            Self::Invalid(reason) => formatter.write_str(reason),
        }
    }
}

impl Error for ProfileError {}

impl From<crate::config::ConfigError> for ProfileError {
    fn from(value: crate::config::ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<PortableProjectError> for ProfileError {
    fn from(value: PortableProjectError) -> Self {
        Self::Portable(value)
    }
}

impl From<ProjectError> for ProfileError {
    fn from(value: ProjectError) -> Self {
        Self::Project(value)
    }
}

impl From<PrivateStateError> for ProfileError {
    fn from(value: PrivateStateError) -> Self {
        Self::State(value)
    }
}

impl From<PluginError> for ProfileError {
    fn from(value: PluginError) -> Self {
        Self::Plugin(value)
    }
}

#[cfg(test)]
mod tests;
