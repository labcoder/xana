use super::*;
use crate::{
    identity::{OperationId, ToolInvocationId},
    memory::{MemoryContext, MemoryControlEdit, MemoryEdit, MemoryScope},
    message::{ToolCall, ToolResult, ToolResultStatus},
    native_runtime::AgentEvent,
    permission::{
        ControllerDecision, PermissionBroker, PermissionPolicy, PermissionScope, PolicyDecision,
    },
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
    tool::{DeferredCleanup, PreparedToolInvocation, ToolContext},
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Fixture {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    owner: MemoryOwner,
    custody: TestCustody,
    registry: ToolRegistry,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let custody = TestCustody::default();
        let store =
            ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody)
                .unwrap();
        let owner = MemoryOwner::new(
            store,
            MemoryContext {
                conversation: Some(Uuid::new_v4()),
                project: Some(Uuid::new_v4()),
                profile: Some(Uuid::new_v4()),
            },
        );
        let mut registry = ToolRegistry::new();
        register(&mut registry, Some(owner.clone())).unwrap();
        Self {
            home,
            workspace: tempfile::tempdir().unwrap(),
            owner,
            custody,
            registry,
        }
    }

    fn prepare<'a>(&'a self, call: &ToolCall, turn: &OwnerTurnInput) -> PreparedToolInvocation<'a> {
        self.registry
            .plan_in_turn(call, self.workspace.path(), Some(turn))
            .unwrap()
    }

    async fn invoke(&self, call: &ToolCall, turn: &OwnerTurnInput, approve: bool) -> ToolResult {
        let policy =
            PermissionPolicy::new(PolicyDecision::Ask, vec![], self.workspace.path()).unwrap();
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (permissions, broker) = PermissionBroker::spawn(policy, approve, events);
        let responder = permissions.clone();
        let listener = tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if let AgentEvent::PermissionRequested { request } = event {
                    responder
                        .decide(
                            request.operation_id,
                            request.invocation_id,
                            ControllerDecision::AllowOnce,
                        )
                        .await
                        .unwrap();
                }
            }
        });
        let result = self
            .registry
            .invoke_in_turn(
                call,
                ToolContext {
                    workspace_root: self.workspace.path(),
                    operation_id: turn.operation_id,
                    invocation_id: ToolInvocationId::new(),
                    permissions: &permissions,
                    events: None,
                    cleanup: DeferredCleanup::default(),
                },
                Some(turn),
            )
            .await;
        permissions.shutdown();
        broker.await.unwrap();
        listener.await.unwrap();
        result
    }
}

fn input(text: &str) -> OwnerTurnInput {
    OwnerTurnInput {
        operation_id: OperationId::new(),
        source_id: Uuid::new_v4(),
        text: Arc::from(text),
        cancellation: CancellationToken::new(),
    }
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: Uuid::new_v4().to_string(),
        name: name.into(),
        arguments,
    }
}

fn remember(statement: &str, quote: &str) -> ToolCall {
    call(
        "memory_update",
        json!({"action":"remember","statement":statement,"quote":quote,"risk":"ordinary"}),
    )
}

async fn execute(
    prepared: &PreparedToolInvocation<'_>,
    turn: &OwnerTurnInput,
) -> Result<String, String> {
    prepared
        .execute(ToolExecutionContext {
            operation_id: turn.operation_id,
            events: None,
            outbound_approval: None,
            cleanup: DeferredCleanup::default(),
        })
        .await
}

#[tokio::test]
async fn ordinary_foreground_save_uses_protected_storage_and_stable_source_receipt() {
    let fixture = Fixture::new();
    let turn = input("Mon thé préféré est le thé vert. Garde cela pour cette conversation.");
    let request = remember("Mon thé préféré est le thé vert", &turn.text);
    let result = fixture.invoke(&request, &turn, false).await;
    assert_eq!(
        result.status,
        ToolResultStatus::Success,
        "{}",
        result.output
    );
    let receipt: Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(receipt["committed"], true);
    assert_eq!(receipt["source_id"], turn.source_id.to_string());
    let records = fixture.owner.page(None, None).unwrap().records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].created.owner_request, turn.source_id);
    assert_eq!(
        records[0].scope,
        MemoryScope::Conversation(fixture.owner.context.conversation.unwrap())
    );
    assert_eq!(records[0].statement, "Mon thé préféré est le thé vert");
    assert_eq!(
        std::fs::read_dir(fixture.workspace.path()).unwrap().count(),
        0
    );
    let marker = fixture
        .owner
        .store
        .document(&format!("memory/explicit-source/{}", turn.source_id), 1024)
        .unwrap()
        .unwrap();
    assert!(!String::from_utf8(marker).unwrap().contains("thé"));
}

#[tokio::test]
async fn receipts_prevent_repeat_inserts_even_after_reopen_and_tool_call_id_change() {
    let fixture = Fixture::new();
    let turn = input("Remember that I prefer blue.");
    let request = remember("I prefer blue", &turn.text);
    let first = fixture.invoke(&request, &turn, false).await;
    assert_eq!(first.status, ToolResultStatus::Success, "{}", first.output);
    let reopened = ProtectedStore::open(fixture.home.path(), &fixture.custody).unwrap();
    let mut registry = ToolRegistry::new();
    register(
        &mut registry,
        Some(MemoryOwner::new(reopened, fixture.owner.context.clone())),
    )
    .unwrap();
    let repeated = remember("I prefer blue", &turn.text);
    let planned = registry
        .plan_in_turn(&repeated, fixture.workspace.path(), Some(&turn))
        .unwrap();
    assert_eq!(execute(&planned, &turn).await.unwrap(), first.output);
    assert_eq!(fixture.owner.page(None, None).unwrap().records.len(), 1);
    let mut forged_source = turn.clone();
    forged_source.source_id = Uuid::new_v4();
    let planned = registry
        .plan_in_turn(&repeated, fixture.workspace.path(), Some(&forged_source))
        .unwrap();
    assert!(
        execute(&planned, &forged_source)
            .await
            .unwrap_err()
            .contains("different owner source")
    );
}

#[tokio::test]
async fn broader_sensitive_and_unspecified_risk_require_review() {
    let fixture = Fixture::new();
    let turn = input("Remember that I prefer blue.");
    for args in [
        json!({"action":"remember","statement":"I prefer blue","quote":turn.text.as_ref(),"risk":"ordinary","scope":"user"}),
        json!({"action":"remember","statement":"I prefer blue","quote":turn.text.as_ref(),"risk":"sensitive"}),
        json!({"action":"remember","statement":"I prefer blue","quote":turn.text.as_ref()}),
    ] {
        let request = call("memory_update", args);
        assert!(matches!(
            fixture.prepare(&request, &turn).scope(),
            PermissionScope::PersonalMemory { review: true, .. }
        ));
        assert_eq!(
            fixture.invoke(&request, &turn, false).await.status,
            ToolResultStatus::Error
        );
    }
    assert!(fixture.owner.page(None, None).unwrap().records.is_empty());
    let request = call(
        "memory_update",
        json!({"action":"remember","statement":"I prefer blue","quote":turn.text.as_ref(),"risk":"ordinary","scope":"project"}),
    );
    assert_eq!(
        fixture.invoke(&request, &turn, true).await.status,
        ToolResultStatus::Success
    );
    assert_eq!(
        fixture.owner.page(None, None).unwrap().records[0].scope,
        MemoryScope::Project(fixture.owner.context.project.unwrap())
    );
}

#[test]
fn source_and_schema_fail_closed_without_foreground_authority() {
    let fixture = Fixture::new();
    let turn = input("What is my favorite color?");
    let request = remember("I prefer blue", "Remember that I prefer blue.");
    assert!(
        fixture
            .registry
            .plan_in_turn(&request, fixture.workspace.path(), Some(&turn))
            .is_err()
    );
    assert!(
        fixture
            .registry
            .plan(&request, fixture.workspace.path())
            .is_err()
    );
    for args in [
        json!({"action":"remember","statement":"blue","quote":turn.text.as_ref(),"scope":format!("project:{}", Uuid::new_v4())}),
        json!({"action":"remember","statement":"blue","quote":turn.text.as_ref(),"confirmed":true}),
        json!({"action":"remember","statement":"x".repeat(4097),"quote":turn.text.as_ref()}),
        json!({"action":"remember","statement":"blue\u{1b}","quote":turn.text.as_ref()}),
    ] {
        assert!(
            fixture
                .registry
                .plan_in_turn(
                    &call("memory_update", args),
                    fixture.workspace.path(),
                    Some(&turn)
                )
                .is_err()
        );
    }
    let mut registry = ToolRegistry::new();
    register(&mut registry, None).unwrap();
    let request = remember("blue", &turn.text);
    assert!(
        registry
            .plan_in_turn(&request, fixture.workspace.path(), Some(&turn))
            .is_err()
    );
    assert_eq!(registry.definitions().len(), 2);
}

#[tokio::test]
async fn commit_rechecks_no_memory_cancellation_restore_and_source_exclusion() {
    for fence in ["no_memory", "cancel", "restore", "source"] {
        let fixture = Fixture::new();
        let turn = input("Remember I prefer blue.");
        let request = remember("I prefer blue", &turn.text);
        let prepared = fixture.prepare(&request, &turn);
        match fence {
            "no_memory" => {
                fixture
                    .owner
                    .controls(
                        MemoryScope::Conversation(fixture.owner.context.conversation.unwrap()),
                        MemoryControlEdit {
                            no_memory: Some(true),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            "cancel" => turn.cancellation.cancel(),
            "restore" => fixture
                .owner
                .store
                .set_document("restore/review-required", b"restored", 4096)
                .unwrap(),
            "source" => {
                let record = fixture
                    .owner
                    .remember(MemoryScope::User, "Old source fact".into(), None)
                    .unwrap();
                fixture
                    .owner
                    .revise(record.id, record.revision, MemoryEdit::Forget)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(execute(&prepared, &turn).await.is_err(), "{fence}");
        assert!(
            fixture
                .owner
                .page(None, None)
                .unwrap()
                .records
                .iter()
                .all(|record| record.statement != "I prefer blue")
        );
        assert!(
            fixture
                .owner
                .store
                .document(&format!("memory/explicit-source/{}", turn.source_id), 1024)
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn correct_rechecks_revision_and_forget_is_idempotent_without_reactivation() {
    let fixture = Fixture::new();
    let record = fixture
        .owner
        .remember(MemoryScope::User, "I prefer red".into(), None)
        .unwrap();
    let turn = input("Correct my saved color to blue.");
    let correction = call(
        "memory_update",
        json!({"action":"correct","id":record.id,"revision":record.revision,"statement":"I prefer blue","quote":turn.text.as_ref(),"risk":"ordinary"}),
    );
    assert!(matches!(
        fixture.prepare(&correction, &turn).scope(),
        PermissionScope::PersonalMemory { review: true, .. }
    ));
    let changed = fixture
        .owner
        .revise(
            record.id,
            record.revision,
            MemoryEdit::Correct {
                statement: "I prefer green".into(),
                valid_until_unix_seconds: None,
            },
        )
        .unwrap();
    assert!(
        execute(&fixture.prepare(&correction, &turn), &turn)
            .await
            .unwrap_err()
            .contains("changed since inspection")
    );
    let forgetting = call(
        "memory_update",
        json!({"action":"forget","id":record.id,"revision":changed.revision}),
    );
    let result = fixture.invoke(&forgetting, &turn, true).await;
    assert_eq!(
        result.status,
        ToolResultStatus::Success,
        "{}",
        result.output
    );
    let repeated = execute(&fixture.prepare(&forgetting, &turn), &turn)
        .await
        .unwrap();
    assert_eq!(repeated, result.output);
    assert_eq!(
        fixture.owner.record(record.id).unwrap().state,
        crate::memory::MemoryState::Forgotten
    );
    assert!(!repeated.contains("green"));
}

#[tokio::test]
async fn lookup_is_scoped_bounded_paged_and_records_handoff() {
    let fixture = Fixture::new();
    for index in 0..70 {
        fixture
            .owner
            .remember(
                MemoryScope::User,
                format!("{index}: {}", "界".repeat(1000)),
                None,
            )
            .unwrap();
    }
    let secret = fixture
        .owner
        .remember(
            MemoryScope::Profile(Uuid::new_v4()),
            "PRIVATE_OTHER_PROFILE".into(),
            None,
        )
        .unwrap();
    let turn = input("Show the things you remember.");
    let request = call("memory_lookup", json!({"limit":8}));
    let result = fixture.invoke(&request, &turn, false).await;
    assert_eq!(
        result.status,
        ToolResultStatus::Success,
        "{}",
        result.output
    );
    assert!(result.output.len() < 16 * 1024 && !result.output.contains("PRIVATE_OTHER_PROFILE"));
    let result: Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(result["records"].as_array().unwrap().len(), 8);
    assert!(
        result["records"][0]["statement_truncated"]
            .as_bool()
            .unwrap()
    );
    let after = result["next_after"].as_u64().unwrap();
    let page = fixture
        .invoke(
            &call("memory_lookup", json!({"after":after,"limit":8})),
            &turn,
            false,
        )
        .await;
    assert_eq!(page.status, ToolResultStatus::Success);
    let key = format!(
        "memory/handoff/{}",
        fixture.owner.context.conversation.unwrap()
    );
    let ids: Vec<Uuid> = serde_json::from_slice(
        &fixture
            .owner
            .store
            .document(&key, 128 * 1024)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(ids.len(), 16);
    assert!(!ids.contains(&secret.id));
    let missing = fixture
        .invoke(
            &call("memory_lookup", json!({"query":"not present"})),
            &turn,
            false,
        )
        .await;
    let missing: Value = serde_json::from_str(&missing.output).unwrap();
    assert!(missing["records"].as_array().unwrap().is_empty());
    assert!(
        missing["next_after"].is_u64(),
        "query work stops after 64 records"
    );
}

#[tokio::test]
async fn disabled_lookup_and_foreign_record_edits_never_reveal_memory() {
    let fixture = Fixture::new();
    let foreign = fixture
        .owner
        .remember(
            MemoryScope::Conversation(Uuid::new_v4()),
            "foreign".into(),
            None,
        )
        .unwrap();
    let turn = input("Please forget that memory.");
    let request = call(
        "memory_update",
        json!({"action":"forget","id":foreign.id,"revision":foreign.revision}),
    );
    assert!(
        fixture
            .registry
            .plan_in_turn(&request, fixture.workspace.path(), Some(&turn))
            .is_err()
    );
    fixture
        .owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                use_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    let result = fixture
        .invoke(&call("memory_lookup", json!({})), &turn, false)
        .await;
    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result.output.contains("disabled"));
}

#[tokio::test]
async fn multilingual_owner_requests_share_one_schema_without_a_language_parser() {
    let fixture = Fixture::new();
    for (request, statement) in [
        (
            "覚えておいてください。私は短い回答が好きです。",
            "私は短い回答が好きです",
        ),
        ("تذكر أنني أفضل الشاي الأخضر", "أفضل الشاي الأخضر"),
        (
            "Recuerda que prefiero ejemplos de Rust.",
            "Prefiero ejemplos de Rust",
        ),
        (
            "Speichere bitte: Ich mag die Farbe Grün.",
            "Ich mag die Farbe Grün",
        ),
        (
            "Remember this exact preference:\nI prefer `Result<T, E>` examples.",
            "I prefer `Result<T, E>` examples",
        ),
    ] {
        let turn = input(request);
        let result = fixture
            .invoke(&remember(statement, request), &turn, false)
            .await;
        assert_eq!(
            result.status,
            ToolResultStatus::Success,
            "{}",
            result.output
        );
    }
    assert_eq!(fixture.owner.page(None, None).unwrap().records.len(), 5);
}

#[test]
fn malformed_lookup_and_update_arguments_are_rejected_before_execution() {
    let fixture = Fixture::new();
    let turn = input("Remember blue.");
    for (name, args) in [
        ("memory_lookup", json!({"limit":0})),
        ("memory_lookup", json!({"limit":9})),
        ("memory_lookup", json!({"query":"界".repeat(100)})),
        ("memory_lookup", json!({"query":["unexpected"]})),
        ("memory_lookup", json!({"after":u64::MAX})),
        ("memory_lookup", json!({"scope":"project:other"})),
        (
            "memory_update",
            json!({"action":"remember","statement":"x".repeat(16385),"quote":"Remember blue."}),
        ),
        (
            "memory_update",
            json!({"action":"remember","statement":{"hidden":"value"},"quote":"Remember blue."}),
        ),
        (
            "memory_update",
            json!({"action":"forget","id":Uuid::nil(),"revision":1}),
        ),
        (
            "memory_update",
            json!({"action":"forget","id":Uuid::new_v4(),"revision":0}),
        ),
    ] {
        assert!(
            fixture
                .registry
                .plan_in_turn(&call(name, args), fixture.workspace.path(), Some(&turn))
                .is_err()
        );
    }
    assert!(fixture.owner.page(None, None).unwrap().records.is_empty());
}
