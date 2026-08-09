use super::*;
use crate::message::ToolResultStatus;
use crate::{
    permission::{ControllerDecision, PermissionBroker, PermissionPolicy, PolicyDecision},
    runtime::AgentEvent,
};
use futures::future::BoxFuture;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tempfile::tempdir;

struct Echo;

impl Tool for Echo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo",
            contract_version: 1,
            description: "Return a fixed test value",
            parameters: json!({"type": "object"}),
            effect_class: EffectClass::Read,
            replay_safety: ReplaySafety::Safe,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        Ok(PlannedToolInvocation::new(
            arguments.clone(),
            PermissionScope::Unscoped,
            (),
        ))
    }

    fn execute<'a>(
        &'a self,
        _planned: &'a PlannedToolInvocation,
        _context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Ok("echoed".to_owned()) })
    }
}

struct AlwaysFails;

impl Tool for AlwaysFails {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "always_fails",
            contract_version: 1,
            description: "Return a fixed test failure",
            parameters: json!({"type": "object"}),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        Ok(PlannedToolInvocation::new(
            arguments.clone(),
            PermissionScope::Unscoped,
            (),
        ))
    }

    fn execute<'a>(
        &'a self,
        _planned: &'a PlannedToolInvocation,
        _context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("planned failure".to_owned()) })
    }
}

struct CountedDefinition {
    calls: Arc<AtomicUsize>,
}

impl Tool for CountedDefinition {
    fn definition(&self) -> ToolDefinition {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolDefinition {
            name: "counted",
            contract_version: 1,
            description: "Prove definitions are cached",
            parameters: json!({"type": "object"}),
            effect_class: EffectClass::Read,
            replay_safety: ReplaySafety::Safe,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        Ok(PlannedToolInvocation::new(
            arguments.clone(),
            PermissionScope::Unscoped,
            (),
        ))
    }

    fn execute<'a>(
        &'a self,
        _planned: &'a PlannedToolInvocation,
        _context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Ok("counted".to_owned()) })
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
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry
        .register(CountedDefinition {
            calls: Arc::clone(&calls),
        })
        .expect("register counted tool");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(registry.definitions()[0].name, "counted");
    assert_eq!(
        registry.definition("counted").map(|item| item.name),
        Some("counted")
    );

    let workspace = tempdir().expect("temporary workspace");
    let result = registry.execute_for_tests(
        &ToolCall {
            id: "call-counted".to_owned(),
            name: "counted".to_owned(),
            arguments: json!({}),
        },
        workspace.path(),
    );

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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

    let result = registry.execute_for_tests(&call, workspace.path());

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

    let result = registry.execute_for_tests(&call, workspace.path());

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

    let result = registry.execute_for_tests(&call, workspace.path());

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
        vec![
            "read_file",
            "list_files",
            "edit_file",
            "run_command",
            "read_document",
            "xana_docs",
        ]
    );
    assert_eq!(definitions[0].effect_class, EffectClass::Read);
    assert_eq!(definitions[0].replay_safety, ReplaySafety::Safe);
    assert_eq!(definitions[1].effect_class, EffectClass::Read);
    assert_eq!(definitions[1].replay_safety, ReplaySafety::Safe);
    assert_eq!(definitions[2].effect_class, EffectClass::Write);
    assert_eq!(definitions[2].replay_safety, ReplaySafety::Never);
    assert_eq!(definitions[3].effect_class, EffectClass::Execute);
    assert_eq!(definitions[3].replay_safety, ReplaySafety::Never);
    assert_eq!(definitions[4].effect_class, EffectClass::Read);
    assert_eq!(definitions[4].replay_safety, ReplaySafety::Safe);
    assert_eq!(definitions[5].effect_class, EffectClass::Read);
    assert_eq!(definitions[5].replay_safety, ReplaySafety::Safe);
}

#[test]
fn builtins_dispatch_read_list_and_edit_with_call_ids() {
    use std::fs;

    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("state.txt"), "status=rough\n").expect("state fixture");
    let registry = ToolRegistry::builtins_for_tests().expect("built-in registry");

    let list = registry.execute_for_tests(
        &ToolCall {
            id: "call-list".to_owned(),
            name: "list_files".to_owned(),
            arguments: json!({"path": "."}),
        },
        workspace.path(),
    );
    let edit = registry.execute_for_tests(
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
    let read = registry.execute_for_tests(
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

struct FakeEffect {
    effects: Arc<AtomicUsize>,
    fail: bool,
}

impl Tool for FakeEffect {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fake_effect",
            contract_version: 1,
            description: "Test the unified permission boundary",
            parameters: json!({"type": "object"}),
            effect_class: EffectClass::Write,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        if arguments.get("invalid").and_then(Value::as_bool) == Some(true) {
            return Err("invalid fake arguments".to_owned());
        }
        Ok(PlannedToolInvocation::new(
            arguments.clone(),
            PermissionScope::Unscoped,
            arguments.clone(),
        ))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let executable = planned.executable::<Value>("fake_effect")?;
            assert_eq!(executable, &planned.final_arguments);
            self.effects.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err("effect failed after authorization".to_owned())
            } else {
                Ok("effect complete".to_owned())
            }
        })
    }
}

fn fake_registry(effects: Arc<AtomicUsize>, fail: bool) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(FakeEffect { effects, fail })
        .expect("fake tool");
    registry
}

fn broker_for(
    default: PolicyDecision,
    controller: bool,
    workspace: &std::path::Path,
) -> (
    crate::permission::PermissionBrokerHandle,
    tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) {
    let policy = PermissionPolicy::new(default, Vec::new(), workspace).expect("test policy");
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (broker, _task) = PermissionBroker::spawn(policy, controller, events);
    (broker, receiver)
}

async fn invoke_fake(
    registry: &ToolRegistry,
    workspace: &std::path::Path,
    permissions: &crate::permission::PermissionBrokerHandle,
    operation_id: OperationId,
    invocation_id: ToolInvocationId,
) -> ToolResult {
    registry
        .invoke(
            &ToolCall {
                id: "provider-call".to_owned(),
                name: "fake_effect".to_owned(),
                arguments: json!({"value": 7}),
            },
            ToolContext {
                workspace_root: workspace,
                operation_id,
                invocation_id,
                permissions,
            },
        )
        .await
}

#[tokio::test]
async fn deny_performs_zero_effects_and_allow_performs_exactly_one() {
    let workspace = tempdir().expect("workspace");
    let denied_effects = Arc::new(AtomicUsize::new(0));
    let denied_registry = fake_registry(Arc::clone(&denied_effects), false);
    let (deny_broker, _events) = broker_for(PolicyDecision::Deny, false, workspace.path());
    let denied = invoke_fake(
        &denied_registry,
        workspace.path(),
        &deny_broker,
        OperationId::new(),
        ToolInvocationId::new(),
    )
    .await;
    assert_eq!(denied_effects.load(Ordering::SeqCst), 0);
    assert_eq!(denied.status, ToolResultStatus::Error);
    assert!(denied.output.contains("permission denied"));

    let allowed_effects = Arc::new(AtomicUsize::new(0));
    let allowed_registry = fake_registry(Arc::clone(&allowed_effects), false);
    let (allow_broker, _events) = broker_for(PolicyDecision::Allow, false, workspace.path());
    let allowed = invoke_fake(
        &allowed_registry,
        workspace.path(),
        &allow_broker,
        OperationId::new(),
        ToolInvocationId::new(),
    )
    .await;
    assert_eq!(allowed_effects.load(Ordering::SeqCst), 1);
    assert_eq!(allowed.status, ToolResultStatus::Success);
}

#[tokio::test]
async fn ask_waits_for_matching_control_decision_and_audit_uses_final_arguments() {
    let workspace = tempdir().expect("workspace");
    let effects = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(fake_registry(Arc::clone(&effects), false));
    let (broker, mut events) = broker_for(PolicyDecision::Ask, true, workspace.path());
    let operation_id = OperationId::new();
    let invocation_id = ToolInvocationId::new();
    let waiter = {
        let registry = Arc::clone(&registry);
        let broker = broker.clone();
        let workspace = workspace.path().to_owned();
        tokio::spawn(async move {
            invoke_fake(&registry, &workspace, &broker, operation_id, invocation_id).await
        })
    };

    assert!(matches!(
        events.recv().await,
        Some(AgentEvent::OperationStateChanged {
            operation_id: actual,
            state: crate::runtime::OperationState::Suspended,
        }) if actual == operation_id
    ));
    let request = match events.recv().await.expect("permission request") {
        AgentEvent::PermissionRequested { request } => request,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(request.final_arguments, json!({"value": 7}));
    broker
        .decide(operation_id, invocation_id, ControllerDecision::AllowOnce)
        .await
        .expect("matching decision");
    let result = waiter.await.expect("tool task");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(result.status, ToolResultStatus::Success);

    assert!(matches!(
        events.recv().await,
        Some(AgentEvent::OperationStateChanged {
            state: crate::runtime::OperationState::Running,
            ..
        })
    ));
    assert!(matches!(
        events.recv().await,
        Some(AgentEvent::PermissionAudited { fact })
            if fact.request.final_arguments == json!({"value": 7})
                && fact.controller_decision == Some(ControllerDecision::AllowOnce)
    ));
}

#[tokio::test]
async fn tool_failure_remains_distinct_from_permission_denial() {
    let workspace = tempdir().expect("workspace");
    let effects = Arc::new(AtomicUsize::new(0));
    let registry = fake_registry(Arc::clone(&effects), true);
    let (broker, _events) = broker_for(PolicyDecision::Allow, false, workspace.path());
    let result = invoke_fake(
        &registry,
        workspace.path(),
        &broker,
        OperationId::new(),
        ToolInvocationId::new(),
    )
    .await;

    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.output, "effect failed after authorization");
}

#[tokio::test]
async fn observer_loss_never_defaults_a_pending_ask_to_allow() {
    let workspace = tempdir().expect("workspace");
    let effects = Arc::new(AtomicUsize::new(0));
    let registry = fake_registry(Arc::clone(&effects), false);
    let (broker, events) = broker_for(PolicyDecision::Ask, true, workspace.path());
    drop(events);

    let result = invoke_fake(
        &registry,
        workspace.path(),
        &broker,
        OperationId::new(),
        ToolInvocationId::new(),
    )
    .await;
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result.output.contains("permission denied"));
}

#[tokio::test]
async fn invalid_arguments_never_reach_permission_evaluation() {
    let workspace = tempdir().expect("workspace");
    let effects = Arc::new(AtomicUsize::new(0));
    let registry = fake_registry(Arc::clone(&effects), false);
    let (broker, mut events) = broker_for(PolicyDecision::Allow, false, workspace.path());
    let result = registry
        .invoke(
            &ToolCall {
                id: "invalid-call".to_owned(),
                name: "fake_effect".to_owned(),
                arguments: json!({"invalid": true}),
            },
            ToolContext {
                workspace_root: workspace.path(),
                operation_id: OperationId::new(),
                invocation_id: ToolInvocationId::new(),
                permissions: &broker,
            },
        )
        .await;

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}
