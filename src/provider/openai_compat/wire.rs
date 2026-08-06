//! Private OpenAI-compatible JSON shapes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum WireRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum WireToolKind {
    Function,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WireFunctionCall {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WireToolCall {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) kind: WireToolKind,
    pub(super) function: WireFunctionCall,
}

#[derive(Debug, Serialize)]
pub(super) struct WireToolDefinition<'a> {
    #[serde(rename = "type")]
    pub(super) kind: WireToolKind,
    pub(super) function: WireFunctionDefinition<'a>,
}

#[derive(Debug, Serialize)]
pub(super) struct WireFunctionDefinition<'a> {
    pub(super) name: &'a str,
    pub(super) description: &'a str,
    pub(super) parameters: &'a serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WireMessage {
    pub(super) role: WireRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<WireToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct WireChatRequest<'a> {
    pub(super) model: &'a str,
    pub(super) messages: Vec<WireMessage>,
    pub(super) stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) tools: Vec<WireToolDefinition<'a>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireChatResponse {
    pub(super) choices: Vec<WireChatChoice>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireChatChoice {
    pub(super) message: WireMessage,
}
