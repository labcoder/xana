//! Headless, bounded agent loop.
//!
//! The agent receives owned provider, tool, workspace, and limit values. It
//! does not read configuration, environment variables, or terminal state.

use crate::message::{ContentBlock, Message, ToolCall};
use crate::prompt::PromptSnapshot;
use crate::provider::openai_compat::OpenAiCompatClient;
use crate::tool::ToolRegistry;
use anyhow::{Result, bail};
use std::path::PathBuf;

pub(crate) struct Agent {
    provider: OpenAiCompatClient,
    tools: ToolRegistry,
    workspace_root: PathBuf,
    prompt: PromptSnapshot,
    max_tool_rounds: usize,
}

impl Agent {
    pub(crate) fn new(
        provider: OpenAiCompatClient,
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

    pub(crate) fn run_turn(&self, messages: &mut Vec<Message>) -> Result<Message> {
        let definitions = self.tools.definitions();

        for _ in 0..self.max_tool_rounds {
            let request_messages = self.prompt.messages_for_request(messages)?;
            let assistant = self
                .provider
                .send_message(&request_messages, &definitions)?;
            let calls = requested_tools(&assistant);

            if calls.is_empty() {
                return Ok(assistant);
            }

            messages.push(assistant);

            for call in calls {
                let result = self.tools.execute(&call, &self.workspace_root);
                messages.push(Message::tool_result(result));
            }
        }

        bail!(
            "model exceeded the {}-round tool limit",
            self.max_tool_rounds
        )
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
mod tests {
    use super::*;
    use crate::{
        context::ContextBudget,
        message::Role,
        prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
    };
    use tempfile::tempdir;

    #[test]
    fn requested_tools_preserve_model_order_and_values() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("I'll inspect both.".to_owned()),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-a".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-b".to_owned(),
                    name: "list_files".to_owned(),
                    arguments: serde_json::json!({"path": "src"}),
                }),
            ],
        };

        let calls = requested_tools(&message);

        assert_eq!(
            calls,
            vec![
                ToolCall {
                    id: "call-a".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                ToolCall {
                    id: "call-b".to_owned(),
                    name: "list_files".to_owned(),
                    arguments: serde_json::json!({"path": "src"}),
                },
            ]
        );
    }

    #[test]
    fn requested_tools_are_empty_for_final_text() {
        let message = Message::text(Role::Assistant, "Finished.");

        assert!(requested_tools(&message).is_empty());
    }

    #[test]
    fn zero_round_limit_fails_before_history_changes() {
        let provider =
            OpenAiCompatClient::new("http://127.0.0.1:9/v1".to_owned(), "test-model".to_owned());
        let tools = ToolRegistry::new();
        let workspace = tempdir().expect("temporary workspace");
        let environment = PromptEnvironment {
            operating_system: "test".to_owned(),
            working_directory: workspace.path().to_owned(),
            configured_shell: "test shell".to_owned(),
            surface: PromptSurface::Cli,
        };
        let prompt = assemble_snapshot(PromptInputs {
            tool_definitions: &tools.definitions(),
            environment: &environment,
            product_documentation: None,
            project_sources: &[],
            budget: ContextBudget {
                total_tokens: 8_192,
                conversation_reserve_tokens: 2_048,
            },
        })
        .expect("test prompt");
        let agent = Agent::new(provider, tools, workspace.path().to_owned(), prompt, 0);
        let mut messages = Vec::new();

        let result = agent.run_turn(&mut messages);

        assert!(result.is_err());
        assert!(messages.is_empty());
    }
}
