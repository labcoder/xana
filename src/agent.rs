//! Headless, bounded asynchronous agent loop.
//!
//! The agent receives owned provider, prompt, tool, workspace, and limit
//! values. Runtime services provide operation identity, permissions, and passive
//! events; no frontend or process-global state enters here.

use crate::{
    identity::{OperationId, StepId, ToolInvocationId},
    message::{ContentBlock, Message, ToolCall},
    permission::PermissionBrokerHandle,
    prompt::PromptSnapshot,
    runtime::AgentEvent,
    tool::{ToolContext, ToolDefinition, ToolRegistry},
};
use anyhow::{Result, bail};
use futures::future::BoxFuture;
use std::{error::Error, fmt, path::PathBuf};
use tokio::sync::mpsc;

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
        }
    }

    pub(crate) async fn run_turn(
        &self,
        operation_id: OperationId,
        messages: &mut Vec<Message>,
        permissions: PermissionBrokerHandle,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Message> {
        let definitions = self.tools.definitions();
        let delta_sink = EventDeltaSink {
            operation_id,
            events: events.clone(),
        };

        for _ in 0..self.max_tool_rounds {
            let request_messages = self.prompt.messages_for_request(messages)?;
            let step_id = StepId::new();
            let assistant = self
                .provider
                .stream_message(&request_messages, &definitions, step_id, &delta_sink)
                .await?;
            let calls = requested_tools(&assistant);

            if calls.is_empty() {
                return Ok(assistant);
            }

            messages.push(assistant);

            for call in calls {
                let invocation_id = ToolInvocationId::new();
                let result = self
                    .tools
                    .invoke(
                        &call,
                        ToolContext {
                            workspace_root: &self.workspace_root,
                            operation_id,
                            invocation_id,
                            permissions: &permissions,
                        },
                    )
                    .await;
                let result_message = Message::tool_result(result);
                let _ = events.send(AgentEvent::ToolFinished {
                    operation_id,
                    invocation_id,
                    result: result_message.clone(),
                });
                messages.push(result_message);
            }
        }

        bail!(
            "model exceeded the {}-round tool limit",
            self.max_tool_rounds
        )
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
