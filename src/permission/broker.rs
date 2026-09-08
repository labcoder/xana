use super::{
    ControllerDecision, Evaluation, PermissionAuditFact, PermissionPolicy, PermissionRequest,
    PolicyDecision, policy::SessionGrants,
};
use crate::{
    identity::{OperationId, ToolInvocationId},
    native_runtime::{AgentEvent, AgentEventSender, OperationState},
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

type PermissionKey = (OperationId, ToolInvocationId);
const MAX_DENIALS: usize = 1024;

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
    public_web_turns: HashMap<OperationId, String>,
    // Exact prepared requests, not provider IDs or raw/default argument spellings.
    // Bounded for long-lived clients; exhaustion closes the Ask lane, not policy.
    denials: HashSet<blake3::Hash>,
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
    ReplacePolicy {
        policy: PermissionPolicy,
        reply: oneshot::Sender<bool>,
    },
    Authorize {
        request: Box<PermissionRequest>,
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
    OperationFinished(OperationId),
    Shutdown,
}

impl PermissionBroker {
    pub(crate) fn spawn(
        policy: PermissionPolicy,
        controller_present: bool,
        events: impl Into<AgentEventSender>,
    ) -> (PermissionBrokerHandle, JoinHandle<()>) {
        Self::spawn_with_audit_events(policy, controller_present, events.into(), true, [])
    }

    pub(crate) fn spawn_for_durable_runtime<'a>(
        policy: PermissionPolicy,
        controller_present: bool,
        events: impl Into<AgentEventSender>,
        committed: impl IntoIterator<Item = &'a PermissionAuditFact>,
    ) -> (PermissionBrokerHandle, JoinHandle<()>) {
        Self::spawn_with_audit_events(policy, controller_present, events.into(), false, committed)
    }

    fn spawn_with_audit_events<'a>(
        policy: PermissionPolicy,
        controller_present: bool,
        events: AgentEventSender,
        emit_audit_events: bool,
        committed: impl IntoIterator<Item = &'a PermissionAuditFact>,
    ) -> (PermissionBrokerHandle, JoinHandle<()>) {
        let mut denials = HashSet::new();
        for fact in committed {
            if fact.effective == PolicyDecision::Deny
                && fact.controller_decision == Some(ControllerDecision::Deny)
            {
                denials.insert(denial_key(&fact.request));
                if denials.len() == MAX_DENIALS {
                    break;
                }
            }
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        let broker = Self {
            policy,
            grants: SessionGrants::default(),
            public_web_turns: HashMap::new(),
            denials,
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
                BrokerCommand::ReplacePolicy { policy, reply } => {
                    let allowed = self.pending.is_empty();
                    if allowed {
                        self.policy = policy;
                        self.grants = SessionGrants::default();
                        self.public_web_turns.clear();
                    }
                    let _ = reply.send(allowed);
                }
                BrokerCommand::OperationFinished(operation) => {
                    self.public_web_turns.remove(&operation);
                }
                BrokerCommand::Authorize { request, reply } => {
                    self.authorize(*request, reply);
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
        if self.denials.contains(&denial_key(&request))
            || (matches!(evaluation, Evaluation::Ask { .. }) && self.denials.len() >= MAX_DENIALS)
        {
            self.finish_immediate(
                request,
                policy_evaluation,
                None,
                PolicyDecision::Deny,
                reply,
            );
            return;
        }
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
                // General tool authority is not outbound consent. Preserve an
                // independently selected web preference even in Allow mode.
                let outbound = self.public_web_decision(&request);
                self.finish_immediate(
                    request,
                    policy_evaluation,
                    outbound,
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
            Evaluation::Ask { .. } if self.public_web_decision(&request).is_some() => {
                self.finish_immediate(
                    request,
                    policy_evaluation,
                    Some(ControllerDecision::AllowPublicWebTurn),
                    PolicyDecision::Allow,
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

    fn public_web_decision(&self, request: &PermissionRequest) -> Option<ControllerDecision> {
        request
            .outbound_review
            .as_ref()
            .and_then(|review| review.public_web_scope())
            .filter(|web| {
                web.persisted_allow
                    || self.public_web_turns.get(&request.operation_id) == Some(&web.route)
            })
            .map(|_| ControllerDecision::AllowPublicWebTurn)
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
            && (scope != &pending.request.scope
                || matches!(
                    scope,
                    crate::permission::PermissionScope::PersonalMemory { .. }
                ))
        {
            return Err(DecisionError::ScopeMismatch {
                operation_id,
                invocation_id,
            });
        }
        if matches!(
            decision,
            ControllerDecision::SaveOutboundAllow | ControllerDecision::SaveOutboundDeny
        ) && !matches!(
            pending.request.scope,
            crate::permission::PermissionScope::External { .. }
        ) {
            return Err(DecisionError::ScopeMismatch {
                operation_id,
                invocation_id,
            });
        }

        if decision == ControllerDecision::AllowPublicWebTurn {
            let web = pending
                .request
                .outbound_review
                .as_ref()
                .and_then(|review| review.public_web_scope())
                .ok_or(DecisionError::ScopeMismatch {
                    operation_id,
                    invocation_id,
                })?;
            if self.public_web_turns.len() >= MAX_DENIALS
                && !self.public_web_turns.contains_key(&operation_id)
            {
                return Err(DecisionError::GrantLimit);
            }
            self.public_web_turns
                .insert(operation_id, web.route.clone());
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
            ControllerDecision::AllowPublicWebTurn => PolicyDecision::Allow,
            ControllerDecision::AllowSession { .. } => PolicyDecision::Allow,
            // These choices authorize only entry into the mandatory outbound
            // guard, which persists and enforces the exact send decision.
            ControllerDecision::SaveOutboundAllow | ControllerDecision::SaveOutboundDeny => {
                PolicyDecision::Allow
            }
        };
        if effective == PolicyDecision::Deny && self.denials.len() < MAX_DENIALS {
            self.denials.insert(denial_key(&pending.request));
        }
        let _ = self.events.send(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        });
        self.finish_immediate(
            pending.request,
            pending.policy_evaluation,
            Some(decision.clone()),
            effective,
            pending.reply,
        );
        if decision == ControllerDecision::AllowPublicWebTurn {
            // Concurrent web tools may already be waiting. Re-evaluate their
            // policy after this grant instead of showing redundant prompts.
            let keys = self
                .pending
                .iter()
                .filter_map(|(key, pending)| {
                    (key.0 == operation_id
                        && pending
                            .request
                            .outbound_review
                            .as_ref()
                            .and_then(|review| review.public_web_scope())
                            .is_some_and(|web| {
                                self.public_web_turns.get(&operation_id) == Some(&web.route)
                            }))
                    .then_some(*key)
                })
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(pending) = self.pending.remove(&key) {
                    self.authorize(pending.request, pending.reply);
                }
            }
        }
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

fn denial_key(request: &PermissionRequest) -> blake3::Hash {
    let mut value = serde_json::to_value((
        request.operation_id,
        &request.tool_name,
        request.effect_class,
        &request.scope,
        &request.final_arguments,
        &request.outbound_review,
    ))
    .expect("permission requests serialize");
    value.sort_all_objects();
    blake3::hash(&serde_json::to_vec(&value).expect("JSON values serialize"))
}

impl PermissionBrokerHandle {
    pub(crate) async fn replace_policy(
        &self,
        policy: PermissionPolicy,
    ) -> Result<(), BrokerUnavailable> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(BrokerCommand::ReplacePolicy { policy, reply })
            .map_err(|_| BrokerUnavailable)?;
        if receiver.await.map_err(|_| BrokerUnavailable)? {
            Ok(())
        } else {
            Err(BrokerUnavailable)
        }
    }
    pub(crate) async fn authorize(
        &self,
        request: PermissionRequest,
    ) -> Result<Authorization, BrokerUnavailable> {
        let operation_id = request.operation_id;
        let invocation_id = request.invocation_id;
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(BrokerCommand::Authorize {
                request: Box::new(request),
                reply,
            })
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

    pub(crate) fn operation_finished(&self, operation: OperationId) {
        let _ = self
            .commands
            .send(BrokerCommand::OperationFinished(operation));
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
