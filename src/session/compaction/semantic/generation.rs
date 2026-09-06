//! Bounded no-tool generation and metadata-only failure evidence.

use super::*;
use crate::provider::{HelperGenerationPolicy, ProviderErrorKind};
use std::time::{Duration, Instant};

const MAX_REASONING_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HelperLimits {
    pub(crate) max_input_tokens: usize,
    pub(crate) max_output_tokens: usize,
}

impl Default for HelperLimits {
    fn default() -> Self {
        Self {
            max_input_tokens: 5_120,
            max_output_tokens: OUTPUT_RESERVE as usize,
        }
    }
}

impl HelperLimits {
    pub(crate) fn from_plan(plan: &crate::prompt::PromptBudgetPlan) -> Self {
        Self {
            max_input_tokens: plan.input_budget_tokens.min(MAX_SOURCE_TOKENS),
            max_output_tokens: plan.output_reserve_tokens.min(OUTPUT_RESERVE as usize),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HelperFailure {
    Cancelled,
    Authorization,
    Admission,
    InputLimit,
    OutputBytes,
    ReasoningBytes,
    UnexpectedReasoning,
    OutputTokens,
    Timeout,
    ProviderRequest,
    ProviderRejected,
    ProviderTransport,
    InvalidStream,
    ProviderOther,
    InvalidJson,
    InvalidShape,
    EmptySummary,
    SummaryLimit,
    Settlement,
}

impl std::fmt::Display for HelperFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "semantic helper failed: {self:?}")
    }
}
impl std::error::Error for HelperFailure {}

#[derive(Debug)]
pub(crate) struct HelperObservation {
    pub(crate) summary: Option<CompactionSummary>,
    pub(crate) failure: Option<HelperFailure>,
    pub(crate) elapsed_millis: u64,
    pub(crate) admission_millis: u64,
    pub(crate) provider_millis: u64,
    pub(crate) input_tokens: usize,
    pub(crate) output_bytes: usize,
    pub(crate) reasoning_bytes: usize,
    pub(crate) usage: Option<ProviderUsage>,
}

impl HelperObservation {
    fn into_result(self) -> Result<CompactionSummary> {
        self.summary
            .ok_or_else(|| self.failure.unwrap_or(HelperFailure::InvalidShape).into())
    }
}

#[cfg(test)]
pub(crate) async fn request(
    provider: &dyn ConversationalProvider,
    budget: &UsageBudget,
    operation: OperationId,
    messages: &[Message],
    max_summary_bytes: usize,
    cancellation: &CancellationToken,
) -> Result<CompactionSummary> {
    request_observed(
        provider,
        budget,
        operation,
        messages,
        max_summary_bytes,
        cancellation,
        HelperLimits::default(),
    )
    .await
    .into_result()
}

pub(crate) async fn request_observed(
    provider: &dyn ConversationalProvider,
    budget: &UsageBudget,
    operation: OperationId,
    messages: &[Message],
    max_summary_bytes: usize,
    cancellation: &CancellationToken,
    limits: HelperLimits,
) -> HelperObservation {
    observe(
        provider,
        budget,
        operation,
        messages,
        max_summary_bytes,
        cancellation,
        limits,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn request_validated(
    provider: &dyn ConversationalProvider,
    budget: &UsageBudget,
    operation: OperationId,
    messages: &[Message],
    max_summary_bytes: usize,
    cancellation: &CancellationToken,
    limits: HelperLimits,
    validate: &(dyn Fn() -> Result<()> + Sync),
) -> Result<CompactionSummary> {
    observe(
        provider,
        budget,
        operation,
        messages,
        max_summary_bytes,
        cancellation,
        limits,
        Some(validate),
    )
    .await
    .into_result()
}

#[allow(clippy::too_many_arguments)]
async fn observe(
    provider: &dyn ConversationalProvider,
    budget: &UsageBudget,
    operation: OperationId,
    messages: &[Message],
    max_summary_bytes: usize,
    cancellation: &CancellationToken,
    limits: HelperLimits,
    validate: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> HelperObservation {
    let started = Instant::now();
    let capabilities = provider.helper_capabilities();
    let schema = summary_schema();
    // Structured-response schemas consume input too; the local reserve cannot
    // count known wire material as free even when tokenization is estimated.
    let schema_tokens = if capabilities.structured_output {
        crate::context::estimate_tokens(
            &serde_json::to_string(&schema).expect("fixed helper schema"),
        )
    } else {
        0
    };
    let input_tokens = messages
        .iter()
        .map(crate::prompt::estimate_message_tokens)
        .sum::<usize>()
        .saturating_add(schema_tokens);
    let sink = BoundedSink {
        reject_reasoning: capabilities.disable_reasoning,
        ..Default::default()
    };
    let mut admission_millis = 0;
    let mut provider_millis = 0;
    let result = async {
        if cancellation.is_cancelled() {
            return Err(HelperFailure::Cancelled);
        }
        if limits.max_input_tokens == 0
            || input_tokens > limits.max_input_tokens.min(MAX_SOURCE_TOKENS)
            || limits.max_output_tokens == 0
            || limits.max_output_tokens > OUTPUT_RESERVE as usize
        {
            return Err(HelperFailure::InputLimit);
        }
        let generation = HelperGenerationPolicy {
            max_output_tokens: limits.max_output_tokens,
            json_schema: capabilities.structured_output.then_some(&schema),
            disable_reasoning: capabilities.disable_reasoning,
            zero_temperature: capabilities.zero_temperature,
        };
        generation
            .validate(capabilities)
            .map_err(|_| HelperFailure::ProviderRequest)?;
        let _lane = budget
            .foreground_helper_lease(cancellation)
            .await
            .map_err(|_| {
                if cancellation.is_cancelled() {
                    HelperFailure::Cancelled
                } else {
                    HelperFailure::Admission
                }
            })?;
        if let Some(validate) = validate {
            validate().map_err(|_| HelperFailure::Authorization)?;
        }
        let step = StepId::new();
        let reservation = budget
            .reroute(
                "semantic-compaction".into(),
                limits.max_output_tokens as u64,
            )
            .admit(operation, step, input_tokens as u64)
            .map_err(|_| HelperFailure::Admission)?;
        admission_millis = millis(started.elapsed());
        let dispatch_started = Instant::now();
        let mut result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(HelperFailure::Cancelled),
            _ = sink.cancellation.cancelled() => Err(sink.limit_failure()),
            result = tokio::time::timeout(Duration::from_secs(120),
                provider.stream_helper_message(messages, generation, step, &sink)) => {
                match result {
                    Ok(Ok(message)) => parse_summary(message, max_summary_bytes),
                    Ok(Err(error)) => Err(provider_failure(error.kind())),
                    Err(_) => Err(HelperFailure::Timeout),
                }
            }
        };
        provider_millis = millis(dispatch_started.elapsed());
        // A ready provider future may win after its last delta crossed a bound.
        if cancellation.is_cancelled() {
            result = Err(HelperFailure::Cancelled);
        } else if sink.cancellation.is_cancelled() {
            result = Err(sink.limit_failure());
        }
        let usage = *sink.usage.lock().expect("usage mutex not poisoned");
        reservation
            .settle(Receipt {
                cumulative: None,
                total_tokens: usage.and_then(|usage| usage.total_tokens),
                reported_cost_microunits: usage.and_then(|usage| usage.cost_microunits),
                outcome: if cancellation.is_cancelled() {
                    Outcome::Interrupted
                } else if result.is_ok() {
                    Outcome::Completed
                } else {
                    Outcome::Failed
                },
            })
            .map_err(|_| HelperFailure::Settlement)?;
        result
    }
    .await;
    let usage = *sink.usage.lock().expect("usage mutex not poisoned");
    let (summary, failure) = match result {
        Ok(summary) => (Some(summary), None),
        Err(error) => (None, Some(error)),
    };
    HelperObservation {
        summary,
        failure,
        elapsed_millis: millis(started.elapsed()),
        admission_millis,
        provider_millis,
        input_tokens,
        output_bytes: sink.text_bytes.load(Ordering::Relaxed),
        reasoning_bytes: sink.reasoning_bytes.load(Ordering::Relaxed),
        usage,
    }
}

fn provider_failure(kind: ProviderErrorKind) -> HelperFailure {
    match kind {
        ProviderErrorKind::Request => HelperFailure::ProviderRequest,
        ProviderErrorKind::Rejected => HelperFailure::ProviderRejected,
        ProviderErrorKind::Transport => HelperFailure::ProviderTransport,
        ProviderErrorKind::InvalidStream => HelperFailure::InvalidStream,
        ProviderErrorKind::Timeout => HelperFailure::Timeout,
        ProviderErrorKind::OutputLimit => HelperFailure::OutputTokens,
        ProviderErrorKind::Other => HelperFailure::ProviderOther,
    }
}

fn parse_summary(
    message: Message,
    max_bytes: usize,
) -> std::result::Result<CompactionSummary, HelperFailure> {
    if message.role != Role::Assistant || message.content.len() != 1 {
        return Err(HelperFailure::InvalidShape);
    }
    let ContentBlock::Text(text) = &message.content[0] else {
        return Err(HelperFailure::InvalidShape);
    };
    if text.len() > MAX_OUTPUT_BYTES {
        return Err(HelperFailure::OutputBytes);
    }
    let summary: CompactionSummary =
        serde_json::from_str(text).map_err(|_| HelperFailure::InvalidJson)?;
    if summary == CompactionSummary::default() {
        return Err(HelperFailure::EmptySummary);
    }
    if !validate_summary(&summary, max_bytes) {
        return Err(HelperFailure::SummaryLimit);
    }
    Ok(summary)
}

#[derive(Default)]
struct BoundedSink {
    reject_reasoning: bool,
    text_bytes: AtomicUsize,
    reasoning_bytes: AtomicUsize,
    cancellation: CancellationToken,
    usage: Mutex<Option<ProviderUsage>>,
}
impl BoundedSink {
    fn limit_failure(&self) -> HelperFailure {
        if self.reject_reasoning && self.reasoning_bytes.load(Ordering::Relaxed) > 0 {
            HelperFailure::UnexpectedReasoning
        } else if self.reasoning_bytes.load(Ordering::Relaxed) > MAX_REASONING_BYTES {
            HelperFailure::ReasoningBytes
        } else {
            HelperFailure::OutputBytes
        }
    }
    fn count(&self, counter: &AtomicUsize, count: usize, limit: usize) {
        if counter
            .fetch_add(count, Ordering::Relaxed)
            .saturating_add(count)
            > limit
        {
            self.cancellation.cancel();
        }
    }
}
impl DeltaSink for BoundedSink {
    fn text_delta(&self, _: StepId, text: &str) {
        self.count(&self.text_bytes, text.len(), MAX_OUTPUT_BYTES);
    }
    fn reasoning_delta(&self, _: StepId, text: &str) {
        self.count(&self.reasoning_bytes, text.len(), MAX_REASONING_BYTES);
        if self.reject_reasoning && !text.is_empty() {
            self.cancellation.cancel();
        }
    }
    fn usage(&self, usage: ProviderUsage) {
        *self.usage.lock().expect("usage mutex not poisoned") = Some(usage);
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn summary_schema() -> serde_json::Value {
    // Use the common raw-HTTP schema subset. For example, Anthropic rejects
    // maxLength/maxItems instead of stripping them like some SDKs do. Local
    // parsing still enforces the exact byte/item bounds on every adapter.
    let item = |meaning: &str| {
        serde_json::json!({
            "type":"array", "items":{"type":"string"},
            "description":format!("{meaning} At most16 items; each at most512 UTF-8 bytes")
        })
    };
    serde_json::json!({
        "type":"object", "additionalProperties":false,
        "required":["goal","constraints","progress","decisions","unresolved","references"],
        "properties":{
            "goal":{"type":["string","null"],"description":"Current task objective, or null if unknown. At most512 UTF-8 bytes"},
            "constraints":item("Current scope and restrictions in the original wording and language; no obsolete values or rejected instructions."),
            "progress":item("Only confirmed completed work; no intentions or correction history."),
            "decisions":item("Current choices and exact current values; replace superseded values rather than narrating changes."),
            "unresolved":item("Current remaining work and questions, in their original wording and language; replace resolved or superseded items."),
            "references":item("Evidence locations such as file paths, artifact IDs and URLs, not copies of messages or active facts. Empty if none.")
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn shared_wire_schema_uses_the_raw_http_supported_subset() {
        let schema = super::summary_schema();
        let encoded = schema.to_string();
        for unsupported in ["maxLength", "maxItems", "pattern", "format"] {
            assert!(!encoded.contains(unsupported));
        }
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"].as_array().unwrap().len(), 6);
    }
}
