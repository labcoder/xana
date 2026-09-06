//! Bounded Desktop facts and typed owner intent. No runtime handle, provider,
//! credential value, or raw database capability crosses this seam.
use super::{DesktopControlPlane, DesktopError, control_error};
use crate::{
    app::autonomy_commands,
    autonomy::{Job, supervision, triggers::Trigger},
    cli::{AutonomyCommand, CreateTask, HostCommand},
    paths::XanaPaths,
    storage::ProtectedStore,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopTaskDraft {
    pub name: String,
    pub workspace: String,
    pub profile: String,
    pub project: Option<String>,
    pub text: String,
    pub reminder: bool,
    pub workspace_reads: bool,
    pub trigger: DesktopTaskTrigger,
    pub expires: String,
}

/// One exact source, independent from the task's action and authority expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopTaskTrigger {
    Once {
        at: String,
    },
    Daily {
        time: String,
        timezone: String,
    },
    Files {
        root: String,
    },
    GithubRun {
        run: String,
        credential: DesktopGithubCredential,
    },
}

/// A named lookup location, never a token value or implicit account fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopGithubCredential {
    Environment { variable: String },
    Stored { id: String },
}

#[derive(Debug, Clone)]
pub struct DesktopTaskPreview {
    pub text: String,
    grant_digest: String,
}
pub use crate::autonomy::supervision::{
    TaskOverview as DesktopScheduledTask, WorkGroup as DesktopWorkGroup,
};
#[derive(Debug, Clone)]
pub struct DesktopAutonomySnapshot {
    pub jobs: Vec<DesktopScheduledTask>,
    pub next_after: Option<u64>,
    pub policy_revision: u64,
    pub detached_enabled: bool,
    pub startup_enabled: bool,
    pub host_status: String,
}
#[derive(Debug, Clone, Copy)]
pub enum DesktopScheduleEdit {
    Pause,
    Resume,
    ReviewUnknownAndResume,
    Cancel,
}
#[derive(Debug, Clone, Copy)]
pub enum DesktopHostEdit {
    Start,
    Stop,
    StopAndLock,
    Disable,
    EnableStartup,
    DisableStartup,
}

impl DesktopControlPlane {
    pub fn review_scheduled_task(
        &self,
        id: &str,
    ) -> Result<(DesktopScheduledTask, String), DesktopError> {
        let store = autonomy_commands::store(&self.paths).map_err(control_error)?;
        let job = store
            .autonomy_job(id.parse().map_err(control_error)?)
            .map_err(control_error)?;
        let detail = supervision::review(&self.paths, &store, &job).map_err(control_error)?;
        Ok((
            supervision::overview(&job),
            serde_json::to_string_pretty(&detail).map_err(control_error)?,
        ))
    }
    pub fn autonomy_snapshot(&self, after: u64) -> Result<DesktopAutonomySnapshot, DesktopError> {
        let store = autonomy_commands::store(&self.paths).map_err(control_error)?;
        let page = supervision::page(&store, after).map_err(control_error)?;
        let policy = store.autonomy_policy().map_err(control_error)?;
        let health = crate::local_host::inspect_descriptor_health(
            self.paths.runtime_dir(),
            self.paths.data_dir(),
        )
        .map_err(control_error)?;
        Ok(DesktopAutonomySnapshot {
            jobs: page.jobs,
            next_after: page.next_after,
            policy_revision: policy.revision,
            detached_enabled: policy.detached_enabled,
            startup_enabled: policy.startup_enabled,
            host_status: format!(
                "{health:?}\nStop impact: {}",
                serde_json::to_string(&store.autonomy_stop_impact().map_err(control_error)?)
                    .map_err(control_error)?
            ),
        })
    }
    pub fn preview_scheduled_task(
        &self,
        draft: &DesktopTaskDraft,
    ) -> Result<DesktopTaskPreview, DesktopError> {
        let store = autonomy_commands::store(&self.paths).map_err(control_error)?;
        let job = prepare_draft(&self.paths, &store, draft)?;
        preview(&job)
    }
    pub fn create_scheduled_task(
        &self,
        draft: DesktopTaskDraft,
        reviewed: DesktopTaskPreview,
    ) -> Result<String, DesktopError> {
        let store = autonomy_commands::store(&self.paths).map_err(control_error)?;
        let job = prepare_draft(&self.paths, &store, &draft)?;
        let job = commit_reviewed(&store, job, &reviewed)?;
        Ok(format!(
            "Created {} ({}) at revision {}. Detached host must be explicitly started to run it.",
            job.name, job.id, job.revision
        ))
    }
    pub fn inspect_scheduled_task(&self, id: &str, receipts: bool) -> Result<String, DesktopError> {
        let id = id.parse().map_err(control_error)?;
        let command = if receipts {
            AutonomyCommand::Receipts { id, after: 0 }
        } else {
            AutonomyCommand::Review { id }
        };
        serde_json::to_string_pretty(
            &autonomy_commands::execute(command, &self.paths).map_err(control_error)?,
        )
        .map_err(control_error)
    }
    pub fn edit_scheduled_task(
        &self,
        id: &str,
        revision: u64,
        edit: DesktopScheduleEdit,
    ) -> Result<String, DesktopError> {
        let id = id.parse().map_err(control_error)?;
        let command = match edit {
            DesktopScheduleEdit::Pause => AutonomyCommand::Pause { id, revision },
            DesktopScheduleEdit::Cancel => AutonomyCommand::Cancel { id, revision },
            DesktopScheduleEdit::Resume => AutonomyCommand::Resume {
                id,
                revision,
                review_unknown: false,
            },
            DesktopScheduleEdit::ReviewUnknownAndResume => AutonomyCommand::Resume {
                id,
                revision,
                review_unknown: true,
            },
        };
        autonomy_commands::execute(command, &self.paths).map_err(control_error)?;
        Ok("Schedule edit committed; refresh for the current revision. Cancellation requests are not a terminal stop receipt.".into())
    }
    pub fn edit_background_host(
        &self,
        revision: u64,
        edit: DesktopHostEdit,
    ) -> Result<String, DesktopError> {
        let command = match edit {
            DesktopHostEdit::Start => HostCommand::Start { revision },
            DesktopHostEdit::Stop => HostCommand::Stop {
                revision,
                lock: false,
            },
            DesktopHostEdit::StopAndLock => HostCommand::Stop {
                revision,
                lock: true,
            },
            DesktopHostEdit::Disable => HostCommand::Disable { revision },
            DesktopHostEdit::EnableStartup => HostCommand::Startup {
                revision,
                enable: true,
            },
            DesktopHostEdit::DisableStartup => HostCommand::Startup {
                revision,
                enable: false,
            },
        };
        let value = autonomy_commands::execute(AutonomyCommand::Host { command }, &self.paths)
            .map_err(control_error)?;
        serde_json::to_string_pretty(&value).map_err(control_error)
    }
}

fn prepare_draft(
    paths: &XanaPaths,
    store: &ProtectedStore,
    draft: &DesktopTaskDraft,
) -> Result<Job, DesktopError> {
    autonomy_commands::prepare(paths, store, creation_args(draft)?).map_err(control_error)
}

fn creation_args(draft: &DesktopTaskDraft) -> Result<CreateTask, DesktopError> {
    let mut metadata = vec![
        &draft.name,
        &draft.workspace,
        &draft.profile,
        &draft.expires,
    ];
    metadata.extend(draft.project.iter());
    match &draft.trigger {
        DesktopTaskTrigger::Once { at } => metadata.push(at),
        DesktopTaskTrigger::Daily { time, timezone } => metadata.extend([time, timezone]),
        DesktopTaskTrigger::Files { root } => metadata.push(root),
        DesktopTaskTrigger::GithubRun { run, credential } => {
            metadata.push(run);
            metadata.push(match credential {
                DesktopGithubCredential::Environment { variable } => variable,
                DesktopGithubCredential::Stored { id } => id,
            });
        }
    }
    if draft.text.len() > 16384 || metadata.into_iter().any(|value| value.len() > 4096) {
        return Err(control_error(
            "Task text or metadata exceeds its review bound",
        ));
    }
    let mut args = CreateTask {
        name: draft.name.clone(),
        workspace: draft.workspace.clone().into(),
        profile: draft.profile.clone(),
        project: draft
            .project
            .as_ref()
            .map(|id| id.parse())
            .transpose()
            .map_err(control_error)?,
        reminder: draft.reminder.then(|| draft.text.clone()),
        prompt: (!draft.reminder).then(|| draft.text.clone()),
        workspace_reads: draft.workspace_reads && !draft.reminder,
        at: None,
        daily: None,
        timezone: None,
        expires: draft.expires.clone(),
        authorize: true,
        reviewed_route: None,
        watch_root: None,
        github_run: None,
        github_credential: None,
    };
    match &draft.trigger {
        DesktopTaskTrigger::Once { at } => args.at = Some(at.clone()),
        DesktopTaskTrigger::Daily { time, timezone } => {
            args.daily = Some(time.clone());
            args.timezone = Some(timezone.clone());
        }
        DesktopTaskTrigger::Files { root } => args.watch_root = Some(root.into()),
        DesktopTaskTrigger::GithubRun { run, credential } => {
            args.github_run = Some(run.clone());
            args.github_credential = Some(match credential {
                DesktopGithubCredential::Environment { variable } => format!("env:{variable}"),
                DesktopGithubCredential::Stored { id } => format!("stored:{id}"),
            });
        }
    }
    Ok(args)
}

// Only immutable authority participates. Recomputed occurrence times, file
// baselines, and status observations may change without changing the grant.
fn grant(job: &Job) -> serde_json::Value {
    let source = match &job.trigger {
        Some(Trigger::Files(watch)) => {
            serde_json::json!({"kind":"selected_files", "root":watch.root, "identity":watch.identity})
        }
        Some(Trigger::Github(watch)) => {
            serde_json::json!({"kind":"github_run", "repository":watch.repository, "run":watch.run, "credential_source":watch.credential})
        }
        None => serde_json::Value::Null,
    };
    serde_json::json!({"name":job.name,"scope":job.scope,"action":job.action,"schedule":job.schedule,"source":source,"expires_at":job.expires_at,"budget":job.budget})
}

fn grant_digest(job: &Job) -> Result<String, DesktopError> {
    Ok(
        blake3::hash(&serde_json::to_vec(&grant(job)).map_err(control_error)?)
            .to_hex()
            .to_string(),
    )
}

fn preview(job: &Job) -> Result<DesktopTaskPreview, DesktopError> {
    let source_policy = match &job.trigger {
        Some(Trigger::Files(_)) => {
            "Selected files: metadata only, at most 256 entries / 16 levels, stable-change debounce. Links, root replacement, overflow or unknown Xana write origin require review. No source text becomes task instructions."
        }
        Some(Trigger::Github(_)) => {
            "Named CI: api.github.com only; Actions read for this repository/run using only the named credential source. No token is read during preview/creation. First observation and unchanged status do not invoke a model. No account scan, CLI credential fallback, log fetch or rerun."
        }
        None => "Calendar trigger: the exact saved time and timezone determine admission.",
    };
    Ok(DesktopTaskPreview {
        text: format!(
            "Exact task and authority\n{}\n\n{source_policy}\nOne shared background job. New approvals require owner action; uncertain outcomes never replay automatically.\nCreating this task does not start the detached host.",
            serde_json::to_string_pretty(&grant(job)).map_err(control_error)?
        ),
        grant_digest: grant_digest(job)?,
    })
}

fn commit_reviewed(
    store: &ProtectedStore,
    job: Job,
    reviewed: &DesktopTaskPreview,
) -> Result<Job, DesktopError> {
    if reviewed.grant_digest != grant_digest(&job)? {
        return Err(control_error(
            "Task, route or source identity changed; preview the exact task again",
        ));
    }
    store.autonomy_create(job).map_err(control_error)
}
