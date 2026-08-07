//! Foreground owner for transient conversation and operation state.
//!
//! Commands may affect execution. Events are passive observations; except for
//! the explicit permission request transport, a closed receiver never changes an
//! operation result.

mod protocol;

pub(crate) use protocol::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand};

use crate::{
    agent::Agent,
    identity::OperationId,
    message::{Message, Role},
    permission::{PermissionBroker, PermissionBrokerHandle, PermissionPolicy},
};
use std::{error::Error, fmt, sync::Arc};
use tokio::{sync::mpsc, task::JoinHandle};

const COMMAND_CAPACITY: usize = 16;

pub(crate) struct RuntimeHandle {
    commands: mpsc::Sender<RuntimeCommand>,
    events: mpsc::UnboundedReceiver<AgentEvent>,
}

struct Runtime {
    agent: Arc<Agent>,
    history: Vec<Message>,
    active: Option<ActiveOperation>,
    commands: mpsc::Receiver<RuntimeCommand>,
    events: mpsc::UnboundedSender<AgentEvent>,
    permissions: PermissionBrokerHandle,
    completions: mpsc::UnboundedReceiver<OperationCompletion>,
    completion_sender: mpsc::UnboundedSender<OperationCompletion>,
}

struct ActiveOperation {
    operation_id: OperationId,
    task: JoinHandle<()>,
}

struct OperationCompletion {
    operation_id: OperationId,
    history: Vec<Message>,
    result: Result<Message, String>,
}

impl RuntimeHandle {
    pub(crate) fn spawn(agent: Agent, policy: PermissionPolicy, controller_present: bool) -> Self {
        let (command_sender, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let (completion_sender, completion_receiver) = mpsc::unbounded_channel();
        let (permissions, _broker_task) =
            PermissionBroker::spawn(policy, controller_present, event_sender.clone());
        let runtime = Runtime {
            agent: Arc::new(agent),
            history: Vec::new(),
            active: None,
            commands: command_receiver,
            events: event_sender,
            permissions,
            completions: completion_receiver,
            completion_sender,
        };
        tokio::spawn(runtime.run());

        Self {
            commands: command_sender,
            events: event_receiver,
        }
    }

    pub(crate) async fn send(&self, command: RuntimeCommand) -> Result<(), RuntimeUnavailable> {
        self.commands
            .send(command)
            .await
            .map_err(|_| RuntimeUnavailable)
    }

    pub(crate) async fn next_event(&mut self) -> Option<AgentEvent> {
        self.events.recv().await
    }
}

impl Runtime {
    async fn run(mut self) {
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        self.permissions.controller_lost();
                        self.interrupt_active();
                        return;
                    };
                    if self.handle_command(command).await {
                        return;
                    }
                }
                completion = self.completions.recv(), if self.active.is_some() => {
                    if let Some(completion) = completion {
                        self.handle_completion(completion);
                    }
                }
            }
        }
    }

    /// Returns true when the runtime should stop.
    async fn handle_command(&mut self, command: RuntimeCommand) -> bool {
        match command {
            RuntimeCommand::SubmitTurn {
                operation_id,
                input,
            } => {
                if input.trim().is_empty() {
                    self.emit(AgentEvent::CommandRejected {
                        reason: "turn input must not be blank".to_owned(),
                    });
                    return false;
                }
                if let Some(active) = &self.active {
                    self.emit(AgentEvent::CommandRejected {
                        reason: format!(
                            "operation {} is already active; only one root turn may run",
                            active.operation_id
                        ),
                    });
                    return false;
                }

                let agent = Arc::clone(&self.agent);
                let permissions = self.permissions.clone();
                let events = self.events.clone();
                let completions = self.completion_sender.clone();
                let mut history = self.history.clone();
                history.push(Message::text(Role::User, input));
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id,
                    state: OperationState::Running,
                });
                let task = tokio::spawn(async move {
                    let result = agent
                        .run_turn(operation_id, &mut history, permissions, events)
                        .await
                        .map_err(|error| error.to_string());
                    if let Ok(message) = &result {
                        history.push(message.clone());
                    }
                    let _ = completions.send(OperationCompletion {
                        operation_id,
                        history,
                        result,
                    });
                });
                self.active = Some(ActiveOperation { operation_id, task });
            }
            RuntimeCommand::ClearConversation => {
                if self.active.is_some() {
                    self.emit(AgentEvent::CommandRejected {
                        reason: "cannot clear conversation while an operation is active".to_owned(),
                    });
                } else {
                    self.history.clear();
                    self.emit(AgentEvent::ConversationCleared);
                }
            }
            RuntimeCommand::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            } => {
                if let Err(error) = self
                    .permissions
                    .decide(operation_id, invocation_id, decision)
                    .await
                {
                    self.emit(AgentEvent::CommandRejected {
                        reason: error.to_string(),
                    });
                }
            }
            RuntimeCommand::Shutdown => {
                self.permissions.shutdown();
                self.interrupt_active();
                return true;
            }
        }
        false
    }

    fn handle_completion(&mut self, completion: OperationCompletion) {
        let Some(active) = self.active.take() else {
            return;
        };
        if active.operation_id != completion.operation_id {
            active.task.abort();
            self.emit(AgentEvent::CommandRejected {
                reason: format!(
                    "completion for {} did not match active operation {}",
                    completion.operation_id, active.operation_id
                ),
            });
            return;
        }

        match completion.result {
            Ok(message) => {
                self.history = completion.history;
                self.emit(AgentEvent::AssistantMessage {
                    operation_id: completion.operation_id,
                    message,
                });
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id: completion.operation_id,
                    state: OperationState::Finished(OperationOutcome::Completed),
                });
            }
            Err(reason) => {
                self.emit(AgentEvent::OperationFailed {
                    operation_id: completion.operation_id,
                    reason,
                });
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id: completion.operation_id,
                    state: OperationState::Finished(OperationOutcome::Failed),
                });
            }
        }
    }

    fn interrupt_active(&mut self) {
        if let Some(active) = self.active.take() {
            active.task.abort();
            self.emit(AgentEvent::OperationStateChanged {
                operation_id: active.operation_id,
                state: OperationState::Finished(OperationOutcome::Interrupted),
            });
        }
    }

    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeUnavailable;

impl fmt::Display for RuntimeUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Xana's foreground runtime is unavailable")
    }
}

impl Error for RuntimeUnavailable {}

#[cfg(test)]
mod tests;
