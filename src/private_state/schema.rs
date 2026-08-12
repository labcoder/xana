use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

use crate::config::OutboundDataClass;
use crate::identity::ProjectId;

pub(super) const PRIVATE_RECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectLifecycle {
    Active,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectRecord {
    pub(crate) id: ProjectId,
    pub(crate) name: String,
    pub(crate) canonical_workspace: PathBuf,
    pub(crate) lifecycle: ProjectLifecycle,
    pub(crate) created_unix_ms: u64,
    pub(crate) updated_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectRegistryDocument {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) projects: BTreeMap<ProjectId, ProjectRecord>,
    #[serde(default)]
    pub(crate) conversation_memberships: BTreeMap<String, ProjectId>,
    /// Immutable, redacted profile resolution recorded when a conversation starts.
    #[serde(default)]
    pub(crate) conversation_profiles: BTreeMap<String, FrozenProfileSnapshot>,
    /// Explicit continuation relation; the source conversation is never mutated.
    #[serde(default)]
    pub(crate) conversation_predecessors: BTreeMap<String, String>,
}

impl Default for ProjectRegistryDocument {
    fn default() -> Self {
        Self {
            version: PRIVATE_RECORD_VERSION,
            projects: BTreeMap::new(),
            conversation_memberships: BTreeMap::new(),
            conversation_profiles: BTreeMap::new(),
            conversation_predecessors: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenProfileSnapshot {
    pub(crate) profile_id: String,
    pub(crate) profile_name: String,
    pub(crate) scope: String,
    pub(crate) digest: String,
    /// Typed by the profile module on both write and read. This schema-level
    /// value keeps private-state persistence independent of profile resolution.
    pub(crate) resolved: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalBindingRecord {
    pub(crate) project_id: ProjectId,
    pub(crate) portable_root: PathBuf,
    #[serde(default)]
    pub(crate) manifest_digest: String,
    #[serde(default)]
    pub(crate) bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectBindingsDocument {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) projects: BTreeMap<ProjectId, LocalBindingRecord>,
}

impl Default for ProjectBindingsDocument {
    fn default() -> Self {
        Self {
            version: PRIVATE_RECORD_VERSION,
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledPackageRecord {
    pub(crate) package: String,
    #[serde(default)]
    pub(crate) source_digest: String,
    /// Digest of the version selected for new scopes and conversations.
    #[serde(default)]
    pub(crate) active_revision: String,
    #[serde(default)]
    pub(crate) retained_revisions: Vec<String>,
    #[serde(default)]
    pub(crate) enabled_scopes: Vec<String>,
    #[serde(default)]
    pub(crate) source: Option<PackageSourceRecord>,
    #[serde(default)]
    pub(crate) revisions: BTreeMap<String, PackageRevisionRecord>,
    #[serde(default)]
    pub(crate) pending_revision: Option<String>,
    #[serde(default)]
    pub(crate) previous_revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackageSourceKind {
    Directory,
    Git,
    Linked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageSourceRecord {
    pub(crate) kind: PackageSourceKind,
    /// User-selected location. This is private installation state and is never
    /// copied into portable project configuration.
    pub(crate) location: String,
    #[serde(default)]
    pub(crate) revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageRevisionRecord {
    pub(crate) digest: String,
    #[serde(default)]
    pub(crate) manifest_version: Option<String>,
    pub(crate) installed_unix_ms: u64,
    pub(crate) mutable: bool,
    /// Relative to Xana's managed plugin store for immutable versions. Linked
    /// development versions instead use `linked_root`.
    #[serde(default)]
    pub(crate) managed_root: Option<PathBuf>,
    #[serde(default)]
    pub(crate) linked_root: Option<PathBuf>,
    #[serde(default)]
    pub(crate) skill_names: Vec<String>,
    #[serde(default)]
    pub(crate) mcp_server_names: Vec<String>,
    #[serde(default)]
    pub(crate) capability_digest: String,
    /// Exact scopes approved for this reviewed capability set. Active package
    /// projection mirrors this list and rollback can restore it safely.
    #[serde(default)]
    pub(crate) approved_scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageStateDocument {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) plugins: BTreeMap<String, InstalledPackageRecord>,
}

impl Default for PackageStateDocument {
    fn default() -> Self {
        Self {
            version: PRIVATE_RECORD_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EndpointKind {
    McpHttp,
    ExternalAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EndpointTrustRecord {
    pub(crate) connection: String,
    pub(crate) kind: EndpointKind,
    pub(crate) endpoint: String,
    pub(crate) identity_digest: String,
    pub(crate) approved_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EndpointTrustDocument {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) endpoints: BTreeMap<String, EndpointTrustRecord>,
}

impl Default for EndpointTrustDocument {
    fn default() -> Self {
        Self {
            version: PRIVATE_RECORD_VERSION,
            endpoints: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalAgentSkillRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_modes: Vec<String>,
    pub(crate) output_modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalAgentStateRecord {
    pub(crate) connection: String,
    pub(crate) configured_endpoint: String,
    pub(crate) identity_digest: String,
    pub(crate) agent_name: String,
    pub(crate) description: String,
    pub(crate) owner: Option<String>,
    pub(crate) agent_version: String,
    pub(crate) interface_url: String,
    pub(crate) protocol_binding: String,
    pub(crate) protocol_version: String,
    pub(crate) streaming: bool,
    pub(crate) input_modes: Vec<String>,
    pub(crate) output_modes: Vec<String>,
    pub(crate) security_schemes: Vec<String>,
    pub(crate) skills: Vec<ExternalAgentSkillRecord>,
    pub(crate) refreshed_unix_ms: u64,
    #[serde(default)]
    pub(crate) trusted_identity_digest: Option<String>,
    #[serde(default)]
    pub(crate) trusted_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalAgentStateDocument {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) agents: BTreeMap<String, ExternalAgentStateRecord>,
    #[serde(default)]
    pub(crate) tasks: BTreeMap<String, ExternalAgentTaskRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalAgentTaskRecord {
    pub(crate) key: String,
    pub(crate) connection: String,
    pub(crate) agent_identity_digest: String,
    pub(crate) task_id: String,
    pub(crate) context_id: Option<String>,
    pub(crate) state: String,
    pub(crate) remote_cancel: String,
    pub(crate) updated_unix_ms: u64,
}

impl Default for ExternalAgentStateDocument {
    fn default() -> Self {
        Self {
            version: PRIVATE_RECORD_VERSION,
            agents: BTreeMap::new(),
            tasks: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SavedOutboundDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutboundDecisionRecord {
    pub(crate) recipient_identity_digest: String,
    pub(crate) data_class: OutboundDataClass,
    pub(crate) decision: SavedOutboundDecision,
    pub(crate) updated_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutboundDecisionDocument {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) decisions: BTreeMap<String, OutboundDecisionRecord>,
}

impl Default for OutboundDecisionDocument {
    fn default() -> Self {
        Self {
            version: PRIVATE_RECORD_VERSION,
            decisions: BTreeMap::new(),
        }
    }
}
