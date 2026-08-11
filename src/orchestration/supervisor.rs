//! Actor-owned child supervision.
//!
//! The facade owns durable child state, bounded admission, scheduling, and
//! command dispatch. Focused child modules own admission, lifecycle,
//! observation projection, and the command-side handle without exposing a
//! second orchestration domain.

mod admission;
mod encoded;
mod handle;
mod lifecycle;
mod runner;

use super::{
    AgentHandleSnapshot, AwaitAgentOptions, AwaitAgentOutcome, BudgetError, BudgetLedger,
    ChildActivity, ChildAdmission, ChildAttribution, ChildCancellationReceipt,
    ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutcome, ChildInspection,
    ChildLifecycle, ChildReport, CollectAgentsOptions, CollectionFailurePolicy,
    CollectionObservation, CollectionResult, MAX_COLLECTION_HANDLES, MAX_PLAN_RESULT_BYTES,
    MaterializedChildReport, OrchestrationBudget, OrchestrationPlan, OrchestrationPlanResult,
    OrchestrationPlanStart, OrchestrationPlanStep, OrchestrationPlanStepResult,
    PlanChildAttribution, PlanValidationDiagnostic, PreparedChild, ReservationRequest,
    SpawnAgentRequest, ValidatedOrchestrationPlan, await_options, build_collection_result,
    collect_options, materialize_completed_report, resolve_handle, truncate_utf8,
    validate_plan_structure, validate_spawn_request, validation_diagnostic,
};
use crate::{
    artifact::ArtifactStore,
    identity::{AgentId, OperationId, OrchestrationPlanId, ThreadId, ToolInvocationId},
    native_runtime::{AgentEvent, AgentEventSender, DroppedAgentEvents, OperationState},
    permission::{ControllerDecision, PermissionBroker, PermissionBrokerHandle},
    session::SessionRecord,
};
use encoded::encoded_len;
use futures::{StreamExt, stream::FuturesUnordered};
use runner::{CHILD_OBSERVATION_CHANNEL_CAPACITY, RunningChild, run_child_execution};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::Arc,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time,
};
use tokio_util::sync::CancellationToken;

const SUPERVISOR_COMMAND_CAPACITY: usize = 16;
const CHILD_COMMIT_CAPACITY: usize = 16;
const MAX_WAITERS_PER_CHILD: usize = 32;
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

async fn wait_for_deadline(deadline: Option<time::Instant>) {
    match deadline {
        Some(deadline) => time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParentExecution {
    pub(crate) agent_id: AgentId,
    pub(crate) thread_id: ThreadId,
}

#[derive(Clone)]
pub(crate) struct ChildCommitSender {
    sender: mpsc::Sender<ChildCommitCommand>,
}

pub(crate) type ChildCommitReceiver = mpsc::Receiver<ChildCommitCommand>;

pub(crate) struct ChildCommitCommand {
    pub(crate) record: SessionRecord,
    pub(crate) acknowledged: oneshot::Sender<Result<(), String>>,
}

impl ChildCommitSender {
    pub(crate) fn channel() -> (Self, ChildCommitReceiver) {
        let (sender, receiver) = mpsc::channel(CHILD_COMMIT_CAPACITY);
        (Self { sender }, receiver)
    }

    pub(crate) async fn append(&self, record: SessionRecord) -> Result<(), SupervisorError> {
        let (acknowledged, acknowledgement) = oneshot::channel();
        self.sender
            .send(ChildCommitCommand {
                record,
                acknowledged,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        acknowledgement
            .await
            .map_err(|_| SupervisorError::Unavailable)?
            .map_err(SupervisorError::Durability)
    }
}

#[derive(Clone)]
pub(crate) struct ChildSupervisorHandle {
    commands: mpsc::Sender<SupervisorCommand>,
    artifact_store: ArtifactStore,
}

pub(crate) struct ChildSupervisor {
    parent: ParentExecution,
    factory: Arc<dyn ChildExecutionFactory>,
    commands: mpsc::Receiver<SupervisorCommand>,
    children: BTreeMap<AgentId, ActiveChild>,
    completions: mpsc::UnboundedReceiver<ChildCompletion>,
    completion_sender: mpsc::UnboundedSender<ChildCompletion>,
    ledger: BudgetLedger,
    admission_queue: VecDeque<AgentId>,
    artifact_store: ArtifactStore,
    artifact_owner: crate::identity::PrincipalId,
    started_plans: BTreeSet<OrchestrationPlanId>,
}

struct ActiveChild {
    snapshot: AgentHandleSnapshot,
    report: Option<ChildReport>,
    terminal_error: Option<SupervisorError>,
    waiters: Vec<oneshot::Sender<Result<ChildReport, SupervisorError>>>,
    prepared: Option<PreparedChild>,
    permissions: Option<PermissionBrokerHandle>,
    cancellation: CancellationToken,
    cancellation_requested: bool,
    task: Option<JoinHandle<()>>,
    running_reserved: bool,
    deadline: Option<time::Instant>,
}

struct AdmissionCandidate {
    snapshot: AgentHandleSnapshot,
    prepared: PreparedChild,
    reservation: ReservationRequest,
    deadline: time::Instant,
}

fn reservation_for(admission: &ChildAdmission) -> ReservationRequest {
    ReservationRequest {
        tool_rounds: admission.max_tool_rounds,
        context_tokens: admission.limits.max_context_tokens,
        report_bytes: admission.limits.max_report_bytes,
        artifact_bytes: admission.limits.max_artifact_bytes,
    }
}

enum SupervisorCommand {
    Spawn {
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
        reply: oneshot::Sender<Result<AgentHandleSnapshot, SupervisorError>>,
    },
    SpawnMany {
        parent_operation_id: OperationId,
        requests: Vec<SpawnAgentRequest>,
        reply: oneshot::Sender<Result<Vec<AgentHandleSnapshot>, SupervisorError>>,
    },
    Await {
        agent_id: AgentId,
        reply: oneshot::Sender<Result<ChildReport, SupervisorError>>,
    },
    List {
        reply: oneshot::Sender<Vec<ChildInspection>>,
    },
    Inspect {
        agent_id: AgentId,
        reply: oneshot::Sender<Result<ChildInspection, SupervisorError>>,
    },
    SnapshotMany {
        agent_ids: Vec<AgentId>,
        reply: oneshot::Sender<Result<Vec<ChildInspection>, SupervisorError>>,
    },
    ValidatePlan {
        parent_operation_id: OperationId,
        plan: OrchestrationPlan,
        reply: oneshot::Sender<Result<PlanValidationDiagnostic, SupervisorError>>,
    },
    StartPlan {
        parent_operation_id: OperationId,
        plan: OrchestrationPlan,
        reply: oneshot::Sender<Result<StartedPlan, SupervisorError>>,
    },
    Cancel {
        agent_id: AgentId,
        reply: oneshot::Sender<Result<ChildCancellationReceipt, SupervisorError>>,
    },
    DecidePermission {
        agent_id: AgentId,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

struct ChildCompletion {
    agent_id: AgentId,
    materialized: MaterializedChildReport,
}

struct StartedPlan {
    validated: ValidatedOrchestrationPlan,
    handles: Vec<AgentHandleSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SupervisorError {
    Unavailable,
    InvalidRequest(String),
    Budget(BudgetError),
    Admission(String),
    UnknownAgent(AgentId),
    TooManyWaiters(AgentId),
    DuplicateAgent(AgentId),
    TooManyCollectionHandles { requested: usize, maximum: usize },
    Collection(String),
    Plan(String),
    DuplicatePlan(OrchestrationPlanId),
    Permission(String),
    Durability(String),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("child supervisor is unavailable"),
            Self::InvalidRequest(reason) => write!(formatter, "invalid child request: {reason}"),
            Self::Budget(error) => write!(formatter, "child budget rejected admission: {error}"),
            Self::Admission(reason) => write!(formatter, "child admission failed: {reason}"),
            Self::UnknownAgent(agent_id) => write!(formatter, "unknown child agent {agent_id}"),
            Self::TooManyWaiters(agent_id) => write!(
                formatter,
                "child agent {agent_id} already has the maximum number of waiters"
            ),
            Self::DuplicateAgent(agent_id) => {
                write!(formatter, "child collection repeats agent {agent_id}")
            }
            Self::TooManyCollectionHandles { requested, maximum } => write!(
                formatter,
                "child collection requests {requested} handles, exceeding maximum {maximum}"
            ),
            Self::Collection(reason) => write!(formatter, "child collection failed: {reason}"),
            Self::Plan(reason) => write!(formatter, "orchestration plan rejected: {reason}"),
            Self::DuplicatePlan(plan_id) => {
                write!(
                    formatter,
                    "orchestration plan {plan_id} has already started"
                )
            }
            Self::Permission(reason) => {
                write!(formatter, "child permission decision failed: {reason}")
            }
            Self::Durability(reason) => {
                write!(formatter, "could not commit child state: {reason}")
            }
        }
    }
}

impl std::error::Error for SupervisorError {}

impl ChildSupervisor {
    #[cfg(test)]
    pub(crate) fn new(
        parent: ParentExecution,
        factory: Arc<dyn ChildExecutionFactory>,
    ) -> (ChildSupervisorHandle, Self) {
        Self::with_restored(
            parent,
            factory,
            Vec::new(),
            Vec::new(),
            OrchestrationBudget::new(crate::config::OrchestrationLimits::default(), 8),
            test_artifact_store(),
            crate::identity::PrincipalId::new(),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_budget(
        parent: ParentExecution,
        factory: Arc<dyn ChildExecutionFactory>,
        budget: OrchestrationBudget,
    ) -> (ChildSupervisorHandle, Self) {
        Self::with_restored(
            parent,
            factory,
            Vec::new(),
            Vec::new(),
            budget,
            test_artifact_store(),
            crate::identity::PrincipalId::new(),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_budget_and_artifacts(
        parent: ParentExecution,
        factory: Arc<dyn ChildExecutionFactory>,
        budget: OrchestrationBudget,
        artifact_store: ArtifactStore,
        artifact_owner: crate::identity::PrincipalId,
    ) -> (ChildSupervisorHandle, Self) {
        Self::with_restored(
            parent,
            factory,
            Vec::new(),
            Vec::new(),
            budget,
            artifact_store,
            artifact_owner,
        )
    }

    pub(crate) fn with_restored(
        parent: ParentExecution,
        factory: Arc<dyn ChildExecutionFactory>,
        restored: Vec<ChildInspection>,
        restored_plans: Vec<OrchestrationPlanStart>,
        budget: OrchestrationBudget,
        artifact_store: ArtifactStore,
        artifact_owner: crate::identity::PrincipalId,
    ) -> (ChildSupervisorHandle, Self) {
        let (command_sender, commands) = mpsc::channel(SUPERVISOR_COMMAND_CAPACITY);
        let (completion_sender, completions) = mpsc::unbounded_channel();
        let handle_artifact_store = artifact_store.clone();
        let restored_reservations = restored
            .iter()
            .map(|inspection| reservation_for(&inspection.handle.admission))
            .collect::<Vec<_>>();
        let children = restored
            .into_iter()
            .map(|inspection| {
                let agent_id = inspection.handle.admission.attribution.agent_id;
                (
                    agent_id,
                    ActiveChild {
                        snapshot: inspection.handle,
                        report: inspection.report,
                        terminal_error: None,
                        waiters: Vec::new(),
                        prepared: None,
                        permissions: None,
                        cancellation: CancellationToken::new(),
                        cancellation_requested: false,
                        task: None,
                        running_reserved: false,
                        deadline: None,
                    },
                )
            })
            .collect();
        (
            ChildSupervisorHandle {
                commands: command_sender,
                artifact_store: handle_artifact_store,
            },
            Self {
                parent,
                factory,
                commands,
                children,
                completions,
                completion_sender,
                ledger: BudgetLedger::with_consumed(budget, &restored_reservations),
                admission_queue: VecDeque::new(),
                artifact_store,
                artifact_owner,
                started_plans: restored_plans
                    .into_iter()
                    .map(|start| start.plan_id)
                    .collect(),
            },
        )
    }

    pub(crate) async fn run(
        mut self,
        commits: ChildCommitSender,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) {
        loop {
            let next_deadline = self.next_deadline();
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        self.shutdown_children(&commits, &events).await;
                        return;
                    };
                    if self.handle_command(command, &commits, &events).await {
                        return;
                    }
                }
                completion = self.completions.recv() => {
                    if let Some(completion) = completion {
                        self.handle_completion(completion, &commits, &events).await;
                        self.start_queued_children(&commits, &events).await;
                    }
                }
                _ = wait_for_deadline(next_deadline) => {
                    self.expire_deadlines(&commits, &events).await;
                    self.start_queued_children(&commits, &events).await;
                }
            }
        }
    }

    async fn handle_command(
        &mut self,
        command: SupervisorCommand,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> bool {
        match command {
            SupervisorCommand::Spawn {
                parent_operation_id,
                request,
                reply,
            } => {
                let result = self
                    .spawn_child(parent_operation_id, request, commits, events)
                    .await;
                let _ = reply.send(result);
                self.start_queued_children(commits, events).await;
            }
            SupervisorCommand::SpawnMany {
                parent_operation_id,
                requests,
                reply,
            } => {
                let result = self
                    .spawn_many_children(parent_operation_id, requests, commits, events)
                    .await;
                let _ = reply.send(result);
                self.start_queued_children(commits, events).await;
            }
            SupervisorCommand::Await { agent_id, reply } => {
                let Some(child) = self.children.get_mut(&agent_id) else {
                    let _ = reply.send(Err(SupervisorError::UnknownAgent(agent_id)));
                    return false;
                };
                if let Some(report) = &child.report {
                    let _ = reply.send(Ok(report.clone()));
                } else if let Some(error) = &child.terminal_error {
                    let _ = reply.send(Err(error.clone()));
                } else {
                    child.waiters.retain(|waiter| !waiter.is_closed());
                    if child.waiters.len() >= MAX_WAITERS_PER_CHILD {
                        let _ = reply.send(Err(SupervisorError::TooManyWaiters(agent_id)));
                        return false;
                    }
                    child.waiters.push(reply);
                }
            }
            SupervisorCommand::List { reply } => {
                let _ = reply.send(
                    self.children
                        .values()
                        .map(child_control_projection)
                        .collect::<Vec<_>>(),
                );
            }
            SupervisorCommand::Inspect { agent_id, reply } => {
                let result = self
                    .children
                    .get(&agent_id)
                    .map(child_control_projection)
                    .ok_or(SupervisorError::UnknownAgent(agent_id));
                let _ = reply.send(result);
            }
            SupervisorCommand::SnapshotMany { agent_ids, reply } => {
                let mut seen = BTreeSet::new();
                let result = agent_ids
                    .into_iter()
                    .map(|agent_id| {
                        if !seen.insert(agent_id) {
                            return Err(SupervisorError::DuplicateAgent(agent_id));
                        }
                        self.children
                            .get(&agent_id)
                            .map(child_collection_projection)
                            .ok_or(SupervisorError::UnknownAgent(agent_id))
                    })
                    .collect();
                let _ = reply.send(result);
            }
            SupervisorCommand::ValidatePlan {
                parent_operation_id,
                plan,
                reply,
            } => {
                let result = self.validate_plan(parent_operation_id, plan);
                let _ = reply.send(result.map(|validated| validation_diagnostic(&validated)));
            }
            SupervisorCommand::StartPlan {
                parent_operation_id,
                plan,
                reply,
            } => {
                let result = self
                    .start_plan(parent_operation_id, plan, commits, events)
                    .await;
                let _ = reply.send(result);
                self.start_queued_children(commits, events).await;
            }
            SupervisorCommand::Cancel { agent_id, reply } => {
                let result = self.request_cancellation(agent_id, commits, events).await;
                let _ = reply.send(result);
                self.start_queued_children(commits, events).await;
            }
            SupervisorCommand::DecidePermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
                reply,
            } => {
                let result = match self.children.get(&agent_id) {
                    Some(child) => match &child.permissions {
                        Some(permissions) => permissions
                            .decide(operation_id, invocation_id, decision)
                            .await
                            .map_err(|error| SupervisorError::Permission(error.to_string())),
                        None => Err(SupervisorError::Permission(
                            "child has no active permission controller".to_owned(),
                        )),
                    },
                    None => Err(SupervisorError::UnknownAgent(agent_id)),
                };
                let _ = reply.send(result);
            }
            SupervisorCommand::Shutdown { reply } => {
                self.shutdown_children(commits, events).await;
                let _ = reply.send(());
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
fn test_artifact_store() -> ArtifactStore {
    ArtifactStore::new(
        std::env::temp_dir()
            .join("xana-supervisor-tests")
            .join(uuid::Uuid::new_v4().to_string()),
    )
}

fn validate_child_ceiling(
    prepared: &PreparedChild,
    budget: &OrchestrationBudget,
) -> Result<(), SupervisorError> {
    let child = &prepared.resolved;
    let parent = &budget.limits;
    let invalid = if child.max_tool_rounds > budget.max_tool_rounds {
        Some((
            "max_tool_rounds",
            child.max_tool_rounds as u64,
            budget.max_tool_rounds as u64,
        ))
    } else if child.orchestration.deadline_seconds > parent.deadline_seconds {
        Some((
            "deadline_seconds",
            child.orchestration.deadline_seconds,
            parent.deadline_seconds,
        ))
    } else if child.orchestration.max_context_tokens > parent.max_context_tokens {
        Some((
            "max_context_tokens",
            child.orchestration.max_context_tokens as u64,
            parent.max_context_tokens as u64,
        ))
    } else if child.orchestration.max_report_bytes > parent.max_report_bytes {
        Some((
            "max_report_bytes",
            child.orchestration.max_report_bytes as u64,
            parent.max_report_bytes as u64,
        ))
    } else if child.orchestration.max_artifact_bytes > parent.max_artifact_bytes {
        Some((
            "max_artifact_bytes",
            child.orchestration.max_artifact_bytes as u64,
            parent.max_artifact_bytes as u64,
        ))
    } else {
        None
    };
    if let Some((field, requested, maximum)) = invalid {
        Err(SupervisorError::Admission(format!(
            "resolved child {field}={requested} exceeds parent ceiling {maximum}"
        )))
    } else {
        Ok(())
    }
}

fn child_control_projection(child: &ActiveChild) -> ChildInspection {
    ChildInspection {
        handle: child.snapshot.clone(),
        // Await and collection own full report delivery. Control-plane list
        // and detail events keep only the handle's bounded report reference.
        report: None,
        projected_interruption: false,
    }
}

fn child_collection_projection(child: &ActiveChild) -> ChildInspection {
    ChildInspection {
        handle: child.snapshot.clone(),
        report: child.report.clone(),
        projected_interruption: false,
    }
}

fn emit_lifecycle(
    events: &mpsc::UnboundedSender<AgentEvent>,
    attribution: &ChildAttribution,
    lifecycle: ChildLifecycle,
) {
    let _ = events.send(AgentEvent::ChildLifecycleChanged {
        attribution: attribution.clone(),
        lifecycle,
    });
}
