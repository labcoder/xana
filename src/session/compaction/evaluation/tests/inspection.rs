use super::*;
use crate::session::compaction::evaluation::diagnostics::{ActiveField, AssertionId};

#[test]
fn fixed_assertion_ids_map_every_fact_and_canary_to_exact_active_and_reference_fields() {
    use ActiveField::*;
    use AssertionId::*;
    for case in corpus() {
        for (cycle_index, cycle) in case.cycles().into_iter().enumerate() {
            let (required, canaries) = assertion_observations(cycle, None);
            assert_eq!(
                required.iter().map(|hit| hit.id).collect::<Vec<_>>(),
                [Scope, CurrentTarget, UnresolvedWork, NoDeploy]
            );
            assert_eq!(
                canaries.iter().map(|hit| hit.id).collect::<Vec<_>>(),
                if cycle_index == 0 {
                    vec![OriginalTarget, ToolScope, ToolCompletion]
                } else {
                    vec![
                        OriginalTarget,
                        SupersededTarget,
                        SupersededUnresolvedWork,
                        ToolScope,
                        ToolCompletion,
                    ]
                }
            );
            assert!(
                required
                    .iter()
                    .chain(&canaries)
                    .all(|hit| { hit.active_fields.is_empty() && !hit.references_hit })
            );
            for (is_required, values) in [(true, cycle.required), (false, cycle.forbidden)] {
                for (index, value) in values.iter().enumerate() {
                    for field in [
                        Some(Goal),
                        Some(Constraints),
                        Some(Progress),
                        Some(Decisions),
                        Some(Unresolved),
                        None,
                    ] {
                        let mut summary = CompactionSummary::default();
                        let text = format!("Rejected or retained synthetic marker: {value}");
                        match field {
                            Some(Goal) => summary.goal = Some(text),
                            Some(Constraints) => summary.constraints.push(text),
                            Some(Progress) => summary.progress.push(text),
                            Some(Decisions) => summary.decisions.push(text),
                            Some(Unresolved) => summary.unresolved.push(text),
                            None => summary.references.push(text),
                        }
                        let (required, canaries) = assertion_observations(cycle, Some(&summary));
                        let hits = if is_required { &required } else { &canaries };
                        assert_eq!(
                            hits[index].active_fields,
                            field.into_iter().collect::<Vec<_>>()
                        );
                        assert_eq!(hits[index].references_hit, field.is_none());
                        assert!(hits.iter().enumerate().all(|(other, hit)| {
                            other == index || (hit.active_fields.is_empty() && !hit.references_hit)
                        }));
                        let score = score_cycle(&case.id, cycle, Some(&summary));
                        assert_eq!(score.retained, usize::from(is_required && field.is_some()));
                        assert_eq!(score.canaries_pass, is_required || field.is_none());
                    }
                }
            }
            // Report all matching fields once, including simultaneous historical evidence.
            let text = cycle.required.join("; ");
            let summary = CompactionSummary {
                goal: Some(text.clone()),
                constraints: vec![text.clone(), text.clone()],
                progress: vec![text.clone()],
                decisions: vec![text.clone()],
                unresolved: vec![text.clone()],
                references: vec![text],
            };
            let (required, _) = assertion_observations(cycle, Some(&summary));
            assert!(required.iter().all(|hit| {
                hit.active_fields == [Goal, Constraints, Progress, Decisions, Unresolved]
                    && hit.references_hit
            }));
            assert_eq!(score_cycle(&case.id, cycle, Some(&summary)).retained, 4);
        }
    }
}

#[tokio::test]
async fn explicit_inspection_uses_the_same_calls_and_never_changes_scores_or_saved_evidence() {
    for case_id in ["en-0", "ja-6"] {
        let (_home, store, budget) = setup();
        let case = corpus()
            .into_iter()
            .find(|case| case.id == case_id)
            .unwrap();
        let summaries = case
            .cycles()
            .into_iter()
            .map(|cycle| {
                let mut summary = fixture_summary(cycle);
                // The conservative canary remains a failure even if this is a rejection.
                summary
                    .progress
                    .push(format!("Rejected: {}", cycle.forbidden[0]));
                summary
                    .references
                    .push("SYNTHETIC_INSPECTION_MARKER".into());
                summary
            })
            .collect::<Vec<_>>();
        let mut scores = Vec::new();
        for inspect in [false, true] {
            let helper = ScriptedHelper::summaries(summaries.clone());
            let outcome = evaluate_selected(
                &helper,
                &budget,
                "a".repeat(64),
                &CancellationToken::new(),
                Some(case_id),
                inspect,
                semantic::HelperLimits::default(),
            )
            .await
            .unwrap();
            assert_eq!(helper.requests.lock().unwrap().len(), summaries.len());
            assert!(!outcome.report.passes());
            assert!(
                semantic::HelperPolicy::approve(&store, "a".repeat(64), &outcome.report).is_err()
            );
            assert!(
                semantic::HelperPolicy::load(&store, &"a".repeat(64))
                    .unwrap()
                    .is_none()
            );
            assert!(outcome.report.helper[0].helper_succeeded);
            assert!(!outcome.report.helper[0].canaries_pass);
            scores.push(serde_json::to_value(&outcome.report.helper).unwrap());
            let report = serde_json::to_vec(&outcome.report).unwrap();
            store
                .set_document("test/metadata-only-evaluation", &report, 512 * 1024)
                .unwrap();
            let retained = store
                .document("test/metadata-only-evaluation", 512 * 1024)
                .unwrap()
                .unwrap();
            let retained = String::from_utf8(retained).unwrap();
            for omitted in [
                "SYNTHETIC_INSPECTION_MARKER",
                "PRIVATE_REASONING_TEXT",
                "Rejected:",
            ] {
                assert!(!retained.contains(omitted));
            }
            assert!(retained.contains("original_target"));
            if inspect {
                let inspection =
                    serde_json::to_value(outcome.inspection.as_ref().unwrap()).unwrap();
                assert_eq!(inspection["case_id"], case_id);
                let objects = inspection["summaries"].as_array().unwrap();
                assert_eq!(objects.len(), summaries.len());
                assert!(objects.len() <= 2);
                for (index, object) in objects.iter().enumerate() {
                    assert_eq!(object["cycle"], index + 1);
                    assert_eq!(
                        object["summary"],
                        serde_json::to_value(&summaries[index]).unwrap()
                    );
                    assert_eq!(object.as_object().unwrap().len(), 2);
                }
                let text = serde_json::to_string(&inspection).unwrap();
                assert!(text.contains("SYNTHETIC_INSPECTION_MARKER"));
                assert!(!text.contains("PRIVATE_REASONING_TEXT"));
            } else {
                assert!(outcome.inspection.is_none());
            }
        }
        assert_eq!(
            scores[0], scores[1],
            "inspection cannot change strict scoring"
        );
    }
}

#[tokio::test]
async fn inspection_requires_a_known_single_case_before_dispatch() {
    let (_home, _store, budget) = setup();
    let helper = ScriptedHelper::summaries(Vec::new());
    for case_id in [None, Some("not-a-case")] {
        assert!(
            evaluate_selected(
                &helper,
                &budget,
                "a".repeat(64),
                &CancellationToken::new(),
                case_id,
                true,
                semantic::HelperLimits::default(),
            )
            .await
            .is_err()
        );
    }
    assert!(helper.requests.lock().unwrap().is_empty());
}

struct InvalidAnswer(&'static str);

impl ConversationalProvider for InvalidAnswer {
    fn stream_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        _step: StepId,
        _sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async { panic!("inspection must use bounded helper generation") })
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
        step: StepId,
        sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async move {
            sink.reasoning_delta(step, "PRIVATE_REASONING_TEXT");
            sink.text_delta(step, self.0);
            Ok(Message::text(Role::Assistant, self.0))
        })
    }
}

#[tokio::test]
async fn invalid_raw_answers_and_provider_errors_never_enter_inspection() {
    let (_home, _store, budget) = setup();
    let partial = InvalidAnswer("{\"goal\":\"PRIVATE_PARTIAL_ANSWER");
    let invalid = InvalidAnswer("{\"goal\":\"PRIVATE_SHAPE_ANSWER\",\"unrecognized\":true}");
    let error = ScriptedHelper {
        summaries: Mutex::new(VecDeque::from([Err(ProviderError::new(
            "PRIVATE_PROVIDER_ERROR",
        ))])),
        requests: Mutex::new(Vec::new()),
    };
    let oversized = ScriptedHelper::summaries(vec![CompactionSummary {
        goal: Some("x".repeat(513)),
        ..Default::default()
    }]);
    for helper in [
        &partial as &dyn ConversationalProvider,
        &invalid,
        &error,
        &oversized,
    ] {
        let outcome = evaluate_selected(
            helper,
            &budget,
            "a".repeat(64),
            &CancellationToken::new(),
            Some("en-6"),
            true,
            semantic::HelperLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.report.calls.len(), 1);
        assert!(outcome.report.calls[0].failure.is_some());
        assert!(
            outcome.report.calls[0]
                .required_facts
                .iter()
                .chain(&outcome.report.calls[0].canaries)
                .all(|hit| hit.active_fields.is_empty() && !hit.references_hit)
        );
        assert!(!outcome.report.passes());
        let inspection = serde_json::to_value(outcome.inspection.unwrap()).unwrap();
        assert_eq!(inspection["summaries"], serde_json::json!([]));
        assert!(
            !serde_json::to_string(&outcome.report)
                .unwrap()
                .contains("PRIVATE_")
        );
        assert!(!inspection.to_string().contains("PRIVATE_"));
    }
}

#[tokio::test]
async fn a_failed_second_cycle_keeps_only_the_valid_first_summary() {
    let (_home, _store, budget) = setup();
    let case = corpus().into_iter().find(|case| case.id == "en-6").unwrap();
    let first = fixture_summary(case.cycles()[0]);
    let helper = ScriptedHelper {
        summaries: Mutex::new(VecDeque::from([
            Ok(first.clone()),
            Err(ProviderError::new("PRIVATE_SECOND_CYCLE_ERROR")),
        ])),
        requests: Mutex::new(Vec::new()),
    };
    let outcome = evaluate_selected(
        &helper,
        &budget,
        "a".repeat(64),
        &CancellationToken::new(),
        Some("en-6"),
        true,
        semantic::HelperLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(helper.requests.lock().unwrap().len(), 2);
    assert_eq!(outcome.report.calls.len(), 2);
    assert!(outcome.report.calls[1].failure.is_some());
    assert!(!outcome.report.passes());
    let inspection = serde_json::to_value(outcome.inspection.unwrap()).unwrap();
    assert_eq!(
        inspection["summaries"],
        serde_json::json!([
            {"cycle": 1, "summary": first}
        ])
    );
    assert!(!inspection.to_string().contains("PRIVATE_"));
}

#[test]
fn inspection_output_seam_preserves_summary_and_two_cycle_bounds() {
    let mut inspection = diagnostics::SyntheticInspection::new("en-6");
    let valid = CompactionSummary {
        goal: Some("\u{0001}".repeat(500)),
        constraints: vec!["\u{0001}".repeat(500); 7],
        ..Default::default()
    };
    let invalid = CompactionSummary {
        goal: Some("x".repeat(513)),
        ..Default::default()
    };
    let oversized = CompactionSummary {
        constraints: vec!["x".repeat(512); 8],
        ..Default::default()
    };
    let too_many = CompactionSummary {
        constraints: vec!["x".into(); 17],
        ..Default::default()
    };
    inspection.record(1, &invalid);
    inspection.record(1, &oversized);
    inspection.record(1, &too_many);
    inspection.record(1, &CompactionSummary::default());
    inspection.record(0, &valid);
    inspection.record(3, &valid);
    inspection.record(1, &valid);
    inspection.record(2, &valid);
    inspection.record(2, &valid);
    let value = serde_json::to_value(&inspection).unwrap();
    assert_eq!(value["summaries"].as_array().unwrap().len(), 2);
    assert!(serde_json::to_vec_pretty(&inspection).unwrap().len() < 64 * 1024);
    for object in value["summaries"].as_array().unwrap() {
        let summary: CompactionSummary = serde_json::from_value(object["summary"].clone()).unwrap();
        assert!(crate::session::compaction::validate_summary(
            &summary,
            SUMMARY_MAX_BYTES
        ));
    }
}
