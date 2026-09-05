use super::*;
use crate::{
    provider::ProviderError,
    storage::{RecoveryIdentity, TestCustody},
    tool::ToolDefinition,
};
use futures::future::BoxFuture;

enum Behavior {
    Summary(CompactionSummary),
    ToolCall,
    Oversized,
    Failure,
    Wait,
}
struct Helper(Behavior);
impl ConversationalProvider for Helper {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step: StepId,
        sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async move {
            assert!(tools.is_empty(), "semantic helper cannot invoke tools");
            assert_eq!(messages[0].role, Role::System);
            match &self.0 {
                Behavior::Summary(summary) => {
                    sink.usage(ProviderUsage {
                        total_tokens: Some(100),
                        ..Default::default()
                    });
                    Ok(Message::text(
                        Role::Assistant,
                        serde_json::to_string(summary).unwrap(),
                    ))
                }
                Behavior::ToolCall => Ok(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolCall(crate::message::ToolCall {
                        id: "forbidden".into(),
                        name: "run_command".into(),
                        arguments: serde_json::json!({"command":"not executed"}),
                    })],
                }),
                Behavior::Oversized => {
                    sink.text_delta(step, &"x".repeat(MAX_OUTPUT_BYTES + 1));
                    std::future::pending().await
                }
                Behavior::Failure => Err(ProviderError::new("synthetic private provider error")),
                Behavior::Wait => std::future::pending().await,
            }
        })
    }
}

fn setup() -> (tempfile::TempDir, ProtectedStore, UsageBudget) {
    let home = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        home.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let budget = UsageBudget::new(
        store.clone(),
        "semantic-test-root".into(),
        "test".into(),
        OUTPUT_RESERVE,
    );
    (home, store, budget)
}

fn source() -> Vec<Message> {
    source_messages(
        None,
        &[&Message::text(
            Role::User,
            "Only project Alpha; never deploy. Correction: use Beta. Still unresolved: test.",
        )],
    )
    .unwrap()
}

#[tokio::test]
async fn helper_is_bounded_accounted_and_has_no_tools() {
    let (_home, store, budget) = setup();
    let summary = CompactionSummary {
        goal: Some("Build Beta".into()),
        constraints: vec!["Never deploy".into()],
        unresolved: vec!["Test".into()],
        ..Default::default()
    };
    let result = request(
        &Helper(Behavior::Summary(summary.clone())),
        &budget,
        OperationId::new(),
        &source(),
        4096,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result, summary);
    let records = store.usage_page(None, None, None).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].receipt.as_ref().unwrap().total_tokens, Some(100));
}

#[tokio::test]
async fn failure_tool_output_and_overflow_cannot_become_summaries() {
    let (_home, store, budget) = setup();
    for behavior in [Behavior::Failure, Behavior::ToolCall, Behavior::Oversized] {
        let result = request(
            &Helper(behavior),
            &budget,
            OperationId::new(),
            &source(),
            4096,
            &CancellationToken::new(),
        )
        .await;
        assert!(result.is_err());
        assert!(!format!("{:?}", result.err()).contains("synthetic private provider error"));
    }
    let records = store.usage_page(None, None, None).unwrap();
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .all(|record| record.receipt.as_ref().unwrap().outcome == Outcome::Failed)
    );
}

#[tokio::test]
async fn cancellation_settles_and_budget_refuses_before_dispatch() {
    let (_home, store, budget) = setup();
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    let cancel = tokio::spawn(async move {
        tokio::task::yield_now().await;
        signal.cancel();
    });
    assert!(
        request(
            &Helper(Behavior::Wait),
            &budget,
            OperationId::new(),
            &source(),
            4096,
            &cancellation
        )
        .await
        .is_err()
    );
    cancel.await.unwrap();
    let records = store.usage_page(None, None, None).unwrap();
    assert_eq!(
        records[0].receipt.as_ref().unwrap().outcome,
        Outcome::Interrupted
    );
    store
        .update_usage_policy(|policy| policy.root_tokens = Some(1))
        .unwrap();
    assert!(
        request(
            &Helper(Behavior::Wait),
            &budget,
            OperationId::new(),
            &source(),
            4096,
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(store.usage_page(None, None, None).unwrap().len(), 1);
}

#[test]
fn oversize_source_does_not_partially_summarize_or_expose_images() {
    let message = Message::text(Role::User, "x".repeat(MAX_SOURCE_TOKENS * 5));
    assert!(source_messages(None, &[&message]).is_none());
}

#[test]
fn forty_multilingual_cases_compare_baseline_without_faking_promotion() {
    use crate::session::compaction::evaluation::{self, EvaluationReport};
    let (_home, store, _) = setup();
    let cases = evaluation::corpus();
    assert_eq!(cases.len(), 40);
    let mut report = EvaluationReport {
        version: evaluation::CORPUS_VERSION.into(),
        corpus_digest: evaluation::corpus_digest(),
        route_digest: "a".repeat(64),
        fixture: true,
        baseline: Vec::new(),
        helper: Vec::new(),
        elapsed_millis: 0,
    };
    for case in cases {
        let baseline = CompactionSummary::derive(None, &case.messages, 4096);
        report
            .baseline
            .push(evaluation::score(&case, Some(&baseline)));
        let mocked = CompactionSummary {
            goal: Some(case.required[0].clone()),
            constraints: vec![case.required[3].clone()],
            decisions: vec![case.required[1].clone()],
            unresolved: vec![case.required[2].clone()],
            ..Default::default()
        };
        report.helper.push(evaluation::score(&case, Some(&mocked)));
    }
    assert!(
        report.passes(),
        "fixed mock establishes evaluator wiring, not model quality"
    );
    assert!(
        HelperPolicy::approve(&store, report.route_digest.clone(), &report).is_err(),
        "mocked scores must never approve a production helper"
    );
    assert!(
        HelperPolicy::load(&store, &report.route_digest)
            .unwrap()
            .is_none()
    );
    report.helper[0].canaries_pass = false;
    assert!(
        !report.passes(),
        "one explicit correction/scope failure blocks promotion"
    );
}

#[test]
fn provenance_rejects_mutated_summary() {
    let mut summary = CompactionSummary {
        goal: Some("source-bound goal".into()),
        ..Default::default()
    };
    let provenance = SemanticProvenance {
        helper_version: HELPER_VERSION,
        route_digest: "a".repeat(64),
        evaluation_digest: "b".repeat(64),
        summary_digest: summary_digest(&summary),
    };
    assert!(provenance.valid_for(&summary));
    summary.goal = Some("different goal".into());
    assert!(!provenance.valid_for(&summary));
}

#[tokio::test]
async fn enriched_checkpoint_reopens_with_source_provenance_and_revocation_blocks_reuse() {
    let (_home, store, budget) = setup();
    let workspace = tempfile::tempdir().unwrap();
    let id = crate::identity::SessionId::new();
    let mut session = crate::session::DurableSession::create_protected(
        store.clone(),
        workspace.path().canonicalize().unwrap(),
        id,
    )
    .unwrap();
    for text in ["First task", "Second task", "Latest request"] {
        session
            .append_message(Message::text(Role::User, text))
            .unwrap();
    }
    let plan = crate::prompt::PromptBudgetPlan::derive(
        &crate::prompt::PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        crate::prompt::ModelBudgetFacts {
            connection: "test".into(),
            model: "test".into(),
            context_tokens: None,
            max_output_tokens: None,
            reasoning: false,
        },
    )
    .unwrap();
    let mut candidate = session
        .prepare_compaction(
            OperationId::new(),
            crate::session::CompactionReason::Manual,
            &plan,
        )
        .unwrap();
    let route = "a".repeat(64);
    // Test-only direct policy construction does not exercise or claim live
    // model-quality promotion; that gate is separately checked above.
    let policy = HelperPolicy {
        version: HELPER_VERSION,
        route_digest: route.clone(),
        evaluation_digest: "b".repeat(64),
        corpus_digest: crate::session::compaction::evaluation::corpus_digest(),
        authorization_store: None,
        route_validator: None,
    };
    store
        .set_document(
            &policy_name(&route),
            &serde_json::to_vec(&policy).unwrap(),
            4096,
        )
        .unwrap();
    let policy = HelperPolicy::load(&store, &route)
        .unwrap()
        .unwrap()
        .with_route_validator(Arc::new(|_| Ok(())));
    let summary = CompactionSummary {
        goal: Some("Preserved first task".into()),
        unresolved: vec!["Second task remains".into()],
        ..Default::default()
    };
    let mut rejected = session
        .prepare_compaction(
            OperationId::new(),
            crate::session::CompactionReason::Manual,
            &plan,
        )
        .unwrap();
    let original = rejected.checkpoint.clone();
    let checks = Arc::new(AtomicUsize::new(0));
    let changing = policy.clone().with_route_validator(Arc::new(move |_| {
        // Before work and after the shared lane is acquired are valid; a route
        // revoked during the provider request cannot enrich the checkpoint.
        ensure!(
            checks.fetch_add(1, Ordering::SeqCst) < 2,
            "recipient changed"
        );
        Ok(())
    }));
    assert!(
        enrich(
            &mut rejected,
            &Helper(Behavior::Summary(summary.clone())),
            &budget,
            &changing,
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(rejected.checkpoint, original);
    let denied = policy
        .clone()
        .with_route_validator(Arc::new(|_| anyhow::bail!("recipient removed")));
    assert!(
        enrich(
            &mut rejected,
            &Helper(Behavior::Wait),
            &budget,
            &denied,
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(
        store.usage_page(None, None, None).unwrap().len(),
        1,
        "pre-dispatch rejection cannot consume a request"
    );
    enrich(
        &mut candidate,
        &Helper(Behavior::Summary(summary.clone())),
        &budget,
        &policy,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let checkpoint = session.commit_compaction(candidate).unwrap();
    assert_eq!(checkpoint.summary, summary);
    assert!(checkpoint.semantic.is_some());
    let originals = store.history_page(id, None, Some(0), 10).unwrap();
    assert_eq!(originals.messages.len(), 3);
    drop(session);
    let (mut reopened, _) =
        crate::session::DurableSession::resume_protected(store.clone(), id).unwrap();
    assert_eq!(
        reopened.prompt_continuation().unwrap().checkpoint,
        Some(checkpoint)
    );
    reopened
        .append_message(Message::text(Role::User, "Next request"))
        .unwrap();
    let mut candidate = reopened
        .prepare_compaction(
            OperationId::new(),
            crate::session::CompactionReason::Manual,
            &plan,
        )
        .unwrap();
    HelperPolicy::revoke(&store, &route).unwrap();
    assert!(
        enrich(
            &mut candidate,
            &Helper(Behavior::Wait),
            &budget,
            &policy,
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(candidate.checkpoint.semantic.is_none());
}

#[test]
fn privacy_change_or_restore_gate_rejects_prepared_checkpoint() {
    let (_home, store, _) = setup();
    let workspace = tempfile::tempdir().unwrap();
    let mut session = crate::session::DurableSession::create_protected(
        store.clone(),
        workspace.path().canonicalize().unwrap(),
        crate::identity::SessionId::new(),
    )
    .unwrap();
    for text in ["Old task", "Current task"] {
        session
            .append_message(Message::text(Role::User, text))
            .unwrap();
    }
    let plan = crate::prompt::PromptBudgetPlan::derive(
        &crate::prompt::PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        crate::prompt::ModelBudgetFacts {
            connection: "test".into(),
            model: "test".into(),
            context_tokens: None,
            max_output_tokens: None,
            reasoning: false,
        },
    )
    .unwrap();
    let candidate = session
        .prepare_compaction(
            OperationId::new(),
            crate::session::CompactionReason::Manual,
            &plan,
        )
        .unwrap();
    let owner = crate::memory::MemoryOwner::new(store.clone(), Default::default());
    owner
        .controls(
            crate::memory::MemoryScope::User,
            crate::memory::MemoryControlEdit {
                learning_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(session.commit_compaction(candidate).is_err());
    assert!(session.prompt_continuation().unwrap().checkpoint.is_none());
    store
        .set_document("restore/review-required", b"{}", 4096)
        .unwrap();
    assert!(
        session
            .prepare_compaction(
                OperationId::new(),
                crate::session::CompactionReason::Manual,
                &plan
            )
            .is_err()
    );
    assert_eq!(session.conversation().unwrap().len(), 2);
}
