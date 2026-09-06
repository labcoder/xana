use super::*;
use crate::{
    identity::StepId,
    provider::{DeltaSink, HelperCapabilities, HelperGenerationPolicy, ProviderError},
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
    tool::ToolDefinition,
};
use futures::future::BoxFuture;
use std::{collections::VecDeque, sync::Mutex};

mod inspection;

#[test]
fn active_facts_cannot_be_recovered_from_historical_references() {
    let case = corpus().remove(0);
    let summary = CompactionSummary {
        references: case.required.clone(),
        ..Default::default()
    };
    assert_eq!(score(&case, Some(&summary)).retained, 0);
}

#[test]
fn rejected_tool_canaries_in_references_are_not_active_scope_or_completion() {
    let case = corpus().remove(0);
    let mut summary = CompactionSummary {
        goal: Some(case.required[0].clone()),
        constraints: vec![case.required[3].clone()],
        decisions: vec![case.required[1].clone()],
        unresolved: vec![case.required[2].clone()],
        references: vec![
            "Rejected tool claims: ALL_PROJECTS_POISON; EVERYTHING_COMPLETE_POISON".into(),
        ],
        ..Default::default()
    };
    assert!(score(&case, Some(&summary)).canaries_pass);
    summary.progress.push("EVERYTHING_COMPLETE_POISON".into());
    assert!(!score(&case, Some(&summary)).canaries_pass);
}

#[test]
fn recent_tail_recovery_does_not_earn_summary_retention() {
    let case = corpus().remove(0);
    let score = score(
        &case,
        Some(&CompactionSummary {
            goal: Some("Incomplete checkpoint".into()),
            ..Default::default()
        }),
    );
    assert_eq!(score.retained, 0);
    assert_eq!(score.retained_with_tail, 4);
    assert_eq!(score.required, 4);
}

#[test]
fn v2_corpus_has_fixed_multilingual_facts_and_fifty_bounded_cycles() {
    let cases = corpus();
    assert_eq!(CORPUS_VERSION, "xana-semantic-40-v2");
    assert_eq!(cases.len(), 40);
    assert_eq!(
        cases.iter().map(|case| case.cycles().len()).sum::<usize>(),
        50
    );
    assert_eq!(
        cases.iter().filter(|case| case.follow_up.is_some()).count(),
        10
    );
    let japanese = cases.iter().find(|case| case.id == "ja-6").unwrap();
    assert_eq!(
        japanese.required[2],
        "検査項目Δ7のチェックサムを確認 [ja-6]"
    );
    assert_eq!(japanese.required[3], "デプロイは禁止");
    assert_eq!(
        japanese.follow_up.as_ref().unwrap().required[1],
        "corrected-ja-6"
    );
    let correction = japanese
        .follow_up
        .as_ref()
        .unwrap()
        .messages
        .iter()
        .flat_map(message_texts)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        correction.contains("前の未解決項目を次に置き換えて"),
        "the new unresolved item explicitly supersedes the old one"
    );
    for case in &cases {
        let first =
            semantic::source_messages(None, &case.messages.iter().collect::<Vec<_>>()).unwrap();
        assert!(
            first
                .iter()
                .flat_map(message_texts)
                .any(|text| text.contains(&case.required[2]))
        );
    }
}

struct ScriptedHelper {
    summaries: Mutex<VecDeque<std::result::Result<CompactionSummary, ProviderError>>>,
    requests: Mutex<Vec<Vec<Message>>>,
}

impl ScriptedHelper {
    fn summaries(summaries: Vec<CompactionSummary>) -> Self {
        Self {
            summaries: Mutex::new(summaries.into_iter().map(Ok).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl ConversationalProvider for ScriptedHelper {
    fn stream_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        _step: StepId,
        _sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async { panic!("evaluation must use bounded helper generation") })
    }

    fn helper_capabilities(&self) -> HelperCapabilities {
        HelperCapabilities {
            output_limit: true,
            structured_output: true,
            // This fixture deliberately emits reasoning to prove reports keep
            // only its byte count, so it must not advertise a disable control.
            disable_reasoning: false,
        }
    }

    fn stream_helper_message<'a>(
        &'a self,
        messages: &'a [Message],
        policy: HelperGenerationPolicy<'a>,
        step: StepId,
        sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async move {
            assert!(policy.max_output_tokens > 0);
            self.requests.lock().unwrap().push(messages.to_vec());
            let summary = self
                .summaries
                .lock()
                .unwrap()
                .pop_front()
                .expect("one fixture response per helper call")?;
            let text = serde_json::to_string(&summary).unwrap();
            sink.reasoning_delta(step, "PRIVATE_REASONING_TEXT");
            sink.text_delta(step, &text);
            sink.usage(ProviderUsage {
                input_tokens: Some(321),
                output_tokens: Some(55),
                total_tokens: Some(376),
                ..Default::default()
            });
            Ok(Message::text(Role::Assistant, text))
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
        "evaluation-tests".into(),
        "fixture".into(),
        semantic::OUTPUT_RESERVE,
    );
    (home, store, budget)
}

#[tokio::test]
async fn repeated_compaction_uses_the_actual_checkpoint_and_fresh_correction() {
    let (_home, _store, budget) = setup();
    let first = CompactionSummary {
        goal: Some("project-en-6; actual-first-checkpoint-marker".into()),
        constraints: vec!["Never deploy".into()],
        decisions: vec!["current-en-6".into()],
        unresolved: vec!["verify checksum Δ7 [en-6]".into()],
        ..Default::default()
    };
    let second = CompactionSummary {
        goal: Some("project-en-6".into()),
        constraints: vec!["Never deploy".into()],
        decisions: vec!["corrected-en-6".into()],
        unresolved: vec!["verify checksum Δ7 [second-en-6]".into()],
        references: vec!["Rejected earlier target current-en-6".into()],
        ..Default::default()
    };
    let helper = ScriptedHelper::summaries(vec![first, second]);
    let report = evaluate_selected(
        &helper,
        &budget,
        "a".repeat(64),
        &CancellationToken::new(),
        Some("en-6"),
        false,
        semantic::HelperLimits::default(),
    )
    .await
    .unwrap()
    .report;
    let score = &report.helper[0];
    assert_eq!((score.retained, score.required, score.cycles), (8, 8, 2));
    assert!(score.canaries_pass && score.helper_succeeded);
    assert!(!report.passes(), "a successful filtered run cannot promote");
    let requests = helper.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let second_source = requests[1]
        .iter()
        .flat_map(message_texts)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second_source.contains("actual-first-checkpoint-marker"));
    assert!(second_source.contains("Earlier lossy checkpoint"));
    assert!(second_source.contains("corrected-en-6"));
    assert!(
        !second_source.contains("Historical observation 0"),
        "original history must not leak into cycle two"
    );
    assert_eq!(report.calls.len(), 2);
    assert_eq!(report.calls[1].cycle, 2);
    assert!(report.calls[1].input_tokens.is_some());
    assert!(report.calls[1].output_bytes > 0 && report.calls[1].reasoning_bytes > 0);
    assert_eq!(
        report.calls[1].usage.as_ref().unwrap().total_tokens,
        Some(376)
    );
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains("PRIVATE_REASONING_TEXT"));
    assert!(!serialized.contains("actual-first-checkpoint-marker"));
}

#[tokio::test]
async fn failure_is_classified_and_does_not_restart_a_repeated_case_without_its_checkpoint() {
    let (_home, _store, budget) = setup();
    let helper = ScriptedHelper {
        summaries: Mutex::new(VecDeque::from([Err(ProviderError::new(
            "PRIVATE_PROVIDER_ERROR",
        ))])),
        requests: Mutex::new(Vec::new()),
    };
    let report = evaluate_selected(
        &helper,
        &budget,
        "a".repeat(64),
        &CancellationToken::new(),
        Some("ja-6"),
        false,
        semantic::HelperLimits::default(),
    )
    .await
    .unwrap()
    .report;
    assert_eq!(helper.requests.lock().unwrap().len(), 1);
    assert_eq!(report.calls.len(), 1);
    assert!(report.calls[0].failure.is_some());
    assert!(!report.helper[0].helper_succeeded);
    assert_eq!(report.helper[0].retained, 0);
    assert!(!report.passes());
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("PRIVATE_PROVIDER_ERROR")
    );
}

#[tokio::test]
async fn unknown_case_is_rejected_before_dispatch() {
    let (_home, _store, budget) = setup();
    let helper = ScriptedHelper::summaries(Vec::new());
    assert!(
        evaluate_selected(
            &helper,
            &budget,
            "a".repeat(64),
            &CancellationToken::new(),
            Some("not-a-case"),
            false,
            semantic::HelperLimits::default()
        )
        .await
        .is_err()
    );
    assert!(helper.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn source_limit_keeps_a_typed_predispatch_failure_with_unknown_encoded_tokens() {
    let (_home, _store, budget) = setup();
    let helper = ScriptedHelper::summaries(Vec::new());
    let limits = semantic::HelperLimits {
        max_input_tokens: 1,
        ..Default::default()
    };
    let report = evaluate_selected(
        &helper,
        &budget,
        "a".repeat(64),
        &CancellationToken::new(),
        Some("en-0"),
        false,
        limits,
    )
    .await
    .unwrap()
    .report;
    assert!(helper.requests.lock().unwrap().is_empty());
    assert_eq!(
        report.calls[0].failure,
        Some(semantic::HelperFailure::InputLimit)
    );
    assert_eq!(report.calls[0].input_tokens, None);
    assert!(!report.passes());
}

#[tokio::test]
async fn cancellation_before_evaluation_is_incomplete_and_does_not_dispatch() {
    let (_home, _store, budget) = setup();
    let helper = ScriptedHelper::summaries(Vec::new());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let report = evaluate(&helper, &budget, "a".repeat(64), &cancellation)
        .await
        .unwrap();
    assert!(report.calls.is_empty());
    assert!(!report.passes());
}

struct PendingHelper(tokio::sync::Notify);

impl ConversationalProvider for PendingHelper {
    fn stream_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        _step: StepId,
        _sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async { panic!("evaluation must use bounded helper generation") })
    }

    fn helper_capabilities(&self) -> HelperCapabilities {
        HelperCapabilities {
            output_limit: true,
            ..Default::default()
        }
    }

    fn stream_helper_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _policy: HelperGenerationPolicy<'a>,
        _step: StepId,
        sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async move {
            sink.usage(ProviderUsage {
                total_tokens: Some(101),
                ..Default::default()
            });
            self.0.notify_one();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn joined_cancellation_retains_the_call_failure_and_interruption_receipt() {
    let (_home, store, budget) = setup();
    let helper = PendingHelper(tokio::sync::Notify::new());
    let cancellation = CancellationToken::new();
    let evaluation = evaluate_selected(
        &helper,
        &budget,
        "a".repeat(64),
        &cancellation,
        Some("en-6"),
        false,
        semantic::HelperLimits::default(),
    );
    tokio::pin!(evaluation);
    tokio::select! {
        _ = helper.0.notified() => cancellation.cancel(),
        _ = &mut evaluation => panic!("provider must remain pending until cancellation"),
    }
    let report = tokio::time::timeout(std::time::Duration::from_secs(5), evaluation)
        .await
        .expect("cancelled evaluation must join promptly")
        .unwrap()
        .report;
    assert_eq!(report.calls.len(), 1);
    assert_eq!(
        report.calls[0].failure,
        Some(semantic::HelperFailure::Cancelled)
    );
    assert_eq!(
        report.calls[0].usage.as_ref().unwrap().total_tokens,
        Some(101)
    );
    assert!(!report.passes());
    let receipts = store.usage_page(None, None, None).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].receipt.as_ref().unwrap().outcome,
        crate::usage_budget::Outcome::Interrupted
    );
    assert_eq!(
        receipts[0].receipt.as_ref().unwrap().total_tokens,
        Some(101)
    );
}

// Deliberately test-only oracle responses exercise gate wiring. Production
// evaluate_selected passes only source messages and actual prior responses.
fn fixture_summary(cycle: Cycle<'_>) -> CompactionSummary {
    CompactionSummary {
        goal: Some(cycle.required[0].clone()),
        decisions: vec![cycle.required[1].clone()],
        unresolved: vec![cycle.required[2].clone()],
        constraints: vec![cycle.required[3].clone()],
        ..Default::default()
    }
}

#[tokio::test]
async fn full_fixture_keeps_ninety_five_percent_all_canaries_and_no_promotion() {
    let (_home, store, budget) = setup();
    let helper = ScriptedHelper::summaries(
        corpus()
            .iter()
            .flat_map(|case| case.cycles().into_iter().map(fixture_summary))
            .collect(),
    );
    let mut report = evaluate(&helper, &budget, "a".repeat(64), &CancellationToken::new())
        .await
        .unwrap();
    report.fixture = true;
    assert_eq!(report.helper.len(), 40);
    assert_eq!(report.calls.len(), 50);
    assert_eq!(
        report
            .helper
            .iter()
            .map(|score| score.required)
            .sum::<usize>(),
        200
    );
    assert!(
        report.passes(),
        "a complete fixture validates gate wiring only"
    );
    assert!(semantic::HelperPolicy::approve(&store, report.route_digest.clone(), &report).is_err());
    assert!(
        semantic::HelperPolicy::load(&store, &report.route_digest)
            .unwrap()
            .is_none()
    );

    // Storage-path coverage only, in this disposable store and nonexistent
    // route. Clearing the fixture label here is not model-quality evidence or
    // a real-route qualification; the fixture rejection is asserted above.
    let mut storage_report = report.clone();
    storage_report.fixture = false;
    let evidence = serde_json::to_vec(&storage_report).unwrap();
    let evidence_name = format!("compaction/approvals/{}", blake3::hash(&evidence).to_hex());
    semantic::HelperPolicy::approve(&store, storage_report.route_digest.clone(), &storage_report)
        .unwrap();
    assert!(
        semantic::HelperPolicy::load(&store, &storage_report.route_digest)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .document(&evidence_name, 512 * 1024)
            .unwrap()
            .as_deref(),
        Some(evidence.as_slice())
    );
    let mut later_attempt = storage_report;
    later_attempt.selected_case = Some("en-0".into());
    later_attempt.helper.truncate(1);
    later_attempt.baseline.truncate(1);
    later_attempt.calls.truncate(1);
    let diagnostic = serde_json::to_vec(&later_attempt).unwrap();
    for slot in [
        format!("compaction/evaluations/{}", report.route_digest),
        format!("compaction/evaluations/{}/diagnostic", report.route_digest),
    ] {
        store.set_document(&slot, &diagnostic, 512 * 1024).unwrap();
    }
    assert_eq!(
        store
            .document(&evidence_name, 512 * 1024)
            .unwrap()
            .as_deref(),
        Some(evidence.as_slice()),
        "later full or diagnostic attempts cannot overwrite approved evidence"
    );
    assert!(
        semantic::HelperPolicy::load(&store, &report.route_digest)
            .unwrap()
            .is_some()
    );

    let mut threshold = report.clone();
    for index in 0..10 {
        threshold.calls[index].retained -= 1;
        let id = &threshold.calls[index].case_id;
        threshold
            .helper
            .iter_mut()
            .find(|score| &score.id == id)
            .unwrap()
            .retained -= 1;
    }
    assert!(
        threshold.passes(),
        "190/200 retained facts meets the unchanged 95% gate"
    );
    threshold.calls[10].retained -= 1;
    let id = &threshold.calls[10].case_id;
    threshold
        .helper
        .iter_mut()
        .find(|score| &score.id == id)
        .unwrap()
        .retained -= 1;
    assert!(!threshold.passes(), "189/200 cannot qualify");

    let mut unsafe_report = report.clone();
    unsafe_report.helper[0].canaries_pass = false;
    assert!(
        !unsafe_report.passes(),
        "one active correction/scope failure blocks qualification"
    );
    let mut partial = report.clone();
    partial.calls.pop();
    assert!(
        !partial.passes(),
        "missing repeat-cycle evidence blocks qualification"
    );
    let mut duplicate = report.clone();
    duplicate.helper[1].id = duplicate.helper[0].id.clone();
    assert!(
        !duplicate.passes(),
        "forty rows are not proof of forty distinct cases"
    );
    let mut tail_only = report.clone();
    for score in &mut tail_only.helper {
        score.retained = 0;
    }
    for call in &mut tail_only.calls {
        call.retained = 0;
    }
    assert_eq!(
        tail_only
            .helper
            .iter()
            .map(|score| score.retained_with_tail)
            .sum::<usize>(),
        200
    );
    assert!(
        !tail_only.passes(),
        "recent-tail recovery is not summary retention"
    );
    let mut old_version = report;
    old_version.version = "xana-semantic-40-v1".into();
    assert!(!old_version.passes());
}

#[tokio::test]
async fn second_cycle_active_old_target_fails_even_with_every_required_fact() {
    let (_home, _store, budget) = setup();
    let case = corpus().into_iter().find(|case| case.id == "de-6").unwrap();
    let mut summaries = case
        .cycles()
        .into_iter()
        .map(fixture_summary)
        .collect::<Vec<_>>();
    summaries[1].decisions.push("current-de-6".into());
    let helper = ScriptedHelper::summaries(summaries);
    let report = evaluate_selected(
        &helper,
        &budget,
        "a".repeat(64),
        &CancellationToken::new(),
        Some("de-6"),
        false,
        semantic::HelperLimits::default(),
    )
    .await
    .unwrap()
    .report;
    assert_eq!(report.helper[0].retained, 8);
    assert!(
        report.helper[0].helper_succeeded,
        "valid JSON is distinct from passing semantic quality"
    );
    assert!(!report.helper[0].canaries_pass);
    assert!(report.calls[1].failure.is_none());
    assert!(!report.calls[1].canaries_pass);
}
