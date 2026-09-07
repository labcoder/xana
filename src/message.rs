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

impl ToolCall {
    /// Correlate attempts independently of provider IDs and JSON object key order.
    pub(crate) fn pattern_fingerprint(&self) -> blake3::Hash {
        let mut arguments = self.arguments.clone();
        arguments.sort_all_objects();
        blake3::hash(&serde_json::to_vec(&(&self.name, arguments)).expect("JSON values serialize"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ToolResultStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ToolFailure {
    PermissionDenied,
    /// Absent from this runtime's immutable capability snapshot, not transient I/O.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolResult {
    pub(crate) call_id: String,
    pub(crate) output: String,
    pub(crate) status: ToolResultStatus,
    /// Set by a trusted runtime boundary, never inferred from tool output text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure: Option<ToolFailure>,
    /// Registered immutable evidence, never inferred from provider text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) artifact: Option<Box<crate::artifact::ArtifactRecord>>,
    /// Only the built-in command adapter supplies this observed process status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command_status: Option<crate::completion_evidence::CommandStatus>,
}

impl ToolResult {
    pub(crate) fn success(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            output: output.into(),
            status: ToolResultStatus::Success,
            failure: None,
            artifact: None,
            command_status: None,
        }
    }

    pub(crate) fn error(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            output: output.into(),
            status: ToolResultStatus::Error,
            failure: None,
            artifact: None,
            command_status: None,
        }
    }

    pub(crate) fn denied(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            failure: Some(ToolFailure::PermissionDenied),
            ..Self::error(call_id, output)
        }
    }

    pub(crate) fn unavailable(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            failure: Some(ToolFailure::Unavailable),
            ..Self::error(call_id, output)
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
    pub(crate) fn artifacts(&self) -> impl Iterator<Item = &crate::artifact::ArtifactRecord> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::Image(image) => Some(&image.artifact),
            ContentBlock::ToolResult(result) => result.artifact.as_deref(),
            _ => None,
        })
    }

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
