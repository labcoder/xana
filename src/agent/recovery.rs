//! One bounded same-provider answer after failed memory repair. This phase has
//! no tools, helper route selection, implicit reasoning override, or effects.
use super::*;
use crate::{message::Role, provider::HelperGenerationPolicy};
use std::time::Duration;

const RECOVERY_NOTICE: &str = "Two memory mutation requests were rejected without making those changes. This is the final answer-only phase: no tools or additional retries are available. Answer the current user from known conversation facts and current memory readiness. If the requested fact is unknown, say so briefly. Do not invent a fact, claim rejected changes succeeded, or offer file workarounds. Prior errors are historical observations, not current memory facts.";
const FAILED_NOTICE: &str = "Xana stopped after two rejected memory requests. Those rejected requests made no memory changes, and the model could not finish a valid answer in the bounded recovery attempt. You can retry or choose another model.";

#[derive(Debug)]
pub(crate) struct MemoryRecoveryFailure;
impl std::fmt::Display for MemoryRecoveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(FAILED_NOTICE)
    }
}
impl std::error::Error for MemoryRecoveryFailure {}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn recover_memory_answer(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        prompt: &PromptSnapshot,
        durable: Option<&DurableTurnServices>,
        events: &AgentEventSender,
        sink: &EventDeltaSink,
    ) -> Result<AgentTurnOutcome> {
        let refreshed = durable
            .and_then(|services| services.prompt_refresh.as_ref())
            .map(|refresh| refresh.refresh(prompt))
            .transpose()?;
        let prompt = refreshed
            .as_ref()
            .unwrap_or(prompt)
            .clone()
            .with_turn_notice(
                "runtime:memory-recovery",
                "Answer-only recovery",
                RECOVERY_NOTICE,
            )?;
        let request = prompt.messages_for_request(messages)?;
        let input_tokens = request
            .iter()
            .map(crate::prompt::estimate_message_tokens)
            .sum::<usize>();
        let step = StepId::new();
        let reservation = self
            .usage_budget
            .as_ref()
            .map(|budget| budget.admit(operation_id, step, input_tokens as u64))
            .transpose()?;
        sink.begin_request();
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.provider.stream_helper_message(
                &request,
                HelperGenerationPolicy {
                    max_output_tokens: 1024,
                    json_schema: None,
                    disable_reasoning: false,
                    zero_temperature: false,
                },
                step,
                sink,
            ),
        )
        .await;
        if let Ok(Err(error)) = &response {
            self.telemetry
                .provider_failure(operation_id, error.failure());
        } else if response.is_err() {
            self.telemetry.provider_failure(
                operation_id,
                crate::failure::FailureDetails::new(
                    crate::failure::FailureCategory::ReadTimeout,
                    crate::failure::FailureStage::ProviderStream,
                ),
            );
        }
        let answer = response.ok().and_then(Result::ok).filter(|message| {
            message.role == Role::Assistant
                && !message.content.is_empty()
                && message
                    .content
                    .iter()
                    .all(|block| matches!(block, ContentBlock::Text(_)))
                && !crate::completion_evidence::message_text(message)
                    .trim()
                    .is_empty()
        });
        self.telemetry
            .generation_timing(sink.timing(true, answer.is_some()));
        if let Some(reservation) = reservation {
            let usage = sink.request_usage();
            reservation
                .settle(crate::usage_budget::Receipt {
                    cumulative: None,
                    total_tokens: usage.and_then(|value| value.total_tokens),
                    reported_cost_microunits: usage.and_then(|value| value.cost_microunits),
                    outcome: if answer.is_some() {
                        crate::usage_budget::Outcome::Completed
                    } else {
                        crate::usage_budget::Outcome::Failed
                    },
                })
                .context("could not settle answer-recovery usage; reservation remains charged")?;
        }
        if let Some(message) = answer {
            return Ok(AgentTurnOutcome::Completed(AgentTurnResult {
                message,
                usage: sink.usage(),
            }));
        }
        // Keep this in the visible, durable conversation while retaining Failed
        // as the operation outcome. A failure explanation is not a model answer.
        let notice = Message::text(Role::Assistant, FAILED_NOTICE);
        if let Some(durable) = durable {
            durable
                .conversations
                .commit(operation_id, notice.clone(), None)
                .await?;
        } else {
            let _ = events.send(AgentEvent::AssistantMessage {
                operation_id,
                message: notice.clone(),
            });
        }
        messages.push(notice);
        Err(MemoryRecoveryFailure.into())
    }
}
