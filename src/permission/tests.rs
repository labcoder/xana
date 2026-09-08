use super::broker::DecisionError;
use super::*;
use crate::{
    identity::{OperationId, StepId, ToolInvocationId},
    native_runtime::{AgentEvent, AgentEventSender},
    tool::EffectClass,
};
use std::path::{Path, PathBuf};
use tempfile::tempdir;
use tokio::sync::mpsc;

fn request(scope: PermissionScope) -> PermissionRequest {
    PermissionRequest {
        operation_id: OperationId::new(),
        invocation_id: ToolInvocationId::new(),
        tool_name: "read_file".to_owned(),
        effect_class: EffectClass::Read,
        final_arguments: serde_json::json!({"path": "notes.txt"}),
        scope,
        outbound_review: None,
    }
}

fn workspace_request(path: &Path) -> PermissionRequest {
    request(PermissionScope::WorkspacePath {
        canonical_path: path.to_owned(),
    })
}

fn public_web_request(operation: OperationId, route: &str) -> PermissionRequest {
    use crate::outbound::{
        OutboundItem, OutboundRequest, PublicWebReview, RecipientIdentity, RecipientKind,
    };
    let recipient = RecipientIdentity::new(
        RecipientKind::WebSearch,
        "search",
        "https://api.exa.ai/search",
        route.as_bytes(),
    )
    .unwrap();
    let item = OutboundItem::new(
        crate::config::OutboundDataClass::PromptText,
        "query",
        None,
        "test",
        b"game time".to_vec(),
    )
    .unwrap();
    let mut review = OutboundRequest::new(operation, recipient.clone(), "find sources", vec![item])
        .unwrap()
        .review();
    review.public_web = Some(Box::new(PublicWebReview {
        route: route.into(),
        persisted_allow: false,
    }));
    let mut request = request(PermissionScope::External {
        recipient_identity_digest: recipient.identity_digest,
        operation: "web_search".into(),
    });
    request.operation_id = operation;
    request.tool_name = "web_search".into();
    request.effect_class = EffectClass::Network;
    request.outbound_review = Some(review);
    request
}

#[tokio::test]
async fn public_web_turn_covers_pending_and_later_queries_but_not_other_routes_or_turns() {
    let workspace = tempdir().unwrap();
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, vec![], workspace.path()),
        true,
        events,
    );
    let operation = OperationId::new();
    let mut waiting = Vec::new();
    let first = public_web_request(operation, "route-a");
    for request in [first.clone(), public_web_request(operation, "route-a")] {
        let handle = broker.clone();
        waiting.push(tokio::spawn(async move {
            handle.authorize(request).await.unwrap()
        }));
        next_request(&mut receiver).await;
    }
    broker
        .decide(
            operation,
            first.invocation_id,
            ControllerDecision::AllowPublicWebTurn,
        )
        .await
        .unwrap();
    for result in waiting {
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), result)
                .await
                .unwrap()
                .unwrap(),
            Authorization::Allowed(_)
        ));
    }
    assert!(matches!(
        broker
            .authorize(public_web_request(operation, "route-a"))
            .await
            .unwrap(),
        Authorization::Allowed(_)
    ));
    for request in [
        public_web_request(operation, "route-b"),
        public_web_request(OperationId::new(), "route-a"),
        workspace_request(workspace.path()),
    ] {
        let handle = broker.clone();
        let expected = request.clone();
        let wait = tokio::spawn(async move { handle.authorize(request).await.unwrap() });
        assert_eq!(next_request(&mut receiver).await, expected);
        broker
            .decide(
                expected.operation_id,
                expected.invocation_id,
                ControllerDecision::Deny,
            )
            .await
            .unwrap();
        assert!(matches!(wait.await.unwrap(), Authorization::Denied(_)));
    }
    broker.operation_finished(operation);
    let finished = public_web_request(operation, "route-a");
    let handle = broker.clone();
    let expected = finished.clone();
    let waiter = tokio::spawn(async move { handle.authorize(finished).await.unwrap() });
    assert_eq!(
        next_request(&mut receiver).await,
        expected,
        "finished turn grants expire"
    );
    broker
        .decide(
            operation,
            expected.invocation_id,
            ControllerDecision::AllowOnce,
        )
        .await
        .unwrap();
    assert!(matches!(waiter.await.unwrap(), Authorization::Allowed(_)));
    broker.controller_lost();
    assert!(matches!(
        broker
            .authorize(public_web_request(operation, "route-a"))
            .await
            .unwrap(),
        Authorization::Denied(_)
    ));
    broker.shutdown();
    task.await.unwrap();
}

#[test]
fn explicit_search_denial_remains_a_valid_policy_rule() {
    let workspace = tempdir().unwrap();
    let mut deny = rule("deny-search", PolicyDecision::Deny);
    deny.tool = Some("web_search".into());
    let policy = policy(PolicyDecision::Allow, vec![deny], workspace.path());
    assert_eq!(
        policy
            .explain(&public_web_request(OperationId::new(), "exa"))
            .winning_decision,
        PolicyDecision::Deny
    );
}

#[tokio::test]
async fn public_web_preference_is_independent_of_general_tool_allow_but_not_deny() {
    let workspace = tempdir().unwrap();
    for decision in [
        PolicyDecision::Ask,
        PolicyDecision::Allow,
        PolicyDecision::Deny,
    ] {
        let (events, _receiver) = mpsc::unbounded_channel();
        let (broker, task) =
            PermissionBroker::spawn(policy(decision, vec![], workspace.path()), true, events);
        let mut request = public_web_request(OperationId::new(), "route-a");
        request
            .outbound_review
            .as_mut()
            .unwrap()
            .public_web
            .as_mut()
            .unwrap()
            .persisted_allow = true;
        let result = broker.authorize(request).await.unwrap();
        if decision == PolicyDecision::Deny {
            assert!(matches!(result, Authorization::Denied(_)));
        } else {
            let Authorization::Allowed(fact) = result else {
                panic!("expected explicit web consent")
            };
            assert_eq!(
                fact.controller_decision,
                Some(ControllerDecision::AllowPublicWebTurn)
            );
        }
        broker.shutdown();
        task.await.unwrap();
    }
}

fn rule(id: &str, decision: PolicyDecision) -> PermissionRule {
    PermissionRule {
        id: id.to_owned(),
        decision,
        tool: Some("read_file".to_owned()),
        effect: None,
        workspace: None,
        command: None,
    }
}

fn policy(
    default: PolicyDecision,
    rules: Vec<PermissionRule>,
    workspace: &Path,
) -> PermissionPolicy {
    PermissionPolicy::new(default, rules, workspace).expect("permission policy")
}

#[test]
fn configured_default_applies_when_no_rule_matches() {
    let workspace = tempdir().expect("workspace");
    let request = workspace_request(workspace.path());
    let grants = policy::SessionGrants::default();

    for (default, expected) in [
        (PolicyDecision::Deny, PolicyDecision::Deny),
        (PolicyDecision::Ask, PolicyDecision::Ask),
        (PolicyDecision::Allow, PolicyDecision::Allow),
    ] {
        let explanation = policy(default, Vec::new(), workspace.path()).explain(&request);
        assert_eq!(explanation.winning_decision, expected);
        assert!(explanation.matched_rule_ids.is_empty());
    }
    assert!(matches!(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()).evaluate(&request, &grants),
        Evaluation::Ask { rule_ids } if rule_ids.is_empty()
    ));
}

#[test]
fn unattended_ceiling_denies_external_reads_and_effects_even_under_allow() {
    let workspace = tempdir().unwrap();
    let policy = policy(PolicyDecision::Allow, vec![], workspace.path()).workspace_reads_only();
    let grants = policy::SessionGrants::default();
    let local = workspace_request(workspace.path());
    assert_eq!(
        policy.explain(&local).winning_decision,
        PolicyDecision::Allow
    );
    assert!(matches!(
        policy.evaluate(&local, &grants),
        Evaluation::AllowedByPolicy { .. }
    ));
    let external = request(PermissionScope::ExternalPath {
        canonical_path: workspace.path().join("outside"),
    });
    let mut effect = local;
    effect.effect_class = EffectClass::Write;
    for request in [external, effect] {
        assert_eq!(
            policy.explain(&request).winning_decision,
            PolicyDecision::Deny
        );
        assert!(matches!(
            policy.evaluate(&request, &grants),
            Evaluation::Denied { .. }
        ));
    }
}

#[test]
fn personal_memory_default_ask_honors_explicit_rules_and_exact_review() {
    let workspace = tempdir().unwrap();
    let mut memory = request(PermissionScope::PersonalMemory {
        scope: format!("conversation:{}", uuid::Uuid::new_v4()),
        review: false,
    });
    memory.tool_name = "memory_update".into();
    memory.effect_class = EffectClass::Write;
    assert_eq!(
        policy(PolicyDecision::Ask, vec![], workspace.path())
            .explain(&memory)
            .winning_decision,
        PolicyDecision::Allow
    );
    for decision in [PolicyDecision::Deny, PolicyDecision::Ask] {
        let mut explicit = rule("memory-policy", decision);
        explicit.tool = Some("memory_update".into());
        assert_eq!(
            policy(PolicyDecision::Allow, vec![explicit], workspace.path())
                .explain(&memory)
                .winning_decision,
            decision
        );
    }
    memory.scope = PermissionScope::PersonalMemory {
        scope: "user".into(),
        review: true,
    };
    for default in [PolicyDecision::Ask, PolicyDecision::Allow] {
        assert_eq!(
            policy(default, vec![], workspace.path())
                .explain(&memory)
                .winning_decision,
            PolicyDecision::Ask
        );
    }
    assert_eq!(
        policy(PolicyDecision::Deny, vec![], workspace.path())
            .explain(&memory)
            .winning_decision,
        PolicyDecision::Deny
    );
    assert!(
        !scope_contains(&memory.scope, &memory.scope),
        "memory review is never a reusable scope grant"
    );
    assert!(
        policy::SessionGrants::default()
            .insert(&memory, memory.scope.clone())
            .is_err()
    );
}

#[test]
fn personal_memory_scope_does_not_exempt_other_tools_or_unattended_work() {
    let workspace = tempdir().unwrap();
    let mut memory = request(PermissionScope::PersonalMemory {
        scope: "conversation:fixture".into(),
        review: false,
    });
    assert_eq!(
        policy(PolicyDecision::Ask, vec![], workspace.path())
            .explain(&memory)
            .winning_decision,
        PolicyDecision::Ask
    );
    memory.tool_name = "memory_lookup".into();
    assert_eq!(
        policy(PolicyDecision::Allow, vec![], workspace.path())
            .workspace_reads_only()
            .explain(&memory)
            .winning_decision,
        PolicyDecision::Deny
    );
    memory.effect_class = EffectClass::Execute;
    assert_eq!(
        policy(PolicyDecision::Ask, vec![], workspace.path())
            .explain(&memory)
            .winning_decision,
        PolicyDecision::Ask
    );
}

#[test]
fn old_umbrella_and_new_action_denies_cannot_be_bypassed_by_tool_renaming() {
    let root = tempfile::tempdir().unwrap();
    for (action, name) in [
        ("remember", "memory_remember"),
        ("correct", "memory_correct"),
        ("forget", "memory_forget"),
    ] {
        for (rule_name, request_name) in [
            ("memory_update", name),
            (name, "memory_update"),
            (name, name),
        ] {
            let mut memory = request(PermissionScope::PersonalMemory {
                scope: "conversation:test".into(),
                review: false,
            });
            memory.tool_name = request_name.into();
            memory.effect_class = EffectClass::Write;
            memory.final_arguments = serde_json::json!({"action":action});
            let mut deny = rule("deny-memory", PolicyDecision::Deny);
            deny.tool = Some(rule_name.into());
            assert_eq!(
                PermissionPolicy::new(PolicyDecision::Allow, vec![deny], root.path())
                    .unwrap()
                    .explain(&memory)
                    .winning_decision,
                PolicyDecision::Deny
            );
        }
    }
}

#[test]
fn embedded_product_documentation_does_not_prompt_under_the_ask_default() {
    let workspace = tempdir().expect("workspace");
    let mut docs = request(PermissionScope::BuiltInResource {
        id: "xana-documentation".to_owned(),
    });
    docs.tool_name = "xana_docs".to_owned();

    assert_eq!(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path())
            .explain(&docs)
            .winning_decision,
        PolicyDecision::Allow
    );
}

#[test]
fn a_built_in_scope_cannot_exempt_an_unrelated_tool_from_review() {
    let workspace = tempdir().expect("workspace");
    let request = request(PermissionScope::BuiltInResource {
        id: "xana-documentation".to_owned(),
    });

    assert_eq!(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path())
            .explain(&request)
            .winning_decision,
        PolicyDecision::Ask
    );
}

#[test]
fn external_file_reads_always_require_a_controller_unless_denied() {
    let workspace = tempdir().expect("workspace");
    let external = request(PermissionScope::ExternalPath {
        canonical_path: workspace.path().join("outside.txt"),
    });

    assert_eq!(
        policy(PolicyDecision::Allow, Vec::new(), workspace.path())
            .explain(&external)
            .winning_decision,
        PolicyDecision::Ask
    );
    assert_eq!(
        policy(PolicyDecision::Deny, Vec::new(), workspace.path())
            .explain(&external)
            .winning_decision,
        PolicyDecision::Deny
    );
}

#[test]
fn external_services_always_require_an_exact_controller_unless_denied() {
    let workspace = tempdir().expect("workspace");
    let external = request(PermissionScope::External {
        recipient_identity_digest: "recipient-digest".to_owned(),
        operation: "vision.analyze".to_owned(),
    });

    assert_eq!(
        policy(PolicyDecision::Allow, Vec::new(), workspace.path())
            .explain(&external)
            .winning_decision,
        PolicyDecision::Ask
    );
    assert_eq!(
        policy(PolicyDecision::Deny, Vec::new(), workspace.path())
            .explain(&external)
            .winning_decision,
        PolicyDecision::Deny
    );
}

#[tokio::test]
async fn full_child_observation_queue_cannot_hide_a_permission_control_request() {
    let workspace = tempdir().expect("workspace");
    let (events, mut observations, mut controls, dropped) = AgentEventSender::child(1);
    events
        .send(AgentEvent::AssistantTextDelta {
            operation_id: OperationId::new(),
            step_id: StepId::new(),
            text: "fills the observation queue".to_owned(),
        })
        .expect("fill observation queue");
    let (broker, task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        true,
        events,
    );
    let request = workspace_request(workspace.path());
    let waiter = {
        let broker = broker.clone();
        let request = request.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };

    assert!(matches!(
        controls.recv().await,
        Some(AgentEvent::PermissionRequested { request: emitted })
            if emitted == request
    ));
    broker
        .decide(
            request.operation_id,
            request.invocation_id,
            ControllerDecision::Deny,
        )
        .await
        .expect("deny request");
    assert!(matches!(
        waiter
            .await
            .expect("authorization task")
            .expect("authorization"),
        Authorization::Denied(_)
    ));
    assert!(dropped.count() >= 1);
    assert!(observations.try_recv().is_ok());
    broker.shutdown();
    task.await.expect("broker task");
}

#[test]
fn session_grants_deduplicate_and_enforce_the_memory_bound() {
    let mut grants = policy::SessionGrants::default();
    let first = workspace_request(Path::new("/workspace/first"));
    let first_id = grants
        .insert(&first, first.scope.clone())
        .expect("first grant");
    assert_eq!(
        grants
            .insert(&first, first.scope.clone())
            .expect("duplicate grant"),
        first_id
    );

    for index in 1..policy::MAX_SESSION_GRANTS {
        let request = workspace_request(&PathBuf::from(format!("/workspace/{index}")));
        grants
            .insert(&request, request.scope.clone())
            .expect("grant within limit");
    }
    let overflow = workspace_request(Path::new("/workspace/overflow"));
    assert!(grants.insert(&overflow, overflow.scope.clone()).is_err());
}

#[test]
fn deny_then_ask_then_allow_precedence_ignores_rule_order() {
    let workspace = tempdir().expect("workspace");
    let request = workspace_request(workspace.path());
    let cases = [
        (
            vec![
                rule("allow", PolicyDecision::Allow),
                rule("ask", PolicyDecision::Ask),
            ],
            PolicyDecision::Ask,
        ),
        (
            vec![
                rule("ask", PolicyDecision::Ask),
                rule("allow", PolicyDecision::Allow),
            ],
            PolicyDecision::Ask,
        ),
        (
            vec![
                rule("allow", PolicyDecision::Allow),
                rule("deny", PolicyDecision::Deny),
            ],
            PolicyDecision::Deny,
        ),
        (
            vec![
                rule("deny", PolicyDecision::Deny),
                rule("ask", PolicyDecision::Ask),
            ],
            PolicyDecision::Deny,
        ),
    ];

    for (rules, expected) in cases {
        assert_eq!(
            policy(PolicyDecision::Allow, rules, workspace.path())
                .explain(&request)
                .winning_decision,
            expected
        );
    }
}

#[test]
fn pure_explanation_reports_only_matched_ids_and_winner() {
    let workspace = tempdir().expect("workspace");
    let explanation = policy(
        PolicyDecision::Allow,
        vec![
            rule("ask-read", PolicyDecision::Ask),
            rule("deny-read", PolicyDecision::Deny),
        ],
        workspace.path(),
    )
    .explain(&workspace_request(workspace.path()));

    assert_eq!(
        explanation.matched_rule_ids,
        vec!["ask-read".to_owned(), "deny-read".to_owned()]
    );
    assert_eq!(explanation.winning_decision, PolicyDecision::Deny);
}

#[test]
fn tool_effect_workspace_and_command_matchers_are_conjunctive() {
    let workspace = tempdir().expect("workspace");
    let nested = workspace.path().join("nested");
    std::fs::create_dir(&nested).expect("nested");
    let canonical_nested = nested.canonicalize().expect("canonical nested");
    let rules = vec![PermissionRule {
        id: "exact-command".to_owned(),
        decision: PolicyDecision::Allow,
        tool: Some("run_command".to_owned()),
        effect: Some(EffectClass::Execute),
        workspace: Some(PathBuf::from("nested")),
        command: Some("cargo test".to_owned()),
    }];
    let policy = policy(PolicyDecision::Deny, rules, workspace.path());
    let mut command = request(PermissionScope::Command {
        shell: "PowerShell".to_owned(),
        canonical_cwd: canonical_nested,
        command: "cargo test".to_owned(),
    });
    command.tool_name = "run_command".to_owned();
    command.effect_class = EffectClass::Execute;

    assert_eq!(
        policy.explain(&command).winning_decision,
        PolicyDecision::Allow
    );
    if let PermissionScope::Command { command, .. } = &mut command.scope {
        *command = "cargo check".to_owned();
    }
    assert_eq!(
        policy.explain(&command).winning_decision,
        PolicyDecision::Deny
    );
}

#[test]
fn broader_workspace_matches_descendants_but_not_siblings() {
    let workspace = tempdir().expect("workspace");
    let allowed = workspace.path().join("allowed");
    let child = allowed.join("child");
    let sibling = workspace.path().join("sibling");
    std::fs::create_dir_all(&child).expect("child");
    std::fs::create_dir(&sibling).expect("sibling");
    let rules = vec![PermissionRule {
        id: "allowed-tree".to_owned(),
        decision: PolicyDecision::Allow,
        tool: None,
        effect: Some(EffectClass::Read),
        workspace: Some(PathBuf::from("allowed")),
        command: None,
    }];
    let policy = policy(PolicyDecision::Deny, rules, workspace.path());

    assert_eq!(
        policy
            .explain(&workspace_request(
                &child.canonicalize().expect("child canonical")
            ))
            .winning_decision,
        PolicyDecision::Allow
    );
    assert_eq!(
        policy
            .explain(&workspace_request(
                &sibling.canonicalize().expect("sibling canonical")
            ))
            .winning_decision,
        PolicyDecision::Deny
    );
}

#[test]
fn rule_validation_rejects_ambiguous_or_unknown_entries() {
    let blank = PermissionRule {
        id: " ".to_owned(),
        decision: PolicyDecision::Ask,
        tool: Some("read_file".to_owned()),
        effect: None,
        workspace: None,
        command: None,
    };
    assert!(matches!(
        PermissionPolicy::validate_rules(&[blank]),
        Err(PolicyError::BlankRuleId)
    ));
    assert!(matches!(
        PermissionPolicy::validate_rules(&[PermissionRule {
            id: "unknown".to_owned(),
            decision: PolicyDecision::Ask,
            tool: Some("invented".to_owned()),
            effect: None,
            workspace: None,
            command: None,
        }]),
        Err(PolicyError::UnknownTool { .. })
    ));
    assert!(matches!(
        PermissionPolicy::validate_rules(&[PermissionRule {
            id: "empty".to_owned(),
            decision: PolicyDecision::Ask,
            tool: None,
            effect: None,
            workspace: None,
            command: None,
        }]),
        Err(PolicyError::NoMatchers(_))
    ));
}

async fn next_request(receiver: &mut mpsc::UnboundedReceiver<AgentEvent>) -> PermissionRequest {
    loop {
        if let AgentEvent::PermissionRequested { request } =
            receiver.recv().await.expect("permission event")
        {
            return request;
        }
    }
}

#[tokio::test]
async fn ask_is_correlated_and_allow_once_does_not_grant_the_next_invocation() {
    let workspace = tempdir().expect("workspace");
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        true,
        events,
    );
    let first = workspace_request(workspace.path());
    let waiter = {
        let broker = broker.clone();
        let request = first.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };
    assert_eq!(next_request(&mut receiver).await, first);
    broker
        .decide(
            first.operation_id,
            first.invocation_id,
            ControllerDecision::AllowOnce,
        )
        .await
        .expect("matching decision");
    assert!(matches!(
        waiter.await.expect("waiter").expect("authorization"),
        Authorization::Allowed(_)
    ));

    let second = workspace_request(workspace.path());
    let second_waiter = {
        let broker = broker.clone();
        let request = second.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };
    assert_eq!(next_request(&mut receiver).await, second);
    broker
        .decide(
            second.operation_id,
            second.invocation_id,
            ControllerDecision::Deny,
        )
        .await
        .expect("second decision");
    assert!(matches!(
        second_waiter.await.expect("waiter").expect("authorization"),
        Authorization::Denied(_)
    ));
}

#[tokio::test]
async fn denied_prepared_read_is_not_reprompted_by_null_defaults_or_new_call_ids() {
    let workspace = tempdir().unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "red").unwrap();
    let tools = crate::tool::ToolRegistry::builtins_for_tests().unwrap();
    let operation = OperationId::new();
    let prepare = |arguments| {
        tools
            .plan(
                &crate::message::ToolCall {
                    id: ToolInvocationId::new().to_string(),
                    name: "read_file".into(),
                    arguments,
                },
                workspace.path(),
            )
            .unwrap()
            .permission_request(operation, ToolInvocationId::new())
    };
    let first = prepare(serde_json::json!({"path":"notes.txt"}));
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, vec![], workspace.path()),
        true,
        events,
    );
    let waiter = {
        let broker = broker.clone();
        let first = first.clone();
        tokio::spawn(async move { broker.authorize(first).await })
    };
    assert_eq!(next_request(&mut receiver).await, first);
    broker
        .decide(operation, first.invocation_id, ControllerDecision::Deny)
        .await
        .unwrap();
    assert!(matches!(
        waiter.await.unwrap().unwrap(),
        Authorization::Denied(_)
    ));
    let retry = prepare(serde_json::json!({"max_bytes":null,"start_line":null,"path":"notes.txt"}));
    assert_eq!(retry.final_arguments, first.final_arguments);
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), broker.authorize(retry))
            .await
            .unwrap()
            .unwrap(),
        Authorization::Denied(_)
    ));
    while let Ok(event) = receiver.try_recv() {
        assert!(!matches!(event, AgentEvent::PermissionRequested { .. }));
    }
    let mut fresh = prepare(serde_json::json!({"path":"notes.txt"}));
    fresh.operation_id = OperationId::new();
    let waiter = {
        let broker = broker.clone();
        let fresh = fresh.clone();
        tokio::spawn(async move { broker.authorize(fresh).await })
    };
    assert_eq!(next_request(&mut receiver).await, fresh);
    broker
        .decide(
            fresh.operation_id,
            fresh.invocation_id,
            ControllerDecision::AllowOnce,
        )
        .await
        .unwrap();
    assert!(matches!(
        waiter.await.unwrap().unwrap(),
        Authorization::Allowed(_)
    ));
    broker.shutdown();
    task.await.unwrap();
}

#[tokio::test]
async fn restored_denial_capacity_fails_closed_without_reusing_an_allow_grant() {
    let workspace = tempdir().unwrap();
    let facts: Vec<_> = (0..1024)
        .map(|_| PermissionAuditFact {
            request: workspace_request(workspace.path()),
            policy_evaluation: PolicyDecision::Ask,
            controller_decision: Some(ControllerDecision::Deny),
            effective: PolicyDecision::Deny,
        })
        .collect();
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, task) = PermissionBroker::spawn_for_durable_runtime(
        policy(PolicyDecision::Ask, vec![], workspace.path()),
        true,
        events,
        &facts,
    );
    assert!(matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            broker.authorize(workspace_request(workspace.path()))
        )
        .await
        .unwrap()
        .unwrap(),
        Authorization::Denied(_)
    ));
    assert!(
        receiver.try_recv().is_err(),
        "no prompt after denial capacity is exhausted"
    );
    broker.shutdown();
    task.await.unwrap();

    let mut allow = facts[0].clone();
    allow.controller_decision = Some(ControllerDecision::AllowOnce);
    allow.effective = PolicyDecision::Allow;
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, task) = PermissionBroker::spawn_for_durable_runtime(
        policy(PolicyDecision::Ask, vec![], workspace.path()),
        true,
        events,
        [&allow],
    );
    let waiter = {
        let broker = broker.clone();
        let request = allow.request.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };
    let request = next_request(&mut receiver).await;
    broker
        .decide(
            request.operation_id,
            request.invocation_id,
            ControllerDecision::Deny,
        )
        .await
        .unwrap();
    assert!(matches!(
        waiter.await.unwrap().unwrap(),
        Authorization::Denied(_)
    ));
    broker.shutdown();
    task.await.unwrap();
}

#[tokio::test]
async fn session_grant_matches_only_the_bound_tool_effect_and_scope() {
    let workspace = tempdir().expect("workspace");
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        true,
        events,
    );
    let first = workspace_request(workspace.path());
    let waiter = {
        let broker = broker.clone();
        let request = first.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };
    next_request(&mut receiver).await;
    broker
        .decide(
            first.operation_id,
            first.invocation_id,
            ControllerDecision::AllowSession {
                scope: first.scope.clone(),
            },
        )
        .await
        .expect("session decision");
    assert!(matches!(
        waiter.await.expect("waiter").expect("authorization"),
        Authorization::Allowed(_)
    ));

    let second = workspace_request(workspace.path());
    assert!(matches!(
        broker.authorize(second).await.expect("grant authorization"),
        Authorization::Allowed(PermissionAuditFact {
            policy_evaluation: PolicyDecision::Ask,
            controller_decision: None,
            effective: PolicyDecision::Allow,
            ..
        })
    ));

    let (restart_events, mut restart_receiver) = mpsc::unbounded_channel();
    let (restarted, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        true,
        restart_events,
    );
    let after_restart = workspace_request(workspace.path());
    let restarted_waiter = {
        let restarted = restarted.clone();
        let request = after_restart.clone();
        tokio::spawn(async move { restarted.authorize(request).await })
    };
    assert_eq!(next_request(&mut restart_receiver).await, after_restart);
    restarted
        .decide(
            after_restart.operation_id,
            after_restart.invocation_id,
            ControllerDecision::Deny,
        )
        .await
        .expect("cleanup denial");
    restarted_waiter
        .await
        .expect("restarted waiter")
        .expect("typed denial");
}

#[tokio::test]
async fn explicit_deny_cannot_be_overridden_by_a_session_grant() {
    let workspace = tempdir().expect("workspace");
    let (events, mut receiver) = mpsc::unbounded_channel();
    let ask_policy = policy(PolicyDecision::Ask, Vec::new(), workspace.path());
    let (broker, _task) = PermissionBroker::spawn(ask_policy, true, events);
    let first = workspace_request(workspace.path());
    let waiter = {
        let broker = broker.clone();
        let request = first.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };
    next_request(&mut receiver).await;
    broker
        .decide(
            first.operation_id,
            first.invocation_id,
            ControllerDecision::AllowSession {
                scope: first.scope.clone(),
            },
        )
        .await
        .expect("session decision");
    waiter.await.expect("waiter").expect("authorization");

    // Pure evaluator proof: matching deny is selected before grant lookup.
    let mut grants = policy::SessionGrants::default();
    grants
        .insert(&first, first.scope.clone())
        .expect("session grant");
    let deny_policy = policy(
        PolicyDecision::Ask,
        vec![rule("never-read", PolicyDecision::Deny)],
        workspace.path(),
    );
    assert!(matches!(
        deny_policy.evaluate(&first, &grants),
        Evaluation::Denied { .. }
    ));
}

#[tokio::test]
async fn unattended_and_lost_controller_requests_fail_closed() {
    let workspace = tempdir().expect("workspace");
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (unattended, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        false,
        events,
    );
    assert!(matches!(
        unattended
            .authorize(workspace_request(workspace.path()))
            .await
            .expect("typed denial"),
        Authorization::Denied(_)
    ));
    assert!(matches!(
        receiver.recv().await,
        Some(AgentEvent::PermissionAudited { .. })
    ));

    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        true,
        events,
    );
    let pending = workspace_request(workspace.path());
    let waiter = {
        let broker = broker.clone();
        tokio::spawn(async move { broker.authorize(pending).await })
    };
    next_request(&mut receiver).await;
    broker.controller_lost();
    assert!(matches!(
        waiter.await.expect("waiter").expect("typed denial"),
        Authorization::Denied(_)
    ));
}

#[tokio::test]
async fn stale_mismatched_duplicate_and_scope_widening_decisions_are_rejected() {
    let workspace = tempdir().expect("workspace");
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        true,
        events,
    );
    let request = workspace_request(workspace.path());
    let waiter = {
        let broker = broker.clone();
        let request = request.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };
    next_request(&mut receiver).await;

    assert!(matches!(
        broker
            .decide(
                request.operation_id,
                ToolInvocationId::new(),
                ControllerDecision::Deny,
            )
            .await,
        Err(DecisionError::Unknown { .. })
    ));
    assert!(matches!(
        broker
            .decide(
                request.operation_id,
                request.invocation_id,
                ControllerDecision::AllowSession {
                    scope: PermissionScope::Unscoped,
                },
            )
            .await,
        Err(DecisionError::ScopeMismatch { .. })
    ));
    broker
        .decide(
            request.operation_id,
            request.invocation_id,
            ControllerDecision::Deny,
        )
        .await
        .expect("matching denial");
    waiter.await.expect("waiter").expect("authorization");
    assert!(matches!(
        broker
            .decide(
                request.operation_id,
                request.invocation_id,
                ControllerDecision::Deny,
            )
            .await,
        Err(DecisionError::Unknown { .. })
    ));
}

#[tokio::test]
async fn cancelling_authorization_removes_the_pending_request() {
    let workspace = tempdir().expect("workspace");
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Ask, Vec::new(), workspace.path()),
        true,
        events,
    );
    let request = workspace_request(workspace.path());
    let waiter = {
        let broker = broker.clone();
        let request = request.clone();
        tokio::spawn(async move { broker.authorize(request).await })
    };
    next_request(&mut receiver).await;
    waiter.abort();
    let _ = waiter.await;
    tokio::task::yield_now().await;

    assert!(matches!(
        broker
            .decide(
                request.operation_id,
                request.invocation_id,
                ControllerDecision::AllowOnce,
            )
            .await,
        Err(DecisionError::Unknown { .. })
    ));
}

#[tokio::test]
async fn audit_binds_final_arguments_and_controller_decision() {
    let workspace = tempdir().expect("workspace");
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (broker, _task) = PermissionBroker::spawn(
        policy(PolicyDecision::Allow, Vec::new(), workspace.path()),
        false,
        events,
    );
    let request = workspace_request(workspace.path());
    let authorization = broker.authorize(request.clone()).await.expect("allow");
    let fact = match &authorization {
        Authorization::Allowed(fact) | Authorization::Denied(fact) => fact,
    };

    assert_eq!(fact.request.final_arguments, request.final_arguments);
    assert_eq!(fact.request.invocation_id, request.invocation_id);
    assert_eq!(fact.controller_decision, None);
    assert_eq!(fact.effective, PolicyDecision::Allow);
    assert!(matches!(
        receiver.recv().await,
        Some(AgentEvent::PermissionAudited { fact: event_fact }) if event_fact == *fact
    ));
}
