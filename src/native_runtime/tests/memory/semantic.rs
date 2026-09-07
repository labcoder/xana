use super::*;

#[tokio::test]
async fn correction_refreshes_same_turn_and_forget_stops_revoked_dispatch() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let store = ProtectedStore::initialize(
        data.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = crate::identity::SessionId::new();
    let owner = MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(id.to_string().parse().unwrap()),
            ..Default::default()
        },
    );
    let record = owner
        .remember(
            MemoryScope::Conversation(id.to_string().parse().unwrap()),
            "My favorite color is red".into(),
            None,
        )
        .unwrap();
    let session = DurableSession::create_protected(store.clone(), root.clone(), id).unwrap();
    let correction = "My favorite color is blue now; correct that memory.";
    let correction_call = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolCall(ToolCall {
            id: "correct-color".into(),
            name: "memory_update".into(),
            arguments: serde_json::json!({
                "action":"correct", "id":record.id, "revision":1,
                "statement":"My favorite color is blue", "quote":correction, "risk":"ordinary"
            }),
        })],
    };
    let forget_call = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolCall(ToolCall {
            id: "forget-color".into(),
            name: "memory_update".into(),
            arguments: serde_json::json!({"action":"forget", "id":record.id, "revision":2}),
        })],
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(
            vec![
                Ok(correction_call),
                Ok(Message::text(Role::Assistant, "The correction was saved.")),
                Ok(forget_call),
                Ok(Message::text(Role::Assistant, "must never dispatch")),
            ]
            .into(),
        ),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, assembler) = memory_agent(Box::new(provider), root.clone(), Some(owner.clone()));
    let policy = PermissionPolicy::new(PolicyDecision::Allow, vec![], &root).unwrap();
    let mut runtime = RuntimeHandle::spawn_persistent(
        agent,
        policy,
        true,
        session,
        assembler,
        Some(owner.clone()),
    )
    .unwrap();
    for (input, expected) in [
        (correction, OperationOutcome::Completed),
        ("Forget my favorite color memory.", OperationOutcome::Failed),
    ] {
        let operation_id = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input: input.into(),
            })
            .await
            .unwrap();
        let (outcome, reviews) = tokio::time::timeout(Duration::from_secs(5), async {
            let mut reviews = 0;
            loop {
                match runtime.next_event().await.unwrap() {
                    AgentEvent::PermissionRequested { request } => {
                        assert_eq!(request.tool_name, "memory_update");
                        assert!(matches!(
                            request.scope,
                            PermissionScope::PersonalMemory { review: true, .. }
                        ));
                        reviews += 1;
                        runtime
                            .send(RuntimeCommand::DecidePermission {
                                operation_id: request.operation_id,
                                invocation_id: request.invocation_id,
                                decision: ControllerDecision::AllowOnce,
                            })
                            .await
                            .unwrap();
                    }
                    AgentEvent::OperationStateChanged {
                        operation_id: actual,
                        state: OperationState::Finished(outcome),
                    } if actual == operation_id => break (outcome, reviews),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome, expected);
        assert_eq!(
            reviews, 1,
            "correct and forget require exact review even under allow"
        );
    }
    {
        let captured = requests.lock().unwrap();
        assert_eq!(
            captured.len(),
            3,
            "forget must prevent the next provider request"
        );
        let before = crate::completion_evidence::message_text(&captured[0][0]);
        let after = crate::completion_evidence::message_text(&captured[1][0]);
        assert!(before.contains("My favorite color is red"));
        assert!(!after.contains("My favorite color is red"));
        assert!(after.contains("My favorite color is blue"));
        assert_eq!(after.matches("kind=\"personal_memory\"").count(), 1);
    }
    assert_eq!(
        owner.record(record.id).unwrap().state,
        crate::memory::MemoryState::Forgotten
    );
    assert!(runtime.shutdown_owned().await);
}
