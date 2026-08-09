use super::hub::ObservationHub;
use crate::{
    frontend::{
        ClientCommand, ClientCommandResult, ClientCommandValue, ClientEvent, EmbeddedClient,
        EmbeddedObserver, EmbeddedOwner,
    },
    identity::{AgentId, OperationId, ToolInvocationId},
    managed::codex::ApprovalDecision,
    managed_terminal::{ManagedTuiDriver, ManagedTuiEvent},
    orchestration::ChildActivity,
    permission::ControllerDecision,
    runtime::{AgentEvent, OperationState, RuntimeCommand},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

const EXECUTION_COMMAND_CAPACITY: usize = 32;

#[derive(Clone)]
pub(crate) struct HostedExecutionHandle {
    requests: mpsc::Sender<ExecutionRequest>,
    abort: AbortHandle,
    done: CancellationToken,
}

enum ExecutionRequest {
    Command {
        command: ClientCommand,
        reply: oneshot::Sender<ClientCommandResult>,
    },
    ManagedApproval {
        approval_id: uuid::Uuid,
        decision: super::protocol::ManagedApprovalDecision,
        reply: oneshot::Sender<Result<(), String>>,
    },
    FailClosed,
    Shutdown {
        complete: oneshot::Sender<()>,
    },
}

impl HostedExecutionHandle {
    pub(crate) async fn command(
        &self,
        command: ClientCommand,
    ) -> Result<ClientCommandResult, String> {
        let (reply, result) = oneshot::channel();
        self.requests
            .send(ExecutionRequest::Command { command, reply })
            .await
            .map_err(|_| "hosted execution stopped".to_owned())?;
        result
            .await
            .map_err(|_| "hosted execution stopped before returning a command result".to_owned())
    }

    pub(crate) async fn managed_approval(
        &self,
        approval_id: uuid::Uuid,
        decision: super::protocol::ManagedApprovalDecision,
    ) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.requests
            .send(ExecutionRequest::ManagedApproval {
                approval_id,
                decision,
                reply,
            })
            .await
            .map_err(|_| "hosted execution stopped".to_owned())?;
        result
            .await
            .map_err(|_| "hosted execution stopped before resolving the approval".to_owned())?
    }

    pub(crate) async fn fail_closed(&self) {
        let _ = self.requests.send(ExecutionRequest::FailClosed).await;
    }

    pub(crate) async fn shutdown(&self) {
        let (complete, finished) = oneshot::channel();
        if self
            .requests
            .send(ExecutionRequest::Shutdown { complete })
            .await
            .is_ok()
        {
            let _ = finished.await;
        }
    }

    pub(crate) fn abort(&self) {
        self.abort.abort();
    }

    pub(crate) async fn wait_closed(&self) {
        self.done.cancelled().await;
    }
}

pub(crate) fn spawn_managed_execution(
    driver: ManagedTuiDriver,
    hub: ObservationHub,
) -> HostedExecutionHandle {
    let (requests, receiver) = mpsc::channel(EXECUTION_COMMAND_CAPACITY);
    let done = CancellationToken::new();
    let guard = done.clone().drop_guard();
    let task = tokio::spawn(async move {
        let _guard = guard;
        run_managed_execution(driver, receiver, hub).await;
    });
    HostedExecutionHandle {
        requests,
        abort: task.abort_handle(),
        done,
    }
}

async fn run_managed_execution(
    mut driver: ManagedTuiDriver,
    mut requests: mpsc::Receiver<ExecutionRequest>,
    hub: ObservationHub,
) {
    let mut active_operation = None;
    let mut approvals = HashMap::new();
    loop {
        tokio::select! {
            biased;
            request = requests.recv() => {
                let Some(request) = request else {
                    fail_closed_managed(&driver, &mut active_operation, &mut approvals);
                    let _ = driver.shutdown().await;
                    return;
                };
                match request {
                    ExecutionRequest::Command { command, reply } => {
                        let result = execute_managed_command(&driver, &mut active_operation, command).await;
                        let _ = reply.send(result);
                    }
                    ExecutionRequest::ManagedApproval { approval_id, decision, reply } => {
                        let result = resolve_managed_approval(
                            &mut approvals,
                            approval_id,
                            decision,
                        );
                        let _ = hub.publish(super::protocol::HostEvent::ManagedApprovalResolved {
                            approval_id,
                            accepted: result.is_ok(),
                        });
                        let _ = reply.send(result);
                    }
                    ExecutionRequest::FailClosed => {
                        fail_closed_managed(&driver, &mut active_operation, &mut approvals);
                    }
                    ExecutionRequest::Shutdown { complete } => {
                        fail_closed_managed(&driver, &mut active_operation, &mut approvals);
                        let _ = driver.shutdown().await;
                        let _ = complete.send(());
                        return;
                    }
                }
            }
            event = driver.next_event() => {
                let Some(event) = event else { return; };
                match event {
                    ManagedTuiEvent::Notification(event) => {
                        let _ = hub.publish_frontend(ClientEvent::Managed(Box::new(event)));
                    }
                    ManagedTuiEvent::Approval { request, reply } => {
                        let Some(operation_id) = active_operation else {
                            let _ = reply.send(ApprovalDecision::Cancel);
                            continue;
                        };
                        let approval_id = uuid::Uuid::new_v4();
                        approvals.insert(approval_id, reply);
                        let request = super::protocol::ManagedApprovalSnapshot::bounded(
                            approval_id,
                            operation_id,
                            request,
                        );
                        let _ = hub.publish(super::protocol::HostEvent::ManagedApprovalRequested(request));
                    }
                    ManagedTuiEvent::TurnFinished { operation_id, error } => {
                        if active_operation == Some(operation_id) {
                            active_operation = None;
                        }
                        for (_, approval) in approvals.drain() {
                            let _ = approval.send(ApprovalDecision::Cancel);
                        }
                        let _ = hub.publish(super::protocol::HostEvent::ManagedTurnFinished {
                            operation_id,
                            error,
                        });
                    }
                    ManagedTuiEvent::ThreadOpened(_) | ManagedTuiEvent::Cleared => {}
                }
            }
        }
    }
}

async fn execute_managed_command(
    driver: &ManagedTuiDriver,
    active_operation: &mut Option<OperationId>,
    command: ClientCommand,
) -> ClientCommandResult {
    let command_id = command.id;
    let result = match command.value {
        ClientCommandValue::SubmitTurn {
            operation_id,
            input,
            images,
        } if images.is_empty() => {
            if active_operation.is_some() {
                Err("managed runtime already has an active turn".into())
            } else {
                match driver.submit(operation_id, input, Vec::new()).await {
                    Ok(()) => {
                        *active_operation = Some(operation_id);
                        Ok(())
                    }
                    Err(reason) => Err(reason),
                }
            }
        }
        ClientCommandValue::SubmitTurn { .. } => {
            Err("attached managed turns do not accept path-bearing image references".into())
        }
        ClientCommandValue::InterruptOperation { operation_id }
            if *active_operation == Some(operation_id) =>
        {
            driver
                .interrupt(operation_id)
                .then_some(())
                .ok_or_else(|| "managed operation is no longer active".to_owned())
        }
        ClientCommandValue::ClearConversation if active_operation.is_none() => driver.clear().await,
        ClientCommandValue::Shutdown => {
            Err("attached controllers cannot shut down the foreground host".into())
        }
        _ => Err("command is not supported by the managed execution owner".into()),
    };
    match result {
        Ok(()) => ClientCommandResult::accepted(command_id),
        Err(reason) => ClientCommandResult::rejected(command_id, reason),
    }
}

fn fail_closed_managed(
    driver: &ManagedTuiDriver,
    active_operation: &mut Option<OperationId>,
    approvals: &mut HashMap<uuid::Uuid, oneshot::Sender<ApprovalDecision>>,
) {
    for (_, approval) in approvals.drain() {
        let _ = approval.send(ApprovalDecision::Cancel);
    }
    if let Some(operation_id) = active_operation.take() {
        let _ = driver.interrupt(operation_id);
    }
}

fn map_managed_decision(decision: super::protocol::ManagedApprovalDecision) -> ApprovalDecision {
    match decision {
        super::protocol::ManagedApprovalDecision::AcceptOnce => ApprovalDecision::AcceptOnce,
        super::protocol::ManagedApprovalDecision::Decline => ApprovalDecision::Decline,
        super::protocol::ManagedApprovalDecision::Cancel => ApprovalDecision::Cancel,
    }
}

fn resolve_managed_approval(
    approvals: &mut HashMap<uuid::Uuid, oneshot::Sender<ApprovalDecision>>,
    approval_id: uuid::Uuid,
    decision: super::protocol::ManagedApprovalDecision,
) -> Result<(), String> {
    approvals.remove(&approval_id).map_or_else(
        || Err("managed approval is stale, duplicated, or belongs to another request".into()),
        |approval| {
            approval
                .send(map_managed_decision(decision))
                .map_err(|_| "managed approval request is no longer pending".to_owned())
        },
    )
}

pub(crate) fn spawn_native_execution(
    client: EmbeddedClient,
    workspace_host: Arc<WorkspaceHost>,
    conversation: ConversationRef,
    hub: ObservationHub,
) -> HostedExecutionHandle {
    let (owner, observer) = client.into_parts();
    let (requests, receiver) = mpsc::channel(EXECUTION_COMMAND_CAPACITY);
    let done = CancellationToken::new();
    let guard = done.clone().drop_guard();
    let task = tokio::spawn(async move {
        let _guard = guard;
        run_native_execution(owner, observer, receiver, workspace_host, conversation, hub).await;
    });
    HostedExecutionHandle {
        requests,
        abort: task.abort_handle(),
        done,
    }
}

struct NativeExecutionState {
    active_operation: Option<OperationId>,
    root_lease: Option<ActiveRootLease>,
    pending_root: HashMap<ToolInvocationId, (OperationId, ToolInvocationId)>,
    pending_children: HashMap<ToolInvocationId, (AgentId, OperationId, ToolInvocationId)>,
}

impl NativeExecutionState {
    fn new() -> Self {
        Self {
            active_operation: None,
            root_lease: None,
            pending_root: HashMap::new(),
            pending_children: HashMap::new(),
        }
    }

    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Running | OperationState::Suspended,
            } => self.active_operation = Some(*operation_id),
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(_),
            }
            | AgentEvent::OperationFailed { operation_id, .. } => {
                if self.active_operation == Some(*operation_id) {
                    self.active_operation = None;
                    self.root_lease = None;
                }
                self.pending_root
                    .retain(|_, (pending, _)| pending != operation_id);
                self.pending_children
                    .retain(|_, (_, pending, _)| pending != operation_id);
            }
            AgentEvent::PermissionRequested { request } => {
                self.pending_root.insert(
                    request.invocation_id,
                    (request.operation_id, request.invocation_id),
                );
            }
            AgentEvent::PermissionAudited { fact } => {
                self.pending_root.remove(&fact.request.invocation_id);
            }
            AgentEvent::ChildActivity {
                attribution,
                activity: ChildActivity::PermissionRequested { request },
            } => {
                self.pending_children.insert(
                    request.invocation_id,
                    (
                        attribution.agent_id,
                        request.operation_id,
                        request.invocation_id,
                    ),
                );
            }
            AgentEvent::ChildActivity {
                activity: ChildActivity::PermissionAudited { fact },
                ..
            } => {
                self.pending_children.remove(&fact.request.invocation_id);
            }
            AgentEvent::CommandRejected { .. } if self.active_operation.is_none() => {
                self.root_lease = None;
            }
            _ => {}
        }
    }
}

async fn run_native_execution(
    owner: EmbeddedOwner,
    mut observer: EmbeddedObserver,
    mut requests: mpsc::Receiver<ExecutionRequest>,
    workspace_host: Arc<WorkspaceHost>,
    conversation: ConversationRef,
    hub: ObservationHub,
) {
    let mut state = NativeExecutionState::new();
    loop {
        tokio::select! {
            biased;
            request = requests.recv() => {
                let Some(request) = request else {
                    fail_closed_native(&owner, &mut state).await;
                    return;
                };
                match request {
                    ExecutionRequest::Command { command, reply } => {
                        let result = execute_native_command(
                            &owner,
                            &workspace_host,
                            &conversation,
                            &mut state,
                            command,
                        ).await;
                        let _ = reply.send(result);
                    }
                    ExecutionRequest::ManagedApproval { reply, .. } => {
                        let _ = reply.send(Err(
                            "native execution has no managed approval request".into(),
                        ));
                    }
                    ExecutionRequest::FailClosed => fail_closed_native(&owner, &mut state).await,
                    ExecutionRequest::Shutdown { complete } => {
                        fail_closed_native(&owner, &mut state).await;
                        let _ = owner.send(ClientCommand::new(RuntimeCommand::Shutdown)).await;
                        let _ = complete.send(());
                        return;
                    }
                }
            }
            observation = observer.next() => {
                let Some(observation) = observation else { return; };
                if let ClientEvent::Runtime(event) = &observation.event {
                    state.observe(event);
                }
                let _ = hub.publish_frontend(observation.event);
            }
        }
    }
}

async fn execute_native_command(
    owner: &EmbeddedOwner,
    workspace_host: &WorkspaceHost,
    conversation: &ConversationRef,
    state: &mut NativeExecutionState,
    command: ClientCommand,
) -> ClientCommandResult {
    if matches!(command.value, ClientCommandValue::Shutdown) {
        return ClientCommandResult::rejected(
            command.id,
            "attached controllers cannot shut down the foreground host",
        );
    }
    let starts_turn = matches!(command.value, ClientCommandValue::SubmitTurn { .. });
    if starts_turn && state.root_lease.is_none() {
        match workspace_host.acquire_root(conversation.clone()) {
            Ok(lease) => state.root_lease = Some(lease),
            Err(error) => return ClientCommandResult::rejected(command.id, error.to_string()),
        }
    }
    let id = command.id;
    match owner.send(command).await {
        Ok(result) => result,
        Err(_) => {
            if starts_turn {
                state.root_lease = None;
            }
            ClientCommandResult::rejected(id, "hosted native runtime is unavailable")
        }
    }
}

async fn fail_closed_native(owner: &EmbeddedOwner, state: &mut NativeExecutionState) {
    let root = std::mem::take(&mut state.pending_root);
    for (_, (operation_id, invocation_id)) in root {
        let _ = owner
            .send(ClientCommand::new(RuntimeCommand::DecidePermission {
                operation_id,
                invocation_id,
                decision: ControllerDecision::Deny,
            }))
            .await;
    }
    let children = std::mem::take(&mut state.pending_children);
    for (_, (agent_id, operation_id, invocation_id)) in children {
        let _ = owner
            .send(ClientCommand::new(RuntimeCommand::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision: ControllerDecision::Deny,
            }))
            .await;
    }
    if let Some(operation_id) = state.active_operation {
        let _ = owner
            .send(ClientCommand::new(RuntimeCommand::InterruptOperation {
                operation_id,
            }))
            .await;
    }
    state.root_lease = None;
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) enum FakeExecutionEvent {
    Command(ClientCommand),
    FailClosed,
    Shutdown,
}

#[cfg(test)]
pub(crate) fn fake_execution(
    hub: ObservationHub,
) -> (HostedExecutionHandle, mpsc::Receiver<FakeExecutionEvent>) {
    let (requests, mut receiver) = mpsc::channel(EXECUTION_COMMAND_CAPACITY);
    let (seen, seen_receiver) = mpsc::channel(EXECUTION_COMMAND_CAPACITY);
    let done = CancellationToken::new();
    let guard = done.clone().drop_guard();
    let task = tokio::spawn(async move {
        let _guard = guard;
        while let Some(request) = receiver.recv().await {
            match request {
                ExecutionRequest::Command { command, reply } => {
                    let result = ClientCommandResult::accepted(command.id);
                    let _ = seen.send(FakeExecutionEvent::Command(command)).await;
                    let _ = reply.send(result);
                    let _ = hub.publish(super::protocol::HostEvent::ObserverCommandRejected {
                        command: "fake_managed_completion".into(),
                    });
                }
                ExecutionRequest::ManagedApproval { reply, .. } => {
                    let _ =
                        reply.send(Err("fake execution has no pending managed approval".into()));
                }
                ExecutionRequest::FailClosed => {
                    let _ = seen.send(FakeExecutionEvent::FailClosed).await;
                }
                ExecutionRequest::Shutdown { complete } => {
                    let _ = seen.send(FakeExecutionEvent::Shutdown).await;
                    let _ = complete.send(());
                    break;
                }
            }
        }
    });
    (
        HostedExecutionHandle {
            requests,
            abort: task.abort_handle(),
            done,
        },
        seen_receiver,
    )
}

#[cfg(test)]
pub(crate) fn stubborn_execution() -> HostedExecutionHandle {
    let (requests, mut receiver) = mpsc::channel(EXECUTION_COMMAND_CAPACITY);
    let done = CancellationToken::new();
    let guard = done.clone().drop_guard();
    let task = tokio::spawn(async move {
        let _guard = guard;
        while let Some(request) = receiver.recv().await {
            match request {
                ExecutionRequest::Command { command, reply } => {
                    let _ = reply.send(ClientCommandResult::rejected(
                        command.id,
                        "stubborn fixture",
                    ));
                }
                ExecutionRequest::ManagedApproval { reply, .. } => {
                    let _ = reply.send(Err("stubborn fixture".into()));
                }
                ExecutionRequest::FailClosed => {}
                ExecutionRequest::Shutdown { .. } => {
                    std::future::pending::<()>().await;
                }
            }
        }
    });
    HostedExecutionHandle {
        requests,
        abort: task.abort_handle(),
        done,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_host::protocol::ManagedApprovalDecision;

    #[tokio::test]
    async fn managed_approval_resolves_exactly_once_and_wrong_ids_do_not_consume_it() {
        let approval_id = uuid::Uuid::new_v4();
        let wrong_id = uuid::Uuid::new_v4();
        let (reply, decision) = oneshot::channel();
        let mut approvals = HashMap::from([(approval_id, reply)]);

        assert!(
            resolve_managed_approval(
                &mut approvals,
                wrong_id,
                ManagedApprovalDecision::AcceptOnce,
            )
            .is_err()
        );
        assert!(approvals.contains_key(&approval_id));
        resolve_managed_approval(
            &mut approvals,
            approval_id,
            ManagedApprovalDecision::Decline,
        )
        .unwrap();
        assert_eq!(decision.await.unwrap(), ApprovalDecision::Decline);
        assert!(
            resolve_managed_approval(
                &mut approvals,
                approval_id,
                ManagedApprovalDecision::AcceptOnce,
            )
            .is_err()
        );
    }
}
