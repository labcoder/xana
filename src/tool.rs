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
use crate::runtime::ProvisionalApprovalCoordinator;
use crate::shell::Shell;
use futures::future::BoxFuture;
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn execute<'a>(
        &'a self,
        arguments: &'a Value,
        context: ToolContext<'a>,
    ) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Clone, Copy)]
pub(crate) struct ToolContext<'a> {
    pub(crate) workspace_root: &'a Path,
    pub(crate) operation_id: OperationId,
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) approvals: &'a ProvisionalApprovalCoordinator,
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

    pub(crate) async fn execute(&self, call: &ToolCall, context: ToolContext<'_>) -> ToolResult {
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.definition.name == call.name.as_str())
        else {
            return ToolResult::error(call.id.clone(), format!("unknown tool {:?}", call.name));
        };

        match tool.implementation.execute(&call.arguments, context).await {
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
        let approvals = ProvisionalApprovalCoordinator::new(events);
        futures::executor::block_on(self.execute(
            call,
            ToolContext {
                workspace_root,
                operation_id: OperationId::new(),
                invocation_id: ToolInvocationId::new(),
                approvals: &approvals,
            },
        ))
    }
}

#[cfg(test)]
mod tests;
