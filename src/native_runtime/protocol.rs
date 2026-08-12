use crate::{
    identity::{AgentId, OperationId, SessionId, StepId, ToolInvocationId},
    message::Message,
    operation::{InvocationIntent, InvocationResultRecord},
    orchestration::{
        ChildActivity, ChildAttribution, ChildCancellationReceipt, ChildInspection, ChildLifecycle,
        ChildReport,
    },
    permission::{ControllerDecision, PermissionAuditFact, PermissionRequest},
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum RuntimeCommand {
    SubmitTurn {
        operation_id: OperationId,
        input: String,
    },
    SubmitTurnWithImages {
        operation_id: OperationId,
        input: String,
        images: Vec<crate::vision::ImageRef>,
    },
    ClearConversation,
    ResumeOperation {
        session_id: SessionId,
        operation_id: OperationId,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum AgentEvent {
    OperationStateChanged {
        operation_id: OperationId,
        state: OperationState,
    },
    AssistantTextDelta {
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
    OperationFailed {
        operation_id: OperationId,
        reason: String,
    },
    ConversationCleared,
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
