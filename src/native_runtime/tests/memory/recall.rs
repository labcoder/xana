use super::*;
use crate::provider::{HelperCapabilities, HelperGenerationPolicy};
mod qualification;

struct InvalidRecallProvider {
    attempts: Arc<AtomicUsize>,
    recoveries: Arc<AtomicUsize>,
    batch_size: usize,
    recovery_calls_tool: bool,
}

impl ConversationalProvider for InvalidRecallProvider {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        _: &'a [&'a ToolDefinition],
        _: StepId,
        _: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            let system = crate::completion_evidence::message_text(&messages[0]);
            assert!(system.contains("No saved user facts exist in the currently eligible scopes"));
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            // The reported regression: changing invented quotes and omitting
            // the statement. No tool execution or successful answer is scripted.
            Ok(Message {
                role: Role::Assistant,
                content: (0..self.batch_size).map(|index| ContentBlock::ToolCall(ToolCall {
                    id: format!("bad-{attempt}-{index}"),
                    name: "memory_update".into(),
                    arguments: serde_json::json!({"action":"remember", "risk":"ordinary", "quote":format!("invented quote {attempt}")}),
                })).collect(),
            })
        })
    }

    fn helper_capabilities(&self) -> HelperCapabilities {
        HelperCapabilities {
            output_limit: true,
            ..Default::default()
        }
    }

    fn stream_helper_message<'a>(
        &'a self,
        messages: &'a [Message],
        policy: HelperGenerationPolicy<'a>,
        _: StepId,
        _: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            self.recoveries.fetch_add(1, Ordering::SeqCst);
            assert!(policy.max_output_tokens <= 1024);
            assert!(
                !policy.disable_reasoning,
                "do not silently change reasoning preferences"
            );
            assert!(messages.iter().any(|message| {
                crate::completion_evidence::message_text(message).contains("what is my name?")
            }));
            if self.recovery_calls_tool {
                return Ok(save_request("what is my name?", "invented recovery fact"));
            }
            Ok(Message::text(
                Role::Assistant,
                "I don't know your name yet.",
            ))
        })
    }
}

#[tokio::test]
async fn unknown_name_recovers_after_one_repair_and_reopens_without_saving_guesses() {
    exercise_recall_recovery(1, false).await;
}

#[tokio::test]
async fn a_large_invalid_batch_cannot_skip_visible_recovery_or_execute_its_tool_call() {
    exercise_recall_recovery(8, true).await;
}

async fn exercise_recall_recovery(batch_size: usize, recovery_calls_tool: bool) {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let recovery = RecoveryIdentity::generate();
    drop(ProtectedStore::initialize(data.path(), &recovery, &TestCustody::default()).unwrap());
    let id = crate::identity::SessionId::new();
    for phase in 0..2 {
        let store = ProtectedStore::recover(data.path(), &recovery).unwrap();
        let owner = MemoryOwner::new(
            store.clone(),
            MemoryContext {
                conversation: Some(id.to_string().parse().unwrap()),
                ..Default::default()
            },
        );
        let session = if phase == 0 {
            DurableSession::create_protected(store.clone(), root.clone(), id).unwrap()
        } else {
            DurableSession::resume_protected(store.clone(), id)
                .unwrap()
                .0
        };
        let attempts = Arc::new(AtomicUsize::new(0));
        let recoveries = Arc::new(AtomicUsize::new(0));
        let provider = InvalidRecallProvider {
            attempts: attempts.clone(),
            recoveries: recoveries.clone(),
            batch_size,
            recovery_calls_tool,
        };
        let (agent, assembler) =
            memory_agent(Box::new(provider), root.clone(), Some(owner.clone()));
        let policy = PermissionPolicy::new(PolicyDecision::Ask, vec![], &root).unwrap();
        let mut runtime = RuntimeHandle::spawn_persistent(
            agent,
            policy,
            true,
            session,
            assembler,
            Some(owner.clone()),
        )
        .unwrap();
        let operation = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: operation,
                input: "what is my name?".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_finished(&mut runtime, operation)
            )
            .await
            .unwrap(),
            if recovery_calls_tool {
                OperationOutcome::Failed
            } else {
                OperationOutcome::Completed
            }
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            if batch_size == 1 { 2 } else { 1 },
            "only one corrective attempt"
        );
        assert_eq!(
            recoveries.load(Ordering::SeqCst),
            1,
            "one bounded answer-only request"
        );
        assert!(owner.page(None, None).unwrap().records.is_empty());
        assert!(runtime.shutdown_owned().await);
        let (_, restored) = DurableSession::inspect_protected(&store, id).unwrap();
        assert!(
            restored.audits.is_empty(),
            "invalid mutations never ask for approval"
        );
        let messages = restored.conversation_path().unwrap();
        let answer = crate::completion_evidence::message_text(messages.last().unwrap());
        if recovery_calls_tool {
            assert!(
                answer.contains("Xana stopped after two rejected memory requests"),
                "{answer}"
            );
        } else {
            assert_eq!(answer, "I don't know your name yet.");
        }
    }
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}
