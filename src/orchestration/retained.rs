//! Retained purpose and authority, independent of any live child process.
//!
//! Continuation is an explicit new bounded execution under the original parent.
//! Mailbox revisions and conservative reservations survive cancellation/restart.
mod authority;
mod guard;
pub(crate) use guard::RetainedToolGuard;
pub(crate) mod execution;
#[cfg(test)]
mod tests;

use super::{ChildAdmission, ChildReport, ChildTerminalStatus};
use crate::{
    artifact::ArtifactRecord,
    identity::{AgentId, SessionId},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) const WORKER_BYTES: usize = 128 * 1024;
pub(crate) const MAILBOX_LIMIT: usize = 8;
pub(crate) const TEXT_BYTES: usize = 8192;
pub(crate) const EVIDENCE_LIMIT: usize = 64;
pub(crate) const CONTEXT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const CONTEXT_TOTAL_OPS: u64 = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerState {
    Idle,
    Running,
    Draining,
    Stopped,
    Expired,
    NeedsReview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FollowUp {
    pub(crate) id: Uuid,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerReceipt {
    pub(crate) execution: Uuid,
    pub(crate) follow_up: Uuid,
    pub(crate) child: Option<AgentId>,
    pub(crate) status: ChildTerminalStatus,
    pub(crate) summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedWorker {
    pub(crate) version: u32,
    pub(crate) id: AgentId,
    pub(crate) revision: u64,
    pub(crate) session: SessionId,
    pub(crate) goal: String,
    pub(crate) admission: ChildAdmission,
    pub(crate) scope: String,
    pub(crate) workspace_identity: String,
    pub(crate) configuration_digest: String,
    pub(crate) privacy_generation: u64,
    pub(crate) expires_at: i64,
    pub(crate) state: WorkerState,
    pub(crate) mailbox: Vec<FollowUp>,
    pub(crate) accepted_requests: Vec<(Uuid, String)>,
    pub(crate) active: Option<(Uuid, FollowUp)>,
    pub(crate) cancellation: Uuid,
    pub(crate) executions: u64,
    pub(crate) context_bytes: u64,
    pub(crate) context_operations: u64,
    pub(crate) context_receipt: Option<super::context_ops::ContextWorkReceipt>,
    pub(crate) evidence: Vec<ArtifactRecord>,
    pub(crate) last_receipt: Option<WorkerReceipt>,
}

impl RetainedWorker {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.revision > 0 && self.revision < i64::MAX as u64,
            "invalid retained-worker version/revision"
        );
        ensure!(
            self.id == self.admission.attribution.agent_id,
            "retained lineage does not match its original child"
        );
        validate_text(&self.goal)?;
        ensure!(
            self.mailbox.len() <= MAILBOX_LIMIT && self.evidence.len() <= EVIDENCE_LIMIT,
            "retained-worker reference/mailbox limit exceeded"
        );
        ensure!(
            self.accepted_requests.len() <= 64,
            "retained worker follow-up allowance exhausted"
        );
        for message in &self.mailbox {
            validate_text(&message.text)?;
        }
        if let Some((_, message)) = &self.active {
            validate_text(&message.text)?;
        }
        ensure!(
            self.context_bytes <= CONTEXT_TOTAL_BYTES
                && self.context_operations <= CONTEXT_TOTAL_OPS,
            "retained context allowance exhausted"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= WORKER_BYTES,
            "retained worker exceeds its storage limit"
        );
        Ok(())
    }

    pub(crate) fn finish(
        &mut self,
        execution: Uuid,
        report: Option<&ChildReport>,
        reason: &str,
    ) -> Result<WorkerReceipt> {
        ensure!(
            self.active
                .as_ref()
                .is_some_and(|(active, _)| *active == execution),
            "worker execution was replaced or is no longer running"
        );
        let (_, follow_up) = self.active.take().expect("verified active execution");
        let status = report.map_or(ChildTerminalStatus::Interrupted, |r| r.status);
        let receipt = WorkerReceipt {
            execution,
            follow_up: follow_up.id,
            child: report.map(|r| r.attribution.agent_id),
            status,
            summary: super::truncate_utf8(
                report
                    .and_then(|r| r.output.as_deref().or(r.error.as_deref()))
                    .unwrap_or(reason),
                2048,
            ),
        };
        self.state = match (&self.state, status) {
            (WorkerState::Stopped, _) => WorkerState::Stopped,
            (WorkerState::Expired, _) => WorkerState::Expired,
            (WorkerState::Draining, _) if self.mailbox.is_empty() => WorkerState::Stopped,
            (WorkerState::Draining, ChildTerminalStatus::Completed) => WorkerState::Draining,
            (_, ChildTerminalStatus::Completed) => WorkerState::Idle,
            _ => WorkerState::NeedsReview,
        };
        self.last_receipt = Some(receipt.clone());
        Ok(receipt)
    }
}

pub(crate) fn validate_text(text: &str) -> Result<()> {
    ensure!(
        !text.trim().is_empty() && text.len() <= TEXT_BYTES && !text.contains('\0'),
        "worker text must contain 1–8192 UTF-8 bytes"
    );
    Ok(())
}
