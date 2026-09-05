//! Xana-owned foreground runtime for one durable native conversation.
//!
//! Commands may affect execution. Events are passive observations; except for
//! the explicit permission request transport, a closed receiver never changes an
//! operation result.

mod memory_controls;
mod protocol;

pub(crate) use protocol::{
    AgentEvent, AgentEventSender, DroppedAgentEvents, OperationOutcome, OperationState,
    RoundBudgetAction, RoundBudgetCommitFacts, RoundBudgetDecision, RoundBudgetSuspension,
    RuntimeCommand,
};

use crate::{
    agent::{
        Agent, AgentTurnOutcome, AgentTurnUsage, ConversationCommit, ConversationCommitSender,
        DurableTurnServices,
    },
    identity::{OperationId, RoundBudgetId},
    message::{Message, Role},
    operation::{CrashSite, DurableOperationCommand, DurableOperationSender, SuspensionReason},
    orchestration::{
        ChildCommitCommand, ChildCommitReceiver, ChildCommitSender, ChildSupervisor,
        ChildSupervisorHandle,
    },
    permission::{PermissionBroker, PermissionBrokerHandle, PermissionPolicy},
    prompt::{PromptAssembler, PromptSnapshot},
    session::{
        CompactionCheckpoint, CompactionError, CompactionReason, ConversationPage, DurableSession,
        SessionRecord,
    },
    tool::DeferredCleanup,
};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::{collections::BTreeMap, error::Error, fmt, future::Future, sync::Arc};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

const COMMAND_CAPACITY: usize = 16;
const MAX_ROOT_TOOL_ROUNDS: usize = 256;

pub(crate) struct RuntimeHandle {
    runtime_task: Option<JoinHandle<()>>,
    owned_tasks: tokio_util::task::TaskTracker,
    commands: mpsc::Sender<RuntimeCommand>,
    events: mpsc::UnboundedReceiver<AgentEvent>,
    initial_history: ConversationPage,
    exit: watch::Receiver<Option<RuntimeExit>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeExit {
    ShutdownRequested,
    ControllerDropped,
    Panicked,
}

struct Runtime {
    memory_maintenance: Option<JoinHandle<()>>,
    automatic_learning: bool,
    owned_tasks: tokio_util::task::TaskTracker,
    stop_after_compaction: bool,
    compaction_cancelled: bool,
    memory: Option<crate::memory::MemoryOwner>,
    agent: Arc<Agent>,
    history: Vec<Message>,
    active: Option<ActiveOperation>,
    suspended_round_budget: Option<RoundBudgetSuspension>,
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
    compaction_checkpoint: Option<CompactionCheckpoint>,
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
    cleanup: DeferredCleanup,
    rounds_before: usize,
    continuations_used: usize,
    usage_before: AgentTurnUsage,
}

struct OperationCompletion {
    operation_id: OperationId,
    history: Vec<Message>,
    result: Result<AgentTurnOutcome, String>,
}

struct RuntimeSeed {
    automatic_learning: bool,
    memory: Option<crate::memory::MemoryOwner>,
    session: Option<DurableSession>,
    prompt_assembler: Option<PromptAssembler>,
    history: Vec<Message>,
    initial_history: ConversationPage,
    compaction_checkpoint: Option<CompactionCheckpoint>,
    suspended_round_budget: Option<RoundBudgetSuspension>,
}

impl RuntimeSeed {
    #[cfg(test)]
    fn transient() -> Self {
        Self {
            automatic_learning: true,
            memory: None,
            session: None,
            prompt_assembler: None,
            history: Vec::new(),
            initial_history: ConversationPage {
                messages: Vec::new(),
                start: 0,
                total: 0,
                has_older: false,
            },
            compaction_checkpoint: None,
            suspended_round_budget: None,
        }
    }

    fn persistent(
        session: DurableSession,
        prompt_assembler: PromptAssembler,
        memory: Option<crate::memory::MemoryOwner>,
    ) -> Result<Self, RuntimeUnavailable> {
        let initial_history = session
            .initial_conversation_page()
            .map_err(|_| RuntimeUnavailable)?;
        let continuation = session
            .prompt_continuation()
            .map_err(|_| RuntimeUnavailable)?;
        let suspended_round_budget = session.round_budget_suspension();
        Ok(Self {
            automatic_learning: true,
            session: Some(session),
            memory,
            prompt_assembler: Some(prompt_assembler),
            history: continuation.history,
            initial_history,
            compaction_checkpoint: continuation.checkpoint,
            suspended_round_budget,
        })
    }
}

impl RuntimeHandle {
    /// Explicit background composition shares the native durable runtime but
    /// cannot turn scheduled model output into an automatic memory proposal.
    pub(crate) fn spawn_persistent_background(
        agent: Agent,
        policy: PermissionPolicy,
        session: DurableSession,
        prompt_assembler: PromptAssembler,
        memory: Option<crate::memory::MemoryOwner>,
    ) -> Result<Self, RuntimeUnavailable> {
        let mut seed = RuntimeSeed::persistent(session, prompt_assembler, memory)?;
        seed.automatic_learning = false;
        Ok(Self::spawn_inner(agent, policy, false, seed, None))
    }

    /// Bound the normal shutdown handshake, then abort and join only this
    /// runtime and its tracked worker. False means forced/unknown, not stopped
    /// before effects. The caller must retain workspace authority until return.
    pub(crate) async fn shutdown_owned(mut self) -> bool {
        let Some(mut task) = self.runtime_task.take() else {
            return false;
        };
        let deadline = std::time::Duration::from_secs(8);
        let graceful = async {
            let _ = self.send(RuntimeCommand::Shutdown).await;
            (&mut task).await
        };
        let acknowledged = match tokio::time::timeout(deadline, graceful).await {
            Ok(result) => result.is_ok(),
            Err(_) => {
                task.abort();
                let _ = task.await;
                false
            }
        };
        self.owned_tasks.close();
        self.owned_tasks.wait().await;
        acknowledged
    }
    #[cfg(test)]
    pub(crate) fn spawn(agent: Agent, policy: PermissionPolicy, controller_present: bool) -> Self {
        Self::spawn_inner(
            agent,
            policy,
            controller_present,
            RuntimeSeed::transient(),
            None,
        )
    }

    pub(crate) fn spawn_persistent(
        agent: Agent,
        policy: PermissionPolicy,
        controller_present: bool,
        session: DurableSession,
        prompt_assembler: PromptAssembler,
        memory: Option<crate::memory::MemoryOwner>,
    ) -> Result<Self, RuntimeUnavailable> {
        Ok(Self::spawn_inner(
            agent,
            policy,
            controller_present,
            RuntimeSeed::persistent(session, prompt_assembler, memory)?,
            None,
        ))
    }

    #[allow(clippy::too_many_arguments)] // Composition-only ownership handoff.
    pub(crate) fn spawn_persistent_with_supervisor(
        agent: Agent,
        policy: PermissionPolicy,
        controller_present: bool,
        session: DurableSession,
        prompt_assembler: PromptAssembler,
        supervisor_handle: ChildSupervisorHandle,
        supervisor: ChildSupervisor,
        memory: Option<crate::memory::MemoryOwner>,
    ) -> Result<Self, RuntimeUnavailable> {
        Ok(Self::spawn_inner(
            agent,
            policy,
            controller_present,
            RuntimeSeed::persistent(session, prompt_assembler, memory)?,
            Some((supervisor_handle, supervisor)),
        ))
    }

    fn spawn_inner(
        agent: Agent,
        policy: PermissionPolicy,
        controller_present: bool,
        seed: RuntimeSeed,
        child_supervisor: Option<(ChildSupervisorHandle, ChildSupervisor)>,
    ) -> Self {
        let RuntimeSeed {
            automatic_learning,
            memory,
            session,
            prompt_assembler,
            history,
            initial_history,
            compaction_checkpoint,
            suspended_round_budget,
        } = seed;
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
        let owned_tasks = tokio_util::task::TaskTracker::new();
        let runtime = Runtime {
            memory_maintenance: None,
            automatic_learning,
            owned_tasks: owned_tasks.clone(),
            stop_after_compaction: false,
            compaction_cancelled: false,
            memory,
            agent: Arc::new(agent),
            history,
            active: None,
            suspended_round_budget,
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
            compaction_checkpoint,
            child_commits,
            _child_commit_sender: child_commit_sender,
            child_supervisor,
            child_supervisor_task,
        };
        let runtime_task = tokio::spawn(async move {
            let exit = AssertUnwindSafe(runtime.run())
                .catch_unwind()
                .await
                .unwrap_or(RuntimeExit::Panicked);
            let _ = exit_sender.send(Some(exit));
        });

        Self {
            runtime_task: Some(runtime_task),
            owned_tasks,
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
        ConversationPage,
        watch::Receiver<Option<RuntimeExit>>,
    ) {
        let Self {
            runtime_task,
            owned_tasks,
            commands,
            events,
            initial_history,
            exit,
        } = self;
        let observer_exit = exit.clone();
        (
            Self {
                runtime_task,
                owned_tasks,
                commands,
                events: mpsc::unbounded_channel().1,
                initial_history: ConversationPage {
                    messages: Vec::new(),
                    start: 0,
                    total: 0,
                    has_older: false,
                },
                exit,
            },
            events,
            initial_history,
            observer_exit,
        )
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(task) = &self.memory_maintenance {
            task.abort();
        }
        self.permissions.controller_lost();
        if let Some(active) = &self.active {
            active.task.abort();
        }
        if let Some(task) = &self.child_supervisor_task {
            task.abort();
        }
    }
}

impl Runtime {
    async fn run(mut self) -> RuntimeExit {
        let mut maintenance = tokio::time::interval(std::time::Duration::from_secs(30));
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        if let Some(suspension) = self.suspended_round_budget.clone() {
            self.emit(AgentEvent::RoundBudgetReached { suspension });
        }
        loop {
            tokio::select! {
                biased;
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        self.permissions.controller_lost();
                        self.interrupt_active().await;
                        self.shutdown_children().await;
                        return RuntimeExit::ControllerDropped;
                    };
                    if self.handle_command(command).await {
                        return RuntimeExit::ShutdownRequested;
                    }
                }
                completion = self.completions.recv(), if self.active.is_some() => {
                    if let Some(completion) = completion {
                        self.handle_completion(completion).await;
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
                _=maintenance.tick(), if self.automatic_learning && self.active.is_none() && self.memory_maintenance.as_ref().is_none_or(JoinHandle::is_finished) => {
                    if let Some(worker)=self.memory.as_ref().and_then(|owner|owner.learner.clone()) {
                        self.memory_maintenance=Some(self.owned_tasks.spawn(async move {
                            let _=worker.process(false,&tokio_util::sync::CancellationToken::new()).await;
                        }));
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
                if self.active.is_some() || self.suspended_round_budget.is_some() {
                    self.emit(AgentEvent::CommandRejected {
                        reason: "cannot clear conversation while an operation is active or awaiting a round-budget decision".to_owned(),
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
                    self.compaction_checkpoint = None;
                    self.emit(AgentEvent::ConversationCleared);
                }
            }
            RuntimeCommand::CompactConversation { operation_id } => {
                if self.active.is_some() || self.suspended_round_budget.is_some() {
                    self.emit(AgentEvent::CommandRejected {
                        reason: "cannot compact conversation while an operation is active or awaiting a round-budget decision"
                            .to_owned(),
                    });
                } else {
                    self.emit(AgentEvent::CompactionStarted {
                        operation_id,
                        reason: CompactionReason::Manual,
                    });
                    match self.compact_now(operation_id, CompactionReason::Manual).await {
                        Ok(checkpoint) => {
                            if let Ok(Some(prompt)) = self.prepare_turn_prompt()
                                && let Some(ledger) = prompt.ledger(&self.history)
                            {
                                self.emit(AgentEvent::PromptPlanUpdated {
                                    operation_id,
                                    ledger,
                                });
                            }
                            self.emit(AgentEvent::ConversationCompacted { checkpoint });
                        }
                        Err(reason) => self.emit(AgentEvent::CompactionUnavailable {
                            operation_id,
                            reason,
                        }),
                    }
                }
            }
            RuntimeCommand::ResumeOperation { .. } => {
                self.emit(AgentEvent::CommandRejected {
                    reason: "operation recovery is owned by the explicit `xana operation resume` controller"
                        .to_owned(),
                });
            }
            RuntimeCommand::DecideRoundBudget {
                operation_id,
                suspension_id,
                action,
            } => {
                self.decide_round_budget(operation_id, suspension_id, action)
                    .await;
            }
            RuntimeCommand::InterruptOperation { operation_id } => {
                match self.active.as_ref().map(|active| active.operation_id) {
                    Some(active) if active == operation_id => self.interrupt_active().await,
                    Some(active) => self.emit(AgentEvent::CommandRejected {
                        reason: format!(
                            "cannot interrupt operation {operation_id}; active operation is {active}"
                        ),
                    }),
                    None if self
                        .suspended_round_budget
                        .as_ref()
                        .is_some_and(|suspension| suspension.operation_id == operation_id) => {
                        self.emit(AgentEvent::CommandRejected {
                            reason: format!(
                                "operation {operation_id} is already suspended; use its exact stop decision"
                            ),
                        })
                    }
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
                self.interrupt_active().await;
                self.shutdown_children().await;
                return true;
            }
        }
        if self.stop_after_compaction {
            self.permissions.shutdown();
            self.interrupt_active().await;
            self.shutdown_children().await;
        }
        self.stop_after_compaction
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
        if let Some(suspended) = &self.suspended_round_budget {
            self.emit(AgentEvent::CommandRejected {
                reason: format!(
                    "operation {} is suspended at round budget {}; continue or stop it before starting another turn",
                    suspended.operation_id, suspended.id
                ),
            });
            return;
        }

        if images.is_empty() && crate::memory::parse_natural(&input).is_some() {
            if !self.automatic_learning {
                self.emit(AgentEvent::CommandRejected{reason:"Scheduled work cannot exercise owner-only personal memory controls; return to the owner for review".into()});
                return;
            }
            self.run_memory_control(operation_id, input).await;
            return;
        }
        let _foreground = if self.automatic_learning {
            match self
                .memory
                .as_ref()
                .map(|owner| owner.store.foreground_lease())
                .transpose()
            {
                Ok(lease) => lease,
                Err(error) => {
                    self.emit(AgentEvent::CommandRejected {
                        reason: format!("Could not acquire foreground priority: {error:#}"),
                    });
                    return;
                }
            }
        } else {
            None
        };
        let query = input.clone();
        let mut content = vec![crate::message::ContentBlock::Text(input)];
        content.extend(images.into_iter().map(crate::message::ContentBlock::Image));
        let user_message = Message {
            role: Role::User,
            content,
        };
        let mut prompt = match self.prepare_turn_prompt_for(&query) {
            Ok(prompt) => prompt,
            Err(reason) => {
                self.emit(AgentEvent::CommandRejected { reason });
                return;
            }
        };
        if let Some(snapshot) = &prompt {
            let mut candidate = self.history.clone();
            candidate.push(user_message.clone());
            if let Some(ledger) = snapshot.ledger(&candidate)
                && (ledger.estimated_input_tokens > ledger.budget.compaction_threshold_tokens
                    || self
                        .session
                        .as_ref()
                        .is_some_and(DurableSession::retained_pressure))
            {
                self.emit(AgentEvent::CompactionStarted {
                    operation_id,
                    reason: CompactionReason::AutomaticThreshold,
                });
                match self
                    .compact_now(operation_id, CompactionReason::AutomaticThreshold)
                    .await
                {
                    Ok(checkpoint) => {
                        self.emit(AgentEvent::ConversationCompacted { checkpoint });
                        prompt = match self.prepare_turn_prompt_for(&query) {
                            Ok(prompt) => prompt,
                            Err(reason) => {
                                self.emit(AgentEvent::CommandRejected { reason });
                                return;
                            }
                        };
                    }
                    Err(reason) => {
                        self.emit(AgentEvent::CompactionUnavailable {
                            operation_id,
                            reason,
                        });
                        if self.stop_after_compaction || self.compaction_cancelled {
                            return;
                        }
                    }
                }
            }
        }
        let mut candidate = self.history.clone();
        candidate.push(user_message.clone());
        if let Some(snapshot) = &prompt
            && let Err(error) = snapshot.messages_for_request(&candidate)
        {
            self.emit(AgentEvent::CommandRejected {
                reason: format!(
                    "turn still exceeds the safe prompt budget after compaction: {error}"
                ),
            });
            return;
        }

        let input_entry_id = if let Some(session) = &mut self.session {
            match session.append_message(user_message.clone()) {
                Ok(entry_id) => Some(entry_id),
                Err(error) => {
                    self.agent
                        .record_storage_failure(operation_id, "conversation-user-entry");
                    self.emit(AgentEvent::CommandRejected {
                        reason: format!("could not commit user conversation entry: {error:#}"),
                    });
                    return;
                }
            }
        } else {
            None
        };
        if self.automatic_learning
            && let Some(owner) = &self.memory
        {
            let source = input_entry_id
                .and_then(|id| id.to_string().parse().ok())
                .unwrap_or_else(uuid::Uuid::new_v4);
            if let Err(error) = owner.enqueue_user_statement(source, &query) {
                // Metadata-only operational notice; neither input nor helper output
                // enters diagnostics. The accepted source remains in raw history.
                let _=owner.store.set_document("memory/learning-receipt",&serde_json::to_vec(&serde_json::json!({"state":"enqueue_blocked","notice":"Learning could not queue this source; inspect controls, storage and queue capacity. The user Conversation remains available."})).expect("receipt JSON"),4096);
                drop(error);
            }
        }
        self.history.push(user_message.clone());
        self.emit(AgentEvent::UserMessageCommitted {
            operation_id,
            message: user_message,
        });
        if let (Some(session), Some(input_entry_id)) = (&mut self.session, input_entry_id) {
            if let Err(error) = session.append_record(SessionRecord::OperationAccepted {
                operation_id,
                thread_id: session.thread_id(),
                input_entry_id,
            }) {
                self.agent
                    .record_storage_failure(operation_id, "operation-accepted");
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
        let persist_from = self.history.len();
        let round_limit = self.agent.max_tool_rounds();
        self.spawn_tranche(
            operation_id,
            prompt,
            persist_from,
            0,
            0,
            AgentTurnUsage::empty(),
            round_limit,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_tranche(
        &mut self,
        operation_id: OperationId,
        prompt: Option<PromptSnapshot>,
        persist_from: usize,
        rounds_before: usize,
        continuations_used: usize,
        usage_before: AgentTurnUsage,
        round_limit: usize,
    ) {
        let agent = Arc::clone(&self.agent);
        let permissions = self.permissions.clone();
        let events = self.events.clone();
        let completions = self.completion_sender.clone();
        let conversation_committer = self.conversation_committer.clone();
        let durable_operation_sender = self.durable_operation_sender.clone();
        let mut history = self.history.clone();
        let cleanup = DeferredCleanup::default();
        let operation_cleanup = cleanup.clone();
        let foreground_store = self
            .automatic_learning
            .then(|| self.memory.as_ref().map(|owner| owner.store.clone()))
            .flatten();
        let task = self.owned_tasks.spawn(async move {
            let result = AssertUnwindSafe(async {
                let _foreground = foreground_store
                    .as_ref()
                    .map(|store| store.foreground_lease())
                    .transpose()?;
                match prompt {
                    Some(prompt) => {
                        agent
                            .run_tranche_with_prompt_in_scope(
                                operation_id,
                                &mut history,
                                &prompt,
                                permissions,
                                events.into(),
                                Some(DurableTurnServices::new(
                                    conversation_committer,
                                    durable_operation_sender,
                                )),
                                operation_cleanup,
                                round_limit,
                            )
                            .await
                    }
                    None => {
                        agent
                            .run_tranche_in_scope(
                                operation_id,
                                &mut history,
                                permissions,
                                events.into(),
                                operation_cleanup,
                                round_limit,
                            )
                            .await
                    }
                }
            })
            .catch_unwind()
            .await
            .map_err(|_| anyhow::anyhow!("native operation task panicked"))
            .and_then(|result| result)
            .map_err(|error| error.to_string());
            if let Ok(AgentTurnOutcome::Completed(result)) = &result {
                history.push(result.message.clone());
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
            cleanup,
            rounds_before,
            continuations_used,
            usage_before,
        });
    }

    async fn handle_completion(&mut self, completion: OperationCompletion) {
        let Some(active) = self.active.take() else {
            return;
        };
        if active.operation_id != completion.operation_id {
            active.task.abort();
            let _ = active.task.await;
            active.cleanup.drain().await;
            self.emit(AgentEvent::CommandRejected {
                reason: format!(
                    "completion for {} did not match active operation {}",
                    completion.operation_id, active.operation_id
                ),
            });
            return;
        }
        let _ = active.task.await;
        active.cleanup.drain().await;

        if let Some(session) = &mut self.session {
            let persist_from = if active.progress_committed {
                match &completion.result {
                    Ok(AgentTurnOutcome::Completed(_)) => {
                        completion.history.len().saturating_sub(1)
                    }
                    Ok(AgentTurnOutcome::RoundBudgetReached { .. }) | Err(_) => {
                        completion.history.len()
                    }
                }
            } else {
                active.persist_from
            };
            for message in completion.history.iter().skip(persist_from) {
                if let Err(error) = session.append_message(message.clone()) {
                    self.agent.record_storage_failure(
                        completion.operation_id,
                        "conversation-result-entry",
                    );
                    self.emit(AgentEvent::OperationFailed {
                        operation_id: completion.operation_id,
                        reason: format!("could not commit conversation entry: {error:#}"),
                    });
                    return;
                }
            }
        }

        match completion.result {
            Ok(AgentTurnOutcome::Completed(result)) => {
                self.history = completion.history;
                let usage = active.usage_before.merge(result.usage);
                if active.progress_committed
                    && !self.commit_operation_finished(
                        completion.operation_id,
                        OperationOutcome::Completed,
                    )
                {
                    return;
                }
                self.emit(AgentEvent::UsageObserved {
                    operation_id: completion.operation_id,
                    usage,
                });
                self.emit(AgentEvent::AssistantMessage {
                    operation_id: completion.operation_id,
                    message: result.message,
                });
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id: completion.operation_id,
                    state: OperationState::Finished(OperationOutcome::Completed),
                });
            }
            Ok(AgentTurnOutcome::RoundBudgetReached { rounds, usage }) => {
                self.history = completion.history;
                let usage = active.usage_before.merge(usage);
                let rounds_consumed = active.rounds_before.saturating_add(rounds);
                let suspension = self.round_budget_suspension(
                    completion.operation_id,
                    rounds,
                    rounds_consumed,
                    active.continuations_used,
                    usage,
                );
                if active.progress_committed {
                    let Some(session) = &mut self.session else {
                        unreachable!("durably committed operation has a session")
                    };
                    if let Err(error) = session.append_record(SessionRecord::OperationSuspended {
                        operation_id: completion.operation_id,
                        reason: SuspensionReason::RoundBudgetReached(Box::new(suspension.clone())),
                    }) {
                        self.agent.record_storage_failure(
                            completion.operation_id,
                            "round-budget-suspension",
                        );
                        self.emit(AgentEvent::OperationFailed {
                            operation_id: completion.operation_id,
                            reason: format!("could not commit round-budget suspension: {error:#}"),
                        });
                        return;
                    }
                }
                self.suspended_round_budget = Some(suspension.clone());
                self.emit(AgentEvent::RoundBudgetReached { suspension });
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id: completion.operation_id,
                    state: OperationState::Suspended,
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

    fn round_budget_suspension(
        &self,
        operation_id: OperationId,
        last_tranche_rounds: usize,
        rounds_consumed: usize,
        continuations_used: usize,
        usage: AgentTurnUsage,
    ) -> RoundBudgetSuspension {
        let operation = self
            .session
            .as_ref()
            .and_then(|session| session.restored_operation(operation_id));
        let committed =
            operation
                .as_ref()
                .map_or_else(RoundBudgetCommitFacts::empty, |operation| {
                    RoundBudgetCommitFacts {
                        steps: usize_to_u32(operation.step_order.len()),
                        invocations: usize_to_u32(operation.invocation_order.len()),
                        results: usize_to_u32(operation.results.len()),
                    }
                });
        let repeated_tool_patterns = operation.as_ref().map_or(0, repeated_tool_pattern_count);
        let (rounds_consumed, remaining_rounds, allowed_actions) =
            root_round_budget(rounds_consumed);
        RoundBudgetSuspension {
            id: RoundBudgetId::new(),
            operation_id,
            soft_round_limit: usize_to_u32(self.agent.max_tool_rounds()),
            last_tranche_rounds: usize_to_u32(last_tranche_rounds),
            rounds_consumed: usize_to_u32(rounds_consumed),
            hard_round_limit: usize_to_u32(MAX_ROOT_TOOL_ROUNDS),
            remaining_rounds: usize_to_u32(remaining_rounds),
            continuations_used: usize_to_u32(continuations_used),
            committed,
            repeated_tool_patterns,
            usage,
            allowed_actions,
        }
    }

    async fn decide_round_budget(
        &mut self,
        operation_id: OperationId,
        suspension_id: RoundBudgetId,
        action: RoundBudgetAction,
    ) {
        let Some(suspension) = self.suspended_round_budget.clone() else {
            self.emit(AgentEvent::CommandRejected {
                reason: "no root operation is awaiting a round-budget decision".to_owned(),
            });
            return;
        };
        if suspension.operation_id != operation_id || suspension.id != suspension_id {
            self.emit(AgentEvent::CommandRejected {
                reason: format!(
                    "round-budget decision does not match suspended operation {} and suspension {}",
                    suspension.operation_id, suspension.id
                ),
            });
            return;
        }
        if !suspension.allowed_actions.contains(&action) {
            self.emit(AgentEvent::CommandRejected {
                reason: format!("round-budget action {action:?} is not allowed"),
            });
            return;
        }
        if self.active.is_some() {
            self.emit(AgentEvent::CommandRejected {
                reason: "cannot decide a round budget while a root operation is running".to_owned(),
            });
            return;
        }

        let prompt = if action == RoundBudgetAction::Continue {
            match self.prepare_turn_prompt() {
                Ok(prompt) => prompt,
                Err(reason) => {
                    self.emit(AgentEvent::CommandRejected { reason });
                    return;
                }
            }
        } else {
            None
        };
        let decision = RoundBudgetDecision {
            operation_id,
            suspension_id,
            action,
        };
        if let Some(session) = &mut self.session
            && let Err(error) = session.append_record(SessionRecord::RoundBudgetDecisionAppended {
                decision: decision.clone(),
            })
        {
            self.agent
                .record_storage_failure(operation_id, "round-budget-decision");
            self.emit(AgentEvent::CommandRejected {
                reason: format!("could not commit round-budget decision: {error:#}"),
            });
            return;
        }
        if let Err(error) = self
            .agent
            .observe_boundary(CrashSite::AfterRoundBudgetDecision)
        {
            self.suspended_round_budget = None;
            self.emit(AgentEvent::OperationFailed {
                operation_id,
                reason: format!("operation stopped after round-budget decision commit: {error:#}"),
            });
            return;
        }
        self.suspended_round_budget = None;
        self.emit(AgentEvent::RoundBudgetDecisionCommitted {
            decision: decision.clone(),
        });

        match action {
            RoundBudgetAction::Continue => {
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id,
                    state: OperationState::Running,
                });
                let round_limit = usize::try_from(suspension.remaining_rounds)
                    .unwrap_or(usize::MAX)
                    .min(self.agent.max_tool_rounds());
                self.spawn_tranche(
                    operation_id,
                    prompt,
                    self.history.len(),
                    usize::try_from(suspension.rounds_consumed).unwrap_or(usize::MAX),
                    usize::try_from(suspension.continuations_used)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    suspension.usage,
                    round_limit,
                );
            }
            RoundBudgetAction::Stop => {
                self.emit(AgentEvent::UsageObserved {
                    operation_id,
                    usage: suspension.usage,
                });
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id,
                    state: OperationState::Finished(OperationOutcome::Declined),
                });
            }
        }
    }

    async fn interrupt_active(&mut self) {
        if let Some(active) = self.active.take() {
            let ActiveOperation {
                operation_id,
                progress_committed,
                task,
                cleanup,
                ..
            } = active;
            task.abort();
            let _ = task.await;
            cleanup.drain().await;
            if progress_committed {
                if let Some(session) = &mut self.session {
                    let _ = session.append_record(SessionRecord::OperationSuspended {
                        operation_id,
                        reason: SuspensionReason::ProcessInterrupted,
                    });
                }
                self.emit(AgentEvent::OperationStateChanged {
                    operation_id,
                    state: OperationState::Suspended,
                });
                return;
            }
            self.emit(AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(OperationOutcome::Interrupted),
            });
        }
    }

    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    fn prepare_turn_prompt(&mut self) -> Result<Option<PromptSnapshot>, String> {
        self.prepare_turn_prompt_for("")
    }

    fn prepare_turn_prompt_for(&mut self, input: &str) -> Result<Option<PromptSnapshot>, String> {
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
        let mut snapshot = assembler
            .assemble_with_compaction(&sources, self.compaction_checkpoint.as_ref())
            .map_err(|error| format!("could not assemble turn prompt: {error}"))?;
        if let Some(owner) = &self.memory {
            let selection = owner
                .select_for_turn(input, snapshot.budget.total_tokens)
                .map_err(|error| format!("Memory context unavailable: {error:#}"))?;
            let (selected, ids) = snapshot.with_personal_memory(&selection);
            owner
                .record_selection(&selection, &ids)
                .map_err(|error| format!("Memory changed before turn admission: {error:#}"))?;
            snapshot = selected;
        }
        Ok(Some(snapshot))
    }

    async fn compact_now(
        &mut self,
        operation_id: OperationId,
        reason: CompactionReason,
    ) -> Result<CompactionCheckpoint, String> {
        // Failure to establish or revalidate source authority stops automatic
        // dispatch; only NothingToCompact may continue to the normal budget gate.
        self.compaction_cancelled = true;
        let budget = self
            .prompt_assembler
            .as_ref()
            .and_then(PromptAssembler::budget_plan)
            .cloned()
            .ok_or_else(|| {
                "managed or transient runtime owns context; Xana compaction is unavailable"
                    .to_owned()
            })?;
        let session = self.session.as_ref().ok_or_else(|| {
            "managed or transient runtime owns context; Xana compaction is unavailable".to_owned()
        })?;
        let mut candidate = session
            .prepare_compaction(operation_id, reason, &budget)
            .map_err(|error| {
                if error.downcast_ref::<CompactionError>()
                    == Some(&CompactionError::NothingToCompact)
                {
                    self.compaction_cancelled = false;
                    "conversation has no complete older turn to compact".to_owned()
                } else {
                    format!("could not commit durable compaction checkpoint: {error:#}")
                }
            })?;
        self.compaction_cancelled = false;
        if self.agent.semantic_compaction_enabled() {
            let cancellation = tokio_util::sync::CancellationToken::new();
            let agent = self.agent.clone();
            let enrichment = agent.enrich_compaction(&mut candidate, &cancellation);
            tokio::pin!(enrichment);
            loop {
                tokio::select! {
                    biased;
                    command = self.commands.recv() => match command {
                        Some(RuntimeCommand::InterruptOperation { operation_id: interrupted }) if interrupted == operation_id => {
                            self.compaction_cancelled = true;
                            cancellation.cancel();
                            let _ = enrichment.await;
                            return Err("compaction cancelled; the previous checkpoint and raw history are unchanged".into());
                        }
                        Some(RuntimeCommand::Shutdown) | None => {
                            self.stop_after_compaction = true;
                            cancellation.cancel();
                            let _ = enrichment.await;
                            return Err("compaction stopped; the previous checkpoint and raw history are unchanged".into());
                        }
                        Some(_) => self.emit(AgentEvent::CommandRejected { reason: "compaction is active; wait or cancel before changing this Conversation".into() }),
                    },
                    result = &mut enrichment => {
                        if result.is_err() {
                            self.emit(AgentEvent::CompactionUnavailable { operation_id, reason: "semantic helper unavailable, failed, or exceeded its allowance; using the deterministic checkpoint".into() });
                        }
                        break;
                    }
                }
            }
        }
        let session = self
            .session
            .as_mut()
            .expect("compaction session remains owned");
        let checkpoint = session.commit_compaction(candidate).map_err(|error| {
            self.compaction_cancelled = true;
            format!("could not commit compaction after source revalidation: {error:#}")
        })?;
        let continuation = session
            .prompt_continuation()
            .map_err(|error| format!("could not restore compacted continuation: {error:#}"))?;
        self.history = continuation.history;
        self.compaction_checkpoint = continuation.checkpoint;
        Ok(checkpoint)
    }

    fn handle_broker_event(&mut self, event: AgentEvent) {
        let committed = match &event {
            AgentEvent::OperationStateChanged {
                operation_id,
                state,
            } => {
                if let Some(session) = &mut self.session {
                    let record = match state {
                        OperationState::Suspended => Some(SessionRecord::OperationSuspended {
                            operation_id: *operation_id,
                            reason: SuspensionReason::Permission,
                        }),
                        OperationState::Running => Some(SessionRecord::OperationStateChanged {
                            operation_id: *operation_id,
                            state: OperationState::Running,
                        }),
                        OperationState::Finished(_) => None,
                    };
                    if let Some(record) = record
                        && let Err(error) = session.append_record(record)
                    {
                        self.emit(AgentEvent::OperationFailed {
                            operation_id: *operation_id,
                            reason: format!("could not commit operation state: {error:#}"),
                        });
                        return;
                    }
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
                            .store_tool_output(value)
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

impl RoundBudgetCommitFacts {
    const fn empty() -> Self {
        Self {
            steps: 0,
            invocations: 0,
            results: 0,
        }
    }
}

fn repeated_tool_pattern_count(operation: &crate::session::RestoredOperation) -> u32 {
    let mut patterns = BTreeMap::<String, u32>::new();
    for invocation_id in &operation.invocation_order {
        let Some(intent) = operation.intents.get(invocation_id) else {
            continue;
        };
        let Ok(pattern) = serde_json::to_string(&(&intent.target, &intent.final_arguments)) else {
            continue;
        };
        patterns
            .entry(pattern)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
    }
    patterns
        .values()
        .map(|count| count.saturating_sub(1))
        .fold(0, u32::saturating_add)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn root_round_budget(rounds_consumed: usize) -> (usize, usize, Vec<RoundBudgetAction>) {
    let rounds_consumed = rounds_consumed.min(MAX_ROOT_TOOL_ROUNDS);
    let remaining_rounds = MAX_ROOT_TOOL_ROUNDS.saturating_sub(rounds_consumed);
    let mut allowed_actions = vec![RoundBudgetAction::Stop];
    if remaining_rounds > 0 {
        allowed_actions.insert(0, RoundBudgetAction::Continue);
    }
    (rounds_consumed, remaining_rounds, allowed_actions)
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
