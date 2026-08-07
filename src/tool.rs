//! Provider-neutral tool contract, safety metadata, and registry.
//!
//! Concrete tools own argument decoding and typed implementation errors. The
//! registry exposes stable definitions and dispatches model requests without
//! treating effect metadata as permission or containment.

mod edit_file;
mod list_files;
mod read_file;
mod run_command;
mod workspace_path;

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

pub(crate) const BUILTIN_TOOL_NAMES: &[&str] =
    &["read_file", "list_files", "edit_file", "run_command"];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplaySafety {
    Safe,
    Never,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolDefinition {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) parameters: Value,
    #[allow(dead_code)] // persisted and enforced by the later recovery path
    pub(crate) effect_class: EffectClass,
    #[allow(dead_code)] // persisted and enforced by the later recovery path
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
    ) -> BoxFuture<'a, Result<String, String>>;
}

pub(crate) struct PlannedToolInvocation {
    pub(crate) final_arguments: Value,
    pub(crate) scope: PermissionScope,
    executable: Box<dyn Any + Send + Sync>,
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
    DuplicateName { name: &'static str },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName { name } => {
                write!(f, "tool {name:?} is already registered")
            }
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
        let definition = tool.definition();

        if self.definition(definition.name).is_some() {
            return Err(RegistryError::DuplicateName {
                name: definition.name,
            });
        }

        self.tools.push(RegisteredTool {
            definition,
            implementation: Box::new(tool),
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
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.definition.name == call.name.as_str())
        else {
            return ToolResult::error(call.id.clone(), format!("unknown tool {:?}", call.name));
        };

        let planned = match tool
            .implementation
            .plan(&call.arguments, context.workspace_root)
        {
            Ok(planned) => planned,
            Err(error) => return ToolResult::error(call.id.clone(), error),
        };
        let request = PermissionRequest {
            operation_id: context.operation_id,
            invocation_id: context.invocation_id,
            tool_name: tool.definition.name.to_owned(),
            effect_class: tool.definition.effect_class,
            final_arguments: planned.final_arguments.clone(),
            scope: planned.scope.clone(),
        };
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

        match tool.implementation.execute(&planned).await {
            Ok(output) => ToolResult::success(call.id.clone(), output),
            Err(error) => ToolResult::error(call.id.clone(), error),
        }
    }

    pub(crate) fn builtins(shell: Shell) -> Result<Self, RegistryError> {
        let mut registry = Self::new();
        registry.register(read_file::ReadFile)?;
        registry.register(list_files::ListFiles)?;
        registry.register(edit_file::EditFile)?;
        registry.register(run_command::RunCommand::new(shell))?;
        Ok(registry)
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
