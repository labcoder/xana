//! A single bounded text-only helper exchange, not a tool-capable agent loop.
use super::*;
use crate::{
    identity::OperationId,
    message::{ContentBlock, Role},
    usage_budget::{Outcome, Receipt, UsageBudget},
};
use anyhow::{Result, ensure};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

pub(crate) async fn text(
    provider: &dyn ConversationalProvider,
    budget: &UsageBudget,
    operation: OperationId,
    messages: &[Message],
    cancel: &CancellationToken,
) -> Result<String> {
    ensure!(!cancel.is_cancelled(), "helper cancelled before admission");
    let input = messages
        .iter()
        .map(crate::prompt::estimate_message_tokens)
        .sum::<usize>();
    ensure!(
        input <= 5120,
        "helper input exceeds the bounded job allowance"
    );
    let step = StepId::new();
    let reservation = budget.admit(operation, step, input as u64)?;
    let sink = Sink {
        bytes: AtomicUsize::new(0),
        limit: CancellationToken::new(),
        usage: Mutex::new(None),
    };
    let result = tokio::select! {
        biased;
        _=cancel.cancelled()=>Err(anyhow::anyhow!("helper cancelled")),
        _=sink.limit.cancelled()=>Err(anyhow::anyhow!("helper output exceeded its bound")),
        result=tokio::time::timeout(std::time::Duration::from_secs(120),provider.stream_message(messages,&[],step,&sink))=> match result {
            Ok(Ok(message))=>decode(message),
            Ok(Err(_))=>Err(anyhow::anyhow!("helper provider failed; private response omitted")),
            Err(_)=>Err(anyhow::anyhow!("helper deadline exceeded")),
        }
    };
    let usage = sink.usage.lock().expect("helper usage mutex").take();
    reservation.settle(Receipt {
        cumulative: None,
        total_tokens: usage.and_then(|u| u.total_tokens),
        reported_cost_microunits: usage.and_then(|u| u.cost_microunits),
        outcome: if cancel.is_cancelled() {
            Outcome::Interrupted
        } else if result.is_ok() {
            Outcome::Completed
        } else {
            Outcome::Failed
        },
    })?;
    ensure!(
        !cancel.is_cancelled() && !sink.limit.is_cancelled(),
        "helper cancelled or output limited"
    );
    result
}
fn decode(message: Message) -> Result<String> {
    ensure!(
        message.role == Role::Assistant && message.content.len() == 1,
        "helper returned unsupported content"
    );
    let Some(ContentBlock::Text(text)) = message.content.into_iter().next() else {
        anyhow::bail!("helper returned non-text content");
    };
    ensure!(text.len() <= 8192, "helper response exceeds byte allowance");
    Ok(text)
}
struct Sink {
    bytes: AtomicUsize,
    limit: CancellationToken,
    usage: Mutex<Option<ProviderUsage>>,
}
impl DeltaSink for Sink {
    fn text_delta(&self, _: StepId, text: &str) {
        if self
            .bytes
            .fetch_add(text.len(), Ordering::Relaxed)
            .saturating_add(text.len())
            > 8192
        {
            self.limit.cancel();
        }
    }
    fn reasoning_delta(&self, step: StepId, text: &str) {
        self.text_delta(step, text);
    }
    fn usage(&self, usage: ProviderUsage) {
        *self.usage.lock().expect("helper usage mutex") = Some(usage);
    }
}
