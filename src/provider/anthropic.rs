//! Anthropic Messages adapter.
//!
//! Anthropic's top-level system field, ordered content blocks, tool schema
//! shape, and typed SSE events stay private here. The runtime only receives
//! the provider-neutral message model and normalized stream deltas.

#![allow(dead_code)]

use crate::{
    agent::{ChatError, ChatTransport, DeltaSink},
    identity::StepId,
    message::{ContentBlock, Message, Role, ToolCall, ToolResultStatus},
    tool::ToolDefinition,
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::{collections::BTreeMap, error::Error, fmt};

const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnthropicConversionError {
    MissingSystemText,
    InvalidMessageShape(&'static str),
    UnsupportedBlock(&'static str),
}

impl fmt::Display for AnthropicConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSystemText => f.write_str("Anthropic system content must be text"),
            Self::InvalidMessageShape(detail) => write!(f, "invalid internal message: {detail}"),
            Self::UnsupportedBlock(kind) => write!(f, "Anthropic adapter cannot represent {kind}"),
        }
    }
}
impl Error for AnthropicConversionError {}

#[derive(Debug, Clone, Serialize)]
struct WireMessage {
    role: &'static str,
    content: Vec<WireContent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum WireContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Serialize)]
struct WireTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    max_tokens: usize,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    stream: bool,
}

fn convert_messages<'a>(
    messages: &[Message],
    tools: &[&'a ToolDefinition],
    model: &'a str,
    max_tokens: usize,
) -> Result<WireRequest<'a>, AnthropicConversionError> {
    let mut system = Vec::new();
    let mut converted = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {
                for block in &message.content {
                    match block {
                        ContentBlock::Text(text) => system.push(text.as_str()),
                        ContentBlock::Image(_) => {
                            return Err(AnthropicConversionError::UnsupportedBlock(
                                "image (Anthropic image stretch is not enabled)",
                            ));
                        }
                        _ => return Err(AnthropicConversionError::MissingSystemText),
                    }
                }
            }
            Role::User | Role::Assistant => {
                let role = if message.role == Role::User {
                    "user"
                } else {
                    "assistant"
                };
                converted.push(WireMessage {
                    role,
                    content: message
                        .content
                        .iter()
                        .map(convert_content)
                        .collect::<Result<Vec<_>, _>>()?,
                });
            }
            Role::Tool => {
                let [ContentBlock::ToolResult(result)] = message.content.as_slice() else {
                    return Err(AnthropicConversionError::InvalidMessageShape(
                        "tool messages require one tool result",
                    ));
                };
                converted.push(WireMessage {
                    role: "user",
                    content: vec![WireContent::ToolResult {
                        tool_use_id: result.call_id.clone(),
                        content: result.output.clone(),
                        is_error: result.status == ToolResultStatus::Error,
                    }],
                });
            }
        }
    }
    Ok(WireRequest {
        model,
        max_tokens,
        messages: converted,
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        tools: tools
            .iter()
            .map(|definition| WireTool {
                name: definition.name,
                description: definition.description,
                input_schema: &definition.parameters,
            })
            .collect(),
        stream: true,
    })
}

fn convert_content(block: &ContentBlock) -> Result<WireContent, AnthropicConversionError> {
    match block {
        ContentBlock::Text(text) => Ok(WireContent::Text { text: text.clone() }),
        ContentBlock::Image(_) => Err(AnthropicConversionError::UnsupportedBlock(
            "image (Anthropic image stretch is not enabled)",
        )),
        ContentBlock::ToolCall(ToolCall {
            id,
            name,
            arguments,
        }) => Ok(WireContent::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: arguments.clone(),
        }),
        ContentBlock::ToolResult(_) => Err(AnthropicConversionError::UnsupportedBlock(
            "embedded tool result",
        )),
    }
}

#[derive(Debug)]
pub(crate) enum AnthropicError {
    Conversion(AnthropicConversionError),
    Transport(reqwest::Error),
    Http(reqwest::Error),
    Stream(String),
}
impl fmt::Display for AnthropicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversion(error) => error.fmt(f),
            Self::Transport(_) => f.write_str("could not reach Anthropic Messages API"),
            Self::Http(_) => f.write_str("Anthropic Messages API rejected the request"),
            Self::Stream(message) => write!(f, "invalid Anthropic stream: {message}"),
        }
    }
}
impl Error for AnthropicError {}

pub(crate) struct AnthropicClient {
    client: Client,
    endpoint: String,
    api_key: String,
    default_model: String,
}

impl AnthropicClient {
    pub(crate) fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            endpoint: format!("{}/v1/messages", base_url.into().trim_end_matches('/')),
            api_key: api_key.into(),
            default_model: model.into(),
        }
    }

    async fn stream_message_inner(
        &self,
        request: WireRequest<'_>,
        step_id: StepId,
        deltas: &dyn DeltaSink,
    ) -> Result<Message, AnthropicError> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&request)
            .send()
            .await
            .map_err(AnthropicError::Transport)?
            .error_for_status()
            .map_err(AnthropicError::Http)?;
        let mut stream = response.bytes_stream();
        let mut decoder = AnthropicSseDecoder::default();
        let mut accumulator = AnthropicAccumulator::default();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(AnthropicError::Transport)?;
            for event in decoder.push(&chunk).map_err(AnthropicError::Stream)? {
                accumulator
                    .apply(&event, step_id, deltas)
                    .map_err(AnthropicError::Stream)?;
            }
        }
        decoder.finish().map_err(AnthropicError::Stream)?;
        accumulator.finish().map_err(AnthropicError::Stream)
    }
}

impl ChatTransport for AnthropicClient {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ChatError>> {
        Box::pin(async move {
            let request = convert_messages(messages, tools, &self.default_model, 4096)
                .map_err(|error| ChatError::new(error.to_string()))?;
            self.stream_message_inner(request, step_id, deltas)
                .await
                .map_err(|error| ChatError::new(error.to_string()))
        })
    }
}

#[derive(Debug, Default)]
struct AnthropicSseDecoder {
    buffer: Vec<u8>,
}
impl AnthropicSseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.windows(2).position(|pair| pair == b"\n\n") {
            let frame = self.buffer.drain(..end + 2).collect::<Vec<_>>();
            let text = std::str::from_utf8(&frame).map_err(|_| "SSE frame is not UTF-8")?;
            let data = text
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                events.push(serde_json::from_str(&data).map_err(|_| "SSE data is not JSON")?);
            }
        }
        if self.buffer.len() > 256 * 1024 {
            return Err("SSE frame exceeds 256 KiB".into());
        }
        Ok(events)
    }
    fn finish(self) -> Result<(), String> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err("stream ended with an incomplete SSE frame".into())
        }
    }
}

#[derive(Debug, Default)]
struct AnthropicAccumulator {
    text: String,
    tool_calls: BTreeMap<usize, PartialAnthropicTool>,
}
#[derive(Debug, Default)]
struct PartialAnthropicTool {
    id: String,
    name: String,
    input_json: String,
}

impl AnthropicAccumulator {
    fn apply(
        &mut self,
        event: &Value,
        step_id: StepId,
        deltas: &dyn DeltaSink,
    ) -> Result<(), String> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or("event missing type")?;
        match kind {
            "content_block_start" => {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("content block missing index")? as usize;
                let block = event
                    .get("content_block")
                    .ok_or("content block missing body")?;
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {}
                    Some("tool_use") => {
                        self.tool_calls.insert(
                            index,
                            PartialAnthropicTool {
                                id: block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .into(),
                                name: block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .into(),
                                input_json: String::new(),
                            },
                        );
                    }
                    Some(other) => return Err(format!("unsupported content block {other}")),
                    None => return Err("content block missing type".into()),
                }
            }
            "content_block_delta" => {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("content delta missing index")? as usize;
                let delta = event.get("delta").ok_or("content delta missing body")?;
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        self.text.push_str(text);
                        deltas.text_delta(step_id, text);
                    }
                    Some("input_json_delta") => {
                        self.tool_calls
                            .entry(index)
                            .or_default()
                            .input_json
                            .push_str(
                                delta
                                    .get("partial_json")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            );
                    }
                    Some(other) => return Err(format!("unsupported content delta {other}")),
                    None => return Err("content delta missing type".into()),
                }
            }
            "message_start" | "content_block_stop" | "message_delta" | "message_stop" | "ping" => {}
            other => return Err(format!("unsupported Anthropic event {other}")),
        }
        Ok(())
    }
    fn finish(self) -> Result<Message, String> {
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(ContentBlock::Text(self.text));
        }
        for (index, tool) in self.tool_calls {
            if tool.id.is_empty() || tool.name.is_empty() {
                return Err(format!("tool block {index} is missing id or name"));
            }
            let arguments = serde_json::from_str(&tool.input_json)
                .map_err(|_| format!("tool block {index} has invalid JSON input"))?;
            content.push(ContentBlock::ToolCall(ToolCall {
                id: tool.id,
                name: tool.name,
                arguments,
            }));
        }
        Ok(Message {
            role: Role::Assistant,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolResult;

    fn tool() -> ToolDefinition {
        ToolDefinition {
            name: "lookup",
            contract_version: 1,
            description: "lookup",
            parameters: json!({"type":"object","properties":{"q":{"type":"string"}}}),
            effect_class: crate::tool::EffectClass::Read,
            replay_safety: crate::tool::ReplaySafety::Safe,
        }
    }

    #[test]
    fn conversion_moves_system_to_top_level_and_keeps_tool_result_order() {
        let messages = vec![
            Message::text(Role::System, "system"),
            Message::text(Role::User, "question"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    arguments: json!({"q":"x"}),
                })],
            },
            Message::tool_result(ToolResult {
                call_id: "call-1".into(),
                output: "answer".into(),
                status: ToolResultStatus::Success,
            }),
        ];
        let definition = tool();
        let request = convert_messages(&messages, &[&definition], "claude", 1024).unwrap();
        assert_eq!(request.system.as_deref(), Some("system"));
        assert!(matches!(
            request.messages[1].content[0],
            WireContent::ToolUse { .. }
        ));
        assert_eq!(request.messages[2].role, "user");
    }

    #[test]
    fn sse_accumulator_normalizes_text_and_split_tool_json() {
        struct Sink;
        impl DeltaSink for Sink {
            fn text_delta(&self, _: StepId, _: &str) {}
        }
        let mut accumulator = AnthropicAccumulator::default();
        let sink = Sink;
        let step = StepId::new();
        for event in [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"c","name":"lookup","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"x\"}"}}),
            json!({"type":"message_stop"}),
        ] {
            accumulator.apply(&event, step, &sink).unwrap();
        }
        let message = accumulator.finish().unwrap();
        assert!(matches!(&message.content[0], ContentBlock::Text(text) if text == "hello"));
        assert!(
            matches!(&message.content[1], ContentBlock::ToolCall(call) if call.arguments == json!({"q":"x"}))
        );
    }
}
