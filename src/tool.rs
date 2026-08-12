//! Provider-neutral tool contract, safety metadata, and registry.
//!
//! Concrete tools own argument decoding and typed implementation errors. The
//! registry exposes stable definitions and dispatches model requests without
//! treating effect metadata as permission or containment.

mod child_agent;
mod delegate_agent;
mod edit_file;
mod list_files;
mod orchestration_plan;
mod read_document;
mod read_file;
mod run_command;
mod workspace_path;
mod xana_docs;

use crate::identity::{OperationId, ToolInvocationId};
use crate::message::{ToolCall, ToolResult};
use crate::permission::{
    Authorization, PermissionBrokerHandle, PermissionRequest, PermissionScope,
};
use crate::shell::Shell;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::any::Any;
use std::error::Error;
use std::fmt;
use std::path::Path;

pub(crate) const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read_file",
    "list_files",
    "edit_file",
    "run_command",
    "read_document",
    "xana_docs",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EffectClass {
    Read,
    Write,
    Execute,
    #[allow(dead_code)] // network tools arrive through later extensions
    Network,
    #[allow(dead_code)] // external-service tools arrive through later extensions
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplaySafety {
    Safe,
    Never,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolDefinition {
    pub(crate) name: String,
    pub(crate) contract_version: u32,
    pub(crate) description: String,
    pub(crate) parameters: Value,
    pub(crate) effect_class: EffectClass,
    pub(crate) replay_safety: ReplaySafety,
}

pub(crate) trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn plan(
        &self,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Result<PlannedToolInvocation, String>;

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolExecutionContext {
    pub(crate) operation_id: OperationId,
}

pub(crate) struct PlannedToolInvocation {
    pub(crate) final_arguments: Value,
    pub(crate) scope: PermissionScope,
    executable: Box<dyn Any + Send + Sync>,
}

pub(crate) struct PreparedToolInvocation<'a> {
    pub(crate) call_id: String,
    pub(crate) definition: &'a ToolDefinition,
    implementation: &'a dyn Tool,
    planned: PlannedToolInvocation,
}

impl PreparedToolInvocation<'_> {
    pub(crate) fn final_arguments(&self) -> &Value {
        &self.planned.final_arguments
    }

    pub(crate) fn scope(&self) -> &PermissionScope {
        &self.planned.scope
    }

    pub(crate) fn permission_request(
        &self,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    ) -> PermissionRequest {
        PermissionRequest {
            operation_id,
            invocation_id,
            tool_name: self.definition.name.clone(),
            effect_class: self.definition.effect_class,
            final_arguments: self.planned.final_arguments.clone(),
            scope: self.planned.scope.clone(),
        }
    }

    pub(crate) async fn execute(&self, context: ToolExecutionContext) -> Result<String, String> {
        self.implementation.execute(&self.planned, context).await
    }
}

impl PlannedToolInvocation {
    pub(crate) fn new<T>(final_arguments: Value, scope: PermissionScope, executable: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            final_arguments,
            scope,
            executable: Box::new(executable),
        }
    }

    pub(crate) fn executable<T: Any>(&self, tool_name: &str) -> Result<&T, String> {
        self.executable
            .downcast_ref::<T>()
            .ok_or_else(|| format!("{tool_name} received an incompatible invocation plan"))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ToolContext<'a> {
    pub(crate) workspace_root: &'a Path,
    pub(crate) operation_id: OperationId,
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) permissions: &'a PermissionBrokerHandle,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RegistryError {
    DuplicateName { name: String },
    Composition(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName { name } => {
                write!(f, "tool {name:?} is already registered")
            }
            Self::Composition(reason) => write!(f, "could not compose tool capabilities: {reason}"),
        }
    }
}

impl Error for RegistryError {}

struct RegisteredTool {
    definition: ToolDefinition,
    implementation: Box<dyn Tool>,
}

#[derive(Default)]
pub(crate) struct ToolRegistry {
    tools: Vec<RegisteredTool>,
}

impl ToolRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register<T>(&mut self, tool: T) -> Result<(), RegistryError>
    where
        T: Tool + 'static,
    {
        self.register_boxed(Box::new(tool))
    }

    pub(crate) fn register_boxed(&mut self, tool: Box<dyn Tool>) -> Result<(), RegistryError> {
        let definition = tool.definition();

        if self.definition(&definition.name).is_some() {
            return Err(RegistryError::DuplicateName {
                name: definition.name,
            });
        }

        self.tools.push(RegisteredTool {
            definition,
            implementation: tool,
        });
        Ok(())
    }

    pub(crate) fn definitions(&self) -> Vec<&ToolDefinition> {
        self.tools.iter().map(|tool| &tool.definition).collect()
    }

    pub(crate) fn definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools
            .iter()
            .find(|tool| tool.definition.name == name)
            .map(|tool| &tool.definition)
    }

    pub(crate) async fn invoke(&self, call: &ToolCall, context: ToolContext<'_>) -> ToolResult {
        let planned = match self.plan(call, context.workspace_root) {
            Ok(planned) => planned,
            Err(result) => return result,
        };
        let request = planned.permission_request(context.operation_id, context.invocation_id);
        let authorization = match context.permissions.authorize(request).await {
            Ok(authorization) => authorization,
            Err(error) => return ToolResult::error(call.id.clone(), error.to_string()),
        };
        if matches!(authorization, Authorization::Denied(_)) {
            return ToolResult::error(
                call.id.clone(),
                format!("permission denied for tool {:?}", call.name),
            );
        }

        match planned
            .execute(ToolExecutionContext {
                operation_id: context.operation_id,
            })
            .await
        {
            Ok(output) => ToolResult::success(call.id.clone(), output),
            Err(error) => ToolResult::error(call.id.clone(), error),
        }
    }

    pub(crate) fn plan<'a>(
        &'a self,
        call: &ToolCall,
        workspace_root: &Path,
    ) -> Result<PreparedToolInvocation<'a>, ToolResult> {
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.definition.name == call.name.as_str())
        else {
            return Err(ToolResult::error(
                call.id.clone(),
                format!("unknown tool {:?}", call.name),
            ));
        };

        let planned = match tool.implementation.plan(&call.arguments, workspace_root) {
            Ok(planned) => planned,
            Err(error) => return Err(ToolResult::error(call.id.clone(), error)),
        };
        Ok(PreparedToolInvocation {
            call_id: call.id.clone(),
            definition: &tool.definition,
            implementation: tool.implementation.as_ref(),
            planned,
        })
    }

    pub(crate) fn builtins(shell: Shell) -> Result<Self, RegistryError> {
        let exposed = crate::capability::resolve_builtin_tool_names()
            .map_err(|error| RegistryError::Composition(error.to_string()))?;
        Self::builtins_from_names(shell, &exposed)
    }

    pub(crate) fn builtins_for_snapshot(
        shell: Shell,
        snapshot: &crate::capability::AgentCapabilitySnapshot,
    ) -> Result<Self, RegistryError> {
        let exposed = snapshot
            .tool_ids()
            .iter()
            .map(ToString::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        Self::builtins_from_names(shell, &exposed)
    }

    pub(crate) fn builtins_from_names(
        shell: Shell,
        exposed: &std::collections::BTreeSet<String>,
    ) -> Result<Self, RegistryError> {
        let mut registry = Self::new();
        if exposed.contains("read_file") {
            registry.register(read_file::ReadFile)?;
        }
        if exposed.contains("list_files") {
            registry.register(list_files::ListFiles)?;
        }
        if exposed.contains("edit_file") {
            registry.register(edit_file::EditFile)?;
        }
        if exposed.contains("run_command") {
            registry.register(run_command::RunCommand::new(shell))?;
        }
        if exposed.contains("read_document") {
            registry.register(read_document::ReadDocument::default())?;
        }
        if exposed.contains("xana_docs") {
            registry.register(xana_docs::XanaDocs)?;
        }
        Ok(registry)
    }

    pub(crate) fn enable_child_delegation(
        &mut self,
        supervisor: crate::orchestration::ChildSupervisorHandle,
    ) -> Result<(), RegistryError> {
        self.register(child_agent::SpawnAgent::new(supervisor.clone()))?;
        self.register(child_agent::SpawnMany::new(supervisor.clone()))?;
        self.register(child_agent::AwaitAgent::new(supervisor.clone()))?;
        self.register(child_agent::CancelAgent::new(supervisor.clone()))?;
        self.register(child_agent::CollectAgents::new(supervisor.clone()))?;
        self.register(orchestration_plan::ValidateOrchestrationPlan::new(
            supervisor.clone(),
        ))?;
        self.register(orchestration_plan::ExecuteOrchestrationPlan::new(
            supervisor.clone(),
        ))?;
        self.register(delegate_agent::DelegateAgent::new(supervisor))
    }

    #[cfg(test)]
    pub(crate) fn builtins_for_tests() -> Result<Self, RegistryError> {
        let shell = Shell::resolve(crate::shell::ShellConfig::default())
            .expect("the platform shell configuration is valid");
        Self::builtins(shell)
    }

    #[cfg(test)]
    pub(crate) fn execute_for_tests(&self, call: &ToolCall, workspace_root: &Path) -> ToolResult {
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let policy = crate::permission::PermissionPolicy::new(
                crate::permission::PolicyDecision::Allow,
                Vec::new(),
                workspace_root,
            )
            .expect("allow policy");
            let (permissions, _broker) =
                crate::permission::PermissionBroker::spawn(policy, false, events);
            self.invoke(
                call,
                ToolContext {
                    workspace_root,
                    operation_id: OperationId::new(),
                    invocation_id: ToolInvocationId::new(),
                    permissions: &permissions,
                },
            )
            .await
        })
    }
}

#[cfg(test)]
mod tests;
