use crate::{
    identity::{AgentId, OperationId, RoundBudgetId, SessionId, StepId, ToolInvocationId},
    message::Message,
    operation::{InvocationIntent, InvocationResultRecord},
    orchestration::{
        ChildActivity, ChildAttribution, ChildCancellationReceipt, ChildInspection, ChildLifecycle,
        ChildReport,
    },
    permission::{ControllerDecision, PermissionAuditFact, PermissionRequest},
    prompt::PromptPlanLedger,
    session::{CompactionCheckpoint, CompactionReason},
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum RuntimeCommand {
    BrowserControl {
        action: crate::browser::BrowserControl,
    },
    SubmitTurn {
        operation_id: OperationId,
        input: String,
    },
    SubmitFiniteTurn {
        operation_id: OperationId,
        input: String,
        kind: crate::completion_evidence::WorkKind,
        contract: crate::completion_evidence::CompletionContract,
    },
    SubmitTurnWithImages {
        operation_id: OperationId,
        input: String,
        images: Vec<crate::vision::ImageRef>,
    },
    ClearConversation,
    CompactConversation {
        operation_id: OperationId,
    },
    ResumeOperation {
        session_id: SessionId,
        operation_id: OperationId,
    },
    DecideRoundBudget {
        operation_id: OperationId,
        suspension_id: RoundBudgetId,
        action: RoundBudgetAction,
    },
    InterruptOperation {
        operation_id: OperationId,
    },
    SteerOperation {
        operation_id: OperationId,
        input: String,
    },
    DecidePermission {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    DecideChildPermission {
        agent_id: AgentId,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    ListChildren,
    InspectChild {
        agent_id: AgentId,
    },
    CancelChild {
        agent_id: AgentId,
    },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OperationOutcome {
    Completed,
    Failed,
    Declined,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OperationState {
    Running,
    Suspended,
    Finished(OperationOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RoundBudgetAction {
    Continue,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoundBudgetCommitFacts {
    pub(crate) steps: u32,
    pub(crate) invocations: u32,
    pub(crate) results: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoundBudgetSuspension {
    pub(crate) id: RoundBudgetId,
    pub(crate) operation_id: OperationId,
    pub(crate) soft_round_limit: u32,
    pub(crate) last_tranche_rounds: u32,
    pub(crate) rounds_consumed: u32,
    pub(crate) hard_round_limit: u32,
    pub(crate) remaining_rounds: u32,
    pub(crate) continuations_used: u32,
    pub(crate) committed: RoundBudgetCommitFacts,
    pub(crate) repeated_tool_patterns: u32,
    pub(crate) usage: crate::agent::AgentTurnUsage,
    pub(crate) allowed_actions: Vec<RoundBudgetAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoundBudgetDecision {
    pub(crate) operation_id: OperationId,
    pub(crate) suspension_id: RoundBudgetId,
    pub(crate) action: RoundBudgetAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum AgentEvent {
    CompletionEvidenceRecorded {
        operation_id: OperationId,
        evidence: crate::completion_evidence::CompletionEvidence,
    },
    TerminalDiagnostic {
        diagnostic: crate::failure::TerminalDiagnostic,
    },
    BrowserStatus {
        detail: String,
    },
    UserMessageCommitted {
        operation_id: OperationId,
        message: Message,
    },
    OperationStateChanged {
        operation_id: OperationId,
        state: OperationState,
    },
    AssistantTextDelta {
        operation_id: OperationId,
        step_id: StepId,
        text: String,
    },
    ProviderReasoningDelta {
        operation_id: OperationId,
        step_id: StepId,
        text: String,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    PermissionAudited {
        fact: PermissionAuditFact,
    },
    InvocationIntentCommitted {
        intent: InvocationIntent,
    },
    InvocationResultCommitted {
        result: InvocationResultRecord,
    },
    ToolFinished {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        result: Message,
    },
    AssistantMessage {
        operation_id: OperationId,
        message: Message,
    },
    UsageObserved {
        operation_id: OperationId,
        usage: crate::agent::AgentTurnUsage,
    },
    RoundBudgetReached {
        suspension: RoundBudgetSuspension,
    },
    RoundBudgetDecisionCommitted {
        decision: RoundBudgetDecision,
    },
    OperationFailed {
        operation_id: OperationId,
        reason: String,
    },
    ConversationCleared,
    PromptPlanUpdated {
        operation_id: OperationId,
        ledger: PromptPlanLedger,
    },
    CompactionStarted {
        operation_id: OperationId,
        reason: CompactionReason,
    },
    ConversationCompacted {
        checkpoint: CompactionCheckpoint,
    },
    CompactionUnavailable {
        operation_id: OperationId,
        reason: String,
    },
    CommandRejected {
        reason: String,
    },
    ChildLifecycleChanged {
        attribution: ChildAttribution,
        lifecycle: ChildLifecycle,
    },
    ChildActivity {
        attribution: ChildAttribution,
        activity: ChildActivity,
    },
    ChildReportCommitted {
        report: ChildReport,
    },
    ChildListSnapshot {
        children: Vec<ChildInspection>,
    },
    ChildInspectionSnapshot {
        child: Box<ChildInspection>,
    },
    ChildCancellationRequested {
        receipt: ChildCancellationReceipt,
    },
    ExternalAgentActivity {
        operation_id: OperationId,
        activity: crate::a2a::ExternalAgentActivity,
    },
}

impl AgentEvent {
    /// Live progress that may be omitted when an observer falls behind because
    /// an authoritative completion, failure, or report follows it.
    pub(crate) fn is_replaceable_observation(&self) -> bool {
        match self {
            Self::AssistantTextDelta { .. }
            | Self::ProviderReasoningDelta { .. }
            | Self::ChildActivity {
                activity: ChildActivity::AssistantTextDelta { .. },
                ..
            } => true,
            Self::ChildActivity {
                activity: ChildActivity::ProviderReasoningDelta { .. },
                ..
            } => true,
            Self::ChildActivity {
                activity: ChildActivity::ManagedRuntime { notification },
                ..
            } => matches!(
                notification,
                crate::managed::codex::ManagedNotification::AssistantDelta { .. }
                    | crate::managed::codex::ManagedNotification::ReasoningSummaryDelta { .. }
                    | crate::managed::codex::ManagedNotification::ReasoningDelta { .. }
                    | crate::managed::codex::ManagedNotification::PlanDelta { .. }
                    | crate::managed::codex::ManagedNotification::CommandOutputDelta { .. }
                    | crate::managed::codex::ManagedNotification::DiffUpdated(_)
                    | crate::managed::codex::ManagedNotification::TokenUsageUpdated { .. }
            ),
            _ => false,
        }
    }
}

#[derive(Clone)]
pub(crate) struct AgentEventSender {
    transport: AgentEventTransport,
}

#[derive(Clone)]
enum AgentEventTransport {
    Unbounded(mpsc::UnboundedSender<AgentEvent>),
    Child {
        observations: mpsc::Sender<AgentEvent>,
        controls: mpsc::UnboundedSender<AgentEvent>,
        dropped: DroppedAgentEvents,
    },
}

#[derive(Clone, Default)]
pub(crate) struct DroppedAgentEvents(Arc<AtomicUsize>);

impl AgentEventSender {
    pub(crate) fn child(
        observation_capacity: usize,
    ) -> (
        Self,
        mpsc::Receiver<AgentEvent>,
        mpsc::UnboundedReceiver<AgentEvent>,
        DroppedAgentEvents,
    ) {
        let (observations, observation_receiver) = mpsc::channel(observation_capacity);
        let (controls, control_receiver) = mpsc::unbounded_channel();
        let dropped = DroppedAgentEvents::default();
        (
            Self {
                transport: AgentEventTransport::Child {
                    observations,
                    controls,
                    dropped: dropped.clone(),
                },
            },
            observation_receiver,
            control_receiver,
            dropped,
        )
    }

    pub(crate) fn send(&self, event: AgentEvent) -> Result<(), ()> {
        match &self.transport {
            AgentEventTransport::Unbounded(sender) => sender.send(event).map_err(|_| ()),
            AgentEventTransport::Child {
                observations,
                controls,
                dropped,
            } => {
                if matches!(event, AgentEvent::PermissionRequested { .. }) {
                    return controls.send(event).map_err(|_| ());
                }
                match observations.try_send(event) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        dropped.0.fetch_add(1, Ordering::Relaxed);
                        Err(())
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
                }
            }
        }
    }
}

impl DroppedAgentEvents {
    pub(crate) fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

impl From<mpsc::UnboundedSender<AgentEvent>> for AgentEventSender {
    fn from(sender: mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self {
            transport: AgentEventTransport::Unbounded(sender),
        }
    }
}
