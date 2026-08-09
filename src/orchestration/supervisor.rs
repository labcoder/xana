use super::{
    AgentHandleSnapshot, AwaitAgentOptions, AwaitAgentOutcome, BudgetError, BudgetLedger,
    BudgetReservation, ChildActivity, ChildAdmission, ChildAttribution, ChildCancellationReceipt,
    ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutcome, ChildInspection,
    ChildLifecycle, ChildReport, OrchestrationBudget, PreparedChild, ReservationRequest,
    SpawnAgentRequest, truncate_utf8, validate_spawn_request,
};
use crate::{
    identity::{AgentId, OperationId, ThreadId, ToolInvocationId},
    permission::{ControllerDecision, PermissionBroker, PermissionBrokerHandle},
    runtime::{AgentEvent, OperationState},
    session::SessionRecord,
};
use std::{
    collections::{BTreeMap, VecDeque},
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

    async fn append(&self, record: SessionRecord) -> Result<(), SupervisorError> {
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
    reservation: Option<BudgetReservation>,
    running_reserved: bool,
    deadline: Option<time::Instant>,
}

struct AdmissionCandidate {
    snapshot: AgentHandleSnapshot,
    prepared: PreparedChild,
    reservation: ReservationRequest,
    deadline: time::Instant,
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
    outcome: ChildExecutionOutcome,
}

struct RunningChild {
    agent_id: AgentId,
    attribution: ChildAttribution,
    execution: Box<dyn super::ChildExecution>,
    context: ChildExecutionContext,
    child_events: mpsc::UnboundedReceiver<AgentEvent>,
    broker_task: JoinHandle<()>,
    outer_events: mpsc::UnboundedSender<AgentEvent>,
    completions: mpsc::UnboundedSender<ChildCompletion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SupervisorError {
    Unavailable,
    InvalidRequest(String),
    Budget(BudgetError),
    Admission(String),
    UnknownAgent(AgentId),
    TooManyWaiters(AgentId),
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
            OrchestrationBudget::new(crate::config::OrchestrationLimits::default(), 8),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_budget(
        parent: ParentExecution,
        factory: Arc<dyn ChildExecutionFactory>,
        budget: OrchestrationBudget,
    ) -> (ChildSupervisorHandle, Self) {
        Self::with_restored(parent, factory, Vec::new(), budget)
    }

    pub(crate) fn with_restored(
        parent: ParentExecution,
        factory: Arc<dyn ChildExecutionFactory>,
        restored: Vec<ChildInspection>,
        budget: OrchestrationBudget,
    ) -> (ChildSupervisorHandle, Self) {
        let (command_sender, commands) = mpsc::channel(SUPERVISOR_COMMAND_CAPACITY);
        let (completion_sender, completions) = mpsc::unbounded_channel();
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
                        reservation: None,
                        running_reserved: false,
                        deadline: None,
                    },
                )
            })
            .collect();
        (
            ChildSupervisorHandle {
                commands: command_sender,
            },
            Self {
                parent,
                factory,
                commands,
                children,
                completions,
                completion_sender,
                ledger: BudgetLedger::new(budget),
                admission_queue: VecDeque::new(),
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
                        .map(child_inspection)
                        .collect::<Vec<_>>(),
                );
            }
            SupervisorCommand::Inspect { agent_id, reply } => {
                let result = self
                    .children
                    .get(&agent_id)
                    .map(child_inspection)
                    .ok_or(SupervisorError::UnknownAgent(agent_id));
                let _ = reply.send(result);
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

    async fn spawn_child(
        &mut self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentHandleSnapshot, SupervisorError> {
        let candidate = self.prepare_candidate(parent_operation_id, request)?;
        let mut reservations = self
            .ledger
            .reserve_batch(std::slice::from_ref(&candidate.reservation))
            .map_err(SupervisorError::Budget)?;
        let reservation = reservations
            .pop()
            .expect("a one-child reservation returns one token");
        let mut snapshot = candidate.snapshot;
        let attribution = snapshot.admission.attribution.clone();

        if let Err(error) = commits
            .append(SessionRecord::ChildAdmitted {
                handle: snapshot.clone(),
            })
            .await
        {
            self.ledger.release(reservation);
            return Err(error);
        }
        emit_lifecycle(events, &attribution, ChildLifecycle::Admitted);

        if let Err(error) = commits
            .append(SessionRecord::ChildLifecycleChanged {
                agent_id: attribution.agent_id,
                lifecycle: ChildLifecycle::Queued,
            })
            .await
        {
            self.ledger.release(reservation);
            return Err(error);
        }
        snapshot.apply_lifecycle(ChildLifecycle::Queued);
        emit_lifecycle(events, &attribution, ChildLifecycle::Queued);

        let cancellation = CancellationToken::new();
        let agent_id = attribution.agent_id;
        self.children.insert(
            agent_id,
            ActiveChild {
                snapshot: snapshot.clone(),
                report: None,
                terminal_error: None,
                waiters: Vec::new(),
                prepared: Some(candidate.prepared),
                permissions: None,
                cancellation,
                cancellation_requested: false,
                task: None,
                reservation: Some(reservation),
                running_reserved: false,
                deadline: Some(candidate.deadline),
            },
        );
        self.admission_queue.push_back(agent_id);
        Ok(snapshot)
    }

    async fn spawn_many_children(
        &mut self,
        parent_operation_id: OperationId,
        requests: Vec<SpawnAgentRequest>,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Vec<AgentHandleSnapshot>, SupervisorError> {
        if requests.is_empty() {
            return Err(SupervisorError::Budget(BudgetError::EmptyBatch));
        }
        let maximum_fan_out = self.ledger.budget().limits.max_fan_out;
        if requests.len() > maximum_fan_out {
            return Err(SupervisorError::Budget(BudgetError::FanOut {
                requested: requests.len(),
                maximum: maximum_fan_out,
            }));
        }
        let mut candidates = Vec::with_capacity(requests.len());
        for request in requests {
            candidates.push(self.prepare_candidate(parent_operation_id, request)?);
        }
        let reservation_requests = candidates
            .iter()
            .map(|candidate| candidate.reservation.clone())
            .collect::<Vec<_>>();
        let reservations = self
            .ledger
            .reserve_batch(&reservation_requests)
            .map_err(SupervisorError::Budget)?;

        let mut snapshots = candidates
            .iter()
            .map(|candidate| candidate.snapshot.clone())
            .collect::<Vec<_>>();
        for snapshot in &mut snapshots {
            snapshot.apply_lifecycle(ChildLifecycle::Queued);
        }
        if let Err(error) = commits
            .append(SessionRecord::ChildrenBatchAdmitted {
                handles: snapshots.clone(),
            })
            .await
        {
            for reservation in reservations {
                self.ledger.release(reservation);
            }
            return Err(error);
        }

        for ((candidate, reservation), snapshot) in candidates
            .into_iter()
            .zip(reservations)
            .zip(snapshots.iter())
        {
            let attribution = snapshot.admission.attribution.clone();
            let agent_id = attribution.agent_id;
            emit_lifecycle(events, &attribution, ChildLifecycle::Admitted);
            emit_lifecycle(events, &attribution, ChildLifecycle::Queued);
            self.children.insert(
                agent_id,
                ActiveChild {
                    snapshot: snapshot.clone(),
                    report: None,
                    terminal_error: None,
                    waiters: Vec::new(),
                    prepared: Some(candidate.prepared),
                    permissions: None,
                    cancellation: CancellationToken::new(),
                    cancellation_requested: false,
                    task: None,
                    reservation: Some(reservation),
                    running_reserved: false,
                    deadline: Some(candidate.deadline),
                },
            );
            self.admission_queue.push_back(agent_id);
        }
        Ok(snapshots)
    }

    fn prepare_candidate(
        &self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
    ) -> Result<AdmissionCandidate, SupervisorError> {
        validate_spawn_request(&request)
            .map_err(|reason| SupervisorError::InvalidRequest(reason.to_owned()))?;
        let prepared = self
            .factory
            .prepare(&request)
            .map_err(SupervisorError::Admission)?;
        validate_child_ceiling(&prepared, self.ledger.budget())?;
        let attribution = ChildAttribution::new(
            AgentId::new(),
            self.parent.agent_id,
            parent_operation_id,
            self.parent.thread_id,
            &prepared.resolved,
        );
        let admission = ChildAdmission::new(attribution, &request.task, &prepared.resolved);
        let reservation = ReservationRequest {
            tool_rounds: prepared.resolved.max_tool_rounds,
            context_tokens: prepared.resolved.orchestration.max_context_tokens,
            report_bytes: prepared.resolved.orchestration.max_report_bytes,
            artifact_bytes: prepared.resolved.orchestration.max_artifact_bytes,
        };
        let deadline = time::Instant::now()
            + std::time::Duration::from_secs(prepared.resolved.orchestration.deadline_seconds);
        Ok(AdmissionCandidate {
            snapshot: AgentHandleSnapshot::admitted(admission),
            prepared,
            reservation,
            deadline,
        })
    }

    async fn start_queued_children(
        &mut self,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        while self.ledger.can_start() {
            let Some(agent_id) = self.admission_queue.pop_front() else {
                break;
            };
            let eligible = self.children.get(&agent_id).is_some_and(|child| {
                child.snapshot.lifecycle == ChildLifecycle::Queued
                    && !child.cancellation_requested
                    && child.terminal_error.is_none()
                    && child.prepared.is_some()
            });
            if !eligible {
                continue;
            }
            let attribution = self.children[&agent_id]
                .snapshot
                .admission
                .attribution
                .clone();
            if let Err(error) = commits
                .append(SessionRecord::ChildLifecycleChanged {
                    agent_id,
                    lifecycle: ChildLifecycle::Running,
                })
                .await
            {
                let child = self
                    .children
                    .get_mut(&agent_id)
                    .expect("queued child remains supervised");
                child.terminal_error = Some(error.clone());
                for waiter in child.waiters.drain(..) {
                    let _ = waiter.send(Err(error.clone()));
                }
                self.release_child_capacity(agent_id);
                continue;
            }

            self.ledger.mark_started();
            let child = self
                .children
                .get_mut(&agent_id)
                .expect("queued child remains supervised");
            let prepared = child
                .prepared
                .take()
                .expect("eligible queued child has prepared execution");
            child.running_reserved = true;
            child.snapshot.apply_lifecycle(ChildLifecycle::Running);
            emit_lifecycle(events, &attribution, ChildLifecycle::Running);

            let (child_events, child_event_receiver) = mpsc::unbounded_channel();
            let (permissions, broker_task) =
                PermissionBroker::spawn(prepared.permission_policy, true, child_events.clone());
            let execution_context = ChildExecutionContext {
                operation_id: attribution.operation_id,
                permissions: permissions.clone(),
                events: child_events,
                cancellation: child.cancellation.clone(),
            };
            child.permissions = Some(permissions);
            child.task = Some(tokio::spawn(run_child_execution(RunningChild {
                agent_id,
                attribution,
                execution: prepared.execution,
                context: execution_context,
                child_events: child_event_receiver,
                broker_task,
                outer_events: events.clone(),
                completions: self.completion_sender.clone(),
            })));
        }
    }

    async fn handle_completion(
        &mut self,
        completion: ChildCompletion,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get(&completion.agent_id) else {
            return;
        };
        if child.report.is_some() {
            return;
        }
        let attribution = child.snapshot.admission.attribution.clone();
        let max_bytes = child.snapshot.admission.limits.max_report_bytes;
        let report = if child.cancellation_requested {
            ChildReport::cancelled(
                attribution.clone(),
                "child cancellation was requested and observed".to_owned(),
                max_bytes,
            )
        } else {
            match completion.outcome {
                ChildExecutionOutcome::Completed(output) if output.text.len() <= max_bytes => {
                    ChildReport::completed(attribution.clone(), output.text, output.usage)
                }
                ChildExecutionOutcome::Completed(output) => ChildReport::failed(
                    attribution.clone(),
                    format!(
                        "child output contains {} bytes, exceeding the {}-byte inline report limit",
                        output.text.len(),
                        max_bytes
                    ),
                    max_bytes,
                ),
                ChildExecutionOutcome::Failed(reason) => {
                    ChildReport::failed(attribution.clone(), reason, max_bytes)
                }
                ChildExecutionOutcome::Cancelled(reason) => {
                    ChildReport::cancelled(attribution.clone(), reason, max_bytes)
                }
            }
        };
        self.commit_terminal_report(completion.agent_id, report, commits, events)
            .await;
    }

    fn next_deadline(&self) -> Option<time::Instant> {
        self.children
            .values()
            .filter(|child| {
                !child.snapshot.lifecycle.is_terminal()
                    && child.terminal_error.is_none()
                    && !child.cancellation_requested
            })
            .filter_map(|child| child.deadline)
            .min()
    }

    async fn expire_deadlines(
        &mut self,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let now = time::Instant::now();
        let expired = self
            .children
            .iter()
            .filter_map(|(agent_id, child)| {
                (!child.snapshot.lifecycle.is_terminal()
                    && child.terminal_error.is_none()
                    && !child.cancellation_requested
                    && child.deadline.is_some_and(|deadline| deadline <= now))
                .then_some(*agent_id)
            })
            .collect::<Vec<_>>();
        for agent_id in expired {
            let _ = self.request_cancellation(agent_id, commits, events).await;
        }
    }

    fn release_child_capacity(&mut self, agent_id: AgentId) {
        let Some(child) = self.children.get_mut(&agent_id) else {
            return;
        };
        if child.running_reserved {
            self.ledger.mark_stopped();
            child.running_reserved = false;
        }
        if let Some(reservation) = child.reservation.take() {
            self.ledger.release(reservation);
        }
        child.deadline = None;
    }

    async fn commit_terminal_report(
        &mut self,
        agent_id: AgentId,
        report: ChildReport,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get_mut(&agent_id) else {
            return;
        };
        if child.report.is_some() || child.terminal_error.is_some() {
            return;
        }
        if let Err(error) = commits
            .append(SessionRecord::ChildReportCommitted {
                report: report.clone(),
            })
            .await
        {
            child.snapshot.apply_lifecycle(ChildLifecycle::Interrupted);
            child.terminal_error = Some(error.clone());
            child
                .permissions
                .take()
                .inspect(|permissions| permissions.shutdown());
            child.task.take();
            for waiter in child.waiters.drain(..) {
                let _ = waiter.send(Err(error.clone()));
            }
            self.release_child_capacity(agent_id);
            return;
        }
        child.snapshot.apply_report(&report);
        child.report = Some(report.clone());
        child
            .permissions
            .take()
            .inspect(|permissions| permissions.shutdown());
        child.task.take();
        emit_lifecycle(events, &report.attribution, report.lifecycle());
        let _ = events.send(AgentEvent::ChildReportCommitted {
            report: report.clone(),
        });
        for waiter in child.waiters.drain(..) {
            let _ = waiter.send(Ok(report.clone()));
        }
        self.release_child_capacity(agent_id);
    }

    async fn request_cancellation(
        &mut self,
        agent_id: AgentId,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<ChildCancellationReceipt, SupervisorError> {
        let (newly_requested, queued_report) = {
            let child = self
                .children
                .get_mut(&agent_id)
                .ok_or(SupervisorError::UnknownAgent(agent_id))?;
            let newly_requested = !child.snapshot.lifecycle.is_terminal()
                && child.terminal_error.is_none()
                && !child.cancellation_requested;
            let queued_report = if newly_requested {
                child.cancellation_requested = true;
                child.cancellation.cancel();
                if let Some(permissions) = &child.permissions {
                    permissions.shutdown();
                }
                (child.snapshot.lifecycle == ChildLifecycle::Queued).then(|| {
                    ChildReport::cancelled(
                        child.snapshot.admission.attribution.clone(),
                        "queued child cancellation was requested and observed".to_owned(),
                        child.snapshot.admission.limits.max_report_bytes,
                    )
                })
            } else {
                None
            };
            (newly_requested, queued_report)
        };
        if let Some(report) = queued_report {
            self.commit_terminal_report(agent_id, report, commits, events)
                .await;
        }
        let handle = self
            .children
            .get(&agent_id)
            .ok_or(SupervisorError::UnknownAgent(agent_id))?
            .snapshot
            .clone();
        Ok(ChildCancellationReceipt {
            handle,
            newly_requested,
        })
    }

    async fn shutdown_children(
        &mut self,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let active_ids = self
            .children
            .iter()
            .filter_map(|(agent_id, child)| {
                (!child.snapshot.lifecycle.is_terminal() && child.terminal_error.is_none())
                    .then_some(*agent_id)
            })
            .collect::<Vec<_>>();
        for agent_id in &active_ids {
            let _ = self.request_cancellation(*agent_id, commits, events).await;
        }

        let wait_for_terminals = async {
            while self.children.values().any(|child| {
                !child.snapshot.lifecycle.is_terminal() && child.terminal_error.is_none()
            }) {
                let Some(completion) = self.completions.recv().await else {
                    break;
                };
                self.handle_completion(completion, commits, events).await;
            }
        };
        if time::timeout(SHUTDOWN_GRACE, wait_for_terminals)
            .await
            .is_err()
        {
            for agent_id in active_ids {
                self.interrupt_unresponsive_child(agent_id, commits, events)
                    .await;
            }
        }
    }

    async fn interrupt_unresponsive_child(
        &mut self,
        agent_id: AgentId,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get_mut(&agent_id) else {
            return;
        };
        if child.snapshot.lifecycle.is_terminal() || child.terminal_error.is_some() {
            return;
        }
        if let Some(task) = child.task.take() {
            task.abort();
            let _ = task.await;
        }
        let report = ChildReport::interrupted(
            child.snapshot.admission.attribution.clone(),
            "child did not stop within the runtime shutdown grace period".to_owned(),
            child.snapshot.admission.limits.max_report_bytes,
        );
        if let Err(error) = commits
            .append(SessionRecord::ChildReportCommitted {
                report: report.clone(),
            })
            .await
        {
            child.terminal_error = Some(error.clone());
            for waiter in child.waiters.drain(..) {
                let _ = waiter.send(Err(error.clone()));
            }
            self.release_child_capacity(agent_id);
            return;
        }
        child.snapshot.apply_report(&report);
        child.report = Some(report.clone());
        child.permissions.take().inspect(|broker| broker.shutdown());
        emit_lifecycle(events, &report.attribution, report.lifecycle());
        let _ = events.send(AgentEvent::ChildReportCommitted {
            report: report.clone(),
        });
        for waiter in child.waiters.drain(..) {
            let _ = waiter.send(Ok(report.clone()));
        }
        self.release_child_capacity(agent_id);
    }
}

async fn wait_for_deadline(deadline: Option<time::Instant>) {
    match deadline {
        Some(deadline) => time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
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

fn child_inspection(child: &ActiveChild) -> ChildInspection {
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

impl ChildSupervisorHandle {
    #[cfg(test)]
    pub(crate) fn closed_for_test() -> Self {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        Self { commands }
    }

    pub(crate) async fn spawn_agent(
        &self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
    ) -> Result<AgentHandleSnapshot, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Spawn {
                parent_operation_id,
                request,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn spawn_many(
        &self,
        parent_operation_id: OperationId,
        requests: Vec<SpawnAgentRequest>,
    ) -> Result<Vec<AgentHandleSnapshot>, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::SpawnMany {
                parent_operation_id,
                requests,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn await_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<ChildReport, SupervisorError> {
        match self
            .await_agent_with(agent_id, AwaitAgentOptions::default())
            .await?
        {
            AwaitAgentOutcome::Report(report) => Ok(*report),
            AwaitAgentOutcome::TimedOut { .. } => {
                unreachable!("an unbounded await cannot time out")
            }
        }
    }

    pub(crate) async fn await_agent_with(
        &self,
        agent_id: AgentId,
        options: AwaitAgentOptions,
    ) -> Result<AwaitAgentOutcome, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Await { agent_id, reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        if let Some(timeout) = options.timeout {
            match time::timeout(timeout, response).await {
                Ok(report) => {
                    return report
                        .map_err(|_| SupervisorError::Unavailable)?
                        .map(|report| AwaitAgentOutcome::Report(Box::new(report)));
                }
                Err(_) => {
                    let cancellation_requested = if options.cancel_on_timeout {
                        self.cancel_agent(agent_id).await?.newly_requested
                    } else {
                        false
                    };
                    return Ok(AwaitAgentOutcome::TimedOut {
                        agent_id,
                        cancellation_requested,
                    });
                }
            }
        }
        response
            .await
            .map_err(|_| SupervisorError::Unavailable)?
            .map(|report| AwaitAgentOutcome::Report(Box::new(report)))
    }

    pub(crate) async fn list_agents(&self) -> Result<Vec<ChildInspection>, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::List { reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)
    }

    pub(crate) async fn inspect_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<ChildInspection, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Inspect { agent_id, reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn cancel_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<ChildCancellationReceipt, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Cancel { agent_id, reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn delegate_agent(
        &self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
    ) -> Result<(AgentHandleSnapshot, ChildReport), SupervisorError> {
        let handle = self.spawn_agent(parent_operation_id, request).await?;
        let report = self
            .await_agent(handle.admission.attribution.agent_id)
            .await?;
        Ok((handle, report))
    }

    pub(crate) async fn decide_permission(
        &self,
        agent_id: AgentId,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    ) -> Result<(), SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::DecidePermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn shutdown(&self) {
        let (reply, response) = oneshot::channel();
        if self
            .commands
            .send(SupervisorCommand::Shutdown { reply })
            .await
            .is_ok()
        {
            let _ = response.await;
        }
    }
}

async fn run_child_execution(child: RunningChild) {
    let RunningChild {
        agent_id,
        attribution,
        execution,
        context,
        mut child_events,
        broker_task,
        outer_events,
        completions,
    } = child;
    let cancellation = context.cancellation.clone();
    let run = execution.run(context);
    tokio::pin!(run);
    let outcome = loop {
        tokio::select! {
            result = &mut run => break result,
            _ = cancellation.cancelled() => {
                break ChildExecutionOutcome::Cancelled(
                    "child execution observed cancellation".to_owned()
                );
            }
            event = child_events.recv() => {
                let Some(event) = event else { continue };
                if let Some(activity) = child_activity(event) {
                    let _ = outer_events.send(AgentEvent::ChildActivity {
                        attribution: attribution.clone(),
                        activity,
                    });
                }
            }
        }
    };
    while let Ok(event) = child_events.try_recv() {
        if let Some(activity) = child_activity(event) {
            let _ = outer_events.send(AgentEvent::ChildActivity {
                attribution: attribution.clone(),
                activity,
            });
        }
    }
    broker_task.abort();
    let _ = broker_task.await;
    let _ = completions.send(ChildCompletion { agent_id, outcome });
}

fn child_activity(event: AgentEvent) -> Option<ChildActivity> {
    match event {
        AgentEvent::AssistantTextDelta { step_id, text, .. } => {
            Some(ChildActivity::AssistantTextDelta { step_id, text })
        }
        AgentEvent::PermissionRequested { request } => {
            Some(ChildActivity::PermissionRequested { request })
        }
        AgentEvent::PermissionAudited { fact } => Some(ChildActivity::PermissionAudited { fact }),
        AgentEvent::ToolFinished {
            invocation_id,
            result,
            ..
        } => Some(ChildActivity::ToolFinished {
            invocation_id,
            result,
        }),
        AgentEvent::OperationFailed { reason, .. } => Some(ChildActivity::Warning {
            message: truncate_utf8(&reason, 4096),
        }),
        AgentEvent::OperationStateChanged {
            state: OperationState::Suspended,
            ..
        } => Some(ChildActivity::Suspended),
        AgentEvent::OperationStateChanged { .. }
        | AgentEvent::InvocationIntentCommitted { .. }
        | AgentEvent::InvocationResultCommitted { .. }
        | AgentEvent::AssistantMessage { .. }
        | AgentEvent::ConversationCleared
        | AgentEvent::CommandRejected { .. }
        | AgentEvent::ChildLifecycleChanged { .. }
        | AgentEvent::ChildActivity { .. }
        | AgentEvent::ChildReportCommitted { .. }
        | AgentEvent::ChildListSnapshot { .. }
        | AgentEvent::ChildInspectionSnapshot { .. }
        | AgentEvent::ChildCancellationRequested { .. } => None,
    }
}
