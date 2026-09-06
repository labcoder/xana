//! Headless loaded-owner gates. Wall-clock samples are observations, not FPS or
//! universal latency guarantees; the ordinary gate asserts causal scheduling.

use super::*;
use crate::{
    identity::SessionId,
    memory::{MemoryContext, MemoryOwner, MemoryScope},
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
    telemetry::{ContextPhase, ContextPhaseEvent, RuntimeTelemetry, RuntimeTelemetryEvent},
};
use std::path::PathBuf;

const INITIAL_ENTRIES: usize = 386;
const MEMORY_FACTS: usize = 32;

#[derive(Default)]
struct ContextTimings(Mutex<Vec<ContextPhaseEvent>>);
impl RuntimeTelemetry for ContextTimings {
    fn record(&self, _: RuntimeTelemetryEvent) {}
    fn context_phase(&self, event: ContextPhaseEvent) {
        self.0.lock().unwrap().push(event);
    }
}

struct LoadedOwner {
    _directory: tempfile::TempDir,
    store: ProtectedStore,
    id: SessionId,
    root: PathBuf,
    memory: MemoryOwner,
}

impl LoadedOwner {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let store = ProtectedStore::initialize(
            directory.path(),
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
        )
        .unwrap();
        let id = SessionId::new();
        let memory = MemoryOwner::new(
            store.clone(),
            MemoryContext {
                conversation: Some(id.to_string().parse().unwrap()),
                ..Default::default()
            },
        );
        for index in 0..MEMORY_FACTS {
            memory
                .remember(
                    MemoryScope::User,
                    format!("Fixture preference {index}: concise Rust examples"),
                    None,
                )
                .unwrap();
        }
        let mut session =
            DurableSession::create_protected(store.clone(), root.clone(), id).unwrap();
        for index in 0..INITIAL_ENTRIES {
            let role = if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            session
                .append_message(Message::text(
                    role,
                    format!(
                        "Original fixture {index}: {}",
                        "evidence 日本語 λ ".repeat(16)
                    ),
                ))
                .unwrap();
        }
        let (_, assembler, _) = loaded_agent(&root, Arc::new(ContextTimings::default()));
        session
            .compact_conversation(
                OperationId::new(),
                CompactionReason::Manual,
                assembler.budget_plan().unwrap(),
            )
            .unwrap();
        assert!(session.prompt_continuation().unwrap().checkpoint.is_some());
        drop(session);
        Self {
            _directory: directory,
            store,
            id,
            root,
            memory,
        }
    }

    fn reopen_with_new_turn(&self) -> DurableSession {
        let (mut session, _) =
            DurableSession::resume_protected(self.store.clone(), self.id).unwrap();
        session
            .append_message(Message::text(Role::User, "New complete fixture request"))
            .unwrap();
        session
            .append_message(Message::text(
                Role::Assistant,
                "New complete fixture result",
            ))
            .unwrap();
        session
    }

    fn spawn(
        &self,
        session: DurableSession,
        timings: Arc<ContextTimings>,
    ) -> (RuntimeHandle, CapturedRequests) {
        let (agent, assembler, requests) = loaded_agent(&self.root, timings);
        let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), &self.root).unwrap();
        (
            RuntimeHandle::spawn_persistent(
                agent,
                policy,
                true,
                session,
                assembler,
                Some(self.memory.clone()),
            )
            .unwrap(),
            requests,
        )
    }
}

fn loaded_agent(
    root: &std::path::Path,
    timings: Arc<ContextTimings>,
) -> (Agent, PromptAssembler, CapturedRequests) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(vec![Ok(Message::text(Role::Assistant, "Fixture completed"))].into()),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: Vec::new(),
    };
    let (agent, assembler) = persistent_agent_with_budget(
        Box::new(provider),
        root.into(),
        PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        Some(32_768),
    );
    (agent.with_runtime_telemetry(timings), assembler, requests)
}

/// Releases the deliberately occupied worker even if an assertion unwinds.
struct ReleaseWorker(Option<std::sync::mpsc::Sender<()>>);
impl Drop for ReleaseWorker {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

async fn next_bounded(runtime: &mut RuntimeHandle) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(5), runtime.next_event())
        .await
        .expect("runtime must answer control while original-source work is pending")
        .expect("runtime event channel remains open")
}

#[test]
fn loaded_owner_answers_control_and_cancels_before_original_source_worker_is_released() {
    // One occupied blocking worker makes the source read causally pending. This
    // is not a race against disk speed or a tiny elapsed-time assertion.
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    executor.block_on(async {
        let fixture = LoadedOwner::new();
        let session = fixture.reopen_with_new_turn();
        let old_checkpoint = session.prompt_continuation().unwrap().checkpoint.unwrap();
        let old_revision = fixture.store.history_metadata(fixture.id).unwrap().revision;
        let (_, assembler, _) = loaded_agent(&fixture.root, Arc::new(ContextTimings::default()));
        assert!(
            !session
                .begin_compaction(
                    OperationId::new(),
                    CompactionReason::Manual,
                    assembler.budget_plan().unwrap()
                )
                .unwrap()
                .is_ready(),
            "fixture must require archived originals"
        );
        let timings = Arc::new(ContextTimings::default());
        let (mut runtime, requests) = fixture.spawn(session, timings.clone());

        let (release, wait) = std::sync::mpsc::channel();
        let release = ReleaseWorker(Some(release));
        let (started, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            let _ = wait.recv();
        });
        ready.await.unwrap();
        let operation = OperationId::new();
        runtime
            .send(RuntimeCommand::CompactConversation {
                operation_id: operation,
            })
            .await
            .unwrap();
        assert!(matches!(next_bounded(&mut runtime).await,
            AgentEvent::CompactionStarted {operation_id,..} if operation_id == operation));

        runtime
            .send(RuntimeCommand::ClearConversation)
            .await
            .unwrap();
        assert!(
            matches!(next_bounded(&mut runtime).await,
            AgentEvent::CommandRejected {reason} if reason.contains("compaction is active")),
            "control must be observed before any original-source work can finish"
        );
        runtime
            .send(RuntimeCommand::InterruptOperation {
                operation_id: OperationId::new(),
            })
            .await
            .unwrap();
        assert!(
            matches!(
                next_bounded(&mut runtime).await,
                AgentEvent::CommandRejected { .. }
            ),
            "a mismatched cancellation must not cancel this compaction"
        );
        runtime
            .send(RuntimeCommand::InterruptOperation {
                operation_id: operation,
            })
            .await
            .unwrap();
        assert!(matches!(next_bounded(&mut runtime).await,
            AgentEvent::CompactionUnavailable {operation_id,reason}
                if operation_id == operation && reason.contains("cancelled")));
        assert!(
            !worker.is_finished(),
            "cancellation must not wait for the source worker"
        );
        runtime.send(RuntimeCommand::ListChildren).await.unwrap();
        assert!(
            matches!(next_bounded(&mut runtime).await,
            AgentEvent::ChildListSnapshot {children} if children.is_empty()),
            "the loaded owner must accept ordinary inspection after cancellation"
        );
        assert!(requests.lock().unwrap().is_empty());
        assert_eq!(
            fixture.store.history_metadata(fixture.id).unwrap().revision,
            old_revision
        );
        let phases = timings.0.lock().unwrap().clone();
        assert!(
            phases
                .iter()
                .any(|event| event.phase == ContextPhase::SourceAdmission)
        );
        assert!(!phases.iter().any(|event| matches!(
            event.phase,
            ContextPhase::SourcePreparation
                | ContextPhase::HelperGeneration
                | ContextPhase::CheckpointCommit
        )));

        drop(release);
        worker.await.unwrap();
        assert!(runtime.shutdown_owned().await);
        let (restored, _) =
            DurableSession::resume_protected(fixture.store.clone(), fixture.id).unwrap();
        assert_eq!(
            restored.prompt_continuation().unwrap().checkpoint,
            Some(old_checkpoint)
        );
        assert_eq!(
            fixture.store.history_metadata(fixture.id).unwrap().revision,
            old_revision
        );
        assert_eq!(
            fixture
                .store
                .history_page(fixture.id, None, Some(0), 1)
                .unwrap()
                .total,
            INITIAL_ENTRIES + 2
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_new_user_message_rejects_before_compaction_or_history_mutation() {
    let fixture = LoadedOwner::new();
    let session = fixture.reopen_with_new_turn();
    let old_checkpoint = session.prompt_continuation().unwrap().checkpoint.unwrap();
    let old_revision = fixture.store.history_metadata(fixture.id).unwrap().revision;
    let timings = Arc::new(ContextTimings::default());
    let (mut runtime, requests) = fixture.spawn(session, timings.clone());
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: OperationId::new(),
            input: "x".repeat(128 * 1024),
        })
        .await
        .unwrap();
    assert!(
        matches!(next_bounded(&mut runtime).await,
        AgentEvent::CommandRejected {reason}
            if reason.contains("new user message and required tool schemas")
                && reason.contains("compacting older history cannot help")),
        "an irreducible oversized request must not start compaction"
    );
    runtime.send(RuntimeCommand::ListChildren).await.unwrap();
    assert!(matches!(next_bounded(&mut runtime).await,
        AgentEvent::ChildListSnapshot {children} if children.is_empty()));
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        timings
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|event| event.phase == ContextPhase::PromptPreparation)
    );
    assert_eq!(
        fixture.store.history_metadata(fixture.id).unwrap().revision,
        old_revision
    );
    assert!(runtime.shutdown_owned().await);
    let (restored, _) =
        DurableSession::resume_protected(fixture.store.clone(), fixture.id).unwrap();
    assert_eq!(
        restored.prompt_continuation().unwrap().checkpoint,
        Some(old_checkpoint)
    );
    assert_eq!(
        fixture
            .store
            .history_page(fixture.id, None, Some(0), 1)
            .unwrap()
            .total,
        INITIAL_ENTRIES + 2
    );
}

#[tokio::test(flavor = "current_thread")]
async fn replaceable_checkpoint_does_not_make_a_near_limit_new_request_irreducible() {
    let fixture = LoadedOwner::new();
    let session = fixture.reopen_with_new_turn();
    let checkpoint = session.prompt_continuation().unwrap().checkpoint.unwrap();
    let (_, assembler, _) = loaded_agent(&fixture.root, Arc::new(ContextTimings::default()));
    let selection = fixture
        .memory
        .select_for_turn("", assembler.budget_plan().unwrap().input_budget_tokens)
        .unwrap();
    let current = assembler
        .assemble_with_compaction(&[], Some(&checkpoint))
        .unwrap()
        .with_personal_memory(&selection)
        .0;
    let mut smaller = checkpoint.clone();
    smaller.summary = crate::session::CompactionSummary {
        goal: Some("A smaller replacement continuation".into()),
        ..Default::default()
    };
    let replacement = assembler
        .assemble_with_compaction(&[], Some(&smaller))
        .unwrap()
        .with_personal_memory(&selection)
        .0;
    assert!(replacement.system_tokens < current.system_tokens);
    let user_tokens =
        current.budget.total_tokens - current.system_tokens - current.tool_schema_tokens + 1;
    let empty_user_tokens = crate::prompt::estimate_message_tokens(&Message::text(Role::User, ""));
    let input = "x".repeat((user_tokens - empty_user_tokens) * 3);
    let user_message = Message::text(Role::User, input.clone());
    assert!(
        current
            .validate_history(std::iter::once(&user_message))
            .is_err(),
        "the old snapshot-based preflight must reject this boundary fixture"
    );
    assert!(
        replacement
            .validate_history(std::iter::once(&user_message))
            .is_ok(),
        "a smaller replacement checkpoint could make this request fit"
    );

    let (mut runtime, _) = fixture.spawn(session, Arc::new(ContextTimings::default()));
    let operation = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: operation,
            input,
        })
        .await
        .unwrap();
    assert!(
        matches!(next_bounded(&mut runtime).await,
        AgentEvent::CompactionStarted { operation_id, .. } if operation_id == operation),
        "a replaceable current summary must not cause premature rejection"
    );
    assert!(runtime.shutdown_owned().await);
}

async fn compact(runtime: &mut RuntimeHandle, operation: OperationId) {
    runtime
        .send(RuntimeCommand::CompactConversation {
            operation_id: operation,
        })
        .await
        .unwrap();
    loop {
        match next_bounded(runtime).await {
            AgentEvent::ConversationCompacted { checkpoint }
                if checkpoint.operation_id == operation =>
            {
                break;
            }
            AgentEvent::CompactionUnavailable { reason, .. }
            | AgentEvent::CommandRejected { reason } => {
                panic!("fixture compaction failed: {reason}")
            }
            _ => {}
        }
    }
}

fn report(label: &str, samples: &[Duration]) {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort();
    let percentile = |percentage: usize| {
        sorted[(sorted.len() * percentage).div_ceil(100) - 1].as_secs_f64() * 1000.0
    };
    eprintln!(
        "{label}: n={} p50_ms={:.3} p95_ms={:.3}",
        sorted.len(),
        percentile(50),
        percentile(95)
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "measured real-runtime fixture; observations only, run explicitly with --nocapture"]
async fn measure_loaded_owner_cold_warm_compaction_and_prompt_preparation() {
    const SAMPLES: usize = 20;
    let fixture = LoadedOwner::new();
    let timings = Arc::new(ContextTimings::default());
    let mut cold_operations = Vec::new();
    let mut warm_operations = Vec::new();
    let mut turn_operations = Vec::new();
    for _ in 0..SAMPLES {
        let session = fixture.reopen_with_new_turn();
        let (mut runtime, requests) = fixture.spawn(session, timings.clone());
        let cold = OperationId::new();
        cold_operations.push(cold);
        compact(&mut runtime, cold).await;
        let turn = OperationId::new();
        turn_operations.push(turn);
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: turn,
                input: "Give concise Rust examples for this fixture".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), receive_finished(&mut runtime, turn))
                .await
                .unwrap(),
            OperationOutcome::Completed
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "no memory relevance-model call"
        );
        let warm = OperationId::new();
        warm_operations.push(warm);
        compact(&mut runtime, warm).await;
        assert!(runtime.shutdown_owned().await);
    }
    eprintln!(
        "headless runtime: initial_entries={INITIAL_ENTRIES} memory_facts={MEMORY_FACTS} samples={SAMPLES} debug_assertions={}; cold=resumed process-local proof cache, OS/database caches not flushed",
        cfg!(debug_assertions)
    );
    let events = timings.0.lock().unwrap();
    for (label, operations) in [("cold", cold_operations), ("warm", warm_operations)] {
        for phase in [
            ContextPhase::SourceAdmission,
            ContextPhase::SourcePreparation,
            ContextPhase::CheckpointCommit,
        ] {
            let samples = events
                .iter()
                .filter(|event| event.phase == phase && operations.contains(&event.operation_id))
                .map(|event| event.elapsed)
                .collect::<Vec<_>>();
            assert_eq!(samples.len(), SAMPLES);
            report(&format!("{label}.{phase:?}"), &samples);
        }
    }
    let prompt = events
        .iter()
        .filter(|event| {
            event.phase == ContextPhase::PromptPreparation
                && turn_operations.contains(&event.operation_id)
        })
        .map(|event| event.elapsed)
        .collect::<Vec<_>>();
    assert_eq!(prompt.len(), SAMPLES);
    report("loaded.PromptPreparation", &prompt);
    assert!(
        !events
            .iter()
            .any(|event| event.phase == ContextPhase::HelperGeneration),
        "fixture makes no semantic-model call"
    );
}
