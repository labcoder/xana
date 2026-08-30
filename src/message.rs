//! Xana's provider-neutral conversation model.
//!
//! Provider adapters translate these ordered content blocks at their private
//! wire boundary. Durable sessions will also record these internal types.

use crate::vision::ImageRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Role {
    System,
    User,
    Assistant,
    Tool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ToolResultStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolResult {
    pub(crate) call_id: String,
    pub(crate) output: String,
    pub(crate) status: ToolResultStatus,
}

impl ToolResult {
    pub(crate) fn success(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            output: output.into(),
            status: ToolResultStatus::Success,
        }
    }

    pub(crate) fn error(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            output: output.into(),
            status: ToolResultStatus::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ContentBlock {
    Text(String),
    Image(ImageRef),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Message {
    pub(crate) role: Role,
    pub(crate) content: Vec<ContentBlock>,
}

impl Message {
    /// Create the common one-text-block message while taking ownership of text.
    pub(crate) fn text(role: Role, text: impl Into<String>) -> Self {
        let text = text.into();

        Self {
            role,
            content: vec![ContentBlock::Text(text)],
        }
    }

    pub(crate) fn tool_result(result: ToolResult) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(result)],
        }
    }
}
