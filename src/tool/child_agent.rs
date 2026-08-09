use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, ToolExecutionContext,
};
use crate::{
    identity::AgentId,
    orchestration::{
        AwaitAgentOptions, AwaitAgentOutcome, ChildRestrictions, ChildResultSchema,
        ChildSupervisorHandle, CollectAgentsOptions, CollectionFailurePolicy, SpawnAgentRequest,
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

pub(super) struct SpawnMany {
    supervisor: ChildSupervisorHandle,
}

pub(super) struct AwaitAgent {
    supervisor: ChildSupervisorHandle,
}

pub(super) struct CancelAgent {
    supervisor: ChildSupervisorHandle,
}

pub(super) struct CollectAgents {
    supervisor: ChildSupervisorHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    #[serde(default)]
    route: Option<String>,
    task: String,
    #[serde(default)]
    result_schema: ChildResultSchema,
    #[serde(default)]
    restrictions: ChildRestrictions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnManyArgs {
    requests: Vec<SpawnArgs>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectArgs {
    agent_ids: Vec<AgentId>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    failure_policy: CollectionFailurePolicy,
    #[serde(default)]
    cancel_remaining: bool,
    #[serde(default)]
    cancel_on_timeout: bool,
}

impl SpawnAgent {
    pub(super) fn new(supervisor: ChildSupervisorHandle) -> Self {
        Self { supervisor }
    }
}

impl SpawnMany {
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

impl CollectAgents {
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
                        result_schema: args.result_schema,
                        restrictions: args.restrictions.clone(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&handle).map_err(|error| error.to_string())
        })
    }
}

impl Tool for SpawnMany {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "spawn_many",
            contract_version: 1,
            description: "Atomically admit a fixed bounded list of independent Xana child tasks; either every queued handle is returned in input order or none exists",
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["requests"],
                "properties":{
                    "requests":{
                        "type":"array",
                        "minItems":1,
                        "maxItems":64,
                        "items":spawn_parameters()
                    }
                }
            }),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let args: SpawnManyArgs = serde_json::from_value(arguments.clone())
            .map_err(|_| "spawn_many arguments are invalid".to_owned())?;
        if args.requests.is_empty() || args.requests.len() > 64 {
            return Err("spawn_many requires between 1 and 64 child requests".to_owned());
        }
        planned(args)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<SpawnManyArgs>("spawn_many")?;
            let requests = args
                .requests
                .iter()
                .map(|request| SpawnAgentRequest {
                    route: request.route.clone(),
                    task: request.task.clone(),
                    result_schema: request.result_schema,
                    restrictions: request.restrictions.clone(),
                })
                .collect();
            let handles = self
                .supervisor
                .spawn_many(context.operation_id, requests)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&handles).map_err(|error| error.to_string())
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

impl Tool for CollectAgents {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "collect_agents",
            contract_version: 1,
            description: "Collect a bounded set of Xana child reports in requested handle order with explicit timeout and failure policy",
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["agent_ids"],
                "properties":{
                    "agent_ids":{
                        "type":"array",
                        "minItems":1,
                        "maxItems":64,
                        "uniqueItems":true,
                        "items":{"type":"string","format":"uuid"}
                    },
                    "timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_AWAIT_TIMEOUT_MS},
                    "failure_policy":{
                        "type":"string",
                        "enum":["continue_on_error","fail_fast"],
                        "default":"continue_on_error"
                    },
                    "cancel_remaining":{"type":"boolean","default":false},
                    "cancel_on_timeout":{"type":"boolean","default":false}
                }
            }),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let args: CollectArgs = serde_json::from_value(arguments.clone())
            .map_err(|_| "collect_agents arguments are invalid".to_owned())?;
        if args.agent_ids.is_empty() || args.agent_ids.len() > 64 {
            return Err("collect_agents requires between 1 and 64 child handles".to_owned());
        }
        let unique = args
            .agent_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != args.agent_ids.len() {
            return Err("collect_agents child handles must be unique".to_owned());
        }
        if args
            .timeout_ms
            .is_some_and(|value| value == 0 || value > MAX_AWAIT_TIMEOUT_MS)
        {
            return Err(format!(
                "collect_agents timeout_ms must be within 1..={MAX_AWAIT_TIMEOUT_MS}"
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
            let args = planned.executable::<CollectArgs>("collect_agents")?;
            let result = self
                .supervisor
                .collect_agents(
                    args.agent_ids.clone(),
                    CollectAgentsOptions {
                        timeout: args.timeout_ms.map(Duration::from_millis),
                        failure_policy: args.failure_policy,
                        cancel_remaining: args.cancel_remaining,
                        cancel_on_timeout: args.cancel_on_timeout,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&result).map_err(|error| error.to_string())
        })
    }
}

pub(super) fn spawn_parameters() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["task"],
        "properties":{
            "route":{"type":"string","minLength":1,"maxLength":128},
            "task":{"type":"string","minLength":1,"maxLength":262144},
            "result_schema":{"type":"string","enum":["summary","json"],"default":"summary"},
            "restrictions":restriction_schema()
        }
    })
}

pub(super) fn restriction_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "permission_mode":{"type":"string","enum":["deny","ask","allow"]},
            "max_tool_rounds":{"type":"integer","minimum":1},
            "deadline_seconds":{"type":"integer","minimum":1},
            "max_context_tokens":{"type":"integer","minimum":1},
            "max_report_bytes":{"type":"integer","minimum":1},
            "max_artifact_bytes":{"type":"integer","minimum":1},
            "hard_token_limit":{"type":"integer","minimum":1},
            "hard_spend_microusd":{"type":"integer","minimum":1}
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
        let spawn_many = SpawnMany::new(supervisor.clone()).definition();
        let await_agent = AwaitAgent::new(supervisor.clone()).definition();
        let cancel = CancelAgent::new(supervisor).definition();
        let collect = CollectAgents::new(ChildSupervisorHandle::closed_for_test()).definition();

        assert_eq!(spawn.name, "spawn_agent");
        assert_eq!(spawn_many.name, "spawn_many");
        assert_eq!(await_agent.name, "await_agent");
        assert_eq!(cancel.name, "cancel_agent");
        assert_eq!(collect.name, "collect_agents");
        assert!(spawn.parameters["properties"].get("wait").is_none());
        assert_eq!(
            await_agent.parameters["properties"]["cancel_on_timeout"]["default"],
            false
        );
    }
}
