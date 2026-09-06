//! Bounded public projections and owner intents; no database/runtime capability.
use super::{DesktopControlPlane, DesktopError, control_error};
use crate::{app::worker_commands, cli::WorkerCommand, identity::AgentId, storage::ProtectedStore};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct DesktopWorkerSummary {
    pub id: String,
    pub revision: u64,
    pub goal: String,
    pub state: String,
    pub route: String,
    pub queued: usize,
    pub executions: u64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopWorkerIntent {
    Retain {
        session: String,
        agent: String,
        goal: String,
        expires: String,
        evidence: Vec<String>,
        authorize: bool,
    },
    FollowUp {
        id: String,
        revision: u64,
        request_id: String,
        text: String,
    },
    Drain {
        id: String,
        revision: u64,
    },
    Stop {
        id: String,
        revision: u64,
    },
    Recover {
        id: String,
        revision: u64,
        review_unknown: bool,
    },
    Context {
        id: String,
        revision: u64,
        operation: String,
    },
}

#[derive(Clone, Default)]
pub struct DesktopWorkerCancellation(CancellationToken);
impl DesktopWorkerCancellation {
    pub fn cancel(&self) {
        self.0.cancel();
    }
}

impl DesktopControlPlane {
    /// Call on a client background executor, never its paint/event thread.
    pub fn edit_retained_worker_blocking(
        &self,
        intent: DesktopWorkerIntent,
        cancellation: DesktopWorkerCancellation,
    ) -> Result<String, DesktopError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(control_error)?
            .block_on(self.edit_retained_worker(intent, cancellation))
    }
    /// The frontend owns cancellation; this bounded runner owns its Tokio I/O.
    pub fn run_retained_worker_blocking(
        &self,
        id: &str,
        revision: u64,
        cancellation: DesktopWorkerCancellation,
    ) -> Result<String, DesktopError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(control_error)?
            .block_on(self.run_retained_worker(id, revision, cancellation))
    }
    pub fn retained_workers(
        &self,
        after: Option<&str>,
    ) -> Result<Vec<DesktopWorkerSummary>, DesktopError> {
        let store = ProtectedStore::configured(self.paths.data_dir())
            .map_err(control_error)?
            .ok_or_else(|| control_error("Retained workers require protected storage"))?;
        let after = after.map(str::parse).transpose().map_err(control_error)?;
        Ok(store
            .retained_page(after)
            .map_err(control_error)?
            .into_iter()
            .map(|worker| DesktopWorkerSummary {
                id: worker.id.to_string(),
                revision: worker.revision,
                goal: worker.goal,
                state: format!("{:?}", worker.state),
                route: worker.admission.attribution.route,
                queued: worker.mailbox.len(),
                executions: worker.executions,
                expires_at: worker.expires_at,
            })
            .collect())
    }
    pub fn inspect_retained_worker(&self, id: &str) -> Result<String, DesktopError> {
        let store = ProtectedStore::configured(self.paths.data_dir())
            .map_err(control_error)?
            .ok_or_else(|| control_error("Retained workers require protected storage"))?;
        serde_json::to_string_pretty(
            &store
                .retained_worker(id.parse().map_err(control_error)?)
                .map_err(control_error)?,
        )
        .map_err(control_error)
    }
    pub async fn edit_retained_worker(
        &self,
        intent: DesktopWorkerIntent,
        cancellation: DesktopWorkerCancellation,
    ) -> Result<String, DesktopError> {
        let command = worker_command(intent)?;
        let value = worker_commands::execute(self.paths.clone(), command, cancellation.0)
            .await
            .map_err(control_error)?;
        serde_json::to_string_pretty(&value).map_err(control_error)
    }
    pub async fn run_retained_worker(
        &self,
        id: &str,
        revision: u64,
        cancellation: DesktopWorkerCancellation,
    ) -> Result<String, DesktopError> {
        let id: AgentId = id.parse().map_err(control_error)?;
        let command: WorkerCommand = serde_json::from_value(
            serde_json::json!({"command":"run","target":{"id":id,"revision":revision}}),
        )
        .map_err(control_error)?;
        let value = worker_commands::execute(self.paths.clone(), command, cancellation.0)
            .await
            .map_err(control_error)?;
        serde_json::to_string_pretty(&value).map_err(control_error)
    }
}

fn worker_command(intent: DesktopWorkerIntent) -> Result<WorkerCommand, DesktopError> {
    if serde_json::to_vec(&intent).map_err(control_error)?.len() > 32 * 1024 {
        return Err(control_error("Worker intent exceeds 32 KiB"));
    }
    let target = |id: String, revision: u64| -> Result<serde_json::Value, DesktopError> {
        let id: AgentId = id.parse().map_err(control_error)?;
        Ok(serde_json::json!({"id":id,"revision":revision}))
    };
    let wire = match intent {
        DesktopWorkerIntent::Retain {
            session,
            agent,
            goal,
            expires,
            evidence,
            authorize,
        } => {
            serde_json::json!({"command":"retain","session":session,"agent":agent,"goal":goal,"expires":expires,"evidence":evidence,"authorize":authorize})
        }
        DesktopWorkerIntent::FollowUp {
            id,
            revision,
            request_id,
            text,
        } => {
            serde_json::json!({"command":"follow_up","target":target(id,revision)?,"request_id":request_id,"text":text})
        }
        DesktopWorkerIntent::Drain { id, revision } => {
            serde_json::json!({"command":"drain","target":target(id,revision)?})
        }
        DesktopWorkerIntent::Stop { id, revision } => {
            serde_json::json!({"command":"stop","target":target(id,revision)?})
        }
        DesktopWorkerIntent::Recover {
            id,
            revision,
            review_unknown,
        } => {
            serde_json::json!({"command":"recover","target":target(id,revision)?,"review_unknown":review_unknown})
        }
        DesktopWorkerIntent::Context {
            id,
            revision,
            operation,
        } => {
            serde_json::json!({"command":"context","target":target(id,revision)?,"operation":operation})
        }
    };
    serde_json::from_value(wire).map_err(control_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graphical_intents_map_to_the_actual_closed_owner_commands() {
        let id = AgentId::new().to_string();
        for intent in [
            DesktopWorkerIntent::Drain {
                id: id.clone(),
                revision: 2,
            },
            DesktopWorkerIntent::Stop {
                id: id.clone(),
                revision: 2,
            },
            DesktopWorkerIntent::Recover {
                id: id.clone(),
                revision: 2,
                review_unknown: true,
            },
            DesktopWorkerIntent::FollowUp {
                id: id.clone(),
                revision: 2,
                request_id: uuid::Uuid::new_v4().to_string(),
                text: "continue".into(),
            },
            DesktopWorkerIntent::Context {
                id: id.clone(),
                revision: 2,
                operation: "{}".into(),
            },
            DesktopWorkerIntent::Retain {
                session: crate::identity::SessionId::new().to_string(),
                agent: id,
                goal: "bounded".into(),
                expires: "2027-01-01T00:00:00Z".into(),
                evidence: Vec::new(),
                authorize: true,
            },
        ] {
            assert!(worker_command(intent).is_ok());
        }
        assert!(
            worker_command(DesktopWorkerIntent::Stop {
                id: "invalid".into(),
                revision: 1
            })
            .is_err()
        );
    }
}
