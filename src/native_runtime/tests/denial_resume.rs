//! Same-operation authority survives a runtime restart and argument defaults.

use super::*;

#[tokio::test]
async fn restarted_operation_does_not_reask_an_equivalent_denied_read() {
    for protected in [false, true] {
        let data = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let root = workspace.path().canonicalize().unwrap();
        std::fs::write(root.join("note.txt"), "NOT_AUTHORIZED_TO_READ").unwrap();
        let store = protected.then(|| {
            crate::storage::ProtectedStore::initialize(
                data.path(),
                &crate::storage::RecoveryIdentity::generate(),
                &crate::storage::TestCustody::default(),
            )
            .unwrap()
        });
        let id = crate::identity::SessionId::new();
        let session = if let Some(store) = &store {
            DurableSession::create_protected(store.clone(), root.clone(), id).unwrap()
        } else {
            DurableSession::create_with_id(data.path(), root.clone(), id).unwrap()
        };
        let call = |arguments| Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "provider-call".into(),
                name: "read_file".into(),
                arguments,
            })],
        };
        let provider = |responses: Vec<Message>| {
            Box::new(QueueTransport {
                responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                requests: Arc::new(Mutex::new(Vec::new())),
                completed: Arc::new(AtomicBool::new(false)),
                deltas: Vec::new(),
            })
        };
        let (agent, assembler) = core::persistent_tool_agent(
            provider(vec![call(serde_json::json!({"path":"note.txt"}))]),
            root.clone(),
            1,
        );
        let policy = || PermissionPolicy::new(PolicyDecision::Ask, vec![], &root).unwrap();
        let mut runtime =
            RuntimeHandle::spawn_persistent(agent, policy(), true, session, assembler, None)
                .unwrap();
        let operation_id = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input: "read the note".into(),
            })
            .await
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match runtime.next_event().await.unwrap() {
                    AgentEvent::PermissionRequested { request } => {
                        runtime
                            .send(RuntimeCommand::DecidePermission {
                                operation_id,
                                invocation_id: request.invocation_id,
                                decision: ControllerDecision::Deny,
                            })
                            .await
                            .unwrap();
                    }
                    AgentEvent::RoundBudgetReached { suspension } => break suspension,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(runtime.shutdown_owned().await);
        let session = if let Some(store) = &store {
            DurableSession::resume_protected(store.clone(), id)
                .unwrap()
                .0
        } else {
            DurableSession::resume(data.path(), id).unwrap().0
        };
        assert!(
            session
                .unfinished_permission_evidence()
                .any(|fact| fact.controller_decision == Some(ControllerDecision::Deny))
        );
        let (agent, assembler) = core::persistent_tool_agent(
            provider(vec![
                call(serde_json::json!({"path":"note.txt", "max_bytes":null})),
                Message::text(Role::Assistant, "The read was denied."),
            ]),
            root.clone(),
            2,
        );
        let mut runtime =
            RuntimeHandle::spawn_persistent(agent, policy(), true, session, assembler, None)
                .unwrap();
        runtime
            .send(RuntimeCommand::DecideRoundBudget {
                operation_id,
                suspension_id: first.id,
                action: RoundBudgetAction::Continue,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match runtime.next_event().await.unwrap() {
                    AgentEvent::PermissionRequested { .. } => panic!("denied read asked again"),
                    AgentEvent::OperationStateChanged {
                        operation_id: actual,
                        state: OperationState::Finished(outcome),
                    } if actual == operation_id => {
                        assert_eq!(outcome, OperationOutcome::Completed);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(runtime.shutdown_owned().await);
    }
}
