//! Durable tool-invocation records, execution ordering, and recovery planning.

mod recovery;

pub(crate) use recovery::{RecoveryAction, execute_recovery, plan_recovery};

use crate::{
    artifact::ArtifactRef,
    identity::{ContextId, NamedValueId, OperationId, StepId, ToolInvocationId, ToolResultId},
    message::{ToolCall, ToolResult},
    native_runtime::AgentEvent,
    permission::{Authorization, PermissionAuditFact, PermissionBrokerHandle},
    session::SessionRecord,
    tool::{ReplaySafety, ToolRegistry},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tokio::sync::{mpsc, oneshot};

pub(crate) const TOOL_CONTRACT_VERSION: u32 = 1;
pub(crate) const MAX_INLINE_VALUE_BYTES: usize = 32 * 1024;
pub(crate) const MAX_OPERATION_DIAGNOSTIC_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InvocationIntent {
    pub(crate) operation_id: OperationId,
    pub(crate) step_id: StepId,
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) result_id: ToolResultId,
    pub(crate) model_call_id: String,
    pub(crate) target: InvocationTarget,
    pub(crate) final_arguments: Value,
    pub(crate) permission: PermissionAuditFact,
    pub(crate) saved_replay_safety: ReplaySafety,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum InvocationTarget {
    Tool { name: String, contract_version: u32 },
    ContextTransform { operation: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InvocationResultRecord {
    pub(crate) operation_id: OperationId,
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) result_id: ToolResultId,
    pub(crate) outcome: InvocationOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum InvocationOutcome {
    Completed { output: DurableValueRef },
    Failed { error: String },
    Declined { reason: String },
    Interrupted { reason: InterruptionReason },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum DurableValueRef {
    InlineJson(Value),
    Artifact(ArtifactRef),
    Context { id: ContextId, version: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SuspensionReason {
    Permission,
    ProcessInterrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InterruptionReason {
    SavedReplayUnsafe,
    CurrentReplayUnsafe,
    MissingTool,
    ContractChanged,
    ScopeChanged,
    ReauthorizationDenied,
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryDecision {
    Replay,
    Interrupt { reason: InterruptionReason },
    Decline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NamedValueRecord {
    pub(crate) id: NamedValueId,
    pub(crate) operation_id: OperationId,
    pub(crate) name: String,
    pub(crate) value: DurableValueRef,
}

pub(crate) fn bounded_diagnostic(value: impl Into<String>) -> String {
    let value = value.into();
    let mut bounded = String::new();
    for character in value.chars() {
        if bounded.len() + character.len_utf8() > MAX_OPERATION_DIAGNOSTIC_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

#[derive(Clone)]
pub(crate) struct DurableOperationSender {
    sender: mpsc::UnboundedSender<DurableOperationCommand>,
}

pub(crate) enum DurableOperationCommand {
    Append {
        record: Box<SessionRecord>,
        event: Option<Box<AgentEvent>>,
        acknowledged: oneshot::Sender<Result<(), String>>,
    },
    StoreJson {
        value: Value,
        acknowledged: oneshot::Sender<Result<DurableValueRef, String>>,
    },
}

impl DurableOperationSender {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<DurableOperationCommand>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }

    pub(crate) async fn append(
        &self,
        record: SessionRecord,
        event: Option<AgentEvent>,
    ) -> Result<()> {
        let (acknowledged, acknowledgement) = oneshot::channel();
        self.sender
            .send(DurableOperationCommand::Append {
                record: Box::new(record),
                event: event.map(Box::new),
                acknowledged,
            })
            .map_err(|_| anyhow::anyhow!("durable operation writer is unavailable"))?;
        acknowledgement
            .await
            .map_err(|_| anyhow::anyhow!("durable operation writer dropped its reply"))?
            .map_err(anyhow::Error::msg)
    }

    async fn store_json(&self, value: Value) -> Result<DurableValueRef> {
        let (acknowledged, acknowledgement) = oneshot::channel();
        self.sender
            .send(DurableOperationCommand::StoreJson {
                value,
                acknowledged,
            })
            .map_err(|_| anyhow::anyhow!("durable value writer is unavailable"))?;
        acknowledgement
            .await
            .map_err(|_| anyhow::anyhow!("durable value writer dropped its reply"))?
            .map_err(anyhow::Error::msg)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // Shared prefix names exact after-commit/effect boundaries.
pub(crate) enum CrashSite {
    AfterOperationAccepted,
    AfterStepStarted,
    AfterPermissionAudit,
    AfterInvocationIntent,
    AfterEffectBeforeResult,
    AfterInvocationResult,
    AfterConversationResult,
}

pub(crate) trait BoundaryObserver: Send + Sync {
    fn reached(&self, site: CrashSite) -> Result<()>;
}

#[derive(Debug, Default)]
pub(crate) struct NoopBoundaryObserver;

impl BoundaryObserver for NoopBoundaryObserver {
    fn reached(&self, _site: CrashSite) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct OperationExecutor<'a> {
    tools: &'a ToolRegistry,
    workspace_root: &'a Path,
    permissions: PermissionBrokerHandle,
    commits: DurableOperationSender,
    observer: Arc<dyn BoundaryObserver>,
}

impl<'a> OperationExecutor<'a> {
    pub(crate) fn new(
        tools: &'a ToolRegistry,
        workspace_root: &'a Path,
        permissions: PermissionBrokerHandle,
        commits: DurableOperationSender,
        observer: Arc<dyn BoundaryObserver>,
    ) -> Self {
        Self {
            tools,
            workspace_root,
            permissions,
            commits,
            observer,
        }
    }

    pub(crate) async fn invoke_tool(
        &self,
        operation_id: OperationId,
        step_id: StepId,
        invocation_id: ToolInvocationId,
        call: ToolCall,
    ) -> Result<ToolResult> {
        let planned = match self.tools.plan(&call, self.workspace_root) {
            Ok(planned) => planned,
            Err(result) => return Ok(result),
        };
        if planned.definition.contract_version == 0 {
            bail!(
                "tool {:?} has invalid contract version 0",
                planned.definition.name
            );
        }
        let authorization = self
            .permissions
            .authorize(planned.permission_request(operation_id, invocation_id))
            .await
            .context("could not authorize planned tool invocation")?;
        let fact = match &authorization {
            Authorization::Allowed(fact) | Authorization::Denied(fact) => fact.clone(),
        };
        self.commits
            .append(
                SessionRecord::PermissionAudited { fact: fact.clone() },
                Some(AgentEvent::PermissionAudited { fact: fact.clone() }),
            )
            .await?;
        self.observer.reached(CrashSite::AfterPermissionAudit)?;

        let result_id = ToolResultId::new();
        let intent = InvocationIntent {
            operation_id,
            step_id,
            invocation_id,
            result_id,
            model_call_id: planned.call_id.clone(),
            target: InvocationTarget::Tool {
                name: planned.definition.name.to_owned(),
                contract_version: planned.definition.contract_version,
            },
            final_arguments: planned.final_arguments().clone(),
            permission: fact,
            saved_replay_safety: planned.definition.replay_safety,
        };
        self.commits
            .append(
                SessionRecord::InvocationIntentAppended {
                    intent: intent.clone(),
                },
                Some(AgentEvent::InvocationIntentCommitted {
                    intent: intent.clone(),
                }),
            )
            .await?;
        self.observer.reached(CrashSite::AfterInvocationIntent)?;

        let (tool_result, outcome) = if matches!(authorization, Authorization::Denied(_)) {
            let reason = format!("permission denied for tool {:?}", planned.definition.name);
            (
                ToolResult::error(planned.call_id.clone(), reason.clone()),
                InvocationOutcome::Declined {
                    reason: bounded_diagnostic(reason),
                },
            )
        } else {
            let execution = planned
                .execute(crate::tool::ToolExecutionContext { operation_id })
                .await;
            self.observer.reached(CrashSite::AfterEffectBeforeResult)?;
            match execution {
                Ok(output) => {
                    let value = self
                        .commits
                        .store_json(Value::String(output.clone()))
                        .await?;
                    (
                        ToolResult::success(planned.call_id.clone(), output),
                        InvocationOutcome::Completed { output: value },
                    )
                }
                Err(error) => (
                    ToolResult::error(planned.call_id.clone(), error.clone()),
                    InvocationOutcome::Failed {
                        error: bounded_diagnostic(error),
                    },
                ),
            }
        };
        let result = InvocationResultRecord {
            operation_id,
            invocation_id,
            result_id,
            outcome,
        };
        self.commits
            .append(
                SessionRecord::InvocationResultAppended {
                    result: result.clone(),
                },
                Some(AgentEvent::InvocationResultCommitted {
                    result: result.clone(),
                }),
            )
            .await?;
        if let InvocationOutcome::Completed { output } = &result.outcome {
            self.commits
                .append(named_tool_output(&intent, output.clone()), None)
                .await?;
        }
        self.observer.reached(CrashSite::AfterInvocationResult)?;
        Ok(tool_result)
    }
}

fn named_tool_output(intent: &InvocationIntent, value: DurableValueRef) -> SessionRecord {
    SessionRecord::NamedValueSet {
        value: NamedValueRecord {
            id: NamedValueId::new(),
            operation_id: intent.operation_id,
            name: format!("tool_output:{}", intent.invocation_id),
            value,
        },
    }
}

#[cfg(test)]
mod tests;
