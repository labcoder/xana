//! The privacy fence must survive composition into a real next-turn request.
use super::*;
use crate::{
    agent::Agent,
    context::ContextBudget,
    identity::{OperationId, SessionId, StepId},
    message::{Message, Role},
    native_runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
    permission::{PermissionPolicy, PolicyDecision},
    prompt::{
        ModelBudgetFacts, PromptAssembler, PromptBudgetPlan, PromptBudgetPolicy, PromptEnvironment,
        PromptSurface,
    },
    provider::{ConversationalProvider, DeltaSink, ProviderError},
    session::DurableSession,
    tool::{ToolDefinition, ToolRegistry},
};
use futures::future::BoxFuture;
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const OLD: &str = "I prefer OLD_FORGOTTEN_PROMPT_CANARY examples";
const CORRECTED: &str = "I prefer CORRECTED_FORGOTTEN_PROMPT_CANARY examples";
const RETAINED: &str = "I prefer ALLOWED_CURRENT_PROMPT_CANARY examples";

struct CaptureProvider(Arc<Mutex<Option<Vec<Message>>>>);

impl ConversationalProvider for CaptureProvider {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        _step_id: StepId,
        _deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            assert!(tools.is_empty());
            let mut request = self.0.lock().unwrap();
            assert!(request.is_none(), "one foreground request, no helper calls");
            *request = Some(messages.to_vec());
            Ok(Message::text(Role::Assistant, "A synthetic answer."))
        })
    }
}

async fn capture_fresh_turn(store: &ProtectedStore, workspace: &Path) -> String {
    let session_id = SessionId::new();
    let session =
        DurableSession::create_protected(store.clone(), workspace.to_owned(), session_id).unwrap();
    let owner = MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(session_id.to_string().parse().unwrap()),
            ..Default::default()
        },
    );
    let budget = PromptBudgetPlan::derive(
        &PromptBudgetPolicy::default(),
        ModelBudgetFacts {
            connection: "synthetic".into(),
            model: "no-network".into(),
            context_tokens: Some(32_768),
            max_output_tokens: Some(2_048),
            reasoning: false,
        },
    )
    .unwrap();
    let tools = ToolRegistry::new();
    let assembler = PromptAssembler::new(
        vec![],
        PromptEnvironment {
            connection: "synthetic".into(),
            model: "no-network".into(),
            operating_system: "fixture".into(),
            working_directory: workspace.to_owned(),
            configured_shell: "no commands permitted".into(),
            surface: PromptSurface::Cli,
        },
        None,
        ContextBudget {
            total_tokens: budget.input_budget_tokens,
            conversation_reserve_tokens: budget.conversation_reserve_tokens,
        },
    )
    .with_budget_plan(budget);
    let request = Arc::new(Mutex::new(None));
    let agent = Agent::new(
        Box::new(CaptureProvider(request.clone())),
        tools,
        workspace.to_owned(),
        assembler.assemble(&[]).unwrap(),
        2,
    );
    let policy = PermissionPolicy::new(PolicyDecision::Deny, vec![], workspace).unwrap();
    let mut runtime =
        RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler, Some(owner))
            .unwrap();
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "Explain the difference between a list and a set.".into(),
        })
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match runtime.next_event().await {
                Some(AgentEvent::OperationStateChanged {
                    operation_id: actual,
                    state: OperationState::Finished(outcome),
                }) if actual == operation_id => return Ok(outcome),
                Some(AgentEvent::CommandRejected { reason }) => return Err(reason),
                None => return Err("runtime event stream closed".into()),
                _ => {}
            }
        }
    })
    .await;
    // Join even when the turn fails; no detached runtime may outlive the fixture.
    assert!(runtime.shutdown_owned().await);
    assert_eq!(outcome.unwrap().unwrap(), OperationOutcome::Completed);
    let captured = request.lock().unwrap().take().expect("one model request");
    serde_json::to_string(&captured).unwrap()
}

#[tokio::test]
async fn restored_next_turn_omits_forgotten_revisions_before_and_after_owner_review() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
    let recovery = RecoveryIdentity::generate();
    let custody = TestCustody::default();
    let store = ProtectedStore::initialize(paths.data_dir(), &recovery, &custody).unwrap();
    let forgotten_context = MemoryContext {
        conversation: Some(Uuid::new_v4()),
        ..Default::default()
    };
    let owner = MemoryOwner::new(store.clone(), forgotten_context.clone());
    let fact = owner.remember(MemoryScope::User, OLD.into(), None).unwrap();
    // A different source proves that the fence does not simply suppress all
    // memory forever after one Conversation is forgotten.
    let other_owner = MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(Uuid::new_v4()),
            ..Default::default()
        },
    );
    other_owner
        .remember(MemoryScope::User, RETAINED.into(), None)
        .unwrap();
    let snapshot = store
        .backup(&BackupPolicy::default(), 1000, false)
        .unwrap()
        .snapshot
        .unwrap();
    let corrected = owner
        .revise(
            fact.id,
            fact.revision,
            MemoryEdit::Correct {
                statement: CORRECTED.into(),
                valid_until_unix_seconds: None,
            },
        )
        .unwrap();
    let after_correction = capture_fresh_turn(&store, &root).await;
    assert!(after_correction.contains(CORRECTED));
    assert!(after_correction.contains(RETAINED));
    assert!(!after_correction.contains(OLD));
    owner
        .revise(fact.id, corrected.revision, MemoryEdit::Forget)
        .unwrap();
    let after_forgetting = capture_fresh_turn(&store, &root).await;
    assert!(after_forgetting.contains(RETAINED));
    assert!(!after_forgetting.contains(OLD));
    assert!(!after_forgetting.contains(CORRECTED));
    store.lock().unwrap();
    let reopened = ProtectedStore::unlock(paths.data_dir(), &custody).unwrap();
    assert!(owner.eligible().is_err(), "old capability stays revoked");
    drop(reopened);
    drop(other_owner);
    drop(owner);
    drop(store);

    let plan = restore::preview(&paths, &snapshot, &recovery).unwrap();
    restore::apply(&paths, &snapshot, &recovery, &plan.review).unwrap();
    let restored = ProtectedStore::recover(paths.data_dir(), &recovery).unwrap();
    let before_review = capture_fresh_turn(&restored, &root).await;
    for statement in [OLD, CORRECTED, RETAINED] {
        assert!(!before_review.contains(statement));
    }
    let review = restored.review_restored_memory(None).unwrap();
    restored
        .review_restored_memory(review["review"].as_str())
        .unwrap();
    let after_review = capture_fresh_turn(&restored, &root).await;
    assert!(after_review.contains(RETAINED));
    assert!(!after_review.contains(OLD));
    assert!(!after_review.contains(CORRECTED));
    let source_owner = MemoryOwner::new(restored.clone(), forgotten_context);
    assert_eq!(
        source_owner.record(fact.id).unwrap().state,
        MemoryState::Forgotten
    );
    restored.verify_content().unwrap();
}
