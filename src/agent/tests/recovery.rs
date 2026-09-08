use super::*;
use crate::provider::HelperGenerationPolicy;

struct RecoveryProvider {
    reply: u8,
}
impl ConversationalProvider for RecoveryProvider {
    fn stream_message<'a>(
        &'a self,
        _: &'a [Message],
        _: &'a [&'a ToolDefinition],
        _: StepId,
        _: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async { panic!("answer recovery must never call the ordinary tool-capable path") })
    }
    fn stream_helper_message<'a>(
        &'a self,
        _: &'a [Message],
        policy: HelperGenerationPolicy<'a>,
        step: StepId,
        sink: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            assert_eq!(policy.max_output_tokens, 1024);
            assert!(!policy.disable_reasoning && !policy.zero_temperature);
            match self.reply {
                0 => Ok(Message::text(Role::Assistant, " ")),
                1 => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
                2 => Err(ProviderError::classified(
                    crate::provider::ProviderErrorKind::OutputLimit,
                    "truncated",
                )),
                _ => {
                    sink.reasoning_delta(step, "synthetic reasoning");
                    sink.text_delta(step, "I don't know yet.");
                    sink.usage(ProviderUsage {
                        total_tokens: Some(42),
                        ..Default::default()
                    });
                    Ok(Message::text(Role::Assistant, "I don't know yet."))
                }
            }
        })
    }
}

#[tokio::test(start_paused = true)]
async fn memory_answer_recovery_rejects_empty_truncated_and_timed_out_responses() {
    for reply in 0..=3 {
        let workspace = tempdir().unwrap();
        let (provider, _) = ScriptedChatTransport::new(vec![]);
        let mut agent = make_agent(provider, workspace.path(), 8);
        agent.provider = Box::new(RecoveryProvider { reply });
        let (events, mut receiver) = mpsc::unbounded_channel();
        let events: AgentEventSender = events.into();
        let operation = OperationId::new();
        let sink = EventDeltaSink {
            operation_id: operation,
            events: events.clone(),
            usage: Mutex::new(UsageAccumulator::default()),
        };
        let mut messages = vec![Message::text(Role::User, "what is my name?")];
        let began = tokio::time::Instant::now();
        let result = agent
            .recover_memory_answer(
                operation,
                &mut messages,
                &agent.prompt,
                None,
                &events,
                &sink,
            )
            .await;
        assert_eq!(sink.usage().requests, 1);
        if reply == 3 {
            let Ok(AgentTurnOutcome::Completed(result)) = result else {
                panic!("valid answer")
            };
            assert_eq!(result.usage.total_tokens, Some(42));
            assert!(sink.timing(true, true).first_delta.is_some());
        } else {
            assert!(result.is_err());
            assert_eq!(sink.usage().total_tokens, None, "unknown usage is not zero");
            assert!(
                crate::completion_evidence::message_text(messages.last().unwrap())
                    .contains("Xana stopped after two rejected memory requests")
            );
            assert!(matches!(
                receiver.try_recv(),
                Ok(AgentEvent::AssistantMessage { .. })
            ));
            if reply == 1 {
                assert_eq!(began.elapsed(), std::time::Duration::from_secs(15));
            }
        }
    }
}

#[test]
fn generation_timing_resets_each_request_and_does_not_count_empty_deltas() {
    let (events, _) = mpsc::unbounded_channel();
    let sink = EventDeltaSink {
        operation_id: OperationId::new(),
        events: events.into(),
        usage: Mutex::new(UsageAccumulator::default()),
    };
    sink.begin_request();
    sink.text_delta(StepId::new(), "");
    assert!(sink.timing(false, true).first_delta.is_none());
    sink.reasoning_delta(StepId::new(), "secret is never copied into timings");
    let first = sink.timing(false, true).first_delta;
    assert!(first.is_some());
    sink.text_delta(StepId::new(), "final text");
    assert_eq!(sink.timing(false, true).first_delta, first);
    sink.begin_request();
    assert!(sink.timing(true, false).first_delta.is_none());
}
