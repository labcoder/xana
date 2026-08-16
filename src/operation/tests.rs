use super::*;
use crate::{
    identity::{ConversationEntryId, ThreadId},
    message::{Message, Role},
    native_runtime::OperationOutcome,
    permission::{
        PermissionBroker, PermissionPolicy, PermissionRequest, PermissionScope, PolicyDecision,
    },
    session::{DurableSession, RestoredOperation, SessionRecord, SessionStore},
    tool::{EffectClass, PlannedToolInvocation, Tool, ToolDefinition},
};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::tempdir;

#[test]
fn diagnostic_bound_is_utf8_safe() {
    let bounded = bounded_diagnostic("\u{1f980}".repeat(MAX_OPERATION_DIAGNOSTIC_BYTES));

    assert!(bounded.len() <= MAX_OPERATION_DIAGNOSTIC_BYTES);
    assert!(bounded.chars().all(|character| character == '\u{1f980}'));
}

struct CountedTool {
    name: &'static str,
    contract_version: u32,
    replay_safety: ReplaySafety,
    effects: Arc<AtomicUsize>,
}

impl Tool for CountedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.to_owned(),
            contract_version: self.contract_version,
            description: "Count one deterministic test effect".into(),
            parameters: json!({"type": "object"}),
            effect_class: EffectClass::Read,
            replay_safety: self.replay_safety,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &Path,
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
        _context: crate::tool::ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok("observed".to_owned())
        })
    }
}

fn registry(
    name: &'static str,
    contract_version: u32,
    replay_safety: ReplaySafety,
    effects: Arc<AtomicUsize>,
) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools
        .register(CountedTool {
            name,
            contract_version,
            replay_safety,
            effects,
        })
        .expect("register counted tool");
    tools
}

fn permission_fact(
    operation_id: OperationId,
    invocation_id: ToolInvocationId,
    name: &str,
    arguments: &Value,
) -> PermissionAuditFact {
    PermissionAuditFact {
        request: PermissionRequest {
            operation_id,
            invocation_id,
            tool_name: name.to_owned(),
            effect_class: EffectClass::Read,
            final_arguments: arguments.clone(),
            scope: PermissionScope::Unscoped,
            outbound_review: None,
        },
        policy_evaluation: PolicyDecision::Allow,
        controller_decision: None,
        effective: PolicyDecision::Allow,
    }
}

fn pending_operation(
    saved_safety: ReplaySafety,
    name: &str,
    contract_version: u32,
) -> RestoredOperation {
    let operation_id = OperationId::new();
    let step_id = StepId::new();
    let invocation_id = ToolInvocationId::new();
    let arguments = json!({"value": 7});
    let intent = InvocationIntent {
        operation_id,
        step_id,
        invocation_id,
        result_id: ToolResultId::new(),
        model_call_id: "call-recovery".to_owned(),
        target: InvocationTarget::Tool {
            name: name.to_owned(),
            contract_version,
        },
        final_arguments: arguments.clone(),
        permission: permission_fact(operation_id, invocation_id, name, &arguments),
        saved_replay_safety: saved_safety,
    };
    RestoredOperation {
        operation_id,
        thread_id: ThreadId::new(),
        input_entry_id: ConversationEntryId::new(),
        step_order: vec![step_id],
        steps: BTreeMap::from([(step_id, ConversationEntryId::new())]),
        invocation_order: vec![invocation_id],
        intents: BTreeMap::from([(invocation_id, intent)]),
        results: BTreeMap::new(),
        recovery_decisions: Vec::new(),
        finished: None,
    }
}

#[test]
fn recovery_planner_requires_both_safe_declarations_and_same_contract() {
    let effects = Arc::new(AtomicUsize::new(0));
    let safe_tools = registry("counted", 1, ReplaySafety::Safe, Arc::clone(&effects));
    let never_tools = registry("counted", 1, ReplaySafety::Never, Arc::clone(&effects));
    let changed_tools = registry("counted", 2, ReplaySafety::Safe, Arc::clone(&effects));

    let cases = [
        (ReplaySafety::Safe, &safe_tools, None, "safe/safe replays"),
        (
            ReplaySafety::Safe,
            &never_tools,
            Some(InterruptionReason::CurrentReplayUnsafe),
            "current Never interrupts",
        ),
        (
            ReplaySafety::Never,
            &safe_tools,
            Some(InterruptionReason::SavedReplayUnsafe),
            "saved Never interrupts",
        ),
        (
            ReplaySafety::Never,
            &never_tools,
            Some(InterruptionReason::SavedReplayUnsafe),
            "Never/Never interrupts",
        ),
        (
            ReplaySafety::Safe,
            &changed_tools,
            Some(InterruptionReason::ContractChanged),
            "contract change interrupts",
        ),
    ];

    for (saved, current, expected_reason, label) in cases {
        let operation = pending_operation(saved, "counted", 1);
        let actions = plan_recovery(&operation, current).expect(label);
        match expected_reason {
            None => assert!(matches!(
                actions.as_slice(),
                [RecoveryAction::ReplayExactInvocation { .. }]
            )),
            Some(reason) => assert!(matches!(
                actions.as_slice(),
                [RecoveryAction::RecordInterruption { reason: actual, .. }] if *actual == reason
            )),
        }
    }

    let missing = pending_operation(ReplaySafety::Safe, "missing", 1);
    assert!(matches!(
        plan_recovery(&missing, &safe_tools)
            .expect("missing tool is classified")
            .as_slice(),
        [RecoveryAction::RecordInterruption {
            reason: InterruptionReason::MissingTool,
            ..
        }]
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn recovery_planner_preserves_completed_prefix_and_original_order() {
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = registry("counted", 1, ReplaySafety::Safe, effects);
    let mut operation = pending_operation(ReplaySafety::Safe, "counted", 1);
    let first_id = operation.invocation_order[0];
    let first_intent = operation.intents[&first_id].clone();
    operation.results.insert(
        first_id,
        InvocationResultRecord {
            operation_id: operation.operation_id,
            invocation_id: first_id,
            result_id: first_intent.result_id,
            outcome: InvocationOutcome::Completed {
                output: DurableValueRef::InlineJson(json!("done")),
            },
        },
    );
    let second_id = ToolInvocationId::new();
    let second_intent = InvocationIntent {
        invocation_id: second_id,
        result_id: ToolResultId::new(),
        model_call_id: "call-second".to_owned(),
        ..first_intent
    };
    operation.invocation_order.push(second_id);
    operation.intents.insert(second_id, second_intent);

    let actions = plan_recovery(&operation, &tools).expect("partial batch plan");

    assert!(matches!(
        actions.as_slice(),
        [
            RecoveryAction::AlreadyCompleted { .. },
            RecoveryAction::ContinueWithNextInvocation,
            RecoveryAction::ReplayExactInvocation { invocation_id }
        ] if *invocation_id == second_id
    ));
}

#[test]
fn recovery_planner_handles_no_intent_context_work_and_finished_operations() {
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = registry("counted", 1, ReplaySafety::Safe, effects);
    let mut no_intent = pending_operation(ReplaySafety::Safe, "counted", 1);
    no_intent.invocation_order.clear();
    no_intent.intents.clear();
    assert_eq!(
        plan_recovery(&no_intent, &tools).expect("no-intent plan"),
        vec![RecoveryAction::FinishOperation]
    );

    let mut context = pending_operation(ReplaySafety::Safe, "counted", 1);
    let invocation_id = context.invocation_order[0];
    context
        .intents
        .get_mut(&invocation_id)
        .expect("intent")
        .target = InvocationTarget::ContextTransform {
        operation: "snapshot-live-project".to_owned(),
    };
    assert!(matches!(
        plan_recovery(&context, &tools)
            .expect("context transform plan")
            .as_slice(),
        [RecoveryAction::RecordInterruption {
            reason: InterruptionReason::UnknownOutcome,
            ..
        }]
    ));

    let mut finished = no_intent;
    finished.finished = Some(OperationOutcome::Interrupted);
    assert_eq!(
        plan_recovery(&finished, &tools),
        Err(recovery::RecoveryError::AlreadyFinished)
    );
}

fn durable_pending_operation(
    saved_safety: ReplaySafety,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    DurableSession,
    OperationId,
    ToolInvocationId,
    ToolResultId,
) {
    let data = tempdir().expect("data directory");
    let workspace = tempdir().expect("workspace directory");
    let workspace_root = workspace.path().canonicalize().expect("workspace root");
    let mut session =
        DurableSession::create(data.path(), workspace_root).expect("create durable session");
    let operation_id = OperationId::new();
    let input_entry_id = session
        .append_message(Message::text(Role::User, "recover this"))
        .expect("append input");
    session
        .append_record(SessionRecord::OperationAccepted {
            operation_id,
            thread_id: session.thread_id(),
            input_entry_id,
        })
        .expect("accept operation");
    let assistant_entry_id = session
        .append_message(Message::text(Role::Assistant, "calling counted"))
        .expect("append assistant step");
    let step_id = StepId::new();
    session
        .append_record(SessionRecord::StepStarted {
            operation_id,
            step_id,
            assistant_entry_id,
        })
        .expect("start step");
    let invocation_id = ToolInvocationId::new();
    let result_id = ToolResultId::new();
    let arguments = json!({"value": 7});
    session
        .append_record(SessionRecord::PermissionAudited {
            fact: permission_fact(operation_id, invocation_id, "counted", &arguments),
        })
        .expect("append original audit");
    session
        .append_record(SessionRecord::InvocationIntentAppended {
            intent: InvocationIntent {
                operation_id,
                step_id,
                invocation_id,
                result_id,
                model_call_id: "call-recovery".to_owned(),
                target: InvocationTarget::Tool {
                    name: "counted".to_owned(),
                    contract_version: 1,
                },
                final_arguments: arguments,
                permission: permission_fact(
                    operation_id,
                    invocation_id,
                    "counted",
                    &json!({"value": 7}),
                ),
                saved_replay_safety: saved_safety,
            },
        })
        .expect("append intent");
    session
        .append_record(SessionRecord::OperationSuspended {
            operation_id,
            reason: SuspensionReason::ProcessInterrupted,
        })
        .expect("suspend operation");
    (
        data,
        workspace,
        session,
        operation_id,
        invocation_id,
        result_id,
    )
}

#[tokio::test]
async fn explicit_recovery_reauthorizes_and_replays_safe_effect_once() {
    let (data, _workspace, mut session, operation_id, invocation_id, result_id) =
        durable_pending_operation(ReplaySafety::Safe);
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = registry("counted", 1, ReplaySafety::Safe, Arc::clone(&effects));
    let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), session.workspace_root())
        .expect("allow policy");
    let (event_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    let (permissions, broker) =
        PermissionBroker::spawn_for_durable_runtime(policy, true, event_sender);

    let actions = execute_recovery(
        &mut session,
        operation_id,
        &tools,
        &permissions,
        &mut events,
        |_| panic!("allow policy must not ask"),
    )
    .await
    .expect("execute safe recovery");
    permissions.shutdown();
    broker.await.expect("broker task");

    assert!(matches!(
        actions.as_slice(),
        [RecoveryAction::ReplayExactInvocation { invocation_id: actual }] if *actual == invocation_id
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let restored = session
        .restored_operation(operation_id)
        .expect("restored operation");
    assert_eq!(restored.results[&invocation_id].result_id, result_id);
    assert_eq!(restored.finished, Some(OperationOutcome::Interrupted));
    assert!(matches!(
        restored.recovery_decisions.as_slice(),
        [(actual, RecoveryDecision::Replay)] if *actual == invocation_id
    ));
    let (_, restored_session) = DurableSession::inspect_restored(data.path(), session.session_id())
        .expect("inspect recovered named values");
    assert!(restored_session.named_values.values().any(|value| {
        value.operation_id == operation_id
            && value.name == format!("tool_output:{invocation_id}")
            && matches!(value.value, DurableValueRef::InlineJson(_))
    }));
}

#[tokio::test]
async fn accepted_operation_without_an_intent_finishes_without_an_effect() {
    let data = tempdir().expect("data directory");
    let workspace = tempdir().expect("workspace directory");
    let mut session = DurableSession::create(
        data.path(),
        workspace.path().canonicalize().expect("workspace root"),
    )
    .expect("create session");
    let operation_id = OperationId::new();
    let input_entry_id = session
        .append_message(Message::text(Role::User, "accepted only"))
        .expect("append input");
    session
        .append_record(SessionRecord::OperationAccepted {
            operation_id,
            thread_id: session.thread_id(),
            input_entry_id,
        })
        .expect("accept operation");
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = registry("counted", 1, ReplaySafety::Safe, Arc::clone(&effects));
    let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), session.workspace_root())
        .expect("allow policy");
    let (event_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    let (permissions, broker) =
        PermissionBroker::spawn_for_durable_runtime(policy, true, event_sender);

    let actions = execute_recovery(
        &mut session,
        operation_id,
        &tools,
        &permissions,
        &mut events,
        |_| panic!("no invocation may request permission"),
    )
    .await
    .expect("finish accepted operation");
    permissions.shutdown();
    broker.await.expect("broker task");

    assert_eq!(actions, vec![RecoveryAction::FinishOperation]);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(
        session
            .restored_operation(operation_id)
            .expect("operation")
            .finished,
        Some(OperationOutcome::Interrupted)
    );
}

#[tokio::test]
async fn recovery_denial_and_unsafe_unknown_never_repeat_effects() {
    for (saved, current, policy_decision, expected) in [
        (
            ReplaySafety::Safe,
            ReplaySafety::Safe,
            PolicyDecision::Deny,
            "denied",
        ),
        (
            ReplaySafety::Never,
            ReplaySafety::Safe,
            PolicyDecision::Allow,
            "unsafe",
        ),
        (
            ReplaySafety::Safe,
            ReplaySafety::Never,
            PolicyDecision::Allow,
            "changed",
        ),
    ] {
        let (_data, _workspace, mut session, operation_id, invocation_id, result_id) =
            durable_pending_operation(saved);
        let effects = Arc::new(AtomicUsize::new(0));
        let tools = registry("counted", 1, current, Arc::clone(&effects));
        let policy = PermissionPolicy::new(policy_decision, Vec::new(), session.workspace_root())
            .expect("recovery policy");
        let (event_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (permissions, broker) =
            PermissionBroker::spawn_for_durable_runtime(policy, true, event_sender);

        execute_recovery(
            &mut session,
            operation_id,
            &tools,
            &permissions,
            &mut events,
            |_| panic!("these policies do not ask"),
        )
        .await
        .expect(expected);
        permissions.shutdown();
        broker.await.expect("broker task");

        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expected}");
        let restored = session
            .restored_operation(operation_id)
            .expect("restored operation");
        assert_eq!(restored.results[&invocation_id].result_id, result_id);
        assert!(matches!(
            restored.results[&invocation_id].outcome,
            InvocationOutcome::Declined { .. } | InvocationOutcome::Interrupted { .. }
        ));
    }
}

#[test]
fn only_one_session_writer_can_own_recovery() {
    let (data, _workspace, session, _, _, _) = durable_pending_operation(ReplaySafety::Safe);
    let session_id = session.session_id();

    let second = DurableSession::resume(data.path(), session_id);

    let error = match second {
        Ok(_) => panic!("writer must be rejected"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("active writer or recovery controller"));
}

struct InspectingCrashObserver {
    target: CrashSite,
    session_path: PathBuf,
    snapshot: Mutex<Option<Vec<SessionRecord>>>,
}

impl BoundaryObserver for InspectingCrashObserver {
    fn reached(&self, site: CrashSite) -> Result<()> {
        if site == self.target {
            let loaded = SessionStore::inspect(&self.session_path)
                .context("inspect committed crash prefix")?;
            *self
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(
                loaded
                    .records
                    .into_iter()
                    .map(|record| record.record)
                    .collect(),
            );
            bail!("injected crash at {site:?}");
        }
        Ok(())
    }
}

async fn invoke_until_crash(site: CrashSite) -> (Vec<SessionRecord>, usize, OperationId) {
    let (_data, _workspace, mut session, _original_operation, _, _) =
        durable_pending_operation(ReplaySafety::Safe);
    // Remove the fixture's intent by creating a fresh accepted operation in the
    // same session; the executor under test owns all invocation records.
    let input_entry_id = session
        .append_message(Message::text(Role::User, "fresh crash case"))
        .expect("append fresh input");
    let fresh_operation = OperationId::new();
    session
        .append_record(SessionRecord::OperationAccepted {
            operation_id: fresh_operation,
            thread_id: session.thread_id(),
            input_entry_id,
        })
        .expect("accept fresh operation");
    let assistant_entry_id = session
        .append_message(Message::text(Role::Assistant, "fresh step"))
        .expect("append fresh assistant");
    let step_id = StepId::new();
    session
        .append_record(SessionRecord::StepStarted {
            operation_id: fresh_operation,
            step_id,
            assistant_entry_id,
        })
        .expect("start fresh step");
    let session_path = session.path().to_owned();
    let (commits, mut commands) = DurableOperationSender::channel();
    let writer = tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                DurableOperationCommand::Append {
                    record,
                    event: _,
                    acknowledged,
                } => {
                    let result = session
                        .append_record(*record)
                        .map_err(|error| format!("{error:#}"));
                    let _ = acknowledged.send(result);
                }
                DurableOperationCommand::StoreJson {
                    value,
                    acknowledged,
                } => {
                    let result = session
                        .store_json_value(value)
                        .map_err(|error| format!("{error:#}"));
                    let _ = acknowledged.send(result);
                }
            }
        }
        session
    });
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = registry("counted", 1, ReplaySafety::Safe, Arc::clone(&effects));
    let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), Path::new("."))
        .expect("allow policy");
    let (event_sender, _events) = tokio::sync::mpsc::unbounded_channel();
    let (permissions, broker) =
        PermissionBroker::spawn_for_durable_runtime(policy, true, event_sender);
    let observer = Arc::new(InspectingCrashObserver {
        target: site,
        session_path,
        snapshot: Mutex::new(None),
    });
    let executor = OperationExecutor::new(
        &tools,
        Path::new("."),
        permissions.clone(),
        commits.clone(),
        observer.clone(),
        None,
        crate::tool::DeferredCleanup::default(),
    );

    let result = executor
        .invoke_tool(
            fresh_operation,
            step_id,
            ToolInvocationId::new(),
            ToolCall {
                id: "call-crash".to_owned(),
                name: "counted".to_owned(),
                arguments: json!({"value": 1}),
            },
        )
        .await;
    assert!(result.is_err(), "crash site must stop the invocation");
    permissions.shutdown();
    broker.await.expect("broker task");
    drop(executor);
    drop(commits);
    let _session = writer.await.expect("writer task");
    let snapshot = observer
        .snapshot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("crash prefix snapshot");
    (snapshot, effects.load(Ordering::SeqCst), fresh_operation)
}

#[tokio::test]
async fn durable_invocation_crash_sites_expose_exact_committed_prefixes() {
    for (site, expect_intent, expect_result, expected_effects) in [
        (CrashSite::AfterPermissionAudit, false, false, 0),
        (CrashSite::AfterInvocationIntent, true, false, 0),
        (CrashSite::AfterEffectBeforeResult, true, false, 1),
        (CrashSite::AfterInvocationResult, true, true, 1),
    ] {
        let (records, effects, operation_id) = invoke_until_crash(site).await;
        assert_eq!(effects, expected_effects, "{site:?}");
        assert_eq!(
            records.iter().any(|record| matches!(
                record,
                SessionRecord::InvocationIntentAppended { intent }
                    if intent.operation_id == operation_id
            )),
            expect_intent,
            "{site:?}"
        );
        assert_eq!(
            records.iter().any(|record| matches!(
                record,
                SessionRecord::InvocationResultAppended { result }
                    if result.operation_id == operation_id
            )),
            expect_result,
            "{site:?}"
        );
    }
}
