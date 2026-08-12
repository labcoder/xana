use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, ToolExecutionContext,
};
use crate::orchestration::{ChildSupervisorHandle, OrchestrationPlan};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

pub(super) struct ValidateOrchestrationPlan {
    supervisor: ChildSupervisorHandle,
}

pub(super) struct ExecuteOrchestrationPlan {
    supervisor: ChildSupervisorHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    plan: OrchestrationPlan,
}

impl ValidateOrchestrationPlan {
    pub(super) fn new(supervisor: ChildSupervisorHandle) -> Self {
        Self { supervisor }
    }
}

impl ExecuteOrchestrationPlan {
    pub(super) fn new(supervisor: ChildSupervisorHandle) -> Self {
        Self { supervisor }
    }
}

impl Tool for ValidateOrchestrationPlan {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "validate_orchestration_plan".into(),
            contract_version: 1,
            description: "Purely validate one closed versioned Xana orchestration plan, including exact routes and aggregate runtime budgets, without admitting work".into(),
            parameters: parameters(),
            effect_class: EffectClass::Read,
            replay_safety: ReplaySafety::Safe,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        planned(arguments)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<Args>("validate_orchestration_plan")?;
            let diagnostic = self
                .supervisor
                .validate_orchestration_plan(context.operation_id, args.plan.clone())
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&diagnostic).map_err(|error| error.to_string())
        })
    }
}

impl Tool for ExecuteOrchestrationPlan {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "execute_orchestration_plan".into(),
            contract_version: 1,
            description: "Validate then execute one closed Xana spawn/await/collect/cancel plan through the canonical child supervisor".into(),
            parameters: parameters(),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        planned(arguments)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let args = planned.executable::<Args>("execute_orchestration_plan")?;
            let result = self
                .supervisor
                .execute_orchestration_plan(context.operation_id, args.plan.clone())
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&result).map_err(|error| error.to_string())
        })
    }
}

fn planned(arguments: &Value) -> Result<PlannedToolInvocation, String> {
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|_| "orchestration plan arguments are invalid".to_owned())?;
    Ok(PlannedToolInvocation::new(
        serde_json::to_value(&args).map_err(|error| error.to_string())?,
        crate::permission::PermissionScope::Unscoped,
        args,
    ))
}

fn parameters() -> Value {
    let handle_reference = json!({
        "type":"object",
        "additionalProperties":false,
        "required":["step"],
        "properties":{
            "step":{"type":"string","minLength":1,"maxLength":64},
            "index":{"type":"integer","minimum":0,"default":0}
        }
    });
    let spawn_request = super::child_agent::spawn_parameters();
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["plan"],
        "properties":{
            "plan":{
                "type":"object",
                "additionalProperties":false,
                "required":["version","plan_id","steps"],
                "properties":{
                    "version":{"type":"integer","const":1},
                    "plan_id":{"type":"string","format":"uuid"},
                    "steps":{
                        "type":"array",
                        "minItems":1,
                        "maxItems":64,
                        "items":{"oneOf":[
                            {
                                "type":"object",
                                "additionalProperties":false,
                                "required":["operator","id","requests"],
                                "properties":{
                                    "operator":{"const":"spawn"},
                                    "id":{"type":"string","minLength":1,"maxLength":64},
                                    "requests":{"type":"array","minItems":1,"maxItems":64,"items":spawn_request}
                                }
                            },
                            {
                                "type":"object",
                                "additionalProperties":false,
                                "required":["operator","id","handle"],
                                "properties":{
                                    "operator":{"const":"await"},
                                    "id":{"type":"string","minLength":1,"maxLength":64},
                                    "handle":handle_reference.clone(),
                                    "timeout_ms":{"type":"integer","minimum":1,"maximum":600000},
                                    "cancel_on_timeout":{"type":"boolean","default":false}
                                }
                            },
                            {
                                "type":"object",
                                "additionalProperties":false,
                                "required":["operator","id","handles"],
                                "properties":{
                                    "operator":{"const":"collect"},
                                    "id":{"type":"string","minLength":1,"maxLength":64},
                                    "handles":{"type":"array","minItems":1,"maxItems":64,"uniqueItems":true,"items":handle_reference.clone()},
                                    "timeout_ms":{"type":"integer","minimum":1,"maximum":600000},
                                    "failure_policy":{"type":"string","enum":["continue_on_error","fail_fast"],"default":"continue_on_error"},
                                    "cancel_remaining":{"type":"boolean","default":false},
                                    "cancel_on_timeout":{"type":"boolean","default":false}
                                }
                            },
                            {
                                "type":"object",
                                "additionalProperties":false,
                                "required":["operator","id","handle"],
                                "properties":{
                                    "operator":{"const":"cancel"},
                                    "id":{"type":"string","minLength":1,"maxLength":64},
                                    "handle":handle_reference
                                }
                            }
                        ]}
                    }
                }
            }
        }
    })
}
