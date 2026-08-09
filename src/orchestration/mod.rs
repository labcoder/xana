//! Runtime-owned bounded child orchestration.
//!
//! Route resolution is pure over explicit configuration and local availability
//! inputs. Child supervision and collection build on the resolved snapshot in
//! later modules without moving provider or frontend policy into this domain.

mod budget;
mod collection;
mod execution;
mod native;
mod plan;
mod report;
mod routing;
mod supervisor;
mod types;

pub(crate) use budget::{
    BudgetError, BudgetLedger, BudgetReservation, OrchestrationBudget, ReservationRequest,
};
pub(crate) use collection::{CollectionObservation, build_collection_result};
pub(crate) use execution::{
    ChildExecution, ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutcome,
    ChildExecutionOutput, PreparedChild,
};
pub(crate) use native::NativeChildFactory;
#[cfg(test)]
pub(crate) use plan::PlanHandleRef;
pub(crate) use plan::{
    MAX_PLAN_RESULT_BYTES, MAX_PLAN_STEPS, ORCHESTRATION_PLAN_VERSION, OrchestrationPlan,
    OrchestrationPlanResult, OrchestrationPlanStart, OrchestrationPlanStep,
    OrchestrationPlanStepResult, PlanValidationDiagnostic, ValidatedOrchestrationPlan,
    await_options, collect_options, resolve_handle, validate_plan_structure, validation_diagnostic,
};
pub(crate) use report::{MaterializedChildReport, materialize_completed_report};
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
pub(crate) use types::PlanChildAttribution;
pub(crate) use types::{
    AgentHandleSnapshot, AwaitAgentOptions, AwaitAgentOutcome, ChildActivity, ChildAdmission,
    ChildAttribution, ChildCancellationReceipt, ChildInspection, ChildLifecycle, ChildReport,
    ChildReportReference, ChildRestrictions, ChildResultSchema, ChildTerminalStatus, ChildUsage,
    CollectAgentsOptions, CollectedChildResult, CollectedValue, CollectionEntryState,
    CollectionFailurePolicy, CollectionResult, SpawnAgentRequest,
};
pub(crate) use types::{
    CHILD_REPORT_VERSION, COLLECTION_RESULT_VERSION, MAX_CHILD_TASK_PREVIEW_BYTES,
    MAX_COLLECTION_HANDLES, MAX_COLLECTION_PREVIEW_BYTES, MAX_COLLECTION_RESULT_BYTES,
    truncate_utf8, validate_spawn_request,
};
