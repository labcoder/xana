//! Native tools recheck the retained execution's authority immediately before
//! effects. Storage stays in this application-owned decorator, not the agent.
use super::{RetainedWorker, WorkerState, authority::check_scope};
use crate::{
    identity::AgentId,
    paths::XanaPaths,
    storage::ProtectedStore,
    tool::{PlannedToolInvocation, Tool, ToolDefinition, ToolExecutionContext, ToolRegistry},
};
use anyhow::{Result, ensure};
use futures::future::BoxFuture;
use serde_json::Value;
use std::path::Path;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct RetainedToolGuard {
    paths: XanaPaths,
    store: ProtectedStore,
    worker: AgentId,
    cancellation_identity: Uuid,
    execution: Uuid,
    cancellation: CancellationToken,
}

impl RetainedToolGuard {
    pub(crate) fn new(
        paths: XanaPaths,
        store: ProtectedStore,
        worker: &RetainedWorker,
        execution: Uuid,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            paths,
            store,
            worker: worker.id,
            cancellation_identity: worker.cancellation,
            execution,
            cancellation,
        }
    }

    pub(crate) fn wrap(&self, registry: ToolRegistry) -> ToolRegistry {
        registry.map_implementations(|tool| {
            Box::new(GuardedTool {
                tool,
                authority: self.clone(),
            })
        })
    }

    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancellation.is_cancelled(),
            "retained execution cancelled"
        );
        let worker = self.store.retained_worker(self.worker)?;
        ensure!(
            worker.cancellation == self.cancellation_identity
                && matches!(worker.state, WorkerState::Running | WorkerState::Draining)
                && worker
                    .active
                    .as_ref()
                    .is_some_and(|(id, _)| *id == self.execution),
            "retained execution ended or its authority was revoked"
        );
        check_scope(&self.paths, &self.store, &worker)
    }
}

struct GuardedTool {
    tool: Box<dyn Tool>,
    authority: RetainedToolGuard,
}

impl Tool for GuardedTool {
    fn definition(&self) -> ToolDefinition {
        self.tool.definition()
    }

    fn plan(&self, arguments: &Value, workspace: &Path) -> Result<PlannedToolInvocation, String> {
        self.tool.plan(arguments, workspace)
    }

    fn outbound_disposition(
        &self,
        planned: &PlannedToolInvocation,
    ) -> Result<Option<crate::outbound::OutboundDisposition>, String> {
        self.tool.outbound_disposition(planned)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let authority = self.authority.clone();
            tokio::task::spawn_blocking(move || authority.check())
                .await
                .map_err(|_| "retained authority check stopped".to_owned())?
                .map_err(|error| error.to_string())?;
            if self.authority.cancellation.is_cancelled() {
                return Err("retained execution cancelled".into());
            }
            self.tool.execute(planned, context).await
        })
    }
}
