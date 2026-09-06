//! Finite-work completion policy over observed receipts, never model confidence.
//!
//! Execution termination and goal evidence are independent facts. This module
//! performs no provider/tool calls and cannot grant authority or retry effects.

mod collect;
#[cfg(test)]
mod tests;
mod verification;

pub(crate) use collect::add_artifact;
pub(crate) use collect::{from_messages, from_operation};
pub(crate) use verification::verify_artifacts;

use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ContentHash},
    identity::{OperationId, ToolInvocationId},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub(crate) const MAX_EVIDENCE_ITEMS: usize = 64;
pub(crate) const MAX_EVIDENCE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_VERIFY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CompletionContract {
    /// Empty means delivery-only, not independent validation of task correctness.
    pub(crate) conditions: Vec<AcceptanceCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AcceptanceCondition {
    CommandSucceeded {
        command: String,
        cwd: String,
    },
    ArtifactPresent {
        artifact: ArtifactRef,
    },
    /// An assertion without a supported deterministic checker remains unverified.
    Declared {
        id: String,
        revision: ContentHash,
    },
}

impl CompletionContract {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.conditions.len() <= MAX_EVIDENCE_ITEMS,
            "too many completion conditions"
        );
        for condition in &self.conditions {
            match condition {
                AcceptanceCondition::CommandSucceeded { command, cwd } => {
                    ensure!(
                        !command.trim().is_empty()
                            && command.len() <= 4096
                            && !command.contains('\0'),
                        "invalid completion command condition"
                    );
                    ensure!(
                        !cwd.is_empty() && cwd.len() <= 4096 && !cwd.contains('\0'),
                        "invalid completion command directory"
                    );
                }
                AcceptanceCondition::Declared { id, .. } => {
                    ensure!(
                        !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control),
                        "invalid completion condition id"
                    );
                }
                AcceptanceCondition::ArtifactPresent { .. } => {}
            }
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= 16 * 1024,
            "completion contract exceeds bound"
        );
        Ok(())
    }

    pub(crate) fn digest(&self) -> ContentHash {
        ContentHash::for_bytes(&serde_json::to_vec(self).expect("typed contract serializes"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkKind {
    Root,
    Child,
    Scheduled,
    Retained,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceOwner {
    Native,
    Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompletionClaim {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceOutcome {
    DeliveryVerified,
    ConditionsVerified,
    Incomplete,
    NeedsAttention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckOutcome {
    Passed,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandStatus {
    pub(crate) success: bool,
    pub(crate) exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckEvidence {
    pub(crate) invocation: ToolInvocationId,
    pub(crate) generation: OperationId,
    pub(crate) command_digest: ContentHash,
    /// A later possibly-mutating action invalidates an earlier check.
    pub(crate) work_revision: u64,
    pub(crate) outcome: CheckOutcome,
    pub(crate) exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EffectOutcome {
    Acknowledged,
    Failed,
    Declined,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectEvidence {
    pub(crate) invocation: ToolInvocationId,
    pub(crate) generation: OperationId,
    pub(crate) outcome: EffectOutcome,
    pub(crate) replay_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactEvidence {
    pub(crate) generation: OperationId,
    pub(crate) artifact: ArtifactRecord,
    pub(crate) verified: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct BudgetEvidence {
    pub(crate) remaining_requests: Option<u64>,
    pub(crate) remaining_tokens: Option<u64>,
    pub(crate) remaining_actions: Option<u64>,
    pub(crate) remaining_millis: Option<u64>,
    pub(crate) remaining_cost_microusd: Option<u64>,
    pub(crate) exhausted: bool,
    pub(crate) exceeded: bool,
    pub(crate) verification_bytes_reserved: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VerificationState {
    #[default]
    NotNeeded,
    Unavailable,
    Reserved,
    Passed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompletionEvidence {
    #[serde(default)]
    pub(crate) child_reports: Vec<ChildEvidenceRef>,
    pub(crate) version: u32,
    pub(crate) generation: OperationId,
    pub(crate) revision: u64,
    pub(crate) kind: WorkKind,
    pub(crate) owner: EvidenceOwner,
    pub(crate) claim: CompletionClaim,
    pub(crate) contract: CompletionContract,
    pub(crate) contract_digest: ContentHash,
    pub(crate) delivered_hash: Option<ContentHash>,
    pub(crate) delivered_bytes: u64,
    pub(crate) work_revision: u64,
    pub(crate) checks: Vec<CheckEvidence>,
    pub(crate) artifacts: Vec<ArtifactEvidence>,
    pub(crate) effects: Vec<EffectEvidence>,
    pub(crate) omitted_observations: bool,
    pub(crate) budget: BudgetEvidence,
    pub(crate) verification: VerificationState,
    pub(crate) outcome: EvidenceOutcome,
}

impl CompletionEvidence {
    pub(crate) fn new(
        generation: OperationId,
        kind: WorkKind,
        owner: EvidenceOwner,
        claim: CompletionClaim,
        contract: CompletionContract,
    ) -> Result<Self> {
        contract.validate()?;
        Ok(Self {
            child_reports: Vec::new(),
            version: 1,
            generation,
            revision: 1,
            kind,
            owner,
            claim,
            contract_digest: contract.digest(),
            contract,
            delivered_hash: None,
            delivered_bytes: 0,
            work_revision: 0,
            checks: Vec::new(),
            artifacts: Vec::new(),
            effects: Vec::new(),
            omitted_observations: false,
            budget: BudgetEvidence::default(),
            verification: VerificationState::NotNeeded,
            outcome: EvidenceOutcome::Incomplete,
        })
    }

    pub(crate) fn delivered(&mut self, bytes: &[u8]) {
        self.delivered_hash = Some(ContentHash::for_bytes(bytes));
        self.delivered_bytes = bytes.len() as u64;
    }

    pub(crate) fn evaluate(&mut self) {
        self.outcome = if self.claim != CompletionClaim::Completed || self.budget.exceeded {
            EvidenceOutcome::Incomplete
        } else if self.omitted_observations
            || self
                .effects
                .iter()
                .any(|effect| effect.outcome == EffectOutcome::Unknown)
            || self
                .child_reports
                .iter()
                .any(|child| child.outcome == EvidenceOutcome::NeedsAttention)
            || matches!(
                self.verification,
                VerificationState::Reserved
                    | VerificationState::Unavailable
                    | VerificationState::Failed
                    | VerificationState::Cancelled
            )
        {
            EvidenceOutcome::NeedsAttention
        } else if self.delivered_hash.is_none()
            || self.delivered_bytes == 0
            || self
                .child_reports
                .iter()
                .any(|child| child.outcome == EvidenceOutcome::Incomplete)
            || self.effects.iter().any(|effect| {
                effect.outcome == EffectOutcome::Declined
                    || (effect.outcome == EffectOutcome::Failed
                        && !self.checks.iter().any(|check| {
                            check.invocation == effect.invocation
                                && check.outcome == CheckOutcome::Failed
                                && self.check_superseded(check)
                        }))
            })
            || self
                .checks
                .iter()
                .any(|check| check.outcome != CheckOutcome::Passed && !self.check_superseded(check))
            || self
                .artifacts
                .iter()
                .any(|artifact| !artifact.verified || artifact.generation != self.generation)
        {
            EvidenceOutcome::Incomplete
        } else if self
            .contract
            .conditions
            .iter()
            .any(|condition| !self.satisfies(condition))
        {
            EvidenceOutcome::NeedsAttention
        } else if self.contract.conditions.is_empty() {
            EvidenceOutcome::DeliveryVerified
        } else {
            EvidenceOutcome::ConditionsVerified
        };
    }

    fn satisfies(&self, condition: &AcceptanceCondition) -> bool {
        match condition {
            AcceptanceCondition::CommandSucceeded { command, cwd } => {
                let digest = command_digest(command, cwd);
                self.checks.iter().any(|check| {
                    check.generation == self.generation
                        && check.command_digest == digest
                        && check.work_revision == self.work_revision
                        && check.outcome == CheckOutcome::Passed
                })
            }
            AcceptanceCondition::ArtifactPresent { artifact } => {
                self.artifacts.iter().any(|fact| {
                    fact.generation == self.generation
                        && fact.artifact.reference == *artifact
                        && fact.verified
                })
            }
            AcceptanceCondition::Declared { .. } => false,
        }
    }

    fn check_superseded(&self, check: &CheckEvidence) -> bool {
        check.outcome == CheckOutcome::Failed
            && self.checks.iter().any(|later| {
                later.generation == self.generation
                    && later.command_digest == check.command_digest
                    && later.work_revision > check.work_revision
                    && later.work_revision == self.work_revision
                    && later.outcome == CheckOutcome::Passed
                    && later.exit_code == Some(0)
            })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.contract.validate()?;
        ensure!(
            self.version == 1 && self.revision > 0 && self.revision <= 3,
            "invalid completion evidence version/revision"
        );
        ensure!(
            self.contract.digest() == self.contract_digest,
            "completion contract changed"
        );
        ensure!(
            [
                self.checks.len(),
                self.artifacts.len(),
                self.effects.len(),
                self.child_reports.len()
            ]
            .into_iter()
            .all(|n| n <= MAX_EVIDENCE_ITEMS),
            "completion evidence item bound exceeded"
        );
        ensure!(
            self.checks
                .iter()
                .all(|fact| fact.generation == self.generation
                    && fact.work_revision <= self.work_revision)
                && self
                    .effects
                    .iter()
                    .all(|fact| fact.generation == self.generation)
                && self
                    .artifacts
                    .iter()
                    .all(|fact| fact.generation == self.generation),
            "completion evidence generation mismatch"
        );
        for ids in [
            self.checks
                .iter()
                .map(|fact| fact.invocation)
                .collect::<Vec<_>>(),
            self.effects
                .iter()
                .map(|fact| fact.invocation)
                .collect::<Vec<_>>(),
        ] {
            ensure!(
                ids.iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == ids.len(),
                "duplicate completion receipt"
            );
        }
        ensure!(
            self.budget.verification_bytes_reserved <= MAX_VERIFY_BYTES,
            "completion verification byte budget exceeded"
        );
        ensure!(
            self.child_reports
                .iter()
                .all(|child| child.generation != self.generation)
                && self
                    .child_reports
                    .iter()
                    .map(|child| child.generation)
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == self.child_reports.len(),
            "invalid child completion references"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_EVIDENCE_BYTES,
            "completion evidence byte bound exceeded"
        );
        Ok(())
    }

    pub(crate) fn supported(&self) -> bool {
        matches!(
            self.outcome,
            EvidenceOutcome::DeliveryVerified | EvidenceOutcome::ConditionsVerified
        )
    }

    /// External projections and durable records must not trust a claimed outcome
    /// that disagrees with the enclosed facts.
    pub(crate) fn validate_outcome(&self) -> Result<()> {
        self.validate()?;
        let mut evaluated = self.clone();
        evaluated.evaluate();
        ensure!(
            evaluated.outcome == self.outcome,
            "completion outcome contradicts its evidence"
        );
        Ok(())
    }

    pub(crate) fn summary(&self) -> String {
        let outcome = match self.outcome {
            EvidenceOutcome::DeliveryVerified => {
                "Delivered; task correctness was not independently checked"
            }
            EvidenceOutcome::ConditionsVerified => "Declared completion checks verified",
            EvidenceOutcome::Incomplete => {
                "Incomplete: completion evidence contains missing or failed work"
            }
            EvidenceOutcome::NeedsAttention => {
                "Needs attention: completion evidence is unavailable or uncertain"
            }
        };
        format!(
            "{outcome} ({} check(s), {} artifact(s), {} effect receipt(s); verifier {:?})",
            self.checks.len(),
            self.artifacts.len(),
            self.effects.len(),
            self.verification
        )
    }
}

pub(crate) fn command_digest(command: &str, cwd: &str) -> ContentHash {
    ContentHash::for_bytes(&serde_json::to_vec(&(command, cwd)).expect("string pair serializes"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildEvidenceRef {
    pub(crate) generation: OperationId,
    pub(crate) receipt_digest: Option<ContentHash>,
    pub(crate) outcome: EvidenceOutcome,
}

pub(crate) fn message_text(message: &crate::message::Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            crate::message::ContentBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A declaration, one collected/reserved pass, then at most one verification
/// result. Reopening a reserved pass cannot authorize another attempt.
pub(crate) fn valid_transition(
    previous: Option<&CompletionEvidence>,
    next: &CompletionEvidence,
) -> bool {
    if next.validate().is_err() {
        return false;
    }
    let mut evaluated = next.clone();
    evaluated.evaluate();
    if evaluated.outcome != next.outcome {
        return false;
    }
    let Some(previous) = previous else {
        return next.revision == 1
            && next.claim == CompletionClaim::Interrupted
            && next.delivered_hash.is_none()
            && next.checks.is_empty()
            && next.effects.is_empty()
            && next.artifacts.is_empty()
            && next.child_reports.is_empty()
            && next.verification == VerificationState::NotNeeded;
    };
    if previous.generation != next.generation
        || previous.contract != next.contract
        || previous.kind != next.kind
        || previous.owner != next.owner
        || next.revision != previous.revision + 1
    {
        return false;
    }
    if previous.revision == 1 {
        return next.revision == 2
            && !matches!(
                next.verification,
                VerificationState::Passed | VerificationState::Failed
            )
            && next.artifacts.iter().all(|fact| !fact.verified);
    }
    if previous.revision != 2 || previous.verification != VerificationState::Reserved {
        return false;
    }
    let mut frozen = next.clone();
    frozen.revision = previous.revision;
    frozen.verification = previous.verification;
    frozen.outcome = previous.outcome;
    for fact in &mut frozen.artifacts {
        fact.verified = false;
    }
    frozen == *previous
        && matches!(
            next.verification,
            VerificationState::Passed | VerificationState::Failed | VerificationState::Cancelled
        )
}
