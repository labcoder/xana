mod edit_file;
mod list_files;
mod read_file;
mod workspace_path;

use crate::message::{ToolCall, ToolResult};
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectClass {
    Read,
    Write,
    #[allow(dead_code)] // run_command lands in Lesson 2.1
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
    #[allow(dead_code)] // persisted and enforced by the Lesson 2.6 recovery path
    pub(crate) effect_class: EffectClass,
    #[allow(dead_code)] // persisted and enforced by the Lesson 2.6 recovery path
    pub(crate) replay_safety: ReplaySafety,
}

pub(crate) trait Tool {
    fn definition(&self) -> ToolDefinition;

    fn execute(&self, arguments: &Value, workspace_root: &Path) -> Result<String, String>;
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

    pub(crate) fn execute(&self, call: &ToolCall, workspace_root: &Path) -> ToolResult {
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.definition.name == call.name.as_str())
        else {
            return ToolResult::error(call.id.clone(), format!("unknown tool {:?}", call.name));
        };

        match tool.implementation.execute(&call.arguments, workspace_root) {
            Ok(output) => ToolResult::success(call.id.clone(), output),
            Err(error) => ToolResult::error(call.id.clone(), error),
        }
    }

    pub(crate) fn builtins() -> Result<Self, RegistryError> {
        let mut registry = Self::new();
        registry.register(read_file::ReadFile)?;
        registry.register(list_files::ListFiles)?;
        registry.register(edit_file::EditFile)?;
        Ok(registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolResultStatus;
    use serde_json::json;
    use std::cell::Cell;
    use std::rc::Rc;
    use tempfile::tempdir;

    struct Echo;

    impl Tool for Echo {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "echo",
                description: "Return a fixed test value",
                parameters: json!({"type": "object"}),
                effect_class: EffectClass::Read,
                replay_safety: ReplaySafety::Safe,
            }
        }

        fn execute(&self, _arguments: &Value, _workspace_root: &Path) -> Result<String, String> {
            Ok("echoed".to_owned())
        }
    }

    struct AlwaysFails;

    impl Tool for AlwaysFails {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "always_fails",
                description: "Return a fixed test failure",
                parameters: json!({"type": "object"}),
                effect_class: EffectClass::External,
                replay_safety: ReplaySafety::Never,
            }
        }

        fn execute(&self, _arguments: &Value, _workspace_root: &Path) -> Result<String, String> {
            Err("planned failure".to_owned())
        }
    }

    struct CountedDefinition {
        calls: Rc<Cell<usize>>,
    }

    impl Tool for CountedDefinition {
        fn definition(&self) -> ToolDefinition {
            self.calls.set(self.calls.get() + 1);
            ToolDefinition {
                name: "counted",
                description: "Prove definitions are cached",
                parameters: json!({"type": "object"}),
                effect_class: EffectClass::Read,
                replay_safety: ReplaySafety::Safe,
            }
        }

        fn execute(&self, _arguments: &Value, _workspace_root: &Path) -> Result<String, String> {
            Ok("counted".to_owned())
        }
    }

    #[test]
    fn definitions_preserve_registration_order_and_metadata() {
        let mut registry = ToolRegistry::new();
        registry.register(Echo).expect("register echo");
        registry
            .register(AlwaysFails)
            .expect("register failing tool");

        let definitions = registry.definitions();

        assert_eq!(
            definitions.iter().map(|item| item.name).collect::<Vec<_>>(),
            vec!["echo", "always_fails"]
        );
        assert_eq!(definitions[0].effect_class, EffectClass::Read);
        assert_eq!(definitions[0].replay_safety, ReplaySafety::Safe);
        assert_eq!(definitions[1].effect_class, EffectClass::External);
        assert_eq!(definitions[1].replay_safety, ReplaySafety::Never);
    }

    #[test]
    fn definitions_are_cached_and_lookup_returns_registry_owned_value() {
        let calls = Rc::new(Cell::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(CountedDefinition {
                calls: Rc::clone(&calls),
            })
            .expect("register counted tool");

        assert_eq!(calls.get(), 1);
        assert_eq!(registry.definitions()[0].name, "counted");
        assert_eq!(
            registry.definition("counted").map(|item| item.name),
            Some("counted")
        );

        let workspace = tempdir().expect("temporary workspace");
        let result = registry.execute(
            &ToolCall {
                id: "call-counted".to_owned(),
                name: "counted".to_owned(),
                arguments: json!({}),
            },
            workspace.path(),
        );

        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn duplicate_names_are_rejected_before_dispatch() {
        let mut registry = ToolRegistry::new();
        registry.register(Echo).expect("first registration");

        let result = registry.register(Echo);

        assert_eq!(result, Err(RegistryError::DuplicateName { name: "echo" }));
    }

    #[test]
    fn registered_tool_dispatches_through_trait_object() {
        let workspace = tempdir().expect("temporary workspace");
        let mut registry = ToolRegistry::new();
        registry.register(Echo).expect("register echo");
        let call = ToolCall {
            id: "call-echo".to_owned(),
            name: "echo".to_owned(),
            arguments: json!({}),
        };

        let result = registry.execute(&call, workspace.path());

        assert_eq!(result.call_id, "call-echo");
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.output, "echoed");
    }

    #[test]
    fn tool_failure_preserves_call_id_and_internal_status() {
        let workspace = tempdir().expect("temporary workspace");
        let mut registry = ToolRegistry::new();
        registry
            .register(AlwaysFails)
            .expect("register failing tool");
        let call = ToolCall {
            id: "call-failure".to_owned(),
            name: "always_fails".to_owned(),
            arguments: json!({}),
        };

        let result = registry.execute(&call, workspace.path());

        assert_eq!(result.call_id, "call-failure");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(result.output, "planned failure");
        assert!(!result.output.starts_with("ERROR:"));
    }

    #[test]
    fn unknown_tool_returns_correlated_error() {
        let workspace = tempdir().expect("temporary workspace");
        let registry = ToolRegistry::new();
        let call = ToolCall {
            id: "call-unknown".to_owned(),
            name: "load_theme".to_owned(),
            arguments: json!({}),
        };

        let result = registry.execute(&call, workspace.path());

        assert_eq!(result.call_id, "call-unknown");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(result.output.contains("load_theme"));
        assert!(!result.output.starts_with("ERROR:"));
    }

    #[test]
    fn builtins_have_deterministic_order_and_safety_metadata() {
        let registry = ToolRegistry::builtins().expect("built-in registry");
        let definitions = registry.definitions();

        assert_eq!(
            definitions.iter().map(|item| item.name).collect::<Vec<_>>(),
            vec!["read_file", "list_files", "edit_file"]
        );
        assert_eq!(definitions[0].effect_class, EffectClass::Read);
        assert_eq!(definitions[0].replay_safety, ReplaySafety::Safe);
        assert_eq!(definitions[1].effect_class, EffectClass::Read);
        assert_eq!(definitions[1].replay_safety, ReplaySafety::Safe);
        assert_eq!(definitions[2].effect_class, EffectClass::Write);
        assert_eq!(definitions[2].replay_safety, ReplaySafety::Never);
    }

    #[test]
    fn builtins_dispatch_read_list_and_edit_with_call_ids() {
        use std::fs;

        let workspace = tempdir().expect("temporary workspace");
        fs::write(workspace.path().join("state.txt"), "status=rough\n").expect("state fixture");
        let registry = ToolRegistry::builtins().expect("built-in registry");

        let list = registry.execute(
            &ToolCall {
                id: "call-list".to_owned(),
                name: "list_files".to_owned(),
                arguments: json!({"path": "."}),
            },
            workspace.path(),
        );
        let edit = registry.execute(
            &ToolCall {
                id: "call-edit".to_owned(),
                name: "edit_file".to_owned(),
                arguments: json!({
                    "path": "state.txt",
                    "old_text": "status=rough",
                    "new_text": "status=ready"
                }),
            },
            workspace.path(),
        );
        let read = registry.execute(
            &ToolCall {
                id: "call-read".to_owned(),
                name: "read_file".to_owned(),
                arguments: json!({"path": "state.txt"}),
            },
            workspace.path(),
        );

        assert_eq!(list.call_id, "call-list");
        assert_eq!(list.status, ToolResultStatus::Success);
        assert!(list.output.contains("state.txt"));
        assert_eq!(edit.call_id, "call-edit");
        assert_eq!(edit.status, ToolResultStatus::Success);
        assert_eq!(read.call_id, "call-read");
        assert_eq!(read.status, ToolResultStatus::Success);
        assert_eq!(read.output, "status=ready\n");
    }
}
