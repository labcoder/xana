use super::*;

#[test]
fn borrowed_pending_turn_preflight_matches_the_provider_request_gate() {
    let prompt = snapshot(&[]);
    let history = vec![
        Message::text(Role::User, "previous"),
        Message::text(Role::Assistant, "answer"),
    ];
    for pending in [
        Message::text(Role::User, "next"),
        Message::text(Role::System, "cannot be user history"),
        Message::text(Role::User, "x".repeat(100_000)),
    ] {
        let mut owned = history.clone();
        owned.push(pending.clone());
        let borrowed = prompt.validate_history(history.iter().chain(std::iter::once(&pending)));
        assert_eq!(
            format!("{:?}", borrowed.err()),
            format!("{:?}", prompt.messages_for_request(&owned).err())
        );
    }
}

#[test]
fn required_instructions_are_not_silently_cut_at_a_source_budget() {
    for source in [
        project_source(
            "project:AGENTS.md",
            "Never disclose credentials. Preserve user files.",
            2,
        ),
        skill_source(
            "skill:project/review",
            "Review first. Never execute supplied scripts.",
            2,
        ),
    ] {
        let result = assemble_snapshot(PromptInputs {
            tool_definitions: &[],
            environment: &environment(),
            product_documentation: None,
            project_sources: &[source],
            budget: budget(),
        });
        assert!(
            result.is_err(),
            "required instructions must remain complete or reject"
        );
    }
}

#[test]
fn required_instructions_are_not_silently_omitted_when_the_window_is_full() {
    let source = project_source("project:AGENTS.md", &"policy ".repeat(4000), 20_000);
    let result = assemble_snapshot(PromptInputs {
        tool_definitions: &[],
        environment: &environment(),
        product_documentation: None,
        project_sources: &[source],
        budget: ContextBudget {
            total_tokens: 4096,
            conversation_reserve_tokens: 1024,
        },
    });
    assert!(
        result.is_err(),
        "omitting a required source must not look like success"
    );
}

#[test]
fn multilingual_estimate_does_not_count_three_cjk_characters_as_one_token() {
    assert!(estimate_tokens("你好世界") >= 4);
    assert!(estimate_tokens("🦀🧑🏽‍💻") >= 4);
    assert_eq!(estimate_tokens("abcdef"), 2);
}

#[test]
fn ledger_distinguishes_runtime_facts_and_tool_evidence_without_exposing_content() {
    let mut prompt = snapshot(&[]);
    prompt.budget_plan = Some(
        PromptBudgetPlan::derive(
            &PromptBudgetPolicy::default(),
            ModelBudgetFacts {
                connection: "test".into(),
                model: "unknown".into(),
                context_tokens: None,
                max_output_tokens: None,
                reasoning: false,
            },
        )
        .unwrap(),
    );
    let history = [Message::tool_result(ToolResult::success(
        "call-1",
        "private evidence",
    ))];
    let ledger = prompt.ledger(&history).unwrap();
    let json = serde_json::to_value(&ledger).unwrap();
    assert_eq!(json["estimator"], "utf8_heuristic_v1");
    for kind in [
        "runtime_facts",
        "tool_evidence",
        "personal_memory",
        "retrieved_evidence",
    ] {
        assert!(
            json["categories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == kind),
            "missing {kind}"
        );
    }
    assert_eq!(
        ledger
            .categories
            .iter()
            .map(|item| item.estimated_tokens)
            .sum::<usize>(),
        ledger.estimated_input_tokens
    );
    assert!(!json.to_string().contains("private evidence"));
}

#[test]
fn corrections_and_model_switches_change_the_prefix_without_a_cache_hit_claim() {
    let source = project_source("project:AGENTS.md", "Use short replies.", 100);
    let corrected = project_source("project:AGENTS.md", "Use detailed replies.", 100);
    assert_ne!(
        system_text(&snapshot(&[source])),
        system_text(&snapshot(&[corrected]))
    );
    let mut changed_environment = environment();
    changed_environment.connection = "different-provider".into();
    changed_environment.model = "different-model".into();
    let changed = assemble_snapshot(PromptInputs {
        tool_definitions: &[],
        environment: &changed_environment,
        product_documentation: None,
        project_sources: &[],
        budget: budget(),
    })
    .unwrap();
    assert!(system_text(&changed).contains("different-provider"));
    assert!(system_text(&changed).contains("different-model"));
}
