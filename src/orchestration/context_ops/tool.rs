use super::*;
use crate::{
    permission::PermissionScope,
    tool::{
        EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition,
        ToolExecutionContext,
    },
};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone)]
pub(crate) struct WorkerContextTool {
    pub(crate) paths: XanaPaths,
    pub(crate) store: ProtectedStore,
    pub(crate) worker: AgentId,
    pub(crate) cancellation: CancellationToken,
}
impl Tool for WorkerContextTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "context_ops".into(),
            contract_version: 1,
            description: concat!(
                "Compute bounded native search/slice/filter/map/reduce/derive/cite over this worker's selected immutable evidence. ",
                "No interpreter or model calls. Returns a small preview, exact citation-bearing artifact, and cumulative work receipt. ",
                "Inputs require artifact id/hash plus UTF-8 byte offset/length. Use the declared handoff references; arbitrary files are unavailable."
            ).into(),
            parameters: json!({
                "type": "object",
                "required": ["operation"],
                "additionalProperties": false,
                "properties": {
                    "operation": {"type":"string", "enum":["search", "slice", "filter", "map", "reduce", "derive", "cite"]},
                    "inputs": {"type":"array", "maxItems":16, "items":range_schema()},
                    "input": range_schema(),
                    "query": {"type":"string", "maxLength":256},
                    "contains": {"type":"string", "maxLength":256},
                    "label": {"type":"string", "maxLength":256},
                    "transform": {"type":"string", "enum":["trim", "lowercase", "uppercase"]},
                    "reducer": {"type":"string", "enum":["count_bytes", "count_lines", "concat"]}
                }
            }),
            effect_class: EffectClass::Read,
            replay_safety: ReplaySafety::Safe,
        }
    }
    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let operation: ContextOperation =
            serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
        operation.validate().map_err(|e| e.to_string())?;
        Ok(PlannedToolInvocation::new(
            arguments.clone(),
            PermissionScope::BuiltInResource {
                id: format!("retained-evidence:{}", self.worker),
            },
            operation,
        ))
    }
    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let operation = planned
                .executable::<ContextOperation>("context_ops")?
                .clone();
            let mut owner = self.clone();
            owner.cancellation = self.cancellation.child_token();
            let _cancel_on_drop = owner.cancellation.clone().drop_guard();
            tokio::task::spawn_blocking(move || {
                let revision = owner.store.retained_worker(owner.worker)?.revision;
                let receipt = execute(
                    &owner.paths,
                    &owner.store,
                    owner.worker,
                    revision,
                    operation,
                    &owner.cancellation,
                )?;
                Ok(serde_json::to_string(&receipt)?)
            })
            .await
            .map_err(|_| "context operation worker stopped".to_owned())?
            .map_err(|e: anyhow::Error| e.to_string())
        })
    }
}
fn range_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["artifact", "offset", "length"],
        "properties": {
            "artifact": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "content_hash"],
                "properties": {
                    "id": {"type":"string"},
                    "content_hash": {"type":"string", "pattern":"^[a-f0-9]{64}$"}
                }
            },
            "offset": {"type":"integer", "minimum":0},
            "length": {"type":"integer", "minimum":1, "maximum":65536}
        }
    })
}
