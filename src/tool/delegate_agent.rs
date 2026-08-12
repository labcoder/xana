use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, ToolExecutionContext,
};
use crate::{
    orchestration::{
        ChildContextHandoff, ChildRestrictions, ChildResultSchema, ChildSupervisorHandle,
        SpawnAgentRequest,
    },
    permission::PermissionScope,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

pub(super) struct DelegateAgent {
    supervisor: ChildSupervisorHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    #[serde(default)]
    route: Option<String>,
    task: String,
    #[serde(default)]
    result_schema: ChildResultSchema,
    #[serde(default)]
    restrictions: ChildRestrictions,
    #[serde(default)]
    handoff: ChildContextHandoff,
}

impl DelegateAgent {
    pub(super) fn new(supervisor: ChildSupervisorHandle) -> Self {
        Self { supervisor }
    }
}

impl Tool for DelegateAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_agent".into(),
            contract_version: 1,
            description: "Delegate one explicit bounded task to an exact Xana child route and wait for its attributed report".into(),
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["task"],
                "properties":{
                    "route":{"type":"string","minLength":1,"maxLength":128},
                    "task":{"type":"string","minLength":1,"maxLength":262144},
                    "result_schema":{"type":"string","enum":["summary","json"],"default":"summary"},
                    "restrictions":super::child_agent::restriction_schema(),
                    "handoff":super::child_agent::handoff_schema()
                }
            }),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let args: Args = serde_json::from_value(arguments.clone())
            .map_err(|_| "delegate_agent arguments are invalid".to_owned())?;
        let final_arguments = serde_json::to_value(&args).map_err(|error| error.to_string())?;
        Ok(PlannedToolInvocation::new(
            final_arguments,
            PermissionScope::Unscoped,
            args,
        ))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<Args>("delegate_agent")?;
            let (handle, report) = self
                .supervisor
                .delegate_agent(
                    context.operation_id,
                    SpawnAgentRequest {
                        route: args.route.clone(),
                        task: args.task.clone(),
                        result_schema: args.result_schema,
                        restrictions: args.restrictions.clone(),
                        handoff: args.handoff.clone(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&json!({
                "handle": handle,
                "report": report,
            }))
            .map_err(|error| error.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_exposes_one_bounded_task_without_detach_or_recursion() {
        let definition = DelegateAgent {
            supervisor: ChildSupervisorHandle::closed_for_test(),
        }
        .definition();

        assert_eq!(definition.name, "delegate_agent");
        assert_eq!(definition.replay_safety, ReplaySafety::Never);
        assert_eq!(
            definition.parameters["properties"]["task"]["maxLength"],
            262_144
        );
        assert!(definition.parameters["properties"].get("detach").is_none());
    }
}
