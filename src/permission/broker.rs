use super::{
    ControllerDecision, Evaluation, PermissionAuditFact, PermissionPolicy, PermissionRequest,
    PolicyDecision, policy::SessionGrants,
};
use crate::{
    identity::{OperationId, ToolInvocationId},
    runtime::{AgentEvent, AgentEventSender, OperationState},
};
use std::{collections::HashMap, error::Error, fmt};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

type PermissionKey = (OperationId, ToolInvocationId);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Authorization {
    Allowed(PermissionAuditFact),
    Denied(PermissionAuditFact),
}

#[derive(Clone)]
pub(crate) struct PermissionBrokerHandle {
    commands: mpsc::UnboundedSender<BrokerCommand>,
}

pub(crate) struct PermissionBroker {
    policy: PermissionPolicy,
    grants: SessionGrants,
    pending: HashMap<PermissionKey, PendingRequest>,
    controller_present: bool,
    events: AgentEventSender,
    emit_audit_events: bool,
    commands: mpsc::UnboundedReceiver<BrokerCommand>,
}

struct PendingRequest {
    request: PermissionRequest,
    policy_evaluation: PolicyDecision,
    reply: oneshot::Sender<Authorization>,
}

enum BrokerCommand {
    Authorize {
        request: PermissionRequest,
        reply: oneshot::Sender<Authorization>,
    },
    Decide {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
        reply: oneshot::Sender<Result<(), DecisionError>>,
    },
    Cancel {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    },
    ControllerLost,
    Shutdown,
}

impl PermissionBroker {
    pub(crate) fn spawn(
        policy: PermissionPolicy,
        controller_present: bool,
        events: impl Into<AgentEventSender>,
    ) -> (PermissionBrokerHandle, JoinHandle<()>) {
        Self::spawn_with_audit_events(policy, controller_present, events.into(), true)
    }

    pub(crate) fn spawn_for_durable_runtime(
        policy: PermissionPolicy,
        controller_present: bool,
        events: impl Into<AgentEventSender>,
    ) -> (PermissionBrokerHandle, JoinHandle<()>) {
        Self::spawn_with_audit_events(policy, controller_present, events.into(), false)
    }

    fn spawn_with_audit_events(
        policy: PermissionPolicy,
        controller_present: bool,
        events: AgentEventSender,
        emit_audit_events: bool,
    ) -> (PermissionBrokerHandle, JoinHandle<()>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let broker = Self {
            policy,
            grants: SessionGrants::default(),
            pending: HashMap::new(),
            controller_present,
            events,
            emit_audit_events,
            commands: receiver,
        };
        let task = tokio::spawn(broker.run());
        (PermissionBrokerHandle { commands: sender }, task)
    }

    async fn run(mut self) {
        while let Some(command) = self.commands.recv().await {
            match command {
                BrokerCommand::Authorize { request, reply } => {
                    self.authorize(request, reply);
                }
                BrokerCommand::Decide {
                    operation_id,
                    invocation_id,
                    decision,
                    reply,
                } => {
                    let result = self.decide(operation_id, invocation_id, decision);
                    let _ = reply.send(result);
                }
                BrokerCommand::Cancel {
                    operation_id,
                    invocation_id,
                } => {
                    self.cancel((operation_id, invocation_id));
                }
                BrokerCommand::ControllerLost => {
                    self.controller_present = false;
                    self.deny_all_pending();
                }
                BrokerCommand::Shutdown => {
                    self.controller_present = false;
                    self.deny_all_pending();
                    return;
                }
            }
        }
        self.deny_all_pending();
    }

    fn authorize(&mut self, request: PermissionRequest, reply: oneshot::Sender<Authorization>) {
        let evaluation = self.policy.evaluate(&request, &self.grants);
        let policy_evaluation = evaluation.policy_decision();
        match evaluation {
            Evaluation::Denied { .. } => {
                self.finish_immediate(
                    request,
                    policy_evaluation,
                    None,
                    PolicyDecision::Deny,
                    reply,
                );
            }
            Evaluation::AllowedByPolicy { .. } | Evaluation::AllowedBySessionGrant { .. } => {
                self.finish_immediate(
                    request,
                    policy_evaluation,
                    None,
                    PolicyDecision::Allow,
                    reply,
                );
            }
            Evaluation::Ask { .. } if !self.controller_present => {
                self.finish_immediate(
                    request,
                    policy_evaluation,
                    None,
                    PolicyDecision::Deny,
                    reply,
                );
            }
            Evaluation::Ask { .. } => {
                let key = (request.operation_id, request.invocation_id);
                if self.pending.contains_key(&key) {
                    self.finish_immediate(
                        request,
                        policy_evaluation,
                        None,
                        PolicyDecision::Deny,
                        reply,
                    );
                    return;
                }
                self.pending.insert(
                    key,
                    PendingRequest {
                        request: request.clone(),
                        policy_evaluation,
                        reply,
                    },
                );
                let _ = self.events.send(AgentEvent::OperationStateChanged {
                    operation_id: request.operation_id,
                    state: OperationState::Suspended,
                });
                if self
                    .events
                    .send(AgentEvent::PermissionRequested { request })
                    .is_err()
                {
                    self.controller_present = false;
                    self.cancel(key);
                }
            }
        }
    }

    fn finish_immediate(
        &self,
        request: PermissionRequest,
        policy_evaluation: PolicyDecision,
        controller_decision: Option<ControllerDecision>,
        effective: PolicyDecision,
        reply: oneshot::Sender<Authorization>,
    ) {
        let fact = PermissionAuditFact {
            request,
            policy_evaluation,
            controller_decision,
            effective,
        };
        let authorization = if effective == PolicyDecision::Allow {
            Authorization::Allowed(fact.clone())
        } else {
            Authorization::Denied(fact.clone())
        };
        if self.emit_audit_events {
            let _ = self.events.send(AgentEvent::PermissionAudited { fact });
        }
        let _ = reply.send(authorization);
    }

    fn decide(
        &mut self,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    ) -> Result<(), DecisionError> {
        let key = (operation_id, invocation_id);
        let Some(pending) = self.pending.get(&key) else {
            return Err(DecisionError::Unknown {
                operation_id,
                invocation_id,
            });
        };
        if let ControllerDecision::AllowSession { scope } = &decision
            && scope != &pending.request.scope
        {
            return Err(DecisionError::ScopeMismatch {
                operation_id,
                invocation_id,
            });
        }

        if let ControllerDecision::AllowSession { scope } = &decision {
            self.grants
                .insert(&pending.request, scope.clone())
                .map_err(|()| DecisionError::GrantLimit)?;
        }

        let pending = self
            .pending
            .remove(&key)
            .expect("pending permission was checked immediately before removal");
        let effective = match &decision {
            ControllerDecision::Deny => PolicyDecision::Deny,
            ControllerDecision::AllowOnce => PolicyDecision::Allow,
            ControllerDecision::AllowSession { .. } => PolicyDecision::Allow,
        };
        let _ = self.events.send(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        });
        self.finish_immediate(
            pending.request,
            pending.policy_evaluation,
            Some(decision),
            effective,
            pending.reply,
        );
        Ok(())
    }

    fn cancel(&mut self, key: PermissionKey) {
        if let Some(pending) = self.pending.remove(&key) {
            self.finish_immediate(
                pending.request,
                pending.policy_evaluation,
                None,
                PolicyDecision::Deny,
                pending.reply,
            );
        }
    }

    fn deny_all_pending(&mut self) {
        let pending = self
            .pending
            .drain()
            .map(|(_, pending)| pending)
            .collect::<Vec<_>>();
        for pending in pending {
            self.finish_immediate(
                pending.request,
                pending.policy_evaluation,
                None,
                PolicyDecision::Deny,
                pending.reply,
            );
        }
    }
}

impl PermissionBrokerHandle {
    pub(crate) async fn authorize(
        &self,
        request: PermissionRequest,
    ) -> Result<Authorization, BrokerUnavailable> {
        let operation_id = request.operation_id;
        let invocation_id = request.invocation_id;
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(BrokerCommand::Authorize { request, reply })
            .map_err(|_| BrokerUnavailable)?;
        let mut guard = CancellationGuard {
            commands: self.commands.clone(),
            operation_id,
            invocation_id,
            armed: true,
        };
        let authorization = receiver.await.map_err(|_| BrokerUnavailable)?;
        guard.armed = false;
        Ok(authorization)
    }

    pub(crate) async fn decide(
        &self,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    ) -> Result<(), DecisionError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(BrokerCommand::Decide {
                operation_id,
                invocation_id,
                decision,
                reply,
            })
            .map_err(|_| DecisionError::BrokerUnavailable)?;
        receiver
            .await
            .map_err(|_| DecisionError::BrokerUnavailable)?
    }

    pub(crate) fn controller_lost(&self) {
        let _ = self.commands.send(BrokerCommand::ControllerLost);
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.commands.send(BrokerCommand::Shutdown);
    }
}

struct CancellationGuard {
    commands: mpsc::UnboundedSender<BrokerCommand>,
    operation_id: OperationId,
    invocation_id: ToolInvocationId,
    armed: bool,
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.commands.send(BrokerCommand::Cancel {
                operation_id: self.operation_id,
                invocation_id: self.invocation_id,
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrokerUnavailable;

impl fmt::Display for BrokerUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "permission broker is unavailable")
    }
}

impl Error for BrokerUnavailable {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecisionError {
    Unknown {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    },
    ScopeMismatch {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    },
    BrokerUnavailable,
    GrantLimit,
}

impl fmt::Display for DecisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown {
                operation_id,
                invocation_id,
            } => write!(
                f,
                "permission decision {operation_id}/{invocation_id} does not match pending work"
            ),
            Self::ScopeMismatch {
                operation_id,
                invocation_id,
            } => write!(
                f,
                "session permission {operation_id}/{invocation_id} must use the requested scope exactly"
            ),
            Self::BrokerUnavailable => write!(f, "permission broker is unavailable"),
            Self::GrantLimit => write!(f, "session permission grant limit has been reached"),
        }
    }
}

impl Error for DecisionError {}
