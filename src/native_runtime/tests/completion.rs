use super::*;
use crate::completion_evidence::{
    AcceptanceCondition, CompletionContract, EvidenceOutcome, WorkKind,
};

struct StopAfterFiniteAcceptance;
impl BoundaryObserver for StopAfterFiniteAcceptance {
    fn reached(&self, site: CrashSite) -> anyhow::Result<()> {
        if site == CrashSite::AfterOperationAccepted {
            anyhow::bail!("injected crash after atomic admission");
        }
        Ok(())
    }
}

#[tokio::test]
async fn accepted_crash_boundary_already_contains_the_frozen_finite_contract() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(vec![Ok(Message::text(Role::Assistant, "should not run"))].into()),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), workspace.clone());
    let session = DurableSession::create(data.path(), workspace.clone()).unwrap();
    let path = session.path().to_owned();
    let policy = PermissionPolicy::new(PolicyDecision::Allow, vec![], &workspace).unwrap();
    let mut runtime = RuntimeHandle::spawn_persistent(
        agent.with_boundary_observer(Arc::new(StopAfterFiniteAcceptance)),
        policy,
        true,
        session,
        assembler,
        None,
    )
    .unwrap();
    let contract = CompletionContract {
        conditions: vec![AcceptanceCondition::CommandSucceeded {
            command: "cargo test".into(),
            cwd: ".".into(),
        }],
    };
    runtime
        .send(RuntimeCommand::SubmitFiniteTurn {
            operation_id: OperationId::new(),
            input: "finite task".into(),
            kind: WorkKind::Root,
            contract: contract.clone(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            runtime.next_event().await,
            Some(AgentEvent::CommandRejected { .. })
        ) {}
    })
    .await
    .unwrap();
    runtime.shutdown_owned().await;
    assert!(requests.lock().unwrap().is_empty());
    let restored = reduce(&SessionStore::inspect(&path).unwrap().records).unwrap();
    assert_eq!(restored.completion_evidence[0].contract, contract);
    assert_eq!(restored.completion_evidence[0].revision, 1);
}

#[tokio::test]
async fn finite_claim_missing_check_fails_and_receipt_survives_owner_restart() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let provider = QueueTransport {
        responses: Mutex::new(vec![Ok(Message::text(Role::Assistant, "done"))].into()),
        requests: Arc::new(Mutex::new(Vec::new())),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), workspace.clone());
    let session = DurableSession::create(data.path(), workspace.clone()).unwrap();
    let path = session.path().to_owned();
    let policy = PermissionPolicy::new(PolicyDecision::Allow, vec![], &workspace).unwrap();
    let mut runtime =
        RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler, None).unwrap();
    let operation = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitFiniteTurn {
            operation_id: operation,
            input: "complete task".into(),
            kind: WorkKind::Root,
            contract: CompletionContract {
                conditions: vec![AcceptanceCondition::CommandSucceeded {
                    command: "cargo test".into(),
                    cwd: ".".into(),
                }],
            },
        })
        .await
        .unwrap();
    let mut evidence = None;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match runtime.next_event().await.unwrap() {
                AgentEvent::CompletionEvidenceRecorded {
                    evidence: record, ..
                } => evidence = Some(record),
                AgentEvent::OperationStateChanged {
                    state: OperationState::Finished(outcome),
                    ..
                } => {
                    assert_eq!(outcome, OperationOutcome::Failed);
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        evidence.as_ref().unwrap().outcome,
        EvidenceOutcome::NeedsAttention
    );
    runtime.shutdown_owned().await;
    let loaded = SessionStore::inspect(&path).unwrap();
    let restored = reduce(&loaded.records).unwrap();
    assert_eq!(restored.completion_evidence.last(), evidence.as_ref());
    assert!(
        loaded
            .records
            .iter()
            .any(|record| matches!(record.record, SessionRecord::FiniteOperationAccepted { .. }))
    );
    assert!(!loaded.records.iter().any(|record| matches!(
        record.record,
        SessionRecord::OperationFinished {
            outcome: OperationOutcome::Completed,
            ..
        }
    )));
}

#[tokio::test]
async fn ordinary_conversation_has_no_implicit_completion_metric() {
    let (agent, _, _) = queue_agent(vec![Ok(Message::text(Role::Assistant, "hello"))], vec![]);
    let mut runtime = spawn_runtime(agent);
    let operation = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: operation,
            input: "hello".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match runtime.next_event().await.unwrap() {
                AgentEvent::CompletionEvidenceRecorded { .. } => {
                    panic!("ordinary chat must not have a completion score")
                }
                AgentEvent::OperationStateChanged {
                    state: OperationState::Finished(OperationOutcome::Completed),
                    ..
                } => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    runtime.shutdown_owned().await;
}

#[tokio::test]
async fn rejected_preparation_is_not_hidden_by_another_prepared_call_in_the_same_step() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    std::fs::write(workspace.join("present.txt"), "fixture").unwrap();
    let calls = Message {
        role: Role::Assistant,
        content: ["absent.txt", "present.txt"]
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                ContentBlock::ToolCall(ToolCall {
                    id: format!("read-{index}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path":path}),
                })
            })
            .collect(),
    };
    let provider = QueueTransport {
        responses: Mutex::new(
            vec![
                Ok(calls),
                Ok(Message::text(Role::Assistant, "everything passed")),
            ]
            .into(),
        ),
        requests: Arc::new(Mutex::new(Vec::new())),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, assembler) =
        super::core::persistent_tool_agent(Box::new(provider), workspace.clone(), 3);
    let session = DurableSession::create(data.path(), workspace.clone()).unwrap();
    let policy = PermissionPolicy::new(PolicyDecision::Allow, vec![], &workspace).unwrap();
    let mut runtime =
        RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler, None).unwrap();
    runtime
        .send(RuntimeCommand::SubmitFiniteTurn {
            operation_id: OperationId::new(),
            input: "read both".into(),
            kind: WorkKind::Root,
            contract: Default::default(),
        })
        .await
        .unwrap();
    let mut observed = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match runtime.next_event().await.unwrap() {
                AgentEvent::CompletionEvidenceRecorded { evidence, .. } => {
                    assert_eq!(
                        evidence.effects.len(),
                        1,
                        "only the existing file reached prepared dispatch"
                    );
                    assert!(evidence.omitted_observations);
                    assert!(!evidence.supported());
                    observed = true;
                }
                AgentEvent::OperationStateChanged {
                    state: OperationState::Finished(outcome),
                    ..
                } => {
                    assert_eq!(outcome, OperationOutcome::Failed);
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(observed);
    runtime.shutdown_owned().await;
}

#[tokio::test]
async fn actual_command_exit_status_defeats_false_final_claim() {
    for exit_code in [0, 1] {
        let command = format!("exit {exit_code}");
        let data = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let workspace = workspace.path().canonicalize().unwrap();
        let call = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "real-check".into(),
                name: "run_command".into(),
                arguments: serde_json::json!({"command":command,"cwd":"."}),
            })],
        };
        let provider = QueueTransport {
            responses: Mutex::new(
                vec![
                    Ok(call),
                    Ok(Message::text(Role::Assistant, "all checks passed")),
                ]
                .into(),
            ),
            requests: Arc::new(Mutex::new(Vec::new())),
            completed: Arc::new(AtomicBool::new(false)),
            deltas: vec![],
        };
        let (agent, assembler) =
            super::core::persistent_tool_agent(Box::new(provider), workspace.clone(), 3);
        let session = DurableSession::create(data.path(), workspace.clone()).unwrap();
        let policy = PermissionPolicy::new(PolicyDecision::Allow, vec![], &workspace).unwrap();
        let mut runtime =
            RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler, None).unwrap();
        runtime
            .send(RuntimeCommand::SubmitFiniteTurn {
                operation_id: OperationId::new(),
                input: "run the check".into(),
                kind: WorkKind::Root,
                contract: CompletionContract {
                    conditions: vec![AcceptanceCondition::CommandSucceeded {
                        command,
                        cwd: ".".into(),
                    }],
                },
            })
            .await
            .unwrap();
        let mut observed = false;
        let mut delivered = false;
        // This is an exit-status contract, not a shell-startup benchmark. The
        // real command has a 30 s execution limit; allow that limit plus bounded
        // admission/journal time before the test watchdog declares a hang.
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                match runtime.next_event().await.unwrap() {
                    AgentEvent::CompletionEvidenceRecorded { evidence, .. } => {
                        assert_eq!(evidence.checks[0].exit_code, Some(exit_code));
                        assert_eq!(evidence.supported(), exit_code == 0);
                        observed = true
                    }
                    AgentEvent::AssistantMessage { .. } => delivered = true,
                    AgentEvent::OperationStateChanged {
                        state: OperationState::Finished(outcome),
                        ..
                    } => {
                        assert_eq!(
                            outcome,
                            if exit_code == 0 {
                                OperationOutcome::Completed
                            } else {
                                OperationOutcome::Failed
                            }
                        );
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("command evidence settles within the execution limit plus orchestration grace");
        assert!(observed && delivered);
        runtime.shutdown_owned().await;
    }
}
