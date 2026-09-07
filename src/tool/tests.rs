use super::*;
use crate::message::ToolResultStatus;
use crate::{
    native_runtime::AgentEvent,
    permission::{ControllerDecision, PermissionBroker, PermissionPolicy, PolicyDecision},
};
use futures::future::BoxFuture;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tempfile::tempdir;

struct Echo;

#[test]
fn unavailable_registration_reserves_names_without_a_schema_or_execution() {
    let mut registry = ToolRegistry::new();
    registry
        .register_unavailable("echo", "not configured")
        .unwrap();
    assert!(registry.definitions().is_empty());
    assert!(registry.register(Echo).is_err());
    assert!(registry.register_unavailable("echo", "different").is_err());
    let call = ToolCall {
        id: "stale".into(),
        name: "echo".into(),
        arguments: json!({}),
    };
    let result = registry
        .plan_in_turn(&call, Path::new("."), None)
        .err()
        .unwrap();
    assert_eq!(
        result.failure,
        Some(crate::message::ToolFailure::Unavailable)
    );
    assert_eq!(result.call_id, "stale");
    let mut registry = ToolRegistry::new();
    registry.register(Echo).unwrap();
    assert!(
        registry
            .register_unavailable("echo", "not configured")
            .is_err()
    );
}

impl Tool for Echo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            contract_version: 1,
            description: "Return a fixed test value".into(),
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
            name: "always_fails".into(),
            contract_version: 1,
            description: "Return a fixed test failure".into(),
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
            name: "counted".into(),
            contract_version: 1,
            description: "Prove definitions are cached".into(),
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
        definitions
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
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
        registry
            .definition("counted")
            .map(|item| item.name.as_str()),
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

    assert_eq!(
        result,
        Err(RegistryError::DuplicateName {
            name: "echo".into(),
        })
    );
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
fn builtins_expose_one_ordered_schema_and_safety_contract() {
    let registry = ToolRegistry::builtins_for_tests().expect("built-in registry");
    let definitions = registry.definitions();

    assert_eq!(
        definitions
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "read_file",
            "list_files",
            "find_files",
            "grep_files",
            "write_file",
            "edit_file",
            "run_command",
            "web_fetch",
            "read_document",
            "xana_docs",
        ]
    );
    assert_eq!(
        definitions
            .iter()
            .map(|definition| (definition.effect_class, definition.replay_safety))
            .collect::<Vec<_>>(),
        vec![
            (EffectClass::Read, ReplaySafety::Safe),
            (EffectClass::Read, ReplaySafety::Safe),
            (EffectClass::Read, ReplaySafety::Safe),
            (EffectClass::Read, ReplaySafety::Safe),
            (EffectClass::Write, ReplaySafety::Never),
            (EffectClass::Write, ReplaySafety::Never),
            (EffectClass::Execute, ReplaySafety::Never),
            (EffectClass::Network, ReplaySafety::Safe),
            (EffectClass::Read, ReplaySafety::Safe),
            (EffectClass::Read, ReplaySafety::Safe),
        ]
    );

    let read = &registry.definition("read_file").unwrap().parameters;
    assert_eq!(read["properties"]["start_line"]["minimum"], 1);
    assert_eq!(read["properties"]["end_line"]["minimum"], 1);
    assert_eq!(read["properties"]["max_bytes"]["maximum"], 65536);
    let find = &registry.definition("find_files").unwrap().parameters;
    assert_eq!(find["properties"]["max_depth"]["maximum"], 32);
    let grep = &registry.definition("grep_files").unwrap().parameters;
    assert_eq!(grep["properties"]["max_matches"]["maximum"], 1000);
    let write = registry.definition("write_file").unwrap();
    assert_eq!(write.parameters["properties"]["mode"]["enum"][0], "create");
    assert_eq!(write.effect_class, EffectClass::Write);
    let command = &registry.definition("run_command").unwrap().parameters;
    assert_eq!(command["additionalProperties"], false);
    assert_eq!(command["properties"]["timeout_ms"]["maximum"], 120000);
    let web = registry.definition("web_fetch").unwrap();
    assert_eq!(web.effect_class, EffectClass::Network);
    assert_eq!(web.replay_safety, ReplaySafety::Safe);
    assert_eq!(web.parameters["properties"]["redirects"]["maxItems"], 3);
    let document = registry.definition("read_document").unwrap();
    assert_eq!(
        document.description.contains("CSV"),
        cfg!(feature = "documents-csv")
    );
    let docs = &registry.definition("xana_docs").unwrap().parameters;
    assert_eq!(docs["properties"]["max_bytes"]["maximum"], 32768);
}

#[test]
fn builtins_dispatch_typed_file_workflow_with_correlated_serialized_results() {
    use std::fs;

    let workspace = tempdir().expect("temporary workspace");
    fs::create_dir(workspace.path().join("notes")).expect("notes directory");
    fs::write(
        workspace.path().join("state.txt"),
        "status=rough\nowner=unknown\n",
    )
    .expect("state fixture");
    let registry = ToolRegistry::builtins_for_tests().expect("built-in registry");

    let write = registry.execute_for_tests(
        &ToolCall {
            id: "call-write".to_owned(),
            name: "write_file".to_owned(),
            arguments: json!({
                "path": "notes/context.txt",
                "content": "alpha beta gamma\n",
                "mode": "create"
            }),
        },
        workspace.path(),
    );
    let list = registry.execute_for_tests(
        &ToolCall {
            id: "call-list".to_owned(),
            name: "list_files".to_owned(),
            arguments: json!({"path": "."}),
        },
        workspace.path(),
    );
    let find = registry.execute_for_tests(
        &ToolCall {
            id: "call-find".to_owned(),
            name: "find_files".to_owned(),
            arguments: json!({"pattern": "**/*.txt"}),
        },
        workspace.path(),
    );
    let grep = registry.execute_for_tests(
        &ToolCall {
            id: "call-grep".to_owned(),
            name: "grep_files".to_owned(),
            arguments: json!({"query": "beta", "glob": "**/*.txt"}),
        },
        workspace.path(),
    );
    let edit = registry.execute_for_tests(
        &ToolCall {
            id: "call-edit".to_owned(),
            name: "edit_file".to_owned(),
            arguments: json!({
                "path": "state.txt",
                "edits": [
                    {
                        "old_text": "status=rough",
                        "new_text": "status=ready"
                    },
                    {
                        "old_text": "owner=unknown",
                        "new_text": "owner=xana"
                    }
                ]
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
    let page = registry.execute_for_tests(
        &ToolCall {
            id: "call-page".to_owned(),
            name: "read_file".to_owned(),
            arguments: json!({"path": "notes/context.txt", "offset_bytes": 0, "max_bytes": 8}),
        },
        workspace.path(),
    );

    assert_eq!(write.call_id, "call-write");
    assert_eq!(write.status, ToolResultStatus::Success);
    assert_eq!(list.call_id, "call-list");
    assert_eq!(list.status, ToolResultStatus::Success);
    assert!(list.output.contains("state.txt"));
    assert_eq!(find.call_id, "call-find");
    assert_eq!(find.status, ToolResultStatus::Success);
    assert_eq!(
        serde_json::from_str::<Value>(&find.output).unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(grep.call_id, "call-grep");
    assert_eq!(grep.status, ToolResultStatus::Success);
    assert_eq!(
        serde_json::from_str::<Value>(&grep.output).unwrap()["matches"][0]["path"],
        "notes/context.txt"
    );
    assert_eq!(edit.call_id, "call-edit");
    assert_eq!(edit.status, ToolResultStatus::Success);
    assert_eq!(read.call_id, "call-read");
    assert_eq!(read.status, ToolResultStatus::Success);
    assert_eq!(read.output, "status=ready\nowner=xana\n");
    assert_eq!(page.call_id, "call-page");
    assert_eq!(page.status, ToolResultStatus::Success);
    let page: Value = serde_json::from_str(&page.output).expect("paged read result");
    assert_eq!(page["content"], "alpha be");
    assert_eq!(page["next_offset_bytes"], 8);
}

struct FakeEffect {
    effects: Arc<AtomicUsize>,
    fail: bool,
}

impl Tool for FakeEffect {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fake_effect".into(),
            contract_version: 1,
            description: "Test the unified permission boundary".into(),
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
                events: None,
                cleanup: DeferredCleanup::default(),
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
            state: crate::native_runtime::OperationState::Suspended,
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
            state: crate::native_runtime::OperationState::Running,
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
                events: None,
                cleanup: DeferredCleanup::default(),
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
#[tokio::test]
async fn deferred_cleanup_survives_an_interrupted_waiter() {
    let cleanup = DeferredCleanup::default();
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_release = Arc::clone(&release);
    let task_completed = Arc::clone(&completed);
    assert!(cleanup.schedule(Box::pin(async move {
        task_release.notified().await;
        task_completed.store(true, std::sync::atomic::Ordering::SeqCst);
    })));

    let interrupted_scope = cleanup.clone();
    let waiter = tokio::spawn(async move { interrupted_scope.drain().await });
    tokio::task::yield_now().await;
    waiter.abort();
    let _ = waiter.await;

    release.notify_waiters();
    cleanup.drain().await;
    assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn deferred_cleanup_scopes_do_not_drain_each_other() {
    let first = DeferredCleanup::default();
    let second = DeferredCleanup::default();
    let release = Arc::new(tokio::sync::Notify::new());
    let first_release = Arc::clone(&release);
    assert!(first.schedule(Box::pin(async move {
        first_release.notified().await;
    })));
    let second_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = Arc::clone(&second_finished);
    assert!(second.schedule(Box::pin(async move {
        finished.store(true, std::sync::atomic::Ordering::SeqCst);
    })));

    tokio::time::timeout(Duration::from_secs(1), second.drain())
        .await
        .expect("the independent cleanup scope finishes");
    assert!(second_finished.load(std::sync::atomic::Ordering::SeqCst));

    release.notify_waiters();
    first.drain().await;
}
