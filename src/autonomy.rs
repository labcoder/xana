//! Owner-authored durable work. Clocks and execution are injected; this domain
//! never discovers authority from a client, model response, or focused session.
mod calendar;
pub(crate) mod host;
pub(crate) mod runner;
pub(crate) mod startup;
#[cfg(test)]
mod tests;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub(crate) use calendar::{Occurrence, Schedule};
pub(crate) const JOB_BYTES: usize = 64 * 1024;
pub(crate) const PAGE_SIZE: usize = 32;
pub(crate) const QUEUE_LIMIT: usize = 1000;
pub(crate) const RUN_SECONDS: u64 = 120;
pub(crate) const JOB_TOKENS: u64 = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskBudget {
    pub(crate) conservative_tokens: u64,
    pub(crate) run_seconds: u64,
    pub(crate) daily_tokens: u64,
}
impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            conservative_tokens: JOB_TOKENS,
            run_seconds: RUN_SECONDS,
            daily_tokens: 32768,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskScope {
    pub(crate) workspace: PathBuf,
    pub(crate) workspace_identity: String,
    pub(crate) project: Option<Uuid>,
    pub(crate) profile: String,
    pub(crate) profile_id: Uuid,
    /// Exact non-secret resolved route/policy fingerprint; changes require review.
    pub(crate) configuration_digest: String,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Action {
    Reminder {
        text: String,
    },
    NativeTask {
        prompt: String,
        workspace_reads: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobState {
    Ready,
    Running,
    CancelRequested,
    Paused,
    NeedsYou,
    Completed,
    Cancelled,
    Expired,
}
impl JobState {
    pub(crate) fn code(&self) -> i64 {
        match self {
            Self::Ready => 0,
            Self::Running => 1,
            Self::CancelRequested => 2,
            Self::Paused => 3,
            Self::NeedsYou => 4,
            Self::Completed => 5,
            Self::Cancelled => 6,
            Self::Expired => 7,
        }
    }
    pub(crate) fn active(&self) -> bool {
        self.code() < 5
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Job {
    pub(crate) id: Uuid,
    pub(crate) revision: u64,
    pub(crate) conversation: Uuid,
    pub(crate) name: String,
    pub(crate) scope: TaskScope,
    pub(crate) action: Action,
    pub(crate) budget: TaskBudget,
    pub(crate) schedule: Schedule,
    pub(crate) expires_at: i64,
    pub(crate) authorized: bool,
    pub(crate) state: JobState,
    pub(crate) next: Occurrence,
    pub(crate) not_before: i64,
    pub(crate) occurrence: Option<Uuid>,
    pub(crate) pause_after_run: bool,
    pub(crate) last_receipt: Option<RunReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunOutcome {
    Completed,
    NeedsYou,
    Unknown,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunReceipt {
    pub(crate) occurrence: Uuid,
    pub(crate) scheduled_at: i64,
    pub(crate) finished_at: i64,
    pub(crate) outcome: RunOutcome,
    pub(crate) detail: String,
    pub(crate) coalesced: bool,
    pub(crate) dst_adjusted: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum JobEdit {
    Pause,
    Resume { review_unknown: bool },
    Cancel,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct HostPolicy {
    pub(crate) revision: u64,
    pub(crate) detached_enabled: bool,
    pub(crate) startup_enabled: bool,
    pub(crate) stop_requested: bool,
    pub(crate) lock_requested: bool,
}

impl Job {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.revision > 0 && self.revision <= i64::MAX as u64,
            "invalid job revision"
        );
        ensure!(
            !self.id.is_nil() && !self.conversation.is_nil() && !self.scope.profile_id.is_nil(),
            "invalid job identity"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 128
                && !self.name.chars().any(char::is_control),
            "invalid job name"
        );
        ensure!(
            self.scope.workspace.is_absolute() && self.scope.workspace.as_os_str().len() <= 4096,
            "invalid workspace scope"
        );
        for value in [
            &self.scope.profile,
            &self.scope.connection,
            &self.scope.model,
            &self.scope.workspace_identity,
            &self.scope.configuration_digest,
            &self.scope.endpoint,
        ] {
            ensure!(
                !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control),
                "invalid task route"
            );
        }
        let text = match &self.action {
            Action::Reminder { text } => text,
            Action::NativeTask { prompt, .. } => prompt,
        };
        ensure!(
            !text.trim().is_empty() && text.len() <= 16 * 1024 && !text.contains('\0'),
            "task text must be 1–16384 UTF-8 bytes"
        );
        self.schedule.validate()?;
        ensure!(
            self.budget == TaskBudget::default(),
            "saved task budget does not match the supported reviewed ceiling"
        );
        ensure!(
            self.expires_at > 0 && self.next.at > 0 && self.not_before > 0,
            "invalid job time"
        );
        ensure!(
            matches!(self.state, JobState::Running | JobState::CancelRequested)
                == self.occurrence.is_some(),
            "invalid running occurrence"
        );
        if let Some(receipt) = &self.last_receipt {
            receipt.validate()?;
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= JOB_BYTES,
            "job exceeds protected record bound"
        );
        Ok(())
    }
}
impl RunReceipt {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.occurrence.is_nil()
                && self.scheduled_at > 0
                && self.finished_at > 0
                && self.detail.len() <= 16 * 1024,
            "invalid run receipt"
        );
        Ok(())
    }
}

pub(crate) fn now() -> Result<i64> {
    Ok(i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?)
}
