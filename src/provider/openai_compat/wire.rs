//! Private OpenAI-compatible JSON shapes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum WireRole {
    System,
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
    pub(super) content: Option<WireMessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<WireToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum WireMessageContent {
    Text(String),
    Parts(Vec<WireContentPart>),
}

impl WireMessageContent {
    #[cfg(test)]
    pub(super) fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Parts(_) => None,
        }
    }
}

impl From<String> for WireMessageContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for WireMessageContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(super) enum WireContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: WireImageUrl },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct WireImageUrl {
    pub(super) url: String,
}

#[derive(Debug, Serialize)]
pub(super) struct WireChatRequest<'a> {
    pub(super) model: &'a str,
    pub(super) messages: Vec<WireMessage>,
    pub(super) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stream_options: Option<WireStreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_output_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) tools: Vec<WireToolDefinition<'a>>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct WireStreamOptions {
    pub(super) include_usage: bool,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
pub(super) struct WireChatResponse {
    pub(super) choices: Vec<WireChatChoice>,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
pub(super) struct WireChatChoice {
    pub(super) message: WireMessage,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireStreamResponse {
    #[serde(default)]
    pub(super) choices: Vec<WireStreamChoice>,
    #[serde(default)]
    pub(super) usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(super) struct WireUsage {
    #[serde(default)]
    pub(super) prompt_tokens: Option<u64>,
    #[serde(default)]
    pub(super) completion_tokens: Option<u64>,
    #[serde(default)]
    pub(super) total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireStreamChoice {
    pub(super) delta: WireDelta,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct WireDelta {
    #[serde(default)]
    pub(super) content: Option<String>,
    #[serde(default, alias = "reasoning_content")]
    pub(super) reasoning: Option<String>,
    #[serde(default)]
    pub(super) tool_calls: Option<Vec<WireToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireToolCallDelta {
    pub(super) index: usize,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) function: Option<WireFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireFunctionDelta {
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) arguments: Option<String>,
}
