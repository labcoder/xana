//! Deterministic selected-source observations. Polls have no model route and
//! cannot change a job's owner-authored action or grant ceiling.
pub(crate) mod files;
pub(crate) mod github;
#[cfg(test)]
mod tests;

use super::{Job, JobState};
use crate::storage::ProtectedStore;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Trigger {
    Files(files::FileTrigger),
    Github(github::GithubTrigger),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    pub(crate) last_checked: Option<i64>,
    pub(crate) last_event: Option<i64>,
    pub(crate) pending: bool,
    pub(crate) status: String,
    pub(crate) failures: u32,
}

impl Trigger {
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Files(left), Self::Files(right)) => {
                left.root == right.root && left.identity == right.identity
            }
            (Self::Github(left), Self::Github(right)) => {
                left.repository == right.repository
                    && left.run == right.run
                    && left.credential == right.credential
            }
            _ => false,
        }
    }
    pub(crate) fn observation(&self) -> &Observation {
        match self {
            Self::Files(v) => &v.observation,
            Self::Github(v) => &v.observation,
        }
    }
    pub(crate) fn observation_mut(&mut self) -> &mut Observation {
        match self {
            Self::Files(v) => &mut v.observation,
            Self::Github(v) => &mut v.observation,
        }
    }
    pub(crate) fn validate(&self, workspace: &Path) -> Result<()> {
        let observation = self.observation();
        ensure!(
            observation.status.len() <= 512 && observation.failures <= 16,
            "invalid trigger observation"
        );
        match self {
            Self::Files(v) => v.validate(workspace),
            Self::Github(v) => v.validate(),
        }
    }
    pub(crate) fn completed(&self) -> bool {
        matches!(self, Self::Github(v) if v.last.as_ref().is_some_and(|last| last.status == "completed"))
    }
}

/// Refresh one bounded due page without holding a database transaction during
/// filesystem, key-store or HTTP work. Revision checks reject concurrent edits.
pub(crate) async fn refresh_due(
    store: &ProtectedStore,
    now: i64,
    cancelled: &CancellationToken,
) -> Result<()> {
    for mut job in store.autonomy_due(now)? {
        if cancelled.is_cancelled() {
            break;
        }
        if job.expires_at <= now
            || !job.authorized
            || job.trigger.as_ref().is_none_or(|t| t.observation().pending)
        {
            continue;
        }
        let mut trigger = job.trigger.take().expect("filtered trigger");
        let result = match &mut trigger {
            Trigger::Files(watch) => {
                let Some(priority) = store.background_lease()? else {
                    continue;
                };
                let watch = watch.clone();
                let store = store.clone();
                let workspace = job.scope.workspace.clone();
                let checked = tokio::task::spawn_blocking(move || {
                    let result = files::observe(&store, watch, &workspace, now);
                    // A foreground operation may have started during a scan.
                    // Discard that sample; its own-output receipt must settle first.
                    if priority.foreground_active()? {
                        return Ok(None);
                    }
                    result.map(Some)
                })
                .await?;
                let checked = match checked {
                    Ok(Some(watch)) => Ok(watch),
                    Ok(None) => continue,
                    Err(error) => Err(error),
                };
                checked.map(|next| {
                    trigger = Trigger::Files(next);
                    5
                })
            }
            Trigger::Github(watch) => github::observe(watch, now, cancelled).await,
        };
        if cancelled.is_cancelled() {
            break;
        }
        let delay = match result {
            Ok(delay) => delay,
            Err(_) => {
                // Paths, response bodies and credentials are not diagnostics.
                let observation = trigger.observation_mut();
                observation.last_checked = Some(now);
                observation.pending = false;
                observation.status = "Source unavailable, replaced, out of bounds, or unknown Xana effects; owner review required".into();
                job.state = JobState::NeedsYou;
                60
            }
        };
        job.not_before = if trigger.observation().pending {
            now
        } else {
            now.saturating_add(i64::from(delay))
        };
        job.next.at = job.not_before;
        job.trigger = Some(trigger);
        store.autonomy_observed(job)?;
    }
    Ok(())
}

pub(crate) fn validate_dispatch(job: &Job) -> Result<()> {
    if let Some(Trigger::Files(watch)) = &job.trigger {
        files::validate_root(watch, &job.scope.workspace)?;
    }
    ensure!(
        job.trigger.as_ref().is_none_or(|t| t.observation().pending),
        "trigger event is no longer pending"
    );
    Ok(())
}
