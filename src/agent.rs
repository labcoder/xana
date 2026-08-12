//! Headless, bounded asynchronous agent loop.
//!
//! The agent receives owned provider, prompt, tool, workspace, and limit
//! values. Runtime services provide operation identity, permissions, and passive
//! events; no frontend or process-global state enters here.

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
    tool::{ToolContext, ToolRegistry},
};
use anyhow::{Context, Result, bail};
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
}

impl DurableTurnServices {
    pub(crate) fn new(
        conversations: ConversationCommitSender,
        operations: DurableOperationSender,
    ) -> Self {
        Self {
            conversations,
            operations,
        }
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
            .map_err(|_| anyhow::anyhow!("durable conversation writer is unavailable"))?;
        acknowledgement
            .await
            .map_err(|_| anyhow::anyhow!("durable conversation writer dropped its reply"))?
            .map_err(anyhow::Error::msg)
    }
}

pub(crate) struct Agent {
    provider: Box<dyn ConversationalProvider>,
    tools: ToolRegistry,
    workspace_root: PathBuf,
    prompt: PromptSnapshot,
    max_tool_rounds: usize,
    boundary_observer: Arc<dyn BoundaryObserver>,
}

pub(crate) struct AgentTurnResult {
    pub(crate) message: Message,
    pub(crate) usage: AgentTurnUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentTurnUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) requests: u64,
}

impl Agent {
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
        }
    }

    pub(crate) async fn run_turn(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        permissions: PermissionBrokerHandle,
        events: impl Into<AgentEventSender>,
    ) -> Result<Message> {
        self.run_turn_inner(
            operation_id,
            messages,
            &self.prompt,
            permissions,
            events.into(),
            None,
        )
        .await
        .map(|result| result.message)
    }

    pub(crate) async fn run_turn_with_usage(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        permissions: PermissionBrokerHandle,
        events: impl Into<AgentEventSender>,
    ) -> Result<AgentTurnResult> {
        self.run_turn_inner(
            operation_id,
            messages,
            &self.prompt,
            permissions,
            events.into(),
            None,
        )
        .await
    }

    pub(crate) async fn run_turn_with_prompt(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        prompt: &PromptSnapshot,
        permissions: PermissionBrokerHandle,
        events: impl Into<AgentEventSender>,
        durable: Option<DurableTurnServices>,
    ) -> Result<Message> {
        self.run_turn_inner(
            operation_id,
            messages,
            prompt,
            permissions,
            events.into(),
            durable,
        )
        .await
        .map(|result| result.message)
    }

    async fn run_turn_inner(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        prompt: &PromptSnapshot,
        permissions: PermissionBrokerHandle,
        events: AgentEventSender,
        durable: Option<DurableTurnServices>,
    ) -> Result<AgentTurnResult> {
        let definitions = self.tools.definitions();
        let delta_sink = EventDeltaSink {
            operation_id,
            events: events.clone(),
            usage: Mutex::new(UsageAccumulator::default()),
        };

        for _ in 0..self.max_tool_rounds {
            let request_messages = prompt.messages_for_request(messages)?;
            let step_id = StepId::new();
            delta_sink.begin_request();
            let assistant = self
                .provider
                .stream_message(&request_messages, &definitions, step_id, &delta_sink)
                .await?;
            let calls = requested_tools(&assistant);

            if calls.is_empty() {
                return Ok(AgentTurnResult {
                    message: assistant,
                    usage: delta_sink.usage(),
                });
            }

            messages.push(assistant.clone());
            if let Some(durable) = &durable {
                let assistant_entry_id = durable
                    .conversations
                    .commit(operation_id, assistant, None)
                    .await
                    .context("could not commit assistant tool request")?;
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
            }

            for call in calls {
                let invocation_id = ToolInvocationId::new();
                let result = if let Some(durable) = &durable {
                    OperationExecutor::new(
                        &self.tools,
                        &self.workspace_root,
                        permissions.clone(),
                        durable.operations.clone(),
                        Arc::clone(&self.boundary_observer),
                        Some(events.clone()),
                    )
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
                            },
                        )
                        .await
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
        }

        bail!(
            "model exceeded the {}-round tool limit",
            self.max_tool_rounds
        )
    }

    #[cfg(test)]
    pub(crate) fn with_boundary_observer(mut self, observer: Arc<dyn BoundaryObserver>) -> Self {
        self.boundary_observer = observer;
        self
    }

    pub(crate) fn observe_boundary(&self, site: CrashSite) -> Result<()> {
        self.boundary_observer.reached(site)
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
            output_tokens: complete_sum(&usage.requests, |usage| usage.output_tokens),
            total_tokens: complete_sum(&usage.requests, |usage| usage.total_tokens),
            requests: usage.requests.len() as u64,
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
