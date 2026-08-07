use crate::{
    identity::{OperationId, SessionId, StepId, ToolInvocationId},
    message::Message,
    operation::{InvocationIntent, InvocationResultRecord},
    permission::{ControllerDecision, PermissionAuditFact, PermissionRequest},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum RuntimeCommand {
    SubmitTurn {
        operation_id: OperationId,
        input: String,
    },
    ClearConversation,
    ResumeOperation {
        session_id: SessionId,
        operation_id: OperationId,
    },
    DecidePermission {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
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
}
