use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, ToolExecutionContext,
};
use crate::{
    identity::AgentId,
    orchestration::{
        AwaitAgentOptions, AwaitAgentOutcome, ChildSupervisorHandle, SpawnAgentRequest,
    },
    permission::PermissionScope,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const MAX_AWAIT_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

pub(super) struct SpawnAgent {
    supervisor: ChildSupervisorHandle,
}

pub(super) struct AwaitAgent {
    supervisor: ChildSupervisorHandle,
}

pub(super) struct CancelAgent {
    supervisor: ChildSupervisorHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    #[serde(default)]
    route: Option<String>,
    task: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AwaitArgs {
    agent_id: AgentId,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    cancel_on_timeout: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentArgs {
    agent_id: AgentId,
}

impl SpawnAgent {
    pub(super) fn new(supervisor: ChildSupervisorHandle) -> Self {
        Self { supervisor }
    }
}

impl AwaitAgent {
    pub(super) fn new(supervisor: ChildSupervisorHandle) -> Self {
        Self { supervisor }
    }
}

impl CancelAgent {
    pub(super) fn new(supervisor: ChildSupervisorHandle) -> Self {
        Self { supervisor }
    }
}

impl Tool for SpawnAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "spawn_agent",
            contract_version: 1,
            description: "Admit one explicit bounded task to an exact Xana child route and return its durable handle without waiting",
            parameters: spawn_parameters(),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let args: SpawnArgs = serde_json::from_value(arguments.clone())
            .map_err(|_| "spawn_agent arguments are invalid".to_owned())?;
        planned(args)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<SpawnArgs>("spawn_agent")?;
            let handle = self
                .supervisor
                .spawn_agent(
                    context.operation_id,
                    SpawnAgentRequest {
                        route: args.route.clone(),
                        task: args.task.clone(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&handle).map_err(|error| error.to_string())
        })
    }
}

impl Tool for AwaitAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "await_agent",
            contract_version: 1,
            description: "Wait for one Xana child report; timeout stops waiting unless cancel_on_timeout is explicitly true",
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["agent_id"],
                "properties":{
                    "agent_id":{"type":"string","format":"uuid"},
                    "timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_AWAIT_TIMEOUT_MS},
                    "cancel_on_timeout":{"type":"boolean","default":false}
                }
            }),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let args: AwaitArgs = serde_json::from_value(arguments.clone())
            .map_err(|_| "await_agent arguments are invalid".to_owned())?;
        if args
            .timeout_ms
            .is_some_and(|value| value == 0 || value > MAX_AWAIT_TIMEOUT_MS)
        {
            return Err(format!(
                "await_agent timeout_ms must be within 1..={MAX_AWAIT_TIMEOUT_MS}"
            ));
        }
        planned(args)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<AwaitArgs>("await_agent")?;
            let outcome = self
                .supervisor
                .await_agent_with(
                    args.agent_id,
                    AwaitAgentOptions {
                        timeout: args.timeout_ms.map(Duration::from_millis),
                        cancel_on_timeout: args.cancel_on_timeout,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            let output = match outcome {
                AwaitAgentOutcome::Report(report) => json!({
                    "outcome":"terminal",
                    "report":report,
                }),
                AwaitAgentOutcome::TimedOut {
                    agent_id,
                    cancellation_requested,
                } => json!({
                    "outcome":"timed_out",
                    "agent_id":agent_id,
                    "cancellation_requested":cancellation_requested,
                }),
            };
            serde_json::to_string(&output).map_err(|error| error.to_string())
        })
    }
}

impl Tool for CancelAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "cancel_agent",
            contract_version: 1,
            description: "Request cooperative cancellation of one Xana child; use await_agent to observe its terminal outcome",
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["agent_id"],
                "properties":{"agent_id":{"type":"string","format":"uuid"}}
            }),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let args: AgentArgs = serde_json::from_value(arguments.clone())
            .map_err(|_| "cancel_agent arguments are invalid".to_owned())?;
        planned(args)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<AgentArgs>("cancel_agent")?;
            let receipt = self
                .supervisor
                .cancel_agent(args.agent_id)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&receipt).map_err(|error| error.to_string())
        })
    }
}

fn spawn_parameters() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["task"],
        "properties":{
            "route":{"type":"string","minLength":1,"maxLength":128},
            "task":{"type":"string","minLength":1,"maxLength":262144}
        }
    })
}

fn planned<T>(args: T) -> Result<PlannedToolInvocation, String>
where
    T: Serialize + Send + Sync + 'static,
{
    let final_arguments = serde_json::to_value(&args).map_err(|error| error.to_string())?;
    Ok(PlannedToolInvocation::new(
        final_arguments,
        PermissionScope::Unscoped,
        args,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_keep_waiting_and_cancellation_explicit() {
        let supervisor = ChildSupervisorHandle::closed_for_test();
        let spawn = SpawnAgent::new(supervisor.clone()).definition();
        let await_agent = AwaitAgent::new(supervisor.clone()).definition();
        let cancel = CancelAgent::new(supervisor).definition();

        assert_eq!(spawn.name, "spawn_agent");
        assert_eq!(await_agent.name, "await_agent");
        assert_eq!(cancel.name, "cancel_agent");
        assert!(spawn.parameters["properties"].get("wait").is_none());
        assert_eq!(
            await_agent.parameters["properties"]["cancel_on_timeout"]["default"],
            false
        );
    }
}
