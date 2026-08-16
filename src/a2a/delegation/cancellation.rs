//! Runtime-owned cleanup for identified remote A2A tasks.

use super::{A2aWireError, ExternalAgentActivityKind, cancel_remote_task, emit};
use crate::{
    a2a::ExternalAgentManager, config::CredentialReference, identity::OperationId,
    native_runtime::AgentEventSender, tool::DeferredCleanup,
};

#[derive(Clone)]
pub(super) struct RemoteCancellation {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    credential: Option<CredentialReference>,
    connection: String,
    identity_digest: String,
    manager: ExternalAgentManager,
    events: Option<AgentEventSender>,
    operation_id: OperationId,
    agent_name: String,
}

impl RemoteCancellation {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        client: reqwest::Client,
        endpoint: reqwest::Url,
        credential: Option<CredentialReference>,
        connection: String,
        identity_digest: String,
        manager: ExternalAgentManager,
        events: Option<AgentEventSender>,
        operation_id: OperationId,
        agent_name: String,
    ) -> Self {
        Self {
            client,
            endpoint,
            credential,
            connection,
            identity_digest,
            manager,
            events,
            operation_id,
            agent_name,
        }
    }

    fn begin(&self, task_id: &str) -> bool {
        if self
            .manager
            .record_task(
                &self.connection,
                &self.identity_digest,
                task_id,
                None,
                "detached_unknown",
                "requested_unconfirmed",
            )
            .is_err()
        {
            emit(
                self.events.as_ref(),
                self.operation_id,
                &self.connection,
                &self.agent_name,
                ExternalAgentActivityKind::Detached {
                    task_id: task_id.to_owned(),
                },
            );
            return false;
        }
        emit(
            self.events.as_ref(),
            self.operation_id,
            &self.connection,
            &self.agent_name,
            ExternalAgentActivityKind::CancellationRequested {
                task_id: task_id.to_owned(),
            },
        );
        true
    }

    fn unavailable(&self, task_id: String) {
        let _ = self.manager.record_task(
            &self.connection,
            &self.identity_digest,
            &task_id,
            None,
            "detached_unknown",
            "not_scheduled",
        );
        emit(
            self.events.as_ref(),
            self.operation_id,
            &self.connection,
            &self.agent_name,
            ExternalAgentActivityKind::Detached { task_id },
        );
    }

    async fn finish(&self, task_id: String) {
        let outcome = cancel_remote_task(
            &self.client,
            self.endpoint.clone(),
            self.credential.as_ref(),
            &task_id,
        )
        .await;
        let (state, remote_cancel) = cancellation_status(&outcome);
        let persisted = self.manager.record_task(
            &self.connection,
            &self.identity_digest,
            &task_id,
            None,
            state,
            remote_cancel,
        );
        if remote_cancel != "confirmed" || persisted.is_err() {
            emit(
                self.events.as_ref(),
                self.operation_id,
                &self.connection,
                &self.agent_name,
                ExternalAgentActivityKind::Detached { task_id },
            );
        }
    }
}

pub(super) fn cancellation_status(
    outcome: &Result<bool, A2aWireError>,
) -> (&'static str, &'static str) {
    match outcome {
        Ok(true) => ("canceled", "confirmed"),
        Ok(false) => ("detached_unknown", "failed"),
        Err(A2aWireError::TimedOut) => ("detached_unknown", "timed_out"),
        Err(A2aWireError::Cancelled) => ("detached_unknown", "unknown"),
        Err(_) => ("detached_unknown", "failed"),
    }
}

pub(super) struct RemoteTaskLease {
    cleanup: DeferredCleanup,
    cancellation: RemoteCancellation,
    task_id: Option<String>,
}

impl RemoteTaskLease {
    pub(super) fn new(cleanup: DeferredCleanup, cancellation: RemoteCancellation) -> Self {
        Self {
            cleanup,
            cancellation,
            task_id: None,
        }
    }

    pub(super) fn arm(&mut self, task_id: String) {
        self.task_id.get_or_insert(task_id);
    }

    pub(super) fn disarm(&mut self) {
        self.task_id = None;
    }
}

impl Drop for RemoteTaskLease {
    fn drop(&mut self) {
        let Some(task_id) = self.task_id.take() else {
            return;
        };
        if !self.cancellation.begin(&task_id) {
            return;
        }
        let cancellation = self.cancellation.clone();
        let scheduled_task_id = task_id.clone();
        if !self.cleanup.schedule(Box::pin(async move {
            cancellation.finish(scheduled_task_id).await;
        })) {
            self.cancellation.unavailable(task_id);
        }
    }
}
