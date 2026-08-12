use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

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
    pub(crate) source_digest: String,
    pub(crate) active_revision: String,
    #[serde(default)]
    pub(crate) retained_revisions: Vec<String>,
    #[serde(default)]
    pub(crate) enabled_scopes: Vec<String>,
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
