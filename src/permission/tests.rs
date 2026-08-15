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
