use super::{
    InterruptionReason, InvocationOutcome, InvocationResultRecord, InvocationTarget,
    RecoveryDecision, bounded_diagnostic,
};
use crate::{
    identity::{OperationId, ToolInvocationId, ToolResultId},
    message::{Message, ToolCall, ToolResult},
    native_runtime::{AgentEvent, OperationOutcome},
    permission::{Authorization, ControllerDecision, PermissionBrokerHandle, PermissionRequest},
    session::{DurableSession, RestoredOperation, SessionRecord},
    tool::{ReplaySafety, ToolRegistry},
};
use anyhow::{Context, Result, bail};
use std::{error::Error, fmt};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    AlreadyCompleted {
        result_id: ToolResultId,
    },
    ReplayExactInvocation {
        invocation_id: ToolInvocationId,
    },
    RecordInterruption {
        invocation_id: ToolInvocationId,
        result_id: ToolResultId,
        reason: InterruptionReason,
    },
    ContinueWithNextInvocation,
    FinishOperation,
}

pub(crate) async fn execute_recovery<C>(
    session: &mut DurableSession,
    operation_id: OperationId,
    tools: &ToolRegistry,
    permissions: &PermissionBrokerHandle,
    permission_events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    mut controller: C,
) -> Result<Vec<RecoveryAction>>
where
    C: FnMut(&PermissionRequest) -> Result<ControllerDecision>,
{
    let operation = session
        .restored_operation(operation_id)
        .context("operation does not exist in this session")?;
    let actions = plan_recovery(&operation, tools)?;
    for action in &actions {
        match action {
            RecoveryAction::AlreadyCompleted { .. }
            | RecoveryAction::ContinueWithNextInvocation => {}
            RecoveryAction::FinishOperation => {}
            RecoveryAction::RecordInterruption {
                invocation_id,
                result_id,
                reason,
            } => {
                let intent = operation
                    .intents
                    .get(invocation_id)
                    .context("recovery action references a missing intent")?;
                session.append_record(SessionRecord::RecoveryDecisionAppended {
                    operation_id,
                    invocation_id: *invocation_id,
                    decision: RecoveryDecision::Interrupt {
                        reason: reason.clone(),
                    },
                })?;
                append_recovery_result(
                    session,
                    intent,
                    InvocationOutcome::Interrupted {
                        reason: reason.clone(),
                    },
                    ToolResult::error(
                        intent.model_call_id.clone(),
                        format!(
                            "tool outcome is unknown and was not repeated ({reason:?}); manual reconciliation may be required"
                        ),
                    ),
                    *result_id,
                )?;
                break;
            }
            RecoveryAction::ReplayExactInvocation { invocation_id } => {
                let intent = operation
                    .intents
                    .get(invocation_id)
                    .context("recovery action references a missing intent")?;
                let InvocationTarget::Tool {
                    name,
                    contract_version,
                } = &intent.target
                else {
                    bail!("only tool invocations can be replayed")
                };
                let call = ToolCall {
                    id: intent.model_call_id.clone(),
                    name: name.clone(),
                    arguments: intent.final_arguments.clone(),
                };
                let planned = tools
                    .plan(&call, session.workspace_root())
                    .map_err(|result| anyhow::anyhow!(result.output))?;
                if planned.definition.contract_version != *contract_version
                    || planned.final_arguments() != &intent.final_arguments
                    || planned.scope() != &intent.permission.request.scope
                {
                    let reason = if planned.definition.contract_version != *contract_version {
                        InterruptionReason::ContractChanged
                    } else {
                        InterruptionReason::ScopeChanged
                    };
                    session.append_record(SessionRecord::RecoveryDecisionAppended {
                        operation_id,
                        invocation_id: *invocation_id,
                        decision: RecoveryDecision::Interrupt {
                            reason: reason.clone(),
                        },
                    })?;
                    append_recovery_result(
                        session,
                        intent,
                        InvocationOutcome::Interrupted {
                            reason: reason.clone(),
                        },
                        ToolResult::error(
                            intent.model_call_id.clone(),
                            format!("tool recovery was interrupted ({reason:?})"),
                        ),
                        intent.result_id,
                    )?;
                    break;
                }

                let authorization = authorize_current(
                    permissions,
                    permission_events,
                    planned.permission_request(operation_id, *invocation_id),
                    &mut controller,
                )
                .await?;
                let fact = match &authorization {
                    Authorization::Allowed(fact) | Authorization::Denied(fact) => fact.clone(),
                };
                session.append_record(SessionRecord::PermissionAudited { fact })?;
                if matches!(authorization, Authorization::Denied(_)) {
                    session.append_record(SessionRecord::RecoveryDecisionAppended {
                        operation_id,
                        invocation_id: *invocation_id,
                        decision: RecoveryDecision::Decline,
                    })?;
                    append_recovery_result(
                        session,
                        intent,
                        InvocationOutcome::Declined {
                            reason: "current permission denied replay".to_owned(),
                        },
                        ToolResult::error(
                            intent.model_call_id.clone(),
                            "current permission denied tool recovery",
                        ),
                        intent.result_id,
                    )?;
                    break;
                }

                session.append_record(SessionRecord::RecoveryDecisionAppended {
                    operation_id,
                    invocation_id: *invocation_id,
                    decision: RecoveryDecision::Replay,
                })?;
                let (tool_result, outcome) = match planned
                    .execute(crate::tool::ToolExecutionContext {
                        operation_id,
                        events: None,
                    })
                    .await
                {
                    Ok(output) => {
                        let value =
                            session.store_json_value(serde_json::Value::String(output.clone()))?;
                        (
                            ToolResult::success(intent.model_call_id.clone(), output),
                            InvocationOutcome::Completed { output: value },
                        )
                    }
                    Err(error) => (
                        ToolResult::error(intent.model_call_id.clone(), error.clone()),
                        InvocationOutcome::Failed {
                            error: bounded_diagnostic(error),
                        },
                    ),
                };
                append_recovery_result(session, intent, outcome, tool_result, intent.result_id)?;
                break;
            }
        }
    }

    // Recovery deliberately does not continue the provider turn. Once every known
    // effect is reconciled, the interrupted operation terminates explicitly.
    let outcome = OperationOutcome::Interrupted;
    session.append_record(SessionRecord::OperationFinished {
        operation_id,
        outcome,
    })?;
    Ok(actions)
}

fn append_recovery_result(
    session: &mut DurableSession,
    intent: &super::InvocationIntent,
    outcome: InvocationOutcome,
    tool_result: ToolResult,
    result_id: ToolResultId,
) -> Result<()> {
    let result = InvocationResultRecord {
        operation_id: intent.operation_id,
        invocation_id: intent.invocation_id,
        result_id,
        outcome,
    };
    let completed = match &result.outcome {
        InvocationOutcome::Completed { output } => Some(output.clone()),
        InvocationOutcome::Failed { .. }
        | InvocationOutcome::Declined { .. }
        | InvocationOutcome::Interrupted { .. } => None,
    };
    session.append_record(SessionRecord::InvocationResultAppended { result })?;
    if let Some(output) = completed {
        session.append_record(super::named_tool_output(intent, output))?;
    }
    session.append_message(Message::tool_result(tool_result))?;
    Ok(())
}

async fn authorize_current<C>(
    permissions: &PermissionBrokerHandle,
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    request: PermissionRequest,
    controller: &mut C,
) -> Result<Authorization>
where
    C: FnMut(&PermissionRequest) -> Result<ControllerDecision>,
{
    let mut authorization = Box::pin(permissions.authorize(request.clone()));
    loop {
        tokio::select! {
            result = &mut authorization => {
                return result.context("permission broker failed during recovery");
            }
            event = events.recv() => {
                let event = event.context("permission broker event channel closed during recovery")?;
                if let AgentEvent::PermissionRequested { request: pending } = event {
                    if pending.operation_id != request.operation_id
                        || pending.invocation_id != request.invocation_id
                    {
                        bail!("permission broker requested an unrelated recovery decision");
                    }
                    let decision = controller(&pending)?;
                    permissions
                        .decide(pending.operation_id, pending.invocation_id, decision)
                        .await
                        .context("could not submit recovery permission decision")?;
                }
            }
        }
    }
}

pub(crate) fn plan_recovery(
    operation: &RestoredOperation,
    current_tools: &ToolRegistry,
) -> Result<Vec<RecoveryAction>, RecoveryError> {
    if operation.finished.is_some() {
        return Err(RecoveryError::AlreadyFinished);
    }

    let mut actions = Vec::new();
    for (index, invocation_id) in operation.invocation_order.iter().enumerate() {
        let intent =
            operation
                .intents
                .get(invocation_id)
                .ok_or(RecoveryError::MissingIntentIndex {
                    invocation_id: *invocation_id,
                })?;
        if operation.results.contains_key(invocation_id) {
            actions.push(RecoveryAction::AlreadyCompleted {
                result_id: intent.result_id,
            });
            if index + 1 < operation.invocation_order.len() {
                actions.push(RecoveryAction::ContinueWithNextInvocation);
            }
            continue;
        }

        let action = match &intent.target {
            InvocationTarget::ContextTransform { .. } => RecoveryAction::RecordInterruption {
                invocation_id: *invocation_id,
                result_id: intent.result_id,
                reason: InterruptionReason::UnknownOutcome,
            },
            InvocationTarget::Tool {
                name,
                contract_version,
            } => match current_tools.definition(name) {
                None => RecoveryAction::RecordInterruption {
                    invocation_id: *invocation_id,
                    result_id: intent.result_id,
                    reason: InterruptionReason::MissingTool,
                },
                Some(definition) if definition.contract_version != *contract_version => {
                    RecoveryAction::RecordInterruption {
                        invocation_id: *invocation_id,
                        result_id: intent.result_id,
                        reason: InterruptionReason::ContractChanged,
                    }
                }
                Some(definition) => match (intent.saved_replay_safety, definition.replay_safety) {
                    (ReplaySafety::Safe, ReplaySafety::Safe) => {
                        RecoveryAction::ReplayExactInvocation {
                            invocation_id: *invocation_id,
                        }
                    }
                    (ReplaySafety::Never, _) => RecoveryAction::RecordInterruption {
                        invocation_id: *invocation_id,
                        result_id: intent.result_id,
                        reason: InterruptionReason::SavedReplayUnsafe,
                    },
                    (ReplaySafety::Safe, ReplaySafety::Never) => {
                        RecoveryAction::RecordInterruption {
                            invocation_id: *invocation_id,
                            result_id: intent.result_id,
                            reason: InterruptionReason::CurrentReplayUnsafe,
                        }
                    }
                },
            },
        };
        actions.push(action);
        return Ok(actions);
    }

    actions.push(RecoveryAction::FinishOperation);
    Ok(actions)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryError {
    AlreadyFinished,
    MissingIntentIndex { invocation_id: ToolInvocationId },
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "could not plan operation recovery: {self:?}")
    }
}

impl Error for RecoveryError {}
