//! Runtime-owned bounded child orchestration.
//!
//! Route resolution is pure over explicit configuration and local availability
//! inputs. Child supervision and collection build on the resolved snapshot in
//! later modules without moving provider or frontend policy into this domain.

mod budget;
mod execution;
mod native;
mod routing;
mod supervisor;
mod types;

pub(crate) use budget::{
    BudgetError, BudgetLedger, BudgetReservation, OrchestrationBudget, ReservationRequest,
};
pub(crate) use execution::{
    ChildExecution, ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutcome,
    ChildExecutionOutput, PreparedChild,
};
pub(crate) use native::NativeChildFactory;
pub(crate) use routing::{
    EnforcementCapabilities, ExecutionOwner, ResolvedAgentConfig, RouteResolver,
    apply_spawn_restrictions,
};
#[cfg(test)]
pub(crate) use supervisor::SupervisorError;
pub(crate) use supervisor::{
    ChildCommitCommand, ChildCommitReceiver, ChildCommitSender, ChildSupervisor,
    ChildSupervisorHandle, ParentExecution,
};
pub(crate) use types::{
    AgentHandleSnapshot, AwaitAgentOptions, AwaitAgentOutcome, ChildActivity, ChildAdmission,
    ChildAttribution, ChildCancellationReceipt, ChildInspection, ChildLifecycle, ChildReport,
    ChildReportReference, ChildRestrictions, ChildTerminalStatus, ChildUsage, SpawnAgentRequest,
};
pub(crate) use types::{
    CHILD_REPORT_VERSION, MAX_CHILD_TASK_PREVIEW_BYTES, truncate_utf8, validate_spawn_request,
};
