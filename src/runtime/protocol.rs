use crate::{
    identity::{OperationId, StepId, ToolInvocationId},
    message::Message,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum RuntimeCommand {
    SubmitTurn {
        operation_id: OperationId,
        input: String,
    },
    ClearConversation,
    DecideProvisionalApproval {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        approved: bool,
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
    ProvisionalApprovalRequested {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        tool_name: String,
        action: String,
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
