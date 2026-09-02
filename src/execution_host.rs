//! Bounded application-owned coordination across local workspaces and Conversations.
//!
//! The execution host routes runtime-owned work without becoming another agent
//! loop. Filesystem-backed workspace identities define collision domains; one
//! short-held state lock protects admission and projection, while unrelated
//! Runs execute outside it. Frontends receive an atomic summary plus a bounded
//! ordered delta suffix and must request a fresh snapshot after any gap.

mod event_log;

#[cfg(test)]
mod tests;

use crate::{
    controller::{
        ControllerChange, ControllerChangeKind, ControllerClientId, ControllerGrant,
        ControllerLeaseError, ControllerLeaseSnapshot, ControllerLeases,
        ControllerTakeoverConfirmation,
    },
    frontend::{ClientEvent, ClientObservation},
    host_lifecycle::{
        GlobalNotice, GlobalNoticeKind, HostLifecycleState, OwnedExecutionCleanup, ShutdownPlan,
        ShutdownProof, ShutdownReceipt, ShutdownRun, ShutdownRunReceipt,
    },
    identity::OperationId,
    native_runtime::{OperationOutcome, OperationState},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost, WorkspaceHostError},
};
use event_log::{Changes, EventLog};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

pub(crate) const EXECUTION_HOST_PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_HOSTED_CONVERSATIONS: usize = 8;
pub(crate) const MAX_CONCURRENT_RUNS: usize = 4;
const MAX_LABEL_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunAccess {
    ReadOnly,
    WorkspaceWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteCollisionDecision {
    Reject,
    Acknowledge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostedConversationState {
    Idle,
    Running,
    Suspended,
    Completed,
    Failed,
    Declined,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnerRestoration {
    NativeDurableHistory,
    ManagedOpaqueThread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationRegistration {
    pub(crate) conversation: ConversationRef,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) profile: Option<String>,
    pub(crate) permission_mode: String,
}

impl ConversationRegistration {
    pub(crate) fn new(
        conversation: ConversationRef,
        connection: impl Into<String>,
        model: impl Into<String>,
        profile: Option<String>,
        permission_mode: impl Into<String>,
    ) -> Self {
        Self {
            conversation,
            connection: bounded_label(connection.into()),
            model: bounded_label(model.into()),
            profile: profile.map(bounded_label),
            permission_mode: bounded_label(permission_mode.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostedConversationSnapshot {
    pub(crate) conversation: ConversationRef,
    pub(crate) workspace_id: String,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) profile: Option<String>,
    pub(crate) permission_mode: String,
    pub(crate) state: HostedConversationState,
    pub(crate) active_operation: Option<OperationId>,
    pub(crate) pending_approvals: usize,
    pub(crate) activity_count: usize,
    pub(crate) last_error: Option<String>,
    pub(crate) last_outcome: Option<OperationOutcome>,
    pub(crate) controller: Option<ControllerLeaseSnapshot<ConversationRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostedWorkspaceSnapshot {
    pub(crate) workspace_id: String,
    pub(crate) display_name: String,
    pub(crate) conversation_count: usize,
    pub(crate) active_runs: usize,
    pub(crate) active_write_runs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecutionHostSnapshot {
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) workspaces: Vec<HostedWorkspaceSnapshot>,
    pub(crate) conversations: Vec<HostedConversationSnapshot>,
    pub(crate) attached: Option<ConversationRef>,
    pub(crate) active_runs: usize,
    pub(crate) lifecycle: HostLifecycleState,
    pub(crate) global_notices: Vec<GlobalNotice>,
    pub(crate) shutdown_receipt: Option<ShutdownReceipt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HostObservation {
    pub(crate) sequence: u64,
    pub(crate) event: HostEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum HostEvent {
    ConversationRegistered {
        conversation: ConversationRef,
        workspace_id: String,
    },
    RunStarted {
        conversation: ConversationRef,
        operation_id: OperationId,
        access: RunAccess,
        collision_acknowledged: bool,
    },
    RuntimeObservation {
        conversation: ConversationRef,
        observation: ClientEvent,
    },
    RunFinished {
        conversation: ConversationRef,
        operation_id: OperationId,
        state: HostedConversationState,
        error: Option<String>,
    },
    ConversationAttached {
        previous: Option<ConversationRef>,
        conversation: ConversationRef,
        restoration: OwnerRestoration,
    },
    ControllerChanged {
        conversation: ConversationRef,
        controller: Option<ControllerLeaseSnapshot<ConversationRef>>,
        change: ControllerChangeKind,
    },
    GlobalNotice {
        notice: GlobalNotice,
    },
    LifecycleChanged {
        state: HostLifecycleState,
    },
    ShutdownCompleted {
        receipt: ShutdownReceipt,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HostChanges {
    Events(Vec<HostObservation>),
    SnapshotRequired(ExecutionHostSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachReceipt {
    pub(crate) previous: Option<ConversationRef>,
    pub(crate) attached: ConversationRef,
    pub(crate) restoration: OwnerRestoration,
    pub(crate) sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostedRun {
    conversation: ConversationRef,
    operation_id: OperationId,
    generation: u64,
    access: RunAccess,
}

#[derive(Debug)]
pub(crate) enum ExecutionHostError {
    Limit {
        resource: &'static str,
        limit: usize,
    },
    UnknownConversation(ConversationRef),
    InvalidConversation(String),
    ConversationBusy(ConversationRef),
    WriteCollision {
        workspace: PathBuf,
    },
    StaleRun,
    NotAcceptingWork(HostLifecycleState),
    ShutdownIncomplete(&'static str),
    RuntimeGap {
        expected: u64,
        received: u64,
    },
    Workspace(WorkspaceHostError),
    Controller(ControllerLeaseError<ConversationRef>),
    State(String),
}

impl fmt::Display for ExecutionHostError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit { resource, limit } => {
                write!(output, "execution host {resource} limit is {limit}")
            }
            Self::UnknownConversation(conversation) => {
                write!(output, "unknown hosted Conversation {conversation}")
            }
            Self::InvalidConversation(reason) => write!(output, "invalid Conversation: {reason}"),
            Self::ConversationBusy(conversation) => {
                write!(output, "Conversation {conversation} has active work")
            }
            Self::WriteCollision { workspace } => write!(
                output,
                "another write-capable Run is active in {}; serialize the work, use a separate worktree, or explicitly acknowledge the collision risk",
                workspace.display()
            ),
            Self::StaleRun => output.write_str("Run handle is stale or already terminal"),
            Self::NotAcceptingWork(state) => {
                write!(
                    output,
                    "execution host is {state:?} and is not accepting work"
                )
            }
            Self::ShutdownIncomplete(reason) => {
                write!(output, "execution host shutdown is incomplete: {reason}")
            }
            Self::RuntimeGap { expected, received } => write!(
                output,
                "Conversation runtime sequence gap: expected {expected}, received {received}"
            ),
            Self::Workspace(error) => error.fmt(output),
            Self::Controller(error) => error.fmt(output),
            Self::State(reason) => write!(output, "execution host state is unavailable: {reason}"),
        }
    }
}

impl Error for ExecutionHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Workspace(error) => Some(error),
            Self::Controller(error) => Some(error),
            _ => None,
        }
    }
}

impl From<WorkspaceHostError> for ExecutionHostError {
    fn from(value: WorkspaceHostError) -> Self {
        Self::Workspace(value)
    }
}

impl From<ControllerLeaseError<ConversationRef>> for ExecutionHostError {
    fn from(value: ControllerLeaseError<ConversationRef>) -> Self {
        Self::Controller(value)
    }
}

#[derive(Clone)]
pub(crate) struct ExecutionHost {
    state: Arc<Mutex<HostState>>,
}

struct HostState {
    workspaces: BTreeMap<String, WorkspaceSlot>,
    conversations: BTreeMap<ConversationRef, ConversationSlot>,
    attached: Option<ConversationRef>,
    active_runs: usize,
    events: EventLog,
    controllers: ControllerLeases<ConversationRef>,
    lifecycle: HostLifecycleState,
    global_notices: std::collections::VecDeque<GlobalNotice>,
    shutdown_receipt: Option<ShutdownReceipt>,
}

struct WorkspaceSlot {
    host: WorkspaceHost,
    conversations: BTreeSet<ConversationRef>,
    active_runs: usize,
    active_write_runs: usize,
    domain_lease: Option<ActiveRootLease>,
}

struct ConversationSlot {
    registration: ConversationRegistration,
    workspace_id: String,
    state: HostedConversationState,
    active_run: Option<RunState>,
    next_generation: u64,
    runtime_sequence: u64,
    pending_approvals: usize,
    activity_count: usize,
    last_error: Option<String>,
    last_outcome: Option<OperationOutcome>,
}

struct RunState {
    operation_id: OperationId,
    generation: u64,
    access: RunAccess,
}

impl ExecutionHost {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(HostState {
                workspaces: BTreeMap::new(),
                conversations: BTreeMap::new(),
                attached: None,
                active_runs: 0,
                events: EventLog::new(),
                controllers: ControllerLeases::new(),
                lifecycle: HostLifecycleState::Running,
                global_notices: std::collections::VecDeque::new(),
                shutdown_receipt: None,
            })),
        }
    }

    pub(crate) fn register(
        &self,
        workspace: WorkspaceHost,
        registration: ConversationRegistration,
    ) -> Result<(), ExecutionHostError> {
        if matches!(
            registration.conversation,
            ConversationRef::NewNative | ConversationRef::NewManaged { .. }
        ) {
            return Err(ExecutionHostError::InvalidConversation(
                "host registration requires a durable native session or managed thread".to_owned(),
            ));
        }
        let workspace_id = workspace.workspace_id().to_owned();
        let conversation = registration.conversation.clone();
        let mut state = self.lock()?;
        require_admission(&state)?;
        if let Some(existing) = state.conversations.get(&conversation) {
            if existing.workspace_id == workspace_id && existing.registration == registration {
                return Ok(());
            }
            return Err(ExecutionHostError::InvalidConversation(format!(
                "{conversation} is already registered with different execution facts"
            )));
        }
        if state.conversations.len() >= MAX_HOSTED_CONVERSATIONS {
            record_global_notice(
                &mut state,
                GlobalNotice::new(GlobalNoticeKind::ResourcePressure, "conversation_limit"),
            );
            return Err(ExecutionHostError::Limit {
                resource: "Conversation",
                limit: MAX_HOSTED_CONVERSATIONS,
            });
        }
        state
            .workspaces
            .entry(workspace_id.clone())
            .or_insert_with(|| WorkspaceSlot {
                host: workspace,
                conversations: BTreeSet::new(),
                active_runs: 0,
                active_write_runs: 0,
                domain_lease: None,
            })
            .conversations
            .insert(conversation.clone());
        state.conversations.insert(
            conversation.clone(),
            ConversationSlot {
                registration,
                workspace_id: workspace_id.clone(),
                state: HostedConversationState::Idle,
                active_run: None,
                next_generation: 0,
                runtime_sequence: 0,
                pending_approvals: 0,
                activity_count: 0,
                last_error: None,
                last_outcome: None,
            },
        );
        state.events.push(HostEvent::ConversationRegistered {
            conversation,
            workspace_id,
        });
        Ok(())
    }

    pub(crate) fn begin_run(
        &self,
        conversation: &ConversationRef,
        operation_id: OperationId,
        access: RunAccess,
        collision: WriteCollisionDecision,
    ) -> Result<HostedRun, ExecutionHostError> {
        let mut state = self.lock()?;
        require_admission(&state)?;
        if state.active_runs >= MAX_CONCURRENT_RUNS {
            record_global_notice(
                &mut state,
                GlobalNotice::new(GlobalNoticeKind::ResourcePressure, "concurrent_run_limit")
                    .conversation(conversation.clone())
                    .operation(operation_id),
            );
            return Err(ExecutionHostError::Limit {
                resource: "concurrent Run",
                limit: MAX_CONCURRENT_RUNS,
            });
        }
        let workspace_id = state
            .conversations
            .get(conversation)
            .ok_or_else(|| ExecutionHostError::UnknownConversation(conversation.clone()))?
            .workspace_id
            .clone();
        if state
            .conversations
            .get(conversation)
            .is_some_and(|slot| slot.active_run.is_some())
        {
            return Err(ExecutionHostError::ConversationBusy(conversation.clone()));
        }
        let write_collision = access == RunAccess::WorkspaceWrite
            && state
                .workspaces
                .get(&workspace_id)
                .is_some_and(|workspace| workspace.active_write_runs > 0);
        if write_collision && collision == WriteCollisionDecision::Reject {
            let workspace = state.workspaces[&workspace_id].host.workspace().to_owned();
            return Err(ExecutionHostError::WriteCollision { workspace });
        }
        let workspace = state
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| ExecutionHostError::State("workspace registry diverged".to_owned()))?;
        if workspace.active_runs == 0 {
            workspace.domain_lease = Some(workspace.host.acquire_root(conversation.clone())?);
        }
        workspace.active_runs += 1;
        if access == RunAccess::WorkspaceWrite {
            workspace.active_write_runs += 1;
        }
        let slot = state.conversations.get_mut(conversation).ok_or_else(|| {
            ExecutionHostError::State("Conversation registry diverged".to_owned())
        })?;
        slot.next_generation = slot.next_generation.saturating_add(1);
        let generation = slot.next_generation;
        slot.state = HostedConversationState::Running;
        slot.active_run = Some(RunState {
            operation_id,
            generation,
            access,
        });
        slot.last_error = None;
        slot.last_outcome = None;
        state.active_runs += 1;
        state.events.push(HostEvent::RunStarted {
            conversation: conversation.clone(),
            operation_id,
            access,
            collision_acknowledged: write_collision,
        });
        Ok(HostedRun {
            conversation: conversation.clone(),
            operation_id,
            generation,
            access,
        })
    }

    pub(crate) fn record_runtime_observation(
        &self,
        conversation: &ConversationRef,
        observation: &ClientObservation,
    ) -> Result<(), ExecutionHostError> {
        let mut state = self.lock()?;
        let expected = {
            let slot = state
                .conversations
                .get_mut(conversation)
                .ok_or_else(|| ExecutionHostError::UnknownConversation(conversation.clone()))?;
            let expected = slot.runtime_sequence.saturating_add(1);
            if observation.sequence < expected {
                return Err(ExecutionHostError::RuntimeGap {
                    expected,
                    received: observation.sequence,
                });
            }
            // Embedded transports may shed only replaceable live deltas. Keep
            // the received critical suffix progressing while requiring the
            // frontend to refresh its projection after the reported gap.
            slot.runtime_sequence = observation.sequence;
            apply_client_event(slot, &observation.event);
            expected
        };
        state.events.push(HostEvent::RuntimeObservation {
            conversation: conversation.clone(),
            observation: observation.event.clone(),
        });
        if observation.sequence == expected {
            Ok(())
        } else {
            Err(ExecutionHostError::RuntimeGap {
                expected,
                received: observation.sequence,
            })
        }
    }

    pub(crate) fn finish_run(
        &self,
        run: HostedRun,
        outcome: Result<OperationOutcome, String>,
    ) -> Result<(), ExecutionHostError> {
        let mut state = self.lock()?;
        let workspace_id = state
            .conversations
            .get(&run.conversation)
            .ok_or_else(|| ExecutionHostError::UnknownConversation(run.conversation.clone()))?
            .workspace_id
            .clone();
        let slot = state
            .conversations
            .get_mut(&run.conversation)
            .ok_or_else(|| {
                ExecutionHostError::State("Conversation registry diverged".to_owned())
            })?;
        if !slot.active_run.as_ref().is_some_and(|active| {
            active.operation_id == run.operation_id
                && active.generation == run.generation
                && active.access == run.access
        }) {
            return Err(ExecutionHostError::StaleRun);
        }
        let runtime_error = slot.last_error.clone();
        let (terminal, recorded_outcome, error) = match outcome {
            Ok(OperationOutcome::Completed) => (
                HostedConversationState::Completed,
                Some(OperationOutcome::Completed),
                None,
            ),
            Ok(OperationOutcome::Failed) => (
                HostedConversationState::Failed,
                Some(OperationOutcome::Failed),
                runtime_error,
            ),
            Ok(OperationOutcome::Declined) => (
                HostedConversationState::Declined,
                Some(OperationOutcome::Declined),
                None,
            ),
            Ok(OperationOutcome::Interrupted) => (
                HostedConversationState::Interrupted,
                Some(OperationOutcome::Interrupted),
                None,
            ),
            Err(error) => (
                HostedConversationState::Failed,
                None,
                Some(bounded_label(error)),
            ),
        };
        slot.state = terminal;
        slot.active_run = None;
        slot.last_error = error.clone();
        slot.last_outcome = recorded_outcome;
        let workspace = state
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| ExecutionHostError::State("workspace registry diverged".to_owned()))?;
        workspace.active_runs = workspace.active_runs.saturating_sub(1);
        if run.access == RunAccess::WorkspaceWrite {
            workspace.active_write_runs = workspace.active_write_runs.saturating_sub(1);
        }
        if workspace.active_runs == 0 {
            workspace.domain_lease = None;
        }
        state.active_runs = state.active_runs.saturating_sub(1);
        state.events.push(HostEvent::RunFinished {
            conversation: run.conversation,
            operation_id: run.operation_id,
            state: terminal,
            error,
        });
        Ok(())
    }

    pub(crate) fn attach(
        &self,
        conversation: &ConversationRef,
    ) -> Result<AttachReceipt, ExecutionHostError> {
        let (workspace_id, expected_previous, revision) = {
            let state = self.lock()?;
            require_admission(&state)?;
            let slot = state
                .conversations
                .get(conversation)
                .ok_or_else(|| ExecutionHostError::UnknownConversation(conversation.clone()))?;
            if slot.active_run.is_some() {
                return Err(ExecutionHostError::ConversationBusy(conversation.clone()));
            }
            (
                slot.workspace_id.clone(),
                state.attached.clone(),
                state.events.sequence(),
            )
        };
        let restoration = self.validate_restoration(&workspace_id, conversation)?;
        let mut state = self.lock()?;
        require_admission(&state)?;
        if state.events.sequence() != revision || state.attached != expected_previous {
            return Err(ExecutionHostError::State(
                "attachment target changed during validation; retry from a fresh snapshot"
                    .to_owned(),
            ));
        }
        if state
            .conversations
            .get(conversation)
            .is_none_or(|slot| slot.active_run.is_some())
        {
            return Err(ExecutionHostError::ConversationBusy(conversation.clone()));
        }
        let previous = state.attached.replace(conversation.clone());
        let observation = state.events.push(HostEvent::ConversationAttached {
            previous: previous.clone(),
            conversation: conversation.clone(),
            restoration,
        });
        Ok(AttachReceipt {
            previous,
            attached: conversation.clone(),
            restoration,
            sequence: observation.sequence,
        })
    }

    pub(crate) fn acquire_controller(
        &self,
        conversation: &ConversationRef,
        client_id: ControllerClientId,
        takeover: Option<ControllerTakeoverConfirmation>,
        now: Instant,
    ) -> Result<ControllerGrant<ConversationRef>, ExecutionHostError> {
        let mut state = self.lock()?;
        require_admission(&state)?;
        let pending = state
            .conversations
            .get(conversation)
            .ok_or_else(|| ExecutionHostError::UnknownConversation(conversation.clone()))?
            .pending_approvals
            > 0;
        let grant =
            state
                .controllers
                .acquire(conversation.clone(), client_id, takeover, pending, now)?;
        record_controller_change(
            &mut state,
            ControllerChange {
                conversation: conversation.clone(),
                controller: Some(grant.snapshot.clone()),
                change: grant.change,
            },
        );
        Ok(grant)
    }

    // Multi-client detach will use this independently of whole-host shutdown.
    #[allow(dead_code)]
    pub(crate) fn release_controller(
        &self,
        conversation: &ConversationRef,
        client_id: ControllerClientId,
    ) -> Result<(), ExecutionHostError> {
        let mut state = self.lock()?;
        let change = state.controllers.release(conversation, client_id)?;
        record_controller_change(&mut state, change);
        Ok(())
    }

    pub(crate) fn require_controller(
        &self,
        conversation: &ConversationRef,
        client_id: ControllerClientId,
    ) -> Result<(), ExecutionHostError> {
        let state = self.lock()?;
        if !state.conversations.contains_key(conversation) {
            return Err(ExecutionHostError::UnknownConversation(
                conversation.clone(),
            ));
        }
        if state.controllers.is_controller(conversation, client_id) {
            Ok(())
        } else {
            Err(ControllerLeaseError::NotController(conversation.clone()).into())
        }
    }

    /// Stops admission and returns the exact work that its owners must cancel
    /// or await before presenting durable shutdown proof.
    pub(crate) fn request_shutdown(&self) -> Result<ShutdownPlan, ExecutionHostError> {
        let mut state = self.lock()?;
        if state.lifecycle == HostLifecycleState::Stopped {
            return Err(ExecutionHostError::NotAcceptingWork(state.lifecycle));
        }
        if state.lifecycle == HostLifecycleState::Running {
            set_lifecycle(&mut state, HostLifecycleState::Draining);
        }
        let active_runs = state
            .conversations
            .iter()
            .filter_map(|(conversation, slot)| {
                slot.active_run.as_ref().map(|run| ShutdownRun {
                    conversation: conversation.clone(),
                    operation_id: run.operation_id,
                })
            })
            .collect();
        let pending_approval_conversations = state
            .conversations
            .iter()
            .filter(|(_, slot)| slot.pending_approvals > 0)
            .map(|(conversation, _)| conversation.clone())
            .collect();
        for change in state.controllers.expire_all() {
            let conversation = change.conversation.clone();
            record_controller_change(&mut state, change);
            record_global_notice(
                &mut state,
                GlobalNotice::new(GlobalNoticeKind::ControllerLost, "host_shutdown")
                    .conversation(conversation),
            );
        }
        Ok(ShutdownPlan {
            active_runs,
            pending_approval_conversations,
        })
    }

    /// Finalizes shutdown only after the execution owner reports its durable
    /// flush and exact owned-process cleanup state. Remaining Runs become
    /// interrupted; recovery never turns them into successful completion.
    pub(crate) fn complete_shutdown(
        &self,
        proof: ShutdownProof,
    ) -> Result<ShutdownReceipt, ExecutionHostError> {
        let mut state = self.lock()?;
        if let Some(receipt) = &state.shutdown_receipt {
            return Ok(receipt.clone());
        }
        if state.lifecycle != HostLifecycleState::Draining {
            return Err(ExecutionHostError::ShutdownIncomplete(
                "request shutdown before completing it",
            ));
        }
        if !proof.durable_state_flushed {
            return Err(ExecutionHostError::ShutdownIncomplete(
                "durable state was not flushed",
            ));
        }
        if proof.owned_execution != OwnedExecutionCleanup::Clean {
            record_global_notice(
                &mut state,
                GlobalNotice::new(
                    GlobalNoticeKind::HostFailure,
                    "owned_execution_cleanup_unresolved",
                ),
            );
            return Err(ExecutionHostError::ShutdownIncomplete(
                "owned execution cleanup was not proven",
            ));
        }
        set_lifecycle(&mut state, HostLifecycleState::Persisting);
        let interrupted = state
            .conversations
            .iter_mut()
            .filter_map(|(conversation, slot)| {
                slot.pending_approvals = 0;
                let run = slot.active_run.take()?;
                slot.state = HostedConversationState::Interrupted;
                slot.last_error = None;
                slot.last_outcome = Some(OperationOutcome::Interrupted);
                Some((conversation.clone(), run.operation_id))
            })
            .collect::<Vec<_>>();
        for workspace in state.workspaces.values_mut() {
            workspace.active_runs = 0;
            workspace.active_write_runs = 0;
            workspace.domain_lease = None;
        }
        state.active_runs = 0;
        for (conversation, operation_id) in &interrupted {
            state.events.push(HostEvent::RunFinished {
                conversation: conversation.clone(),
                operation_id: *operation_id,
                state: HostedConversationState::Interrupted,
                error: None,
            });
        }
        set_lifecycle(&mut state, HostLifecycleState::Closing);
        let receipt = ShutdownReceipt {
            interrupted_runs: interrupted
                .into_iter()
                .map(|(conversation, operation_id)| ShutdownRunReceipt {
                    conversation,
                    operation_id,
                    status: "interrupted".to_owned(),
                })
                .collect(),
            durable_state_flushed: true,
            owned_execution: proof.owned_execution,
            unresolved_warnings: Vec::new(),
        };
        set_lifecycle(&mut state, HostLifecycleState::Stopped);
        state.shutdown_receipt = Some(receipt.clone());
        state.events.push(HostEvent::ShutdownCompleted {
            receipt: receipt.clone(),
        });
        Ok(receipt)
    }

    pub(crate) fn snapshot(&self) -> Result<ExecutionHostSnapshot, ExecutionHostError> {
        let state = self.lock()?;
        Ok(snapshot_from_state(&state))
    }

    pub(crate) fn changes_after(&self, cursor: u64) -> Result<HostChanges, ExecutionHostError> {
        let state = self.lock()?;
        Ok(
            match state
                .events
                .changes_after(cursor, || snapshot_from_state(&state))
            {
                Changes::Events(events) => HostChanges::Events(events),
                Changes::SnapshotRequired(snapshot) => HostChanges::SnapshotRequired(snapshot),
            },
        )
    }

    fn validate_restoration(
        &self,
        workspace_id: &str,
        conversation: &ConversationRef,
    ) -> Result<OwnerRestoration, ExecutionHostError> {
        let state = self.lock()?;
        let workspace = state
            .workspaces
            .get(workspace_id)
            .ok_or_else(|| ExecutionHostError::State("workspace registry diverged".to_owned()))?;
        match conversation {
            ConversationRef::Native { .. } => {
                workspace
                    .host
                    .conversation_history_page(conversation, None, 1)?;
                Ok(OwnerRestoration::NativeDurableHistory)
            }
            ConversationRef::Managed { .. } => {
                let present = workspace
                    .host
                    .snapshot()?
                    .conversations
                    .iter()
                    .any(|candidate| candidate.conversation == *conversation);
                if !present {
                    return Err(ExecutionHostError::InvalidConversation(
                        "managed owner no longer advertises the selected opaque thread".to_owned(),
                    ));
                }
                Ok(OwnerRestoration::ManagedOpaqueThread)
            }
            ConversationRef::NewNative | ConversationRef::NewManaged { .. } => {
                Err(ExecutionHostError::InvalidConversation(
                    "an unstarted Conversation has no owner state to restore".to_owned(),
                ))
            }
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, HostState>, ExecutionHostError> {
        self.state
            .lock()
            .map_err(|_| ExecutionHostError::State("coordination lock was poisoned".to_owned()))
    }
}

fn apply_client_event(slot: &mut ConversationSlot, event: &ClientEvent) {
    match event {
        ClientEvent::Runtime(event) => match event.as_ref() {
            crate::native_runtime::AgentEvent::PermissionRequested { .. } => {
                slot.pending_approvals = slot.pending_approvals.saturating_add(1);
            }
            crate::native_runtime::AgentEvent::PermissionAudited { .. } => {
                slot.pending_approvals = slot.pending_approvals.saturating_sub(1);
            }
            crate::native_runtime::AgentEvent::OperationStateChanged {
                state: OperationState::Suspended,
                ..
            } => slot.state = HostedConversationState::Suspended,
            crate::native_runtime::AgentEvent::OperationFailed { reason, .. } => {
                slot.last_error = Some(bounded_label(reason.clone()));
            }
            _ => slot.activity_count = slot.activity_count.saturating_add(1),
        },
        ClientEvent::Managed(_) | ClientEvent::Semantic(_) | ClientEvent::PayloadOmitted { .. } => {
            slot.activity_count = slot.activity_count.saturating_add(1);
        }
    }
}

fn snapshot_from_state(state: &HostState) -> ExecutionHostSnapshot {
    let now = Instant::now();
    let workspaces = state
        .workspaces
        .iter()
        .map(|(workspace_id, slot)| HostedWorkspaceSnapshot {
            workspace_id: workspace_id.clone(),
            display_name: workspace_display_name(slot.host.workspace()),
            conversation_count: slot.conversations.len(),
            active_runs: slot.active_runs,
            active_write_runs: slot.active_write_runs,
        })
        .collect();
    let conversations = state
        .conversations
        .iter()
        .map(|(conversation, slot)| HostedConversationSnapshot {
            conversation: conversation.clone(),
            workspace_id: slot.workspace_id.clone(),
            connection: slot.registration.connection.clone(),
            model: slot.registration.model.clone(),
            profile: slot.registration.profile.clone(),
            permission_mode: slot.registration.permission_mode.clone(),
            state: slot.state,
            active_operation: slot.active_run.as_ref().map(|run| run.operation_id),
            pending_approvals: slot.pending_approvals,
            activity_count: slot.activity_count,
            last_error: slot.last_error.clone(),
            last_outcome: slot.last_outcome,
            controller: state.controllers.snapshot(conversation, now),
        })
        .collect();
    ExecutionHostSnapshot {
        version: EXECUTION_HOST_PROTOCOL_VERSION,
        sequence: state.events.sequence(),
        workspaces,
        conversations,
        attached: state.attached.clone(),
        active_runs: state.active_runs,
        lifecycle: state.lifecycle,
        global_notices: state.global_notices.iter().cloned().collect(),
        shutdown_receipt: state.shutdown_receipt.clone(),
    }
}

fn require_admission(state: &HostState) -> Result<(), ExecutionHostError> {
    if state.lifecycle.accepts_work() {
        Ok(())
    } else {
        Err(ExecutionHostError::NotAcceptingWork(state.lifecycle))
    }
}

fn set_lifecycle(state: &mut HostState, lifecycle: HostLifecycleState) {
    state.lifecycle = lifecycle;
    state
        .events
        .push(HostEvent::LifecycleChanged { state: lifecycle });
    crate::diagnostics::emit(
        crate::diagnostics::DiagnosticFact::new(
            crate::config::DiagnosticLevel::Info,
            crate::config::DiagnosticTarget::Runtime,
            crate::diagnostics::EventKind::HostLifecycle,
            if lifecycle == HostLifecycleState::Stopped {
                crate::diagnostics::EventOutcome::Completed
            } else {
                crate::diagnostics::EventOutcome::Started
            },
        )
        .subject(format!("{lifecycle:?}").to_ascii_lowercase()),
    );
}

fn record_global_notice(state: &mut HostState, notice: GlobalNotice) {
    const MAX_GLOBAL_NOTICES: usize = 64;
    if state.global_notices.len() == MAX_GLOBAL_NOTICES {
        state.global_notices.pop_front();
    }
    state.global_notices.push_back(notice.clone());
    let level = match notice.kind {
        GlobalNoticeKind::HostFailure | GlobalNoticeKind::StorageFailure => {
            crate::config::DiagnosticLevel::Error
        }
        GlobalNoticeKind::ControllerLost
        | GlobalNoticeKind::RecoveryAction
        | GlobalNoticeKind::ResourcePressure => crate::config::DiagnosticLevel::Warn,
    };
    crate::diagnostics::emit(
        crate::diagnostics::DiagnosticFact::new(
            level,
            crate::config::DiagnosticTarget::Runtime,
            crate::diagnostics::EventKind::HostLifecycle,
            crate::diagnostics::EventOutcome::Unavailable,
        )
        .subject(&notice.code)
        .correlation(
            notice
                .operation_id
                .map_or_else(|| "host".to_owned(), |operation| operation.to_string()),
        ),
    );
    state.events.push(HostEvent::GlobalNotice { notice });
}

fn record_controller_change(state: &mut HostState, change: ControllerChange<ConversationRef>) {
    crate::diagnostics::emit(
        crate::diagnostics::DiagnosticFact::new(
            crate::config::DiagnosticLevel::Info,
            crate::config::DiagnosticTarget::Frontend,
            crate::diagnostics::EventKind::ControllerAuthorityChanged,
            controller_diagnostic_outcome(change.change),
        )
        .correlation(change.conversation.to_string())
        .subject(controller_change_label(change.change)),
    );
    state.events.push(HostEvent::ControllerChanged {
        conversation: change.conversation,
        controller: change.controller,
        change: change.change,
    });
}

fn controller_change_label(change: ControllerChangeKind) -> &'static str {
    match change {
        ControllerChangeKind::Acquired => "acquired",
        ControllerChangeKind::Renewed => "renewed",
        ControllerChangeKind::Reconnected => "reconnected",
        ControllerChangeKind::TakenOver { .. } => "taken_over",
        ControllerChangeKind::Disconnected { .. } => "disconnected",
        ControllerChangeKind::Released => "released",
        ControllerChangeKind::Expired => "expired",
    }
}

fn controller_diagnostic_outcome(change: ControllerChangeKind) -> crate::diagnostics::EventOutcome {
    match change {
        ControllerChangeKind::Acquired
        | ControllerChangeKind::Renewed
        | ControllerChangeKind::Reconnected
        | ControllerChangeKind::TakenOver { .. } => crate::diagnostics::EventOutcome::Completed,
        ControllerChangeKind::Disconnected { .. } => crate::diagnostics::EventOutcome::Unavailable,
        ControllerChangeKind::Released | ControllerChangeKind::Expired => {
            crate::diagnostics::EventOutcome::Cancelled
        }
    }
}

fn workspace_display_name(workspace: &Path) -> String {
    bounded_label(
        workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace")
            .to_owned(),
    )
}

fn bounded_label(mut value: String) -> String {
    if value.len() <= MAX_LABEL_BYTES {
        return value;
    }
    let mut end = MAX_LABEL_BYTES.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("...");
    value
}
