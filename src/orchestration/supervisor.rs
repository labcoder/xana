use super::{
    AgentHandleSnapshot, ChildActivity, ChildAdmission, ChildAttribution, ChildExecutionContext,
    ChildExecutionFactory, ChildExecutionOutput, ChildLifecycle, ChildReport, SpawnAgentRequest,
    truncate_utf8, validate_spawn_request,
};
use crate::{
    identity::{AgentId, OperationId, ThreadId, ToolInvocationId},
    permission::{ControllerDecision, PermissionBroker, PermissionBrokerHandle},
    runtime::{AgentEvent, OperationState},
    session::SessionRecord,
};
use std::{collections::BTreeMap, fmt, sync::Arc};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const SUPERVISOR_COMMAND_CAPACITY: usize = 16;
const CHILD_COMMIT_CAPACITY: usize = 16;

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
}

struct ActiveChild {
    snapshot: AgentHandleSnapshot,
    report: Option<ChildReport>,
    terminal_error: Option<SupervisorError>,
    waiters: Vec<oneshot::Sender<Result<ChildReport, SupervisorError>>>,
    permissions: Option<PermissionBrokerHandle>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}

enum SupervisorCommand {
    Spawn {
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
        reply: oneshot::Sender<Result<AgentHandleSnapshot, SupervisorError>>,
    },
    Await {
        agent_id: AgentId,
        reply: oneshot::Sender<Result<ChildReport, SupervisorError>>,
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
    result: Result<ChildExecutionOutput, String>,
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
    Busy,
    Admission(String),
    UnknownAgent(AgentId),
    Permission(String),
    Durability(String),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("child supervisor is unavailable"),
            Self::InvalidRequest(reason) => write!(formatter, "invalid child request: {reason}"),
            Self::Busy => formatter.write_str(
                "one child is already active; parallel child admission is not enabled yet",
            ),
            Self::Admission(reason) => write!(formatter, "child admission failed: {reason}"),
            Self::UnknownAgent(agent_id) => write!(formatter, "unknown child agent {agent_id}"),
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
    pub(crate) fn new(
        parent: ParentExecution,
        factory: Arc<dyn ChildExecutionFactory>,
    ) -> (ChildSupervisorHandle, Self) {
        let (command_sender, commands) = mpsc::channel(SUPERVISOR_COMMAND_CAPACITY);
        let (completion_sender, completions) = mpsc::unbounded_channel();
        (
            ChildSupervisorHandle {
                commands: command_sender,
            },
            Self {
                parent,
                factory,
                commands,
                children: BTreeMap::new(),
                completions,
                completion_sender,
            },
        )
    }

    pub(crate) async fn run(
        mut self,
        commits: ChildCommitSender,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) {
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        self.shutdown_children().await;
                        return;
                    };
                    if self.handle_command(command, &commits, &events).await {
                        return;
                    }
                }
                completion = self.completions.recv() => {
                    if let Some(completion) = completion {
                        self.handle_completion(completion, &commits, &events).await;
                    }
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
                    child.waiters.push(reply);
                }
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
                self.shutdown_children().await;
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
        validate_spawn_request(&request)
            .map_err(|reason| SupervisorError::InvalidRequest(reason.to_owned()))?;
        if self
            .children
            .values()
            .any(|child| !child.snapshot.lifecycle.is_terminal() && child.terminal_error.is_none())
        {
            return Err(SupervisorError::Busy);
        }
        let prepared = self
            .factory
            .prepare(&request)
            .map_err(SupervisorError::Admission)?;
        let attribution = ChildAttribution::new(
            AgentId::new(),
            self.parent.agent_id,
            parent_operation_id,
            self.parent.thread_id,
            &prepared.resolved,
        );
        let admission = ChildAdmission::new(attribution.clone(), &request.task, &prepared.resolved);
        let mut snapshot = AgentHandleSnapshot::admitted(admission);

        commits
            .append(SessionRecord::ChildAdmitted {
                handle: snapshot.clone(),
            })
            .await?;
        emit_lifecycle(events, &attribution, ChildLifecycle::Admitted);

        commits
            .append(SessionRecord::ChildLifecycleChanged {
                agent_id: attribution.agent_id,
                lifecycle: ChildLifecycle::Queued,
            })
            .await?;
        snapshot.apply_lifecycle(ChildLifecycle::Queued);
        emit_lifecycle(events, &attribution, ChildLifecycle::Queued);

        let (child_events, child_event_receiver) = mpsc::unbounded_channel();
        let (permissions, broker_task) =
            PermissionBroker::spawn(prepared.permission_policy, true, child_events.clone());
        let cancellation = CancellationToken::new();

        commits
            .append(SessionRecord::ChildLifecycleChanged {
                agent_id: attribution.agent_id,
                lifecycle: ChildLifecycle::Running,
            })
            .await?;
        snapshot.apply_lifecycle(ChildLifecycle::Running);
        emit_lifecycle(events, &attribution, ChildLifecycle::Running);

        let agent_id = attribution.agent_id;
        let completion_sender = self.completion_sender.clone();
        let outer_events = events.clone();
        let execution_context = ChildExecutionContext {
            operation_id: attribution.operation_id,
            permissions: permissions.clone(),
            events: child_events,
            cancellation: cancellation.clone(),
        };
        let task = tokio::spawn(run_child_execution(RunningChild {
            agent_id,
            attribution: attribution.clone(),
            execution: prepared.execution,
            context: execution_context,
            child_events: child_event_receiver,
            broker_task,
            outer_events,
            completions: completion_sender,
        }));
        self.children.insert(
            agent_id,
            ActiveChild {
                snapshot: snapshot.clone(),
                report: None,
                terminal_error: None,
                waiters: Vec::new(),
                permissions: Some(permissions),
                cancellation,
                task: Some(task),
            },
        );
        Ok(snapshot)
    }

    async fn handle_completion(
        &mut self,
        completion: ChildCompletion,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get_mut(&completion.agent_id) else {
            return;
        };
        if child.report.is_some() {
            return;
        }
        let attribution = child.snapshot.admission.attribution.clone();
        let max_bytes = child.snapshot.admission.limits.max_report_bytes;
        let report = match completion.result {
            Ok(output) if output.text.len() <= max_bytes => {
                ChildReport::completed(attribution.clone(), output.text, output.usage)
            }
            Ok(output) => ChildReport::failed(
                attribution.clone(),
                format!(
                    "child output contains {} bytes, exceeding the {}-byte inline report limit",
                    output.text.len(),
                    max_bytes
                ),
                max_bytes,
            ),
            Err(reason) => ChildReport::failed(attribution.clone(), reason, max_bytes),
        };
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
            return;
        }
        child.snapshot.apply_report(&report);
        child.report = Some(report.clone());
        child
            .permissions
            .take()
            .inspect(|permissions| permissions.shutdown());
        child.task.take();
        emit_lifecycle(events, &attribution, report.lifecycle());
        let _ = events.send(AgentEvent::ChildReportCommitted {
            report: report.clone(),
        });
        for waiter in child.waiters.drain(..) {
            let _ = waiter.send(Ok(report.clone()));
        }
    }

    async fn shutdown_children(&mut self) {
        for child in self.children.values_mut() {
            if !child.snapshot.lifecycle.is_terminal() {
                child.cancellation.cancel();
                if let Some(permissions) = child.permissions.take() {
                    permissions.shutdown();
                }
                if let Some(task) = child.task.take() {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
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

    pub(crate) async fn await_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<ChildReport, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Await { agent_id, reply })
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
    let result = loop {
        tokio::select! {
            result = &mut run => break result,
            _ = cancellation.cancelled() => {
                break Err("child execution was interrupted by runtime shutdown".to_owned());
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
    let _ = completions.send(ChildCompletion { agent_id, result });
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
        | AgentEvent::ChildReportCommitted { .. } => None,
    }
}
