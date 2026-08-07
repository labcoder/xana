//! Correlated, policy-free transport for the temporary command approval.

use super::{AgentEvent, OperationState};
use crate::{
    approval::{ApprovalError, RequestedAction},
    identity::{OperationId, ToolInvocationId},
};
use std::{
    collections::{HashMap, hash_map::Entry},
    error::Error,
    fmt,
    sync::Mutex,
};
use tokio::sync::{mpsc, oneshot};

type ApprovalKey = (OperationId, ToolInvocationId);

pub(crate) struct ProvisionalApprovalCoordinator {
    pending: Mutex<HashMap<ApprovalKey, oneshot::Sender<bool>>>,
    events: mpsc::UnboundedSender<AgentEvent>,
}

impl ProvisionalApprovalCoordinator {
    pub(crate) fn new(events: mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            events,
        }
    }

    pub(crate) async fn request(
        &self,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        action: &RequestedAction,
    ) -> Result<bool, ApprovalError> {
        let key = (operation_id, invocation_id);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match pending.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(sender);
                }
                Entry::Occupied(_) => {
                    return Err(ApprovalError::DuplicatePending {
                        operation_id,
                        invocation_id,
                    });
                }
            }
        }
        let _guard = PendingGuard {
            coordinator: self,
            key,
        };

        self.emit(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Suspended,
        });
        if self
            .events
            .send(AgentEvent::ProvisionalApprovalRequested {
                operation_id,
                invocation_id,
                tool_name: action.tool_name.to_owned(),
                action: action.to_string(),
            })
            .is_err()
        {
            return Err(ApprovalError::ControllerUnavailable);
        }

        let approved = receiver
            .await
            .map_err(|_| ApprovalError::DecisionChannelClosed)?;
        self.emit(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        });
        Ok(approved)
    }

    pub(crate) fn decide(
        &self,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        approved: bool,
    ) -> Result<(), ApprovalDecisionError> {
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(operation_id, invocation_id))
            .ok_or(ApprovalDecisionError::Unknown {
                operation_id,
                invocation_id,
            })?;
        sender
            .send(approved)
            .map_err(|_| ApprovalDecisionError::Stale {
                operation_id,
                invocation_id,
            })
    }

    pub(crate) fn deny_all(&self) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, sender)| sender)
            .collect::<Vec<_>>();
        for sender in pending {
            let _ = sender.send(false);
        }
    }

    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

struct PendingGuard<'a> {
    coordinator: &'a ProvisionalApprovalCoordinator,
    key: ApprovalKey,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.coordinator
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecisionError {
    Unknown {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    },
    Stale {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    },
}

impl fmt::Display for ApprovalDecisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown {
                operation_id,
                invocation_id,
            } => write!(
                f,
                "approval decision {operation_id}/{invocation_id} does not match pending work"
            ),
            Self::Stale {
                operation_id,
                invocation_id,
            } => write!(
                f,
                "approval decision {operation_id}/{invocation_id} arrived after its waiter closed"
            ),
        }
    }
}

impl Error for ApprovalDecisionError {}
