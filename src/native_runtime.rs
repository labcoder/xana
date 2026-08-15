//! Xana-owned foreground runtime for one durable native conversation.
//!
//! Commands may affect execution. Events are passive observations; except for
//! the explicit permission request transport, a closed receiver never changes an
//! operation result.

mod protocol;

pub(crate) use protocol::{
    AgentEvent, AgentEventSender, DroppedAgentEvents, OperationOutcome, OperationState,
    RuntimeCommand,
};

use crate::{
    agent::{Agent, ConversationCommit, ConversationCommitSender, DurableTurnServices},
    identity::OperationId,
    message::{Message, Role},
    operation::{CrashSite, DurableOperationCommand, DurableOperationSender, SuspensionReason},
    orchestration::{
        ChildCommitCommand, ChildCommitReceiver, ChildCommitSender, ChildSupervisor,
        ChildSupervisorHandle,
    },
    permission::{PermissionBroker, PermissionBrokerHandle, PermissionPolicy},
    prompt::{PromptAssembler, PromptSnapshot},
    session::{DurableSession, SessionRecord},
};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::{error::Error, fmt, future::Future, sync::Arc};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

const COMMAND_CAPACITY: usize = 16;

pub(crate) struct RuntimeHandle {
    commands: mpsc::Sender<RuntimeCommand>,
    events: mpsc::UnboundedReceiver<AgentEvent>,
    initial_history: Vec<Message>,
    exit: watch::Receiver<Option<RuntimeExit>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeExit {
    ShutdownRequested,
    ControllerDropped,
    Panicked,
}

struct Runtime {
    agent: Arc<Agent>,
    history: Vec<Message>,
    active: Option<ActiveOperation>,
    commands: mpsc::Receiver<RuntimeCommand>,
    events: mpsc::UnboundedSender<AgentEvent>,
    permissions: PermissionBrokerHandle,
    broker_events: mpsc::UnboundedReceiver<AgentEvent>,
    completions: mpsc::UnboundedReceiver<OperationCompletion>,
    completion_sender: mpsc::UnboundedSender<OperationCompletion>,
    conversation_commits: mpsc::UnboundedReceiver<ConversationCommit>,
    conversation_committer: ConversationCommitSender,
    durable_operations: mpsc::UnboundedReceiver<DurableOperationCommand>,
    durable_operation_sender: DurableOperationSender,
    session: Option<DurableSession>,
    prompt_assembler: Option<PromptAssembler>,
    child_commits: ChildCommitReceiver,
    _child_commit_sender: ChildCommitSender,
    child_supervisor: Option<ChildSupervisorHandle>,
    child_supervisor_task: Option<JoinHandle<()>>,
}

struct ActiveOperation {
    operation_id: OperationId,
    persist_from: usize,
    progress_committed: bool,
    task: JoinHandle<()>,
}

struct OperationCompletion {
    operation_id: OperationId,
    history: Vec<Message>,
    result: Result<Message, String>,
}

impl RuntimeHandle {
    #[cfg(test)]
    pub(crate) fn spawn(agent: Agent, policy: PermissionPolicy, controller_present: bool) -> Self {
        Self::spawn_inner(
            agent,
            policy,
            controller_present,
            None,
            None,
            Vec::new(),
            None,
        )
    }

    pub(crate) fn spawn_persistent(
        agent: Agent,
        policy: PermissionPolicy,
        controller_present: bool,
        session: DurableSession,
        prompt_assembler: PromptAssembler,
    ) -> Result<Self, RuntimeUnavailable> {
        let history = session.conversation().map_err(|_| RuntimeUnavailable)?;
        Ok(Self::spawn_inner(
            agent,
            policy,
            controller_present,
            Some(session),
            Some(prompt_assembler),
            history,
            None,
        ))
    }

    pub(crate) fn spawn_persistent_with_supervisor(
        agent: Agent,
        policy: PermissionPolicy,
        controller_present: bool,
        session: DurableSession,
        prompt_assembler: PromptAssembler,
        supervisor_handle: ChildSupervisorHandle,
        supervisor: ChildSupervisor,
    ) -> Result<Self, RuntimeUnavailable> {
        let history = session.conversation().map_err(|_| RuntimeUnavailable)?;
        Ok(Self::spawn_inner(
            agent,
            policy,
            controller_present,
            Some(session),
            Some(prompt_assembler),
            history,
            Some((supervisor_handle, supervisor)),
        ))
    }

    fn spawn_inner(
        agent: Agent,
        policy: PermissionPolicy,
        controller_present: bool,
        session: Option<DurableSession>,
        prompt_assembler: Option<PromptAssembler>,
        history: Vec<Message>,
        child_supervisor: Option<(ChildSupervisorHandle, ChildSupervisor)>,
    ) -> Self {
        let initial_history = history.clone();
        let (command_sender, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let (exit_sender, exit_receiver) = watch::channel(None);
        let (broker_event_sender, broker_event_receiver) = mpsc::unbounded_channel();
        let (completion_sender, completion_receiver) = mpsc::unbounded_channel();
        let (conversation_committer, conversation_commits) = ConversationCommitSender::channel();
        let (durable_operation_sender, durable_operations) = DurableOperationSender::channel();
        let (child_commit_sender, child_commits) = ChildCommitSender::channel();
        let (permissions, _broker_task) = if session.is_some() {
            PermissionBroker::spawn_for_durable_runtime(
                policy,
                controller_present,
                broker_event_sender,
            )
        } else {
            PermissionBroker::spawn(policy, controller_present, broker_event_sender)
        };
        let (child_supervisor, child_supervisor_task) = match child_supervisor {
            Some((handle, supervisor)) => {
                let task =
                    tokio::spawn(supervisor.run(child_commit_sender.clone(), event_sender.clone()));
                (Some(handle), Some(task))
            }
            None => (None, None),
        };
        let runtime = Runtime {
            agent: Arc::new(agent),
            history,
            active: None,
            commands: command_receiver,
            events: event_sender,
            permissions,
            broker_events: broker_event_receiver,
            completions: completion_receiver,
            completion_sender,
            conversation_commits,
            conversation_committer,
            durable_operations,
            durable_operation_sender,
            session,
            prompt_assembler,
            child_commits,
            _child_commit_sender: child_commit_sender,
            child_supervisor,
            child_supervisor_task,
        };
        tokio::spawn(async move {
            let exit = AssertUnwindSafe(runtime.run())
                .catch_unwind()
                .await
                .unwrap_or(RuntimeExit::Panicked);
            let _ = exit_sender.send(Some(exit));
        });

        Self {
            commands: command_sender,
            events: event_receiver,
            initial_history,
            exit: exit_receiver,
        }
    }

    pub(crate) async fn send(&self, command: RuntimeCommand) -> Result<(), RuntimeUnavailable> {
        self.commands
            .send(command)
            .await
            .map_err(|_| RuntimeUnavailable)
    }

    #[cfg(test)]
    pub(crate) async fn next_event(&mut self) -> Option<AgentEvent> {
        self.events.recv().await
    }

    pub(crate) fn into_frontend_parts(
        self,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<AgentEvent>,
        Vec<Message>,
        watch::Receiver<Option<RuntimeExit>>,
    ) {
        let Self {
            commands,
            events,
            initial_history,
            exit,
        } = self;
        let observer_exit = exit.clone();
        (
            Self {
                commands,
                events: mpsc::unbounded_channel().1,
                initial_history: Vec::new(),
                exit,
            },
            events,
            initial_history,
            observer_exit,
        )
    }
}

impl Runtime {
    async fn run(mut self) -> RuntimeExit {
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        self.permissions.controller_lost();
                        self.interrupt_active();
                        self.shutdown_children().await;
                        return RuntimeExit::ControllerDropped;
                    };
                    if self.handle_command(command).await {
                        return RuntimeExit::ShutdownRequested;
                    }
                }
                completion = self.completions.recv(), if self.active.is_some() => {
                    if let Some(completion) = completion {
                        self.handle_completion(completion);
                    }
                }
                broker_event = self.broker_events.recv() => {
                    if let Some(event) = broker_event {
                        self.handle_broker_event(event);
                    }
                }
                commit = self.conversation_commits.recv() => {
                    if let Some(commit) = commit {
                        self.handle_conversation_commit(commit);
                    }
                }
                command = self.durable_operations.recv() => {
                    if let Some(command) = command {
                        self.handle_durable_operation(command);
                    }
                }
                command = self.child_commits.recv() => {
                    if let Some(command) = command {
                        self.handle_child_commit(command);
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
                self.start_turn(operation_id, input, Vec::new()).await;
            }
            RuntimeCommand::SubmitTurnWithImages {
                operation_id,
                input,
                images,
            } => {
                self.start_turn(operation_id, input, images).await;
            }
            RuntimeCommand::ClearConversation => {
                if self.active.is_some() {
                    self.emit(AgentEvent::CommandRejected {
                        reason: "cannot clear conversation while an operation is active".to_owned(),
                    });
                } else {
                    if let Some(session) = &mut self.session
                        && let Err(error) = session.clear_conversation()
                    {
                        self.emit(AgentEvent::CommandRejected {
                            reason: format!("could not commit conversation clear: {error:#}"),
                        });
                        return false;
                    }
                    self.history.clear();
                    self.emit(AgentEvent::ConversationCleared);
                }
            }
            RuntimeCommand::ResumeOperation { .. } => {
                self.emit(AgentEvent::CommandRejected {
                    reason: "operation recovery is owned by the explicit `xana operation resume` controller"
                        .to_owned(),
                });
            }
            RuntimeCommand::InterruptOperation { operation_id } => {
                match self.active.as_ref().map(|active| active.operation_id) {
                    Some(active) if active == operation_id => self.interrupt_active(),
                    Some(active) => self.emit(AgentEvent::CommandRejected {
                        reason: format!(
                            "cannot interrupt operation {operation_id}; active operation is {active}"
                        ),
                    }),
                    None => self.emit(AgentEvent::CommandRejected {
                        reason: format!("cannot interrupt operation {operation_id}; no root turn is active"),
                    }),
                }
            }
            RuntimeCommand::SteerOperation {
                operation_id,
                input: _,
            } => self.emit(AgentEvent::CommandRejected {
                reason: format!(
                    "native operation {operation_id} does not support same-turn steering; submit a queued follow-up instead"
                ),
            }),
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
            RuntimeCommand::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            } => {
                let result = match self.child_supervisor.clone() {
                    Some(supervisor) => await_supervisor_response(
                        &mut self.child_commits,
                        &mut self.session,
                        supervisor.decide_permission(
                            agent_id,
                            operation_id,
                            invocation_id,
                            decision,
                        ),
                    )
                    .await
                    .map_err(|error| error.to_string()),
                    None => Err("this runtime has no child supervisor".to_owned()),
                };
                if let Err(reason) = result {
                    self.emit(AgentEvent::CommandRejected { reason });
                }
            }
            RuntimeCommand::ListChildren => {
                let result = match self.child_supervisor.clone() {
                    Some(supervisor) => await_supervisor_response(
                        &mut self.child_commits,
                        &mut self.session,
                        supervisor.list_agents(),
                    )
                    .await
                    .map_err(|error| error.to_string()),
                    None => Ok(Vec::new()),
                };
                match result {
                    Ok(children) => self.emit(AgentEvent::ChildListSnapshot { children }),
                    Err(reason) => self.emit(AgentEvent::CommandRejected { reason }),
                }
            }
            RuntimeCommand::InspectChild { agent_id } => {
                let result = match self.child_supervisor.clone() {
                    Some(supervisor) => await_supervisor_response(
                        &mut self.child_commits,
                        &mut self.session,
                        supervisor.inspect_agent(agent_id),
                    )
                    .await
                    .map_err(|error| error.to_string()),
                    None => Err("this runtime has no child supervisor".to_owned()),
                };
                match result {
                    Ok(child) => self.emit(AgentEvent::ChildInspectionSnapshot {
                        child: Box::new(child),
                    }),
                    Err(reason) => self.emit(AgentEvent::CommandRejected { reason }),
                }
            }
            RuntimeCommand::CancelChild { agent_id } => {
                let result = match self.child_supervisor.clone() {
                    Some(supervisor) => await_supervisor_response(
                        &mut self.child_commits,
                        &mut self.session,
                        supervisor.cancel_agent(agent_id),
                    )
                    .await
                    .map_err(|error| error.to_string()),
                    None => Err("this runtime has no child supervisor".to_owned()),
                };
                match result {
                    Ok(receipt) => self.emit(AgentEvent::ChildCancellationRequested { receipt }),
                    Err(reason) => self.emit(AgentEvent::CommandRejected { reason }),
                }
            }
            RuntimeCommand::Shutdown => {
                self.permissions.shutdown();
                self.interrupt_active();
                self.shutdown_children().await;
                return true;
            }
        }
        false
    }

    async fn start_turn(
        &mut self,
        operation_id: OperationId,
        input: String,
        images: Vec<crate::vision::ImageRef>,
    ) {
        if input.trim().is_empty() {
            self.emit(AgentEvent::CommandRejected {
                reason: "turn input must not be blank".to_owned(),
            });
            return;
        }
        if let Some(active) = &self.active {
            self.emit(AgentEvent::CommandRejected {
                reason: format!(
                    "operation {} is already active; only one root turn may run",
                    active.operation_id
                ),
            });
            return;
        }

        let agent = Arc::clone(&self.agent);
        let permissions = self.permissions.clone();
        let events = self.events.clone();
        let completions = self.completion_sender.clone();
        let conversation_committer = self.conversation_committer.clone();
        let durable_operation_sender = self.durable_operation_sender.clone();
        let prompt = match self.prepare_turn_prompt() {
            Ok(prompt) => prompt,
            Err(reason) => {
                self.emit(AgentEvent::CommandRejected { reason });
                return;
            }
        };
        let mut content = vec![crate::message::ContentBlock::Text(input)];
        content.extend(images.into_iter().map(crate::message::ContentBlock::Image));
        let user_message = Message {
            role: Role::User,
            content,
        };
        let input_entry_id = if let Some(session) = &mut self.session {
            match session.append_message(user_message.clone()) {
                Ok(entry_id) => Some(entry_id),
                Err(error) => {
                    self.emit(AgentEvent::CommandRejected {
                        reason: format!("could not commit user conversation entry: {error:#}"),
                    });
                    return;
                }
            }
        } else {
            None
        };
        self.history.push(user_message);
        let mut history = self.history.clone();
        if let (Some(session), Some(input_entry_id)) = (&mut self.session, input_entry_id) {
            if let Err(error) = session.append_record(SessionRecord::OperationAccepted {
                operation_id,
                thread_id: session.thread_id(),
                input_entry_id,
            }) {
                self.emit(AgentEvent::CommandRejected {
                    reason: format!("could not commit operation acceptance: {error:#}"),
                });
                return;
            }
            if let Err(error) = self
                .agent
                .observe_boundary(CrashSite::AfterOperationAccepted)
            {
                self.emit(AgentEvent::CommandRejected {
                    reason: format!("operation stopped at accepted boundary: {error:#}"),
                });
                return;
            }
        }
        self.emit(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        });
        let persist_from = history.len();
        let task = tokio::spawn(async move {
            let result = match prompt {
                Some(prompt) => {
                    agent
                        .run_turn_with_prompt(
                            operation_id,
                            &mut history,
                            &prompt,
                            permissions,
                            events,
                            Some(DurableTurnServices::new(
                                conversation_committer,
                                durable_operation_sender,
                            )),
                        )
                        .await
                }
                None => {
                    agent
                        .run_turn(operation_id, &mut history, permissions, events)
                        .await
                }
            }
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
        self.active = Some(ActiveOperation {
            operation_id,
            persist_from,
            progress_committed: self.session.is_some(),
            task,
        });
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

        if let Some(session) = &mut self.session {
            let persist_from = if active.progress_committed {
                if completion.result.is_ok() {
                    completion.history.len().saturating_sub(1)
                } else {
                    completion.history.len()
                }
            } else {
                active.persist_from
            };
            for message in completion.history.iter().skip(persist_from) {
                if let Err(error) = session.append_message(message.clone()) {
                    self.emit(AgentEvent::OperationFailed {
                        operation_id: completion.operation_id,
                        reason: format!("could not commit conversation entry: {error:#}"),
                    });
                    return;
                }
            }
        }

        match completion.result {
            Ok(message) => {
                self.history = completion.history;
                if active.progress_committed
                    && !self.commit_operation_finished(
                        completion.operation_id,
                        OperationOutcome::Completed,
                    )
                {
                    return;
                }
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
                self.history = completion.history;
                if active.progress_committed && self.operation_has_pending(completion.operation_id)
                {
                    if let Some(session) = &mut self.session
                        && let Err(error) =
                            session.append_record(SessionRecord::OperationSuspended {
                                operation_id: completion.operation_id,
                                reason: SuspensionReason::ProcessInterrupted,
                            })
                    {
                        self.emit(AgentEvent::OperationFailed {
                            operation_id: completion.operation_id,
                            reason: format!("could not commit operation suspension: {error:#}"),
                        });
                        return;
                    }
                    self.emit(AgentEvent::OperationFailed {
                        operation_id: completion.operation_id,
                        reason,
                    });
                    self.emit(AgentEvent::OperationStateChanged {
                        operation_id: completion.operation_id,
                        state: OperationState::Suspended,
                    });
                    return;
                }
                if active.progress_committed
                    && !self.commit_operation_finished(
                        completion.operation_id,
                        OperationOutcome::Failed,
                    )
                {
                    return;
                }
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
            if active.progress_committed {
                if let Some(session) = &mut self.session {
                    let _ = session.append_record(SessionRecord::OperationSuspended {
                        operation_id: active.operation_id,
                        reason: SuspensionReason::ProcessInterrupted,
                    });
                }
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id: active.operation_id,
                    state: OperationState::Suspended,
                });
                return;
            }
            self.emit(AgentEvent::OperationStateChanged {
                operation_id: active.operation_id,
                state: OperationState::Finished(OperationOutcome::Interrupted),
            });
        }
    }

    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    fn prepare_turn_prompt(&mut self) -> Result<Option<PromptSnapshot>, String> {
        let Some(session) = &mut self.session else {
            return Ok(None);
        };
        let sources = session
            .refresh_project_context()
            .map_err(|error| format!("could not refresh durable project context: {error:#}"))?;
        let assembler = self
            .prompt_assembler
            .as_ref()
            .ok_or_else(|| "persistent runtime has no prompt assembler".to_owned())?;
        assembler
            .assemble(&sources)
            .map(Some)
            .map_err(|error| format!("could not assemble turn prompt: {error}"))
    }

    fn handle_broker_event(&mut self, event: AgentEvent) {
        let committed = match &event {
            AgentEvent::OperationStateChanged {
                operation_id,
                state,
            } => {
                if *state == OperationState::Suspended
                    && let Some(session) = &mut self.session
                    && let Err(error) = session.append_record(SessionRecord::OperationSuspended {
                        operation_id: *operation_id,
                        reason: SuspensionReason::Permission,
                    })
                {
                    self.emit(AgentEvent::OperationFailed {
                        operation_id: *operation_id,
                        reason: format!("could not commit operation suspension: {error:#}"),
                    });
                    return;
                }
                true
            }
            AgentEvent::PermissionAudited { fact } => {
                if let Some(session) = &mut self.session {
                    if let Err(error) = session.append_audit(fact.clone()) {
                        self.emit(AgentEvent::OperationFailed {
                            operation_id: fact.request.operation_id,
                            reason: format!("could not commit permission audit: {error:#}"),
                        });
                        false
                    } else {
                        true
                    }
                } else {
                    true
                }
            }
            _ => true,
        };
        if committed {
            self.emit(event);
        }
    }

    fn handle_conversation_commit(&mut self, commit: ConversationCommit) {
        let projected_message = commit.message.clone();
        let result = if self
            .active
            .as_ref()
            .is_none_or(|active| active.operation_id != commit.operation_id)
        {
            Err(format!(
                "conversation commit does not match active operation {}",
                commit.operation_id
            ))
        } else if let Some(session) = &mut self.session {
            session
                .append_message(commit.message)
                .map_err(|error| format!("could not commit conversation entry: {error:#}"))
        } else {
            Err("conversation commit has no durable session".to_owned())
        };

        if result.is_ok() {
            if projected_message.role == crate::message::Role::Assistant {
                self.emit(AgentEvent::AssistantMessage {
                    operation_id: commit.operation_id,
                    message: projected_message,
                });
            }
            if let Some((invocation_id, result_message)) = commit.tool_finished {
                self.emit(AgentEvent::ToolFinished {
                    operation_id: commit.operation_id,
                    invocation_id,
                    result: result_message,
                });
            }
        }
        let _ = commit.acknowledged.send(result);
    }

    fn handle_durable_operation(&mut self, command: DurableOperationCommand) {
        match command {
            DurableOperationCommand::Append {
                record,
                event,
                acknowledged,
            } => {
                let result = self
                    .session
                    .as_mut()
                    .ok_or_else(|| "durable operation has no session writer".to_owned())
                    .and_then(|session| {
                        session.append_record(*record).map_err(|error| {
                            format!("could not append operation record: {error:#}")
                        })
                    });
                if result.is_ok()
                    && let Some(event) = event
                {
                    self.emit(*event);
                }
                let _ = acknowledged.send(result);
            }
            DurableOperationCommand::StoreJson {
                value,
                acknowledged,
            } => {
                let result = self
                    .session
                    .as_mut()
                    .ok_or_else(|| "durable value has no session writer".to_owned())
                    .and_then(|session| {
                        session
                            .store_json_value(value)
                            .map_err(|error| format!("could not store durable value: {error:#}"))
                    });
                let _ = acknowledged.send(result);
            }
        }
    }

    fn handle_child_commit(&mut self, command: ChildCommitCommand) {
        handle_child_commit(&mut self.session, command);
    }

    async fn shutdown_children(&mut self) {
        if let Some(supervisor) = self.child_supervisor.take() {
            let shutdown = supervisor.shutdown();
            tokio::pin!(shutdown);
            loop {
                tokio::select! {
                    () = &mut shutdown => break,
                    command = self.child_commits.recv() => {
                        if let Some(command) = command {
                            self.handle_child_commit(command);
                        }
                    }
                }
            }
        }
        if let Some(task) = self.child_supervisor_task.take() {
            let _ = task.await;
        }
    }

    fn commit_operation_finished(
        &mut self,
        operation_id: OperationId,
        outcome: OperationOutcome,
    ) -> bool {
        if let Some(session) = &mut self.session
            && let Err(error) = session.append_record(SessionRecord::OperationFinished {
                operation_id,
                outcome,
            })
        {
            self.emit(AgentEvent::OperationFailed {
                operation_id,
                reason: format!("could not commit operation finish: {error:#}"),
            });
            return false;
        }
        true
    }

    fn operation_has_pending(&self, operation_id: OperationId) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.operation_has_pending(operation_id))
    }
}

async fn await_supervisor_response<T>(
    child_commits: &mut ChildCommitReceiver,
    session: &mut Option<DurableSession>,
    response: impl Future<Output = T>,
) -> T {
    tokio::pin!(response);
    loop {
        tokio::select! {
            result = &mut response => return result,
            command = child_commits.recv() => match command {
                Some(command) => handle_child_commit(session, command),
                None => return response.await,
            }
        }
    }
}

fn handle_child_commit(session: &mut Option<DurableSession>, command: ChildCommitCommand) {
    let result = if let Some(session) = session {
        session
            .append_record(command.record)
            .map_err(|error| format!("could not append child record: {error:#}"))
    } else {
        Ok(())
    };
    let _ = command.acknowledged.send(result);
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
