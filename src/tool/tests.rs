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
    let registry = ToolRegistry::builtins_for_tests().expect("built-in registry");
    let definitions = registry.definitions();

    assert_eq!(
        definitions.iter().map(|item| item.name).collect::<Vec<_>>(),
        vec!["read_file", "list_files", "edit_file", "run_command"]
    );
    assert_eq!(definitions[0].effect_class, EffectClass::Read);
    assert_eq!(definitions[0].replay_safety, ReplaySafety::Safe);
    assert_eq!(definitions[1].effect_class, EffectClass::Read);
    assert_eq!(definitions[1].replay_safety, ReplaySafety::Safe);
    assert_eq!(definitions[2].effect_class, EffectClass::Write);
    assert_eq!(definitions[2].replay_safety, ReplaySafety::Never);
    assert_eq!(definitions[3].effect_class, EffectClass::Execute);
    assert_eq!(definitions[3].replay_safety, ReplaySafety::Never);
}

#[test]
fn builtins_dispatch_read_list_and_edit_with_call_ids() {
    use std::fs;

    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("state.txt"), "status=rough\n").expect("state fixture");
    let registry = ToolRegistry::builtins_for_tests().expect("built-in registry");

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
