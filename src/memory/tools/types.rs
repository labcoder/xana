//! Closed, bounded tool inputs and host-created commit proof; no provider types.

use crate::{
    identity::OperationId,
    memory::{MemoryContext, MemoryScope, MemoryState},
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScopeSelector {
    Conversation,
    Project,
    Profile,
    User,
}

impl ScopeSelector {
    pub(super) fn resolve(self, context: &MemoryContext) -> anyhow::Result<MemoryScope> {
        use anyhow::Context as _;
        Ok(match self {
            Self::Conversation => MemoryScope::Conversation(
                context
                    .conversation
                    .context("No current Conversation memory scope")?,
            ),
            Self::Project => {
                MemoryScope::Project(context.project.context("No current Project memory scope")?)
            }
            Self::Profile => {
                MemoryScope::Profile(context.profile.context("No current Profile memory scope")?)
            }
            Self::User => MemoryScope::User,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpdateAction {
    Remember,
    Correct,
    Forget,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Risk {
    Ordinary,
    Sensitive,
    #[default]
    Uncertain,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateArgs {
    pub(super) action: UpdateAction,
    pub(super) scope: Option<ScopeSelector>,
    pub(super) statement: Option<String>,
    pub(super) quote: Option<String>,
    pub(super) id: Option<Uuid>,
    pub(super) revision: Option<u64>,
    #[serde(default)]
    pub(super) risk: Risk,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LookupArgs {
    #[serde(default)]
    pub(crate) query: String,
    pub(crate) scope: Option<ScopeSelector>,
    #[serde(default)]
    pub(crate) after: u64,
    #[serde(default = "default_limit")]
    pub(crate) limit: usize,
}

fn default_limit() -> usize {
    8
}

#[derive(Clone)]
pub(crate) struct CommitGuard {
    pub(crate) context: MemoryContext,
    pub(crate) operation_id: OperationId,
    pub(crate) source_id: Uuid,
    pub(crate) source_digest: String,
    pub(crate) generation: u64,
    pub(crate) cancellation: CancellationToken,
}

#[derive(Clone, Serialize)]
pub(crate) struct UpdateIntent {
    pub(crate) action: UpdateAction,
    pub(crate) scope: MemoryScope,
    pub(crate) statement: Option<String>,
    pub(crate) id: Option<Uuid>,
    pub(crate) revision: Option<u64>,
}

#[derive(Clone)]
pub(crate) struct UpdatePlan {
    pub(crate) guard: CommitGuard,
    pub(crate) intent: UpdateIntent,
}

#[derive(Clone)]
pub(crate) struct LookupPlan {
    pub(crate) guard: CommitGuard,
    pub(crate) args: LookupArgs,
    pub(crate) scopes: Vec<MemoryScope>,
}

/// Metadata-only durable acknowledgement; it never retains forgotten text.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateReceipt {
    pub(crate) version: u16,
    pub(crate) committed: bool,
    pub(crate) action: UpdateAction,
    pub(crate) id: Uuid,
    pub(crate) revision: u64,
    pub(crate) scope: MemoryScope,
    pub(crate) state: MemoryState,
    pub(crate) source_id: Uuid,
}

#[derive(Serialize)]
pub(crate) struct LookupResult {
    pub(crate) records: Vec<MemoryPreview>,
    pub(crate) next_after: Option<u64>,
    pub(crate) notice: &'static str,
}

#[derive(Serialize)]
pub(crate) struct MemoryPreview {
    pub(crate) id: Uuid,
    pub(crate) revision: u64,
    pub(crate) scope: MemoryScope,
    pub(crate) statement_preview: String,
    pub(crate) statement_truncated: bool,
    pub(crate) valid_until_unix_seconds: Option<u64>,
}
