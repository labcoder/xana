//! Durable context identities, provenance, selectors, and bounded views.

use crate::{
    artifact::{ArtifactRef, ContentHash},
    identity::{ContextId, ContextViewId, PrincipalId},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextKind {
    ProjectInstructions,
    ToolOutput,
    Derived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrustClass {
    Xana,
    User,
    Project,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Provenance {
    ProjectFile {
        relative_path: PathBuf,
    },
    Derived {
        source_id: ContextId,
        source_version: u64,
        selector: ViewSelector,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextRecord {
    pub(crate) id: ContextId,
    pub(crate) version: u64,
    pub(crate) artifact: ArtifactRef,
    pub(crate) kind: ContextKind,
    pub(crate) content_hash: ContentHash,
    pub(crate) logical_size: u64,
    pub(crate) provenance: Provenance,
    pub(crate) trust: TrustClass,
    pub(crate) owner: PrincipalId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ViewSelector {
    Full,
    Lines { start: usize, end: usize },
    LiteralSearch { query: String, max_matches: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MaterializationBudget {
    pub(crate) max_bytes: usize,
    pub(crate) max_estimated_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextViewRecord {
    pub(crate) id: ContextViewId,
    pub(crate) source: ContextId,
    pub(crate) source_version: u64,
    pub(crate) selector: ViewSelector,
    pub(crate) content_hash: ContentHash,
    pub(crate) budget: MaterializationBudget,
}
