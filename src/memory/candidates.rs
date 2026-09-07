//! Governed proposals are data, never authority or executable Skill packages.
//! The protected store owns atomic publication and privacy/revision checks.

use super::{MemoryClaim, MemoryOwner, MemoryRecord, MemoryScope};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) const CANDIDATE_PAGE: usize = 32;
pub(crate) const SKILL_BYTES: usize = 32 * 1024;
pub(crate) const CANDIDATE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateTargetKind {
    Memory,
    Skill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateState {
    Staged,
    AutoApplied,
    Approved,
    ReviewedOnly,
    Rejected,
    Archived,
    Stale,
    Undone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateRisk {
    Ordinary,
    Inferred,
    Ambiguous,
    Sensitive,
    Procedure,
    LegacyUnverified,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidatePayload {
    Memory {
        memory_id: Option<Uuid>,
        base_revision: Option<u64>,
        statement: Option<String>,
        claim: MemoryClaim,
    },
    Skill {
        name: String,
        markdown: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateOrigin {
    OwnerDraft {
        request: Uuid,
        conversation: Option<Uuid>,
    },
    Extractor {
        connection: String,
        model: String,
        route_digest: String,
    },
    LegacyImport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSource {
    pub id: Uuid,
    pub revision: u64,
    pub conversation: Uuid,
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateValidation {
    ExactOwnerQuote,
    OrdinaryStatedAllowlistV1,
    OrdinaryPreferenceV2,
    OwnerDraftInertOnly,
    LegacyEvidenceUnavailable,
    SensitiveContentNotRetained,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateActor {
    Owner,
    DeterministicPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateEvent {
    pub revision: u64,
    pub state: CandidateState,
    pub actor: CandidateActor,
    pub at_unix_seconds: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRecord {
    pub version: u16,
    pub id: Uuid,
    pub revision: u64,
    pub scope: MemoryScope,
    pub payload: CandidatePayload,
    pub origin: CandidateOrigin,
    pub sources: Vec<CandidateSource>,
    pub privacy_generation: u64,
    pub content_hash: String,
    pub risk: CandidateRisk,
    pub validation: CandidateValidation,
    pub state: CandidateState,
    pub created_at_unix_seconds: u64,
    pub changed_at_unix_seconds: u64,
    pub events: Vec<CandidateEvent>,
    pub rejection_reason: Option<String>,
    pub rollback_revision: Option<u64>,
}

impl std::fmt::Debug for CandidateRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidateRecord")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl CandidateRecord {
    pub fn target_kind(&self) -> CandidateTargetKind {
        match self.payload {
            CandidatePayload::Memory { .. } => CandidateTargetKind::Memory,
            CandidatePayload::Skill { .. } => CandidateTargetKind::Skill,
        }
    }

    pub(crate) fn hide_content(&mut self) {
        match &mut self.payload {
            CandidatePayload::Memory { statement, .. } => *statement = None,
            CandidatePayload::Skill { name, markdown } => {
                name.clear();
                *markdown = None;
            }
        }
        self.rejection_reason = None;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateSummary {
    pub id: Uuid,
    pub revision: u64,
    pub scope: MemoryScope,
    pub target_kind: CandidateTargetKind,
    pub state: CandidateState,
    pub risk: CandidateRisk,
    pub changed_at_unix_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidatePage {
    pub records: Vec<CandidateSummary>,
    pub next_after: Option<u64>,
}

#[derive(Clone, Serialize)]
pub struct CandidateInspection {
    pub record: CandidateRecord,
    pub before: Option<MemoryRecord>,
    pub diff: String,
    pub can_approve: bool,
    pub stale_reason: Option<String>,
}

impl std::fmt::Debug for CandidateInspection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidateInspection")
            .field("record", &self.record)
            .field("can_approve", &self.can_approve)
            .field("stale", &self.stale_reason.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum CandidateEdit {
    Approve { confirm_sensitive: bool },
    Reject { reason: String },
    Archive,
    Undo,
}

impl MemoryOwner {
    pub(crate) fn candidate_page(
        &self,
        scope: Option<&MemoryScope>,
        after: Option<u64>,
    ) -> Result<CandidatePage> {
        self.store.candidate_page(scope, after)
    }
    pub(crate) fn candidate(&self, id: Uuid) -> Result<CandidateInspection> {
        self.store.candidate_inspect(id)
    }
    pub(crate) fn stage_skill(
        &self,
        scope: MemoryScope,
        name: String,
        markdown: String,
    ) -> Result<CandidateRecord> {
        self.store
            .candidate_stage_skill(scope, name, markdown, self.origin(super::now()?))
    }
    pub(crate) fn review_candidate(
        &self,
        id: Uuid,
        revision: u64,
        edit: CandidateEdit,
    ) -> Result<CandidateRecord> {
        self.store
            .candidate_review(id, revision, edit, self.origin(super::now()?))
    }
}
