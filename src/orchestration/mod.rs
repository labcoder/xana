//! Runtime-owned bounded child orchestration.
//!
//! Route resolution is pure over explicit configuration and local availability
//! inputs. Child supervision and collection build on the resolved snapshot in
//! later modules without moving provider or frontend policy into this domain.

mod execution;
mod native;
mod routing;
mod supervisor;
mod types;

pub(crate) use execution::{
    ChildExecution, ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutput,
    PreparedChild,
};
pub(crate) use native::NativeChildFactory;
pub(crate) use routing::{ExecutionOwner, ResolvedAgentConfig, RouteResolver};
pub(crate) use supervisor::{
    ChildCommitCommand, ChildCommitReceiver, ChildCommitSender, ChildSupervisor,
    ChildSupervisorHandle, ParentExecution,
};
pub(crate) use types::{
    AgentHandleSnapshot, ChildActivity, ChildAdmission, ChildAttribution, ChildLifecycle,
    ChildReport, ChildReportReference, ChildTerminalStatus, ChildUsage, SpawnAgentRequest,
};
pub(crate) use types::{
    CHILD_REPORT_VERSION, MAX_CHILD_TASK_PREVIEW_BYTES, truncate_utf8, validate_spawn_request,
};
