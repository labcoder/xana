//! Headless, bounded asynchronous agent loop.
//!
//! The agent receives owned provider, prompt, tool, workspace, and limit
//! values. Runtime services provide operation identity, permissions, and passive
//! events; no frontend or process-global state enters here.

mod progress;

use crate::{
    identity::{OperationId, StepId, ToolInvocationId},
    message::{ContentBlock, Message, ToolCall},
    native_runtime::{AgentEvent, AgentEventSender},
    operation::{
        BoundaryObserver, CrashSite, DurableOperationSender, NoopBoundaryObserver,
        OperationExecutor,
    },
    permission::PermissionBrokerHandle,
    prompt::PromptSnapshot,
    provider::{ConversationalProvider, DeltaSink, ProviderUsage},
    telemetry::{
        NoopRuntimeTelemetry, RuntimeTelemetry, RuntimeTelemetryEvent, RuntimeTelemetryKind,
    },
    tool::{DeferredCleanup, ToolContext, ToolRegistry},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub(crate) struct ConversationCommitSender {
    sender: mpsc::UnboundedSender<ConversationCommit>,
}

pub(crate) struct ConversationCommit {
    pub(crate) operation_id: OperationId,
    pub(crate) message: Message,
    pub(crate) tool_finished: Option<(ToolInvocationId, Message)>,
    pub(crate) acknowledged: oneshot::Sender<Result<crate::identity::ConversationEntryId, String>>,
}

#[derive(Clone)]
pub(crate) struct DurableTurnServices {
    conversations: ConversationCommitSender,
    operations: DurableOperationSender,
    owner_input: Option<crate::tool::OwnerTurnInput>,
    prompt_refresh: Option<Arc<dyn RequestPromptRefresh>>,
}

/// Application-owned request context. The engine consumes an owned, budgeted
/// snapshot without learning how context is stored or authorized.
pub(crate) trait RequestPromptRefresh: Send + Sync {
    fn refresh(&self, prompt: &PromptSnapshot) -> Result<PromptSnapshot>;
}

impl DurableTurnServices {
    pub(crate) fn new(
        conversations: ConversationCommitSender,
        operations: DurableOperationSender,
    ) -> Self {
        Self {
            conversations,
            operations,
            owner_input: None,
            prompt_refresh: None,
        }
    }

    pub(crate) fn with_owner_input(mut self, input: Option<crate::tool::OwnerTurnInput>) -> Self {
        self.owner_input = input;
        self
    }

    pub(crate) fn with_prompt_refresh(
        mut self,
        refresh: Option<Arc<dyn RequestPromptRefresh>>,
    ) -> Self {
        self.prompt_refresh = refresh;
        self
    }
}

impl ConversationCommitSender {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<ConversationCommit>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }

    async fn commit(
        &self,
        operation_id: OperationId,
        message: Message,
        tool_finished: Option<(ToolInvocationId, Message)>,
    ) -> Result<crate::identity::ConversationEntryId> {
        let (acknowledged, acknowledgement) = oneshot::channel();
        self.sender
            .send(ConversationCommit {
                operation_id,
                message,
                tool_finished,
                acknowledged,
            })
            .map_err(|_| {
                crate::failure::PersistenceFailure::error(
                    "durable conversation writer is unavailable",
                )
            })?;
        acknowledgement
            .await
            .map_err(|_| {
                crate::failure::PersistenceFailure::error(
                    "durable conversation writer dropped its reply",
                )
            })?
            .map_err(crate::failure::PersistenceFailure::error)
    }
}

pub(crate) struct Agent {
    output_recorder: Option<Arc<dyn crate::operation::output::ToolOutputRecorder>>,
    provider: Box<dyn ConversationalProvider>,
    tools: ToolRegistry,
    workspace_root: PathBuf,
    prompt: PromptSnapshot,
    max_tool_rounds: usize,
    boundary_observer: Arc<dyn BoundaryObserver>,
    telemetry: Arc<dyn RuntimeTelemetry>,
    usage_budget: Option<crate::usage_budget::UsageBudget>,
    semantic_compaction: Option<crate::session::compaction::semantic::HelperPolicy>,
}

pub(crate) struct AgentTurnResult {
    pub(crate) message: Message,
    pub(crate) usage: AgentTurnUsage,
}

pub(crate) enum AgentTurnOutcome {
    Completed(AgentTurnResult),
    RoundBudgetReached {
        rounds: usize,
        usage: AgentTurnUsage,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AgentTurnUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) cache_write_input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) reasoning_tokens: Option<u64>,
    pub(crate) tool_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) cost_microunits: Option<u64>,
    pub(crate) prompt_bytes: Option<u64>,
    pub(crate) tool_schema_bytes: Option<u64>,
    pub(crate) requests: u64,
    pub(crate) request_affinities: Vec<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SessionUsage {
    turns: u64,
    requests: u64,
    input_tokens: UsageCounter,
    cached_input_tokens: UsageCounter,
    cache_write_input_tokens: UsageCounter,
    output_tokens: UsageCounter,
    reasoning_tokens: UsageCounter,
    tool_tokens: UsageCounter,
    total_tokens: UsageCounter,
    cost_microunits: UsageCounter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageCounter {
    value: u64,
    complete: bool,
}

impl Default for UsageCounter {
    fn default() -> Self {
        Self {
            value: 0,
            complete: true,
        }
    }
}

impl SessionUsage {
    pub(crate) fn observe(&mut self, usage: AgentTurnUsage) {
        self.turns = self.turns.saturating_add(1);
        self.requests = self.requests.saturating_add(usage.requests);
        self.input_tokens.observe(usage.input_tokens);
        self.cached_input_tokens.observe(usage.cached_input_tokens);
        self.cache_write_input_tokens
            .observe(usage.cache_write_input_tokens);
        self.output_tokens.observe(usage.output_tokens);
        self.reasoning_tokens.observe(usage.reasoning_tokens);
        self.tool_tokens.observe(usage.tool_tokens);
        self.total_tokens.observe(usage.total_tokens);
        self.cost_microunits.observe(usage.cost_microunits);
    }

    pub(crate) fn render(&self) -> String {
        if self.turns == 0 {
            return "Current process: no completed turns yet; token usage is unknown until a provider reports it. Provider quota, rate-limit reset, and wallet balance are not exposed by this connection.".to_owned();
        }
        let mut details = vec![
            format!("input {}", self.input_tokens.render()),
            format!("output {}", self.output_tokens.render()),
            format!("total {}", self.total_tokens.render()),
        ];
        for (label, counter) in [
            ("cache read", &self.cached_input_tokens),
            ("cache write", &self.cache_write_input_tokens),
            ("reasoning", &self.reasoning_tokens),
            ("tool", &self.tool_tokens),
        ] {
            if counter.value > 0 || !counter.complete {
                details.push(format!("{label} {}", counter.render()));
            }
        }
        if self.cost_microunits.value > 0 || !self.cost_microunits.complete {
            details.push(format!(
                "provider-reported cost {}",
                self.cost_microunits.render_microunits()
            ));
        }
        format!(
            "Current process: {} turn(s), {} provider request(s) · {}. Account quota, rate-limit reset, and credit balance are separate observations and may be unavailable.",
            self.turns,
            self.requests,
            details.join(" · "),
        )
    }
}

impl AgentTurnUsage {
    pub(crate) const fn empty() -> Self {
        Self {
            input_tokens: Some(0),
            cached_input_tokens: Some(0),
            cache_write_input_tokens: Some(0),
            output_tokens: Some(0),
            reasoning_tokens: Some(0),
            tool_tokens: Some(0),
            total_tokens: Some(0),
            cost_microunits: Some(0),
            prompt_bytes: Some(0),
            tool_schema_bytes: Some(0),
            requests: 0,
            request_affinities: Vec::new(),
        }
    }

    pub(crate) fn merge(self, next: Self) -> Self {
        Self {
            input_tokens: merge_count(self.input_tokens, next.input_tokens),
            cached_input_tokens: merge_count(self.cached_input_tokens, next.cached_input_tokens),
            cache_write_input_tokens: merge_count(
                self.cache_write_input_tokens,
                next.cache_write_input_tokens,
            ),
            output_tokens: merge_count(self.output_tokens, next.output_tokens),
            reasoning_tokens: merge_count(self.reasoning_tokens, next.reasoning_tokens),
            tool_tokens: merge_count(self.tool_tokens, next.tool_tokens),
            total_tokens: merge_count(self.total_tokens, next.total_tokens),
            cost_microunits: merge_count(self.cost_microunits, next.cost_microunits),
            prompt_bytes: merge_count(self.prompt_bytes, next.prompt_bytes),
            tool_schema_bytes: merge_count(self.tool_schema_bytes, next.tool_schema_bytes),
            requests: self.requests.saturating_add(next.requests),
            request_affinities: merge_affinities(self.request_affinities, next.request_affinities),
        }
    }
}

fn merge_count(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    left.and_then(|left| right.and_then(|right| left.checked_add(right)))
}

fn merge_affinities(mut left: Vec<[u8; 16]>, right: Vec<[u8; 16]>) -> Vec<[u8; 16]> {
    for affinity in right {
        if left.len() >= 64 {
            break;
        }
        if !left.contains(&affinity) {
            left.push(affinity);
        }
    }
    left
}

impl UsageCounter {
    fn observe(&mut self, observed: Option<u64>) {
        match observed {
            Some(value) => self.value = self.value.saturating_add(value),
            None => self.complete = false,
        }
    }

    fn render(&self) -> String {
        if self.complete {
            self.value.to_string()
        } else if self.value == 0 {
            "unknown".to_owned()
        } else {
            format!("at least {} (partial)", self.value)
        }
    }

    fn render_microunits(&self) -> String {
        if self.complete {
            format!("${:.6}", self.value as f64 / 1_000_000.0)
        } else if self.value == 0 {
            "unknown".to_owned()
        } else {
            format!("at least ${:.6} (partial)", self.value as f64 / 1_000_000.0)
        }
    }
}

impl Agent {
    pub(crate) fn record_context_phase(
        &self,
        operation_id: OperationId,
        phase: crate::telemetry::ContextPhase,
        elapsed: std::time::Duration,
    ) {
        self.telemetry
            .context_phase(crate::telemetry::ContextPhaseEvent {
                operation_id,
                phase,
                elapsed,
            });
    }

    pub(crate) fn with_semantic_compaction(
        mut self,
        policy: Option<crate::session::compaction::semantic::HelperPolicy>,
    ) -> Self {
        self.semantic_compaction = policy;
        self
    }

    pub(crate) fn semantic_compaction_enabled(&self) -> bool {
        self.semantic_compaction.is_some()
    }

    pub(crate) async fn enrich_compaction(
        &self,
        candidate: &mut crate::session::CompactionCandidate,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<bool> {
        let Some(policy) = &self.semantic_compaction else {
            return Ok(false);
        };
        let budget = self
            .usage_budget
            .as_ref()
            .context("semantic helper requires durable usage accounting")?;
        crate::session::compaction::semantic::enrich(
            candidate,
            self.provider.as_ref(),
            budget,
            policy,
            cancellation,
        )
        .await?;
        Ok(true)
    }

    pub(crate) fn with_parent_usage(mut self, parent: OperationId) -> Result<Self> {
        self.usage_budget = self
            .usage_budget
            .map(|budget| budget.inherit_operation(parent))
            .transpose()?;
        Ok(self)
    }
    pub(crate) fn with_usage_budget(
        mut self,
        budget: Option<crate::usage_budget::UsageBudget>,
    ) -> Self {
        self.usage_budget = budget;
        self
    }

    pub(crate) fn completion_usage_budget(&self) -> Option<crate::usage_budget::UsageBudget> {
        self.usage_budget.clone()
    }

    pub(crate) fn new(
        provider: Box<dyn ConversationalProvider>,
        tools: ToolRegistry,
        workspace_root: PathBuf,
        prompt: PromptSnapshot,
        max_tool_rounds: usize,
    ) -> Self {
        Self {
            provider,
            tools,
            workspace_root,
            prompt,
            max_tool_rounds,
            boundary_observer: Arc::new(NoopBoundaryObserver),
            output_recorder: None,
            telemetry: Arc::new(NoopRuntimeTelemetry),
            usage_budget: None,
            semantic_compaction: None,
        }
    }

    #[cfg(test)]
    pub(crate) async fn run_turn(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        permissions: PermissionBrokerHandle,
        events: impl Into<AgentEventSender>,
    ) -> Result<Message> {
        let cleanup = DeferredCleanup::default();
        let result = self
            .run_turn_inner(
                operation_id,
                messages,
                &self.prompt,
                permissions,
                events.into(),
                None,
                cleanup.clone(),
                self.max_tool_rounds,
            )
            .await;
        cleanup.drain().await;
        completed(result?, self.max_tool_rounds).map(|result| result.message)
    }

    #[cfg(test)]
    pub(crate) async fn run_turn_with_usage(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        permissions: PermissionBrokerHandle,
        events: impl Into<AgentEventSender>,
    ) -> Result<AgentTurnResult> {
        let cleanup = DeferredCleanup::default();
        let result = self
            .run_turn_inner(
                operation_id,
                messages,
                &self.prompt,
                permissions,
                events.into(),
                None,
                cleanup.clone(),
                self.max_tool_rounds,
            )
            .await;
        cleanup.drain().await;
        completed(result?, self.max_tool_rounds)
    }

    pub(crate) async fn run_turn_with_usage_in_scope(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        permissions: PermissionBrokerHandle,
        events: AgentEventSender,
        cleanup: DeferredCleanup,
    ) -> Result<AgentTurnResult> {
        completed(
            self.run_turn_inner(
                operation_id,
                messages,
                &self.prompt,
                permissions,
                events,
                None,
                cleanup,
                self.max_tool_rounds,
            )
            .await?,
            self.max_tool_rounds,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_tranche_with_prompt_in_scope(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        prompt: &PromptSnapshot,
        permissions: PermissionBrokerHandle,
        events: AgentEventSender,
        durable: Option<DurableTurnServices>,
        cleanup: DeferredCleanup,
        round_limit: usize,
    ) -> Result<AgentTurnOutcome> {
        self.run_turn_inner(
            operation_id,
            messages,
            prompt,
            permissions,
            events,
            durable,
            cleanup,
            round_limit.min(self.max_tool_rounds),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_tranche_in_scope(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        permissions: PermissionBrokerHandle,
        events: AgentEventSender,
        cleanup: DeferredCleanup,
        round_limit: usize,
    ) -> Result<AgentTurnOutcome> {
        self.run_turn_inner(
            operation_id,
            messages,
            &self.prompt,
            permissions,
            events,
            None,
            cleanup,
            round_limit.min(self.max_tool_rounds),
        )
        .await
    }

    pub(crate) const fn max_tool_rounds(&self) -> usize {
        self.max_tool_rounds
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_turn_inner(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        prompt: &PromptSnapshot,
        permissions: PermissionBrokerHandle,
        events: AgentEventSender,
        durable: Option<DurableTurnServices>,
        cleanup: DeferredCleanup,
        round_limit: usize,
    ) -> Result<AgentTurnOutcome> {
        if round_limit == 0 {
            bail!("native tool-round tranche must contain at least one round");
        }
        let definitions = self.tools.definitions();
        let mut progress = progress::ProgressGuard::from_history(messages);
        if progress.stopped() {
            bail!(progress::STOP_REASON);
        }
        let delta_sink = EventDeltaSink {
            operation_id,
            events: events.clone(),
            usage: Mutex::new(UsageAccumulator::default()),
        };

        for _ in 0..round_limit {
            let refreshed = durable
                .as_ref()
                .and_then(|services| services.prompt_refresh.as_ref())
                .map(|refresh| refresh.refresh(prompt))
                .transpose()?;
            let prompt = refreshed.as_ref().unwrap_or(prompt);
            let request_messages = prompt.messages_for_request(messages)?;
            if let Some(ledger) = prompt.ledger(messages.iter()) {
                // Tool results change the tail within a turn. Report each
                // actual request, not only the pre-tool submission estimate.
                let _ = events.send(AgentEvent::PromptPlanUpdated {
                    operation_id,
                    ledger,
                });
            }
            let step_id = StepId::new();
            delta_sink.begin_request();
            let input_tokens = prompt
                .ledger(messages.iter())
                .map(|ledger| ledger.estimated_input_tokens)
                .unwrap_or_else(|| {
                    request_messages
                        .iter()
                        .map(crate::prompt::estimate_message_tokens)
                        .sum::<usize>()
                });
            let reservation = self
                .usage_budget
                .as_ref()
                .map(|budget| budget.admit(operation_id, step_id, input_tokens as u64))
                .transpose()?;
            let response = self
                .provider
                .stream_message(&request_messages, &definitions, step_id, &delta_sink)
                .await;
            if let Err(error) = &response {
                // Publish the observed origin before accounting or a queued
                // Shutdown can replace the operation's final owner outcome.
                let (route, model) = self.diagnostic_route();
                let diagnostic = crate::failure::TerminalDiagnostic::new(
                    Some(operation_id),
                    None,
                    crate::failure::FailureOrigin::Native,
                    crate::failure::TerminalOutcome::Failed,
                    error.failure(),
                )
                .route(route, model);
                let _ = events.send(AgentEvent::TerminalDiagnostic {
                    diagnostic: diagnostic.clone(),
                });
                self.record_terminal(diagnostic);
                self.telemetry
                    .provider_failure(operation_id, error.failure());
            }
            if let Some(reservation) = reservation {
                let usage = delta_sink.request_usage();
                let settlement = reservation
                    .settle(crate::usage_budget::Receipt {
                        cumulative: None,
                        total_tokens: usage.and_then(|value| value.total_tokens),
                        reported_cost_microunits: usage.and_then(|value| value.cost_microunits),
                        outcome: if response.is_ok() {
                            crate::usage_budget::Outcome::Completed
                        } else {
                            crate::usage_budget::Outcome::Failed
                        },
                    })
                    .context("could not settle provider usage; reservation remains charged");
                if let Err(error) = settlement {
                    self.record_storage_failure(operation_id, "provider-usage-settlement");
                    // Keep the originating provider error typed if both failed;
                    // accounting has its own content-free persistence diagnostic.
                    if response.is_ok() {
                        return Err(error);
                    }
                }
            }
            let assistant = response.map_err(|error| {
                self.telemetry.record(RuntimeTelemetryEvent {
                    operation_id,
                    kind: RuntimeTelemetryKind::ProviderFailed,
                    subject: format!("{:?}", error.kind()),
                });
                let kind = error.kind();
                let display = format!("provider {kind:?}: {error}");
                anyhow::Error::new(error).context(display)
            })?;
            let calls = requested_tools(&assistant);

            if calls.is_empty() {
                return Ok(AgentTurnOutcome::Completed(AgentTurnResult {
                    message: assistant,
                    usage: delta_sink.usage(),
                }));
            }

            if let Some(durable) = &durable {
                let assistant_entry_id = durable
                    .conversations
                    .commit(operation_id, assistant.clone(), None)
                    .await
                    .context("could not commit assistant tool request")?;
                // A rejected ACK must not leave an uncommitted tool call in
                // the next provider request. No tool has been dispatched yet.
                messages.push(assistant);
                durable
                    .operations
                    .append(
                        crate::session::SessionRecord::StepStarted {
                            operation_id,
                            step_id,
                            assistant_entry_id,
                        },
                        None,
                    )
                    .await
                    .context("could not commit operation step")?;
                self.boundary_observer
                    .reached(CrashSite::AfterStepStarted)?;
            } else {
                messages.push(assistant);
            }

            for call in calls {
                let invocation_id = ToolInvocationId::new();
                let result = if let Some(blocked) = progress.blocked_result(&call) {
                    blocked
                } else if let Some(durable) = &durable {
                    OperationExecutor::new(
                        &self.tools,
                        &self.workspace_root,
                        permissions.clone(),
                        durable.operations.clone(),
                        Arc::clone(&self.boundary_observer),
                        Some(events.clone()),
                        cleanup.clone(),
                    )
                    .with_owner_input(durable.owner_input.as_ref())
                    .invoke_tool(operation_id, step_id, invocation_id, call.clone())
                    .await?
                } else {
                    self.tools
                        .invoke(
                            &call,
                            ToolContext {
                                workspace_root: &self.workspace_root,
                                operation_id,
                                invocation_id,
                                permissions: &permissions,
                                events: Some(&events),
                                cleanup: cleanup.clone(),
                            },
                        )
                        .await
                };
                progress.observe(&call, &result);
                let result = if durable.is_none()
                    && let Some(recorder) = &self.output_recorder
                {
                    recorder
                        .record(result)
                        .await
                        .context("could not retain tool evidence; do not replay the effect")?
                } else {
                    result
                };
                let result_message = Message::tool_result(result);
                if let Some(durable) = &durable {
                    let _entry_id = durable
                        .conversations
                        .commit(
                            operation_id,
                            result_message.clone(),
                            Some((invocation_id, result_message.clone())),
                        )
                        .await
                        .context("could not commit tool result")?;
                    self.boundary_observer
                        .reached(CrashSite::AfterConversationResult)?;
                } else {
                    let _ = events.send(AgentEvent::ToolFinished {
                        operation_id,
                        invocation_id,
                        result: result_message.clone(),
                    });
                }
                messages.push(result_message);
            }
            if progress.stopped() {
                bail!(progress::STOP_REASON);
            }
        }

        Ok(AgentTurnOutcome::RoundBudgetReached {
            rounds: round_limit,
            usage: delta_sink.usage(),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_boundary_observer(mut self, observer: Arc<dyn BoundaryObserver>) -> Self {
        self.boundary_observer = observer;
        self
    }

    pub(crate) fn with_runtime_telemetry(mut self, telemetry: Arc<dyn RuntimeTelemetry>) -> Self {
        self.tools.set_runtime_telemetry(telemetry.clone());
        self.telemetry = telemetry;
        self
    }

    pub(crate) fn record_terminal(&self, diagnostic: crate::failure::TerminalDiagnostic) {
        self.telemetry.terminal(diagnostic);
    }

    pub(crate) fn diagnostic_route(&self) -> (Option<&str>, Option<&str>) {
        self.prompt
            .budget_plan
            .as_ref()
            .map_or((None, None), |plan| {
                (Some(plan.connection.as_str()), Some(plan.model.as_str()))
            })
    }

    pub(crate) fn with_output_recorder(
        mut self,
        recorder: Option<Arc<dyn crate::operation::output::ToolOutputRecorder>>,
    ) -> Self {
        self.output_recorder = recorder;
        self
    }

    pub(crate) fn record_storage_failure(&self, operation_id: OperationId, subject: &str) {
        self.telemetry.record(RuntimeTelemetryEvent {
            operation_id,
            kind: RuntimeTelemetryKind::StorageFailed,
            subject: subject.to_owned(),
        });
    }

    pub(crate) fn observe_boundary(&self, site: CrashSite) -> Result<()> {
        self.boundary_observer.reached(site)
    }
}

fn completed(outcome: AgentTurnOutcome, round_limit: usize) -> Result<AgentTurnResult> {
    match outcome {
        AgentTurnOutcome::Completed(result) => Ok(result),
        AgentTurnOutcome::RoundBudgetReached { .. } => {
            bail!("model exceeded the {round_limit}-round tool limit")
        }
    }
}

struct EventDeltaSink {
    operation_id: OperationId,
    events: AgentEventSender,
    usage: Mutex<UsageAccumulator>,
}

#[derive(Default)]
struct UsageAccumulator {
    requests: Vec<Option<ProviderUsage>>,
}

impl EventDeltaSink {
    fn request_usage(&self) -> Option<ProviderUsage> {
        self.usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .requests
            .last()
            .copied()
            .flatten()
    }
    fn begin_request(&self) {
        self.usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .requests
            .push(None);
    }

    fn usage(&self) -> AgentTurnUsage {
        let usage = self
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        AgentTurnUsage {
            input_tokens: complete_sum(&usage.requests, |usage| usage.input_tokens),
            cached_input_tokens: complete_sum(&usage.requests, |usage| usage.cached_input_tokens),
            cache_write_input_tokens: complete_sum(&usage.requests, |usage| {
                usage.cache_write_input_tokens
            }),
            output_tokens: complete_sum(&usage.requests, |usage| usage.output_tokens),
            reasoning_tokens: complete_sum(&usage.requests, |usage| usage.reasoning_tokens),
            tool_tokens: complete_sum(&usage.requests, |usage| usage.tool_tokens),
            total_tokens: complete_sum(&usage.requests, |usage| usage.total_tokens),
            cost_microunits: complete_sum(&usage.requests, |usage| usage.cost_microunits),
            prompt_bytes: complete_sum(&usage.requests, |usage| usage.prompt_bytes),
            tool_schema_bytes: complete_sum(&usage.requests, |usage| usage.tool_schema_bytes),
            requests: usage.requests.len() as u64,
            request_affinities: usage
                .requests
                .iter()
                .filter_map(|usage| usage.and_then(|usage| usage.request_affinity))
                .take(64)
                .collect(),
        }
    }
}

fn complete_sum(
    requests: &[Option<ProviderUsage>],
    field: impl Fn(ProviderUsage) -> Option<u64>,
) -> Option<u64> {
    requests
        .iter()
        .try_fold(0_u64, |total, usage| total.checked_add(field((*usage)?)?))
}

impl DeltaSink for EventDeltaSink {
    fn text_delta(&self, step_id: StepId, text: &str) {
        let _ = self.events.send(AgentEvent::AssistantTextDelta {
            operation_id: self.operation_id,
            step_id,
            text: text.to_owned(),
        });
    }

    fn reasoning_delta(&self, step_id: StepId, text: &str) {
        let _ = self.events.send(AgentEvent::ProviderReasoningDelta {
            operation_id: self.operation_id,
            step_id,
            text: text.to_owned(),
        });
    }

    fn usage(&self, usage: ProviderUsage) {
        if let Some(current) = self
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .requests
            .last_mut()
        {
            *current = Some(usage);
        }
    }
}

fn requested_tools(message: &Message) -> Vec<ToolCall> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call.clone()),
            ContentBlock::Text(_) | ContentBlock::Image(_) | ContentBlock::ToolResult(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
