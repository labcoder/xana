//! Explicit personal-memory ownership, independent of task history and agent tools.
//!
//! Only application owner-input adapters receive this capability. Stored facts
//! are data, never instructions or permission grants. Automatic extraction and
//! model prompt selection are separate consumers of the eligibility contract.

pub(crate) mod candidates;
mod forgetting;
pub(crate) mod learning;
mod natural;
mod selection;
#[cfg(test)]
mod tests;

use crate::storage::ProtectedStore;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::Path,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub use forgetting::{SourceDeletionPreview, SourceDeletionReceipt};
pub(crate) use natural::parse_natural;
pub(crate) use selection::MemorySelection;
pub(crate) const PAGE_SIZE: usize = 64;
pub(crate) const RECORD_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Profile(Uuid),
    Project(Uuid),
    Conversation(Uuid),
}

impl fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Profile(id) => write!(f, "profile:{id}"),
            Self::Project(id) => write!(f, "project:{id}"),
            Self::Conversation(id) => write!(f, "conversation:{id}"),
        }
    }
}

impl FromStr for MemoryScope {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        if value == "user" {
            return Ok(Self::User);
        }
        let (kind, id) = value.split_once(':').ok_or_else(|| {
            anyhow::anyhow!("scope must be user, profile:UUID, project:UUID or conversation:UUID")
        })?;
        let id: Uuid = id.parse()?;
        ensure!(!id.is_nil(), "scope identity must not be nil");
        Ok(match kind {
            "profile" => Self::Profile(id),
            "project" => Self::Project(id),
            "conversation" => Self::Conversation(id),
            _ => anyhow::bail!("unknown memory scope"),
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MemoryContext {
    pub(crate) conversation: Option<Uuid>,
    pub(crate) profile: Option<Uuid>,
    pub(crate) project: Option<Uuid>,
}

impl MemoryContext {
    pub(crate) fn scopes(&self) -> Vec<MemoryScope> {
        let mut scopes = vec![MemoryScope::User];
        scopes.extend(self.profile.map(MemoryScope::Profile));
        scopes.extend(self.project.map(MemoryScope::Project));
        scopes.extend(self.conversation.map(MemoryScope::Conversation));
        scopes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryClaim {
    Stated,
    Inferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    Candidate,
    Active,
    Superseded,
    Stale,
    Forgotten,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryProvenance {
    pub owner_request: Uuid,
    pub conversation: Option<Uuid>,
    pub at_unix_seconds: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub version: u16,
    pub id: Uuid,
    pub revision: u64,
    pub scope: MemoryScope,
    pub statement: String,
    pub claim: MemoryClaim,
    pub state: MemoryState,
    pub created: MemoryProvenance,
    pub changed: MemoryProvenance,
    pub valid_until_unix_seconds: Option<u64>,
}

impl fmt::Debug for MemoryRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryRecord")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl MemoryRecord {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1
                && !self.id.is_nil()
                && self.revision > 0
                && self.revision < i64::MAX as u64,
            "invalid memory record identity or revision"
        );
        validate_statement(&self.statement)?;
        validate_scope(&self.scope)?;
        for origin in [&self.created, &self.changed] {
            ensure!(
                !origin.owner_request.is_nil()
                    && origin.conversation.is_none_or(|id| !id.is_nil())
                    && origin.at_unix_seconds <= i64::MAX as u64,
                "invalid memory provenance"
            );
        }
        ensure!(
            self.changed.at_unix_seconds >= self.created.at_unix_seconds,
            "memory revision time predates its creation"
        );
        ensure!(
            self.valid_until_unix_seconds
                .is_none_or(|time| time <= i64::MAX as u64),
            "invalid memory expiry"
        );
        Ok(())
    }
    pub(crate) fn eligible_at(&self, now: u64) -> bool {
        self.state == MemoryState::Active
            && self.valid_until_unix_seconds.is_none_or(|time| now < time)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryControls {
    pub scope: MemoryScope,
    pub revision: u64,
    pub use_enabled: bool,
    pub learning_enabled: bool,
    pub no_memory: bool,
}

impl MemoryControls {
    pub(crate) fn defaults(scope: MemoryScope) -> Self {
        Self {
            scope,
            revision: 0,
            use_enabled: true,
            learning_enabled: true,
            no_memory: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryPage {
    pub records: Vec<MemoryRecord>,
    pub next_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EligibleMemory {
    pub records: Vec<MemoryRecord>,
    pub use_enabled: bool,
    pub learning_enabled: bool,
    pub restore_review_required: bool,
    pub has_more: bool,
}

#[derive(Clone)]
pub enum MemoryEdit {
    Correct {
        statement: String,
        valid_until_unix_seconds: Option<u64>,
    },
    Scope {
        target: MemoryScope,
        confirm: bool,
    },
    Disable,
    /// Invalidate this fact and automatic reuse of its originating conversation.
    Forget,
    /// A fresh owner request can restore the fact, never the suppressed source.
    Restore {
        confirm: bool,
    },
}

#[derive(Debug, Clone, Default)]
pub struct MemoryControlEdit {
    pub expected_revision: Option<u64>,
    pub use_enabled: Option<bool>,
    pub learning_enabled: Option<bool>,
    pub no_memory: Option<bool>,
}

/// Capability held only by trusted application/owner-input paths, never tools.
#[derive(Clone)]
pub(crate) struct MemoryOwner {
    pub(crate) store: ProtectedStore,
    pub(crate) context: MemoryContext,
    pub(crate) learner: Option<std::sync::Arc<learning::LearningWorker>>,
}

impl MemoryOwner {
    pub(crate) fn new(store: ProtectedStore, context: MemoryContext) -> Self {
        Self {
            store,
            context,
            learner: None,
        }
    }
    pub(crate) fn remember(
        &self,
        scope: MemoryScope,
        statement: String,
        until: Option<u64>,
    ) -> Result<MemoryRecord> {
        self.remember_at(scope, statement, until, now()?)
    }
    pub(crate) fn remember_at(
        &self,
        scope: MemoryScope,
        statement: String,
        until: Option<u64>,
        time: u64,
    ) -> Result<MemoryRecord> {
        let origin = self.origin(time);
        let record = MemoryRecord {
            version: 1,
            id: Uuid::new_v4(),
            revision: 1,
            scope,
            statement,
            claim: MemoryClaim::Stated,
            state: MemoryState::Active,
            created: origin.clone(),
            changed: origin,
            valid_until_unix_seconds: until,
        };
        record.validate()?;
        self.store.memory_insert(&record)?;
        Ok(record)
    }
    pub(crate) fn revise(&self, id: Uuid, revision: u64, edit: MemoryEdit) -> Result<MemoryRecord> {
        self.store
            .memory_revise(id, revision, edit, self.origin(now()?))
    }
    pub(crate) fn record(&self, id: Uuid) -> Result<MemoryRecord> {
        self.store.memory_record(id)
    }
    pub(crate) fn page(
        &self,
        scope: Option<&MemoryScope>,
        after: Option<u64>,
    ) -> Result<MemoryPage> {
        self.store.memory_page(scope, after)
    }
    pub(crate) fn controls(
        &self,
        scope: MemoryScope,
        edit: MemoryControlEdit,
    ) -> Result<MemoryControls> {
        validate_scope(&scope)?;
        self.store.memory_controls(scope, edit)
    }
    pub(crate) fn eligible(&self) -> Result<EligibleMemory> {
        self.store.memory_eligible(&self.context, now()?)
    }
    pub(crate) fn export(&self, scope: Option<&MemoryScope>, path: &Path) -> Result<u64> {
        self.store.memory_export(scope, path)
    }
    fn origin(&self, time: u64) -> MemoryProvenance {
        MemoryProvenance {
            owner_request: Uuid::new_v4(),
            conversation: self.context.conversation,
            at_unix_seconds: time,
        }
    }
}

pub(crate) fn validate_scope(scope: &MemoryScope) -> Result<()> {
    ensure!(
        match scope {
            MemoryScope::User => true,
            MemoryScope::Profile(id) | MemoryScope::Project(id) | MemoryScope::Conversation(id) =>
                !id.is_nil(),
        },
        "scope identity must not be nil"
    );
    Ok(())
}
pub(crate) fn validate_statement(text: &str) -> Result<()> {
    ensure!(
        !text.trim().is_empty()
            && text.len() <= 4096
            && !text
                .chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t'),
        "memory statement must be 1–4096 UTF-8 bytes without terminal controls"
    );
    Ok(())
}
pub(crate) fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
