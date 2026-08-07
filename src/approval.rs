//! Requested-action data shared by tools and the foreground approval protocol.

use crate::identity::{OperationId, ToolInvocationId};
use std::{error::Error, fmt, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestedAction {
    pub(crate) tool_name: &'static str,
    pub(crate) shell: &'static str,
    pub(crate) command: String,
    pub(crate) argv: String,
    pub(crate) cwd: PathBuf,
}

impl fmt::Display for RequestedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} requests local host execution\nshell: {}\ncwd: {}\ncommand: {}\nargv: {}\nThis process uses Xana's ordinary host permissions; it is not contained.",
            self.tool_name,
            self.shell,
            self.cwd.display(),
            self.command,
            self.argv
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovalError {
    DuplicatePending {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    },
    ControllerUnavailable,
    DecisionChannelClosed,
}

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePending {
                operation_id,
                invocation_id,
            } => write!(
                f,
                "approval {operation_id}/{invocation_id} is already pending"
            ),
            Self::ControllerUnavailable => {
                write!(f, "no controlling client can receive the approval request")
            }
            Self::DecisionChannelClosed => {
                write!(f, "the approval decision channel closed without a decision")
            }
        }
    }
}

impl Error for ApprovalError {}
