//! Foreground model-selected memory tools; owner authority never comes from JSON.
//!
//! Native and managed adapters share these tools through the ordinary registry.
//! Exact owner quotations establish provenance, not semantic proof of intent or
//! sensitivity. The selected model interprets requests; uncertain edits require
//! owner review. No classifier, workspace file, or helper-provider call is used.

mod execution;
mod planning;
mod schema;
#[cfg(test)]
mod tests;
pub(crate) mod types;

use super::MemoryOwner;
use crate::tool::{
    OwnerTurnInput, PlannedToolInvocation, RegistryError, Tool, ToolDefinition,
    ToolExecutionContext, ToolRegistry,
};
use futures::future::BoxFuture;
use serde_json::Value;
use std::path::Path;
use types::{LookupPlan, UpdatePlan};

/// Stable declarations remain truthful even when protected storage is absent.
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    vec![schema::lookup(), schema::update()]
}

pub(crate) fn register(
    registry: &mut ToolRegistry,
    owner: Option<MemoryOwner>,
) -> Result<(), RegistryError> {
    registry.register(MemoryLookup {
        owner: owner.clone(),
    })?;
    registry.register(MemoryUpdate { owner })
}

struct MemoryLookup {
    owner: Option<MemoryOwner>,
}

struct MemoryUpdate {
    owner: Option<MemoryOwner>,
}

impl Tool for MemoryLookup {
    fn definition(&self) -> ToolDefinition {
        schema::lookup()
    }

    fn plan(&self, _: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        Err("memory_lookup requires a current foreground owner turn".into())
    }

    fn plan_in_turn(
        &self,
        arguments: &Value,
        _: &Path,
        turn: Option<&OwnerTurnInput>,
    ) -> Result<PlannedToolInvocation, String> {
        planning::lookup(self.owner.as_ref(), arguments, turn).map_err(|error| format!("{error:#}"))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let mut plan = planned.executable::<LookupPlan>("memory_lookup")?.clone();
            if plan.guard.operation_id != context.operation_id {
                return Err("memory lookup belongs to a different owner operation".into());
            }
            let owner = self.owner.clone().ok_or(super::UNAVAILABLE_NOTICE)?;
            let cancellation = plan.guard.cancellation.child_token();
            plan.guard.cancellation = cancellation.clone();
            execution::run(&context.cleanup, cancellation, move || {
                owner
                    .store
                    .memory_tool_lookup(&plan)
                    .and_then(|result| serde_json::to_string(&result).map_err(Into::into))
            })
            .await
        })
    }
}

impl Tool for MemoryUpdate {
    fn definition(&self) -> ToolDefinition {
        schema::update()
    }

    fn plan(&self, _: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        Err("memory_update requires a current foreground owner turn".into())
    }

    fn plan_in_turn(
        &self,
        arguments: &Value,
        _: &Path,
        turn: Option<&OwnerTurnInput>,
    ) -> Result<PlannedToolInvocation, String> {
        planning::update(self.owner.as_ref(), arguments, turn).map_err(|error| format!("{error:#}"))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let mut plan = planned.executable::<UpdatePlan>("memory_update")?.clone();
            if plan.guard.operation_id != context.operation_id {
                return Err("memory update belongs to a different owner operation".into());
            }
            let owner = self.owner.clone().ok_or(super::UNAVAILABLE_NOTICE)?;
            let cancellation = plan.guard.cancellation.child_token();
            plan.guard.cancellation = cancellation.clone();
            execution::run(&context.cleanup, cancellation, move || {
                owner
                    .store
                    .memory_tool_update(&plan)
                    .and_then(|receipt| serde_json::to_string(&receipt).map_err(Into::into))
            })
            .await
        })
    }
}
