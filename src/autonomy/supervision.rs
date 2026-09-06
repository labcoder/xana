//! Shared, bounded supervision facts. A projection never grants authority and
//! receipt bodies stay behind exact inspection, not notification payloads.
use super::{Action, Job, JobState, PAGE_SIZE, RunOutcome};
use crate::{paths::XanaPaths, storage::ProtectedStore};
use anyhow::Result;
use serde::Serialize;
pub(crate) mod attention;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkGroup {
    ComingUp,
    InMotion,
    Paused,
    NeedsYou,
    Completed,
    Cancelled,
    Expired,
}
impl WorkGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::ComingUp => "Coming up",
            Self::InMotion => "In motion",
            Self::Paused => "Paused",
            Self::NeedsYou => "Needs you",
            Self::Completed => "Completed",
            Self::Cancelled => "Cancelled",
            Self::Expired => "Expired",
        }
    }
}

/// Local owner-facing metadata; no prompt, result text, credential, or file list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskOverview {
    pub id: String,
    pub revision: u64,
    pub conversation: String,
    pub project: Option<String>,
    pub name: String,
    pub group: WorkGroup,
    pub next_at: i64,
    pub expires_at: i64,
    pub connection: String,
    pub model: String,
    pub profile: String,
    pub trigger: &'static str,
    pub observation_at: Option<i64>,
    pub last_event_at: Option<i64>,
    pub source_status: Option<String>,
    pub event_pending: bool,
    pub receipt: Option<&'static str>,
    pub receipt_id: Option<String>,
    pub pause_pending: bool,
    pub cancellation_pending: bool,
    pub token_ceiling: u64,
    pub seconds_ceiling: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisionPage {
    pub jobs: Vec<TaskOverview>,
    pub next_after: Option<u64>,
}

pub(crate) fn page(store: &ProtectedStore, after: u64) -> Result<SupervisionPage> {
    let rows = store.autonomy_page(after)?;
    let next_after = (rows.len() == PAGE_SIZE)
        .then(|| rows.last().map(|(id, _)| *id))
        .flatten();
    Ok(SupervisionPage {
        jobs: rows.iter().map(|(_, job)| overview(job)).collect(),
        next_after,
    })
}

pub(crate) fn overview(job: &Job) -> TaskOverview {
    use super::triggers::Trigger;
    TaskOverview {
        id: job.id.to_string(),
        revision: job.revision,
        conversation: job.conversation.to_string(),
        project: job.scope.project.map(|id| id.to_string()),
        name: job.name.clone(),
        group: match job.state {
            JobState::Ready => WorkGroup::ComingUp,
            JobState::Running | JobState::CancelRequested => WorkGroup::InMotion,
            JobState::Paused => WorkGroup::Paused,
            JobState::NeedsYou => WorkGroup::NeedsYou,
            JobState::Completed => WorkGroup::Completed,
            JobState::Cancelled => WorkGroup::Cancelled,
            JobState::Expired => WorkGroup::Expired,
        },
        next_at: job.not_before,
        expires_at: job.expires_at,
        connection: job.scope.connection.clone(),
        model: job.scope.model.clone(),
        profile: job.scope.profile.clone(),
        trigger: match job.trigger {
            Some(Trigger::Files(_)) => "selected files",
            Some(Trigger::Github(_)) => "named GitHub run",
            None => "calendar",
        },
        observation_at: job
            .trigger
            .as_ref()
            .and_then(|t| t.observation().last_checked),
        last_event_at: job
            .trigger
            .as_ref()
            .and_then(|t| t.observation().last_event),
        source_status: job.trigger.as_ref().map(|t| t.observation().status.clone()),
        event_pending: job
            .trigger
            .as_ref()
            .is_some_and(|t| t.observation().pending),
        receipt: job.last_receipt.as_ref().map(|r| match r.outcome {
            RunOutcome::Completed => "completed",
            RunOutcome::NeedsYou => "needs owner review",
            RunOutcome::Unknown => "outcome unknown; no automatic replay",
            RunOutcome::Cancelled => "cancelled",
            RunOutcome::Expired => "expired",
        }),
        receipt_id: job.last_receipt.as_ref().map(|r| r.occurrence.to_string()),
        pause_pending: job.pause_after_run,
        cancellation_pending: job.state == JobState::CancelRequested,
        token_ceiling: job.budget.conservative_tokens,
        seconds_ceiling: job.budget.run_seconds,
    }
}

/// Exact local inspection may disclose the reviewed recipient and scope. It
/// compares current policy without changing the saved grant or resolving secrets.
/// The saved action text belongs only to this explicit local review, not queue
/// projections or attention payloads.
pub(crate) fn review(
    paths: &XanaPaths,
    store: &ProtectedStore,
    job: &Job,
) -> Result<serde_json::Value> {
    use crate::memory::{MemoryControlEdit, MemoryScope};
    let current = super::runner::resolve_scope(
        paths,
        &job.scope.workspace,
        &job.scope.profile,
        job.scope.project,
    );
    let route_matches = current.as_ref().is_ok_and(|scope| scope == &job.scope);
    let scopes = [
        Some(MemoryScope::User),
        Some(MemoryScope::Profile(job.scope.profile_id)),
        job.scope.project.map(MemoryScope::Project),
        Some(MemoryScope::Conversation(job.conversation)),
    ];
    let controls = scopes
        .into_iter()
        .flatten()
        .map(|scope| store.memory_controls(scope, MemoryControlEdit::default()))
        .collect::<Result<Vec<_>>>()?;
    Ok(serde_json::json!({
        "task":overview(job),"saved_scope":job.scope,"trigger":job.trigger,"route_matches":route_matches,
        "current_route":if route_matches {"unchanged"} else {"changed or unavailable; create a newly reviewed task"},
        "authorized":job.authorized,"expired":job.expires_at<=super::now()?,
        "saved_action":job.action,
        "action":match job.action {Action::Reminder{..}=>"local reminder",Action::NativeTask{workspace_reads:true,..}=>"native task with selected workspace reads",Action::NativeTask{..}=>"native task without workspace reads"},
        "budget":job.budget,"memory_controls":controls,
        "attention":"No background human controller. New permissions wait for owner review; opening a task does not grant authority.",
        "privacy":"No-memory controls personal use/learning, not encrypted task history or execution receipts. Inspect budget usage separately; a ceiling is not consumption."
    }))
}

#[cfg(test)]
mod tests;
