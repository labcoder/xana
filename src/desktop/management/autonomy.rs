//! Bounded Desktop facts and typed owner intent. No runtime handle, provider,
//! credential value, or raw database capability crosses this seam.
use super::{DesktopControlPlane, DesktopError, control_error};
use crate::{
    app::autonomy_commands,
    autonomy::{runner, supervision},
    cli::{AutonomyCommand, CreateTask, HostCommand},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopTaskDraft {
    pub name: String,
    pub workspace: String,
    pub profile: String,
    pub project: Option<String>,
    pub text: String,
    pub reminder: bool,
    pub workspace_reads: bool,
    pub daily: bool,
    pub time: String,
    pub timezone: String,
    pub expires: String,
}
#[derive(Debug, Clone)]
pub struct DesktopTaskPreview {
    pub text: String,
    pub route_digest: String,
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
        if draft.text.len() > 16384
            || [
                &draft.name,
                &draft.workspace,
                &draft.profile,
                &draft.time,
                &draft.timezone,
                &draft.expires,
            ]
            .iter()
            .any(|value| value.len() > 4096)
        {
            return Err(control_error(
                "Task text or metadata exceeds its review bound",
            ));
        }
        let project = draft
            .project
            .as_ref()
            .map(|id| id.parse())
            .transpose()
            .map_err(control_error)?;
        let scope = runner::resolve_scope(
            &self.paths,
            std::path::Path::new(&draft.workspace),
            &draft.profile,
            project,
        )
        .map_err(control_error)?;
        Ok(DesktopTaskPreview {
            text: format!(
                "Task: {}\nWorkspace: {}\nProfile: {} ({})\nConnection/model: {} / {}\nRecipient: {}\nAction: {}\nWorkspace read/disclosure grant: {}\nSchedule: {} {}\nAuthority expires: {}\nLimits: one shared background job, 8192 conservative tokens/run, 120 seconds, 32768/day.\nNew approvals require owner action. Unknown outcomes never replay automatically.\n\n{}",
                draft.name,
                scope.workspace.display(),
                scope.profile,
                scope.profile_id,
                scope.connection,
                scope.model,
                scope.endpoint,
                if draft.reminder {
                    "local inbox reminder"
                } else {
                    "native task"
                },
                draft.workspace_reads,
                draft.time,
                if draft.daily {
                    draft.timezone.as_str()
                } else {
                    "one-shot"
                },
                draft.expires,
                draft.text
            ),
            route_digest: scope.configuration_digest,
        })
    }
    pub fn create_scheduled_task(
        &self,
        draft: DesktopTaskDraft,
        reviewed_route: String,
    ) -> Result<String, DesktopError> {
        let args = CreateTask {
            name: draft.name,
            workspace: draft.workspace.into(),
            profile: draft.profile,
            project: draft
                .project
                .map(|id| id.parse())
                .transpose()
                .map_err(control_error)?,
            reminder: draft.reminder.then(|| draft.text.clone()),
            prompt: (!draft.reminder).then_some(draft.text),
            workspace_reads: draft.workspace_reads,
            at: (!draft.daily).then(|| draft.time.clone()),
            daily: draft.daily.then_some(draft.time),
            timezone: draft.daily.then_some(draft.timezone),
            expires: draft.expires,
            authorize: true,
            reviewed_route: Some(reviewed_route),
            watch_root: None,
            github_run: None,
            github_credential: None,
        };
        let job = autonomy_commands::create(&self.paths, args).map_err(control_error)?;
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
