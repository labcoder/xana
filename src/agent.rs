//! Headless, bounded asynchronous agent loop.
//!
//! The agent receives owned provider, prompt, tool, workspace, and limit
//! values. Runtime services provide operation identity, permissions, and passive
//! events; no frontend or process-global state enters here.

use crate::{
    identity::{OperationId, StepId, ToolInvocationId},
    message::{ContentBlock, Message, ToolCall},
    operation::{
        BoundaryObserver, CrashSite, DurableOperationSender, NoopBoundaryObserver,
        OperationExecutor,
    },
    permission::PermissionBrokerHandle,
    prompt::PromptSnapshot,
    runtime::AgentEvent,
    tool::{ToolContext, ToolDefinition, ToolRegistry},
};
use anyhow::{Context, Result, bail};
use futures::future::BoxFuture;
use std::{error::Error, fmt, path::PathBuf, sync::Arc};
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

pub(crate) trait ChatTransport: Send + Sync {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ChatError>>;
}

pub(crate) trait DeltaSink: Send + Sync {
    fn text_delta(&self, step_id: StepId, text: &str);
}

#[derive(Debug)]
pub(crate) struct ChatError {
    message: String,
}

impl ChatError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ChatError {}

pub(crate) struct Agent {
    provider: Box<dyn ChatTransport>,
    tools: ToolRegistry,
    workspace_root: PathBuf,
    prompt: PromptSnapshot,
    max_tool_rounds: usize,
    boundary_observer: Arc<dyn BoundaryObserver>,
}

impl Agent {
    pub(crate) fn new(
        provider: Box<dyn ChatTransport>,
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
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Message> {
        self.run_turn_with_prompt(
            operation_id,
            messages,
            &self.prompt,
            permissions,
            events,
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
        events: mpsc::UnboundedSender<AgentEvent>,
        durable: Option<DurableTurnServices>,
    ) -> Result<Message> {
        let definitions = self.tools.definitions();
        let delta_sink = EventDeltaSink {
            operation_id,
            events: events.clone(),
        };

        for _ in 0..self.max_tool_rounds {
            let request_messages = prompt.messages_for_request(messages)?;
            let step_id = StepId::new();
            let assistant = self
                .provider
                .stream_message(&request_messages, &definitions, step_id, &delta_sink)
                .await?;
            let calls = requested_tools(&assistant);

            if calls.is_empty() {
                return Ok(assistant);
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
    events: mpsc::UnboundedSender<AgentEvent>,
}

impl DeltaSink for EventDeltaSink {
    fn text_delta(&self, step_id: StepId, text: &str) {
        let _ = self.events.send(AgentEvent::AssistantTextDelta {
            operation_id: self.operation_id,
            step_id,
            text: text.to_owned(),
        });
    }
}

fn requested_tools(message: &Message) -> Vec<ToolCall> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call.clone()),
            ContentBlock::Text(_) | ContentBlock::ToolResult(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
