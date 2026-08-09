use super::{ChildAttribution, ChildUsage, ResolvedAgentConfig, SpawnAgentRequest};
use crate::{
    identity::OperationId,
    permission::{PermissionBrokerHandle, PermissionPolicy},
    runtime::AgentEvent,
};
use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) trait ChildExecutionFactory: Send + Sync {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String>;
}

pub(crate) struct PreparedChild {
    pub(crate) resolved: ResolvedAgentConfig,
    pub(crate) permission_policy: PermissionPolicy,
    pub(crate) execution: Box<dyn ChildExecution>,
}

impl PreparedChild {
    pub(crate) fn new(
        resolved: ResolvedAgentConfig,
        permission_policy: PermissionPolicy,
        execution: Box<dyn ChildExecution>,
    ) -> Self {
        Self {
            resolved,
            permission_policy,
            execution,
        }
    }
}

pub(crate) struct ChildExecutionContext {
    pub(crate) attribution: ChildAttribution,
    pub(crate) operation_id: OperationId,
    pub(crate) permissions: PermissionBrokerHandle,
    pub(crate) events: mpsc::UnboundedSender<AgentEvent>,
    pub(crate) cancellation: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildExecutionOutput {
    pub(crate) text: String,
    pub(crate) usage: ChildUsage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildExecutionOutcome {
    Completed(ChildExecutionOutput),
    Failed(String),
    Cancelled(String),
}

pub(crate) trait ChildExecution: Send {
    fn handles_cancellation(&self) -> bool {
        false
    }

    fn run(
        self: Box<Self>,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome>;
}
