use super::*;

/// Check the actual request boundary, not just a rendered readiness notice.
struct UnavailableTransport(QueueTransport);

impl ConversationalProvider for UnavailableTransport {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        assert!(tools.is_empty(), "unavailable memory was advertised");
        let system = crate::completion_evidence::message_text(&messages[0]);
        assert!(system.contains(crate::memory::MemoryReadiness::Unavailable.notice()));
        assert!(!system.contains(crate::memory::MEMORY_GUIDANCE));
        self.0.stream_message(messages, tools, step_id, deltas)
    }
}

#[tokio::test]
async fn unknown_name_in_legacy_home_needs_one_generation_and_no_tool_or_migration() {
    unavailable_turn(false).await;
}

#[tokio::test]
async fn stale_memory_retries_stop_before_a_third_generation_without_permissions_or_effects() {
    unavailable_turn(true).await;
}

async fn unavailable_turn(retry: bool) {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let session = DurableSession::create(data.path(), root.clone()).unwrap();
    let session_id = session.session_id();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = UnavailableTransport(QueueTransport {
        responses: Mutex::new(if retry {
            [
                Ok(save_request(
                    "do you know my name?",
                    "first incorrect guess",
                )),
                Ok(save_request(
                    "do you know my name?",
                    "different incorrect guess",
                )),
            ]
            .into()
        } else {
            [Ok(Message::text(
                Role::Assistant,
                "I don't know your name yet.",
            ))]
            .into()
        }),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    });
    let (agent, assembler) = memory_agent(Box::new(provider), root.clone(), None);
    let policy = PermissionPolicy::new(PolicyDecision::Ask, vec![], &root).unwrap();
    let mut runtime =
        RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler, None).unwrap();
    let operation = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: operation,
            input: "do you know my name?".into(),
        })
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        receive_finished(&mut runtime, operation),
    )
    .await
    .unwrap();
    if retry {
        assert_eq!(outcome, OperationOutcome::Failed);
    } else {
        assert_eq!(outcome, OperationOutcome::Completed);
    }
    assert_eq!(requests.lock().unwrap().len(), if retry { 2 } else { 1 });
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    assert!(runtime.shutdown_owned().await);
    let (_, restored) = DurableSession::inspect_restored(data.path(), session_id).unwrap();
    assert!(
        restored.audits.is_empty(),
        "unavailable tools never reach approval"
    );
    let messages = restored.conversation_path().unwrap();
    let results: Vec<_> = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| {
            if let ContentBlock::ToolResult(result) = block {
                Some(result)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(results.len(), if retry { 2 } else { 0 });
    assert!(
        results
            .iter()
            .all(|result| result.failure == Some(crate::message::ToolFailure::Unavailable))
    );
}
