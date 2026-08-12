//! Anthropic Messages adapter.
//!
//! Anthropic's top-level system field, ordered content blocks, tool schema
//! shape, and typed SSE events stay private here. The runtime only receives
//! the provider-neutral message model and normalized stream deltas.

use crate::{
    credential::SecretString,
    identity::StepId,
    message::{ContentBlock, Message, Role, ToolCall, ToolResultStatus},
    provider::{ConversationalProvider, DeltaSink, ProviderError, ProviderUsage},
    sse::SseDecoder,
    tool::ToolDefinition,
    vision::MediaResolver,
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

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
    #[serde(rename = "image")]
    Image { source: WireImageSource },
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

#[derive(Debug, Clone, Serialize)]
struct WireImageSource {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: String,
    data: String,
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
    media: Option<&MediaResolver>,
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
                            return Err(AnthropicConversionError::UnsupportedBlock("system image"));
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
                        .map(|block| convert_content(block, media))
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
                name: &definition.name,
                description: &definition.description,
                input_schema: &definition.parameters,
            })
            .collect(),
        stream: true,
    })
}

fn convert_content(
    block: &ContentBlock,
    media: Option<&MediaResolver>,
) -> Result<WireContent, AnthropicConversionError> {
    match block {
        ContentBlock::Text(text) => Ok(WireContent::Text { text: text.clone() }),
        ContentBlock::Image(image) => {
            let resolver = media.ok_or(AnthropicConversionError::UnsupportedBlock(
                "image without a media resolver",
            ))?;
            let data = resolver
                .resolve_base64(image)
                .map_err(|_| AnthropicConversionError::UnsupportedBlock("unresolvable image"))?;
            Ok(WireContent::Image {
                source: WireImageSource {
                    kind: "base64",
                    media_type: image.media_type.clone(),
                    data,
                },
            })
        }
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
    Transport(reqwest::Error),
    Http(reqwest::Error),
    Stream(String),
}
impl fmt::Display for AnthropicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(_) => f.write_str("could not reach Anthropic Messages API"),
            Self::Http(_) => f.write_str("Anthropic Messages API rejected the request"),
            Self::Stream(message) => write!(f, "invalid Anthropic stream: {message}"),
        }
    }
}
impl Error for AnthropicError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(source) | Self::Http(source) => Some(source),
            Self::Stream(_) => None,
        }
    }
}

pub(crate) struct AnthropicClient {
    client: Client,
    endpoint: String,
    api_key: SecretString,
    default_model: String,
    media: Option<MediaResolver>,
}

impl AnthropicClient {
    pub(crate) fn new(
        base_url: impl Into<String>,
        api_key: SecretString,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            endpoint: format!("{}/v1/messages", base_url.into().trim_end_matches('/')),
            api_key,
            default_model: model.into(),
            media: None,
        }
    }

    pub(crate) fn with_media_resolver(mut self, media: MediaResolver) -> Self {
        self.media = Some(media);
        self
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
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
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&request)
            .send()
            .await
            .map_err(AnthropicError::Transport)?
            .error_for_status()
            .map_err(AnthropicError::Http)?;
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut accumulator = AnthropicAccumulator::default();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(AnthropicError::Transport)?;
            for event in decoder
                .push(&chunk)
                .map_err(|error| AnthropicError::Stream(error.to_string()))?
            {
                if event.data.is_empty() {
                    continue;
                }
                let event = serde_json::from_slice(&event.data)
                    .map_err(|_| AnthropicError::Stream("SSE data is not JSON".into()))?;
                accumulator
                    .apply(&event, step_id, deltas)
                    .map_err(AnthropicError::Stream)?;
            }
        }
        decoder
            .finish()
            .map_err(|error| AnthropicError::Stream(error.to_string()))?;
        accumulator.finish().map_err(AnthropicError::Stream)
    }
}

impl ConversationalProvider for AnthropicClient {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            let request = convert_messages(
                messages,
                tools,
                &self.default_model,
                4096,
                self.media.as_ref(),
            )
            .map_err(|error| ProviderError::new(error.to_string()))?;
            self.stream_message_inner(request, step_id, deltas)
                .await
                .map_err(|error| ProviderError::new(error.to_string()))
        })
    }
}

const MAX_STREAMED_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAMED_TOOL_BYTES: usize = 256 * 1024;
const MAX_CONTENT_BLOCKS: usize = 64;

#[derive(Debug, Default)]
struct AnthropicAccumulator {
    message_started: bool,
    message_stopped: bool,
    blocks: BTreeMap<usize, PartialAnthropicBlock>,
    stopped_blocks: BTreeSet<usize>,
    stop_reason: Option<String>,
    streamed_text_bytes: usize,
    streamed_tool_bytes: usize,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Debug)]
enum PartialAnthropicBlock {
    Text(String),
    Tool(PartialAnthropicTool),
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
            "message_start" => {
                if self.message_started {
                    return Err("received duplicate message_start".into());
                }
                self.message_started = true;
                self.input_tokens = event
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64);
            }
            "content_block_start" => {
                if !self.message_started || self.message_stopped {
                    return Err("content block started outside an active message".into());
                }
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("content block missing index")? as usize;
                let block = event
                    .get("content_block")
                    .ok_or("content block missing body")?;
                if self.blocks.len() >= MAX_CONTENT_BLOCKS {
                    return Err(format!(
                        "Anthropic response exceeded the {MAX_CONTENT_BLOCKS}-block limit"
                    ));
                }
                let partial = match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        if self.streamed_text_bytes.saturating_add(text.len())
                            > MAX_STREAMED_TEXT_BYTES
                        {
                            return Err("streamed Anthropic text exceeded its byte limit".into());
                        }
                        self.streamed_text_bytes += text.len();
                        PartialAnthropicBlock::Text(text)
                    }
                    Some("tool_use") => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let bytes = id.len().saturating_add(name.len());
                        if self.streamed_tool_bytes.saturating_add(bytes) > MAX_STREAMED_TOOL_BYTES
                        {
                            return Err(
                                "streamed Anthropic tool data exceeded its byte limit".into()
                            );
                        }
                        self.streamed_tool_bytes += bytes;
                        PartialAnthropicBlock::Tool(PartialAnthropicTool {
                            id,
                            name,
                            input_json: String::new(),
                        })
                    }
                    Some(other) => return Err(format!("unsupported content block {other}")),
                    None => return Err("content block missing type".into()),
                };
                if self.blocks.insert(index, partial).is_some() {
                    return Err(format!("content block {index} started more than once"));
                }
            }
            "content_block_delta" => {
                if !self.message_started || self.message_stopped {
                    return Err("content delta arrived outside an active message".into());
                }
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("content delta missing index")? as usize;
                let delta = event.get("delta").ok_or("content delta missing body")?;
                if self.stopped_blocks.contains(&index) {
                    return Err(format!("content delta arrived after block {index} stopped"));
                }
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or_else(|| format!("content delta arrived before block {index} started"))?;
                match (block, delta.get("type").and_then(Value::as_str)) {
                    (PartialAnthropicBlock::Text(complete), Some("text_delta")) => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if self.streamed_text_bytes.saturating_add(text.len())
                            > MAX_STREAMED_TEXT_BYTES
                        {
                            return Err("streamed Anthropic text exceeded its byte limit".into());
                        }
                        self.streamed_text_bytes += text.len();
                        complete.push_str(text);
                        deltas.text_delta(step_id, text);
                    }
                    (PartialAnthropicBlock::Tool(tool), Some("input_json_delta")) => {
                        let fragment = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if self.streamed_tool_bytes.saturating_add(fragment.len())
                            > MAX_STREAMED_TOOL_BYTES
                        {
                            return Err(
                                "streamed Anthropic tool input exceeded its byte limit".into()
                            );
                        }
                        self.streamed_tool_bytes += fragment.len();
                        tool.input_json.push_str(fragment);
                    }
                    (_, Some(other)) => {
                        return Err(format!(
                            "content delta {other} does not match block {index}"
                        ));
                    }
                    (_, None) => return Err("content delta missing type".into()),
                }
            }
            "content_block_stop" => {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("content block stop missing index")?
                    as usize;
                if !self.blocks.contains_key(&index) {
                    return Err(format!("content block {index} stopped before it started"));
                }
                if !self.stopped_blocks.insert(index) {
                    return Err(format!("content block {index} stopped more than once"));
                }
            }
            "message_delta" => {
                self.stop_reason = event
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.output_tokens = event
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64);
            }
            "message_stop" => {
                if !self.message_started || self.message_stopped {
                    return Err("message_stop arrived outside an active message".into());
                }
                if self.stopped_blocks.len() != self.blocks.len() {
                    return Err("message_stop arrived while a content block was active".into());
                }
                self.message_stopped = true;
                if self.input_tokens.is_some() || self.output_tokens.is_some() {
                    deltas.usage(ProviderUsage {
                        input_tokens: self.input_tokens,
                        output_tokens: self.output_tokens,
                        total_tokens: None,
                    });
                }
            }
            "ping" => {}
            other => return Err(format!("unsupported Anthropic event {other}")),
        }
        Ok(())
    }
    fn finish(self) -> Result<Message, String> {
        if !self.message_stopped {
            return Err("Anthropic stream ended before message_stop".into());
        }
        let mut content = Vec::new();
        for (expected, (index, block)) in self.blocks.into_iter().enumerate() {
            if index != expected {
                return Err(format!(
                    "content block {index} was not the expected contiguous index {expected}"
                ));
            }
            match block {
                PartialAnthropicBlock::Text(text) => content.push(ContentBlock::Text(text)),
                PartialAnthropicBlock::Tool(tool) => {
                    if tool.id.is_empty() || tool.name.is_empty() {
                        return Err(format!("tool block {index} is missing id or name"));
                    }
                    let input = if tool.input_json.is_empty() {
                        "{}"
                    } else {
                        &tool.input_json
                    };
                    let arguments = serde_json::from_str(input)
                        .map_err(|_| format!("tool block {index} has invalid JSON input"))?;
                    content.push(ContentBlock::ToolCall(ToolCall {
                        id: tool.id,
                        name: tool.name,
                        arguments,
                    }));
                }
            }
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
    use crate::{
        artifact::ArtifactStore,
        identity::PrincipalId,
        message::ToolResult,
        vision::{ImageRef, MediaResolver},
    };

    fn tool() -> ToolDefinition {
        ToolDefinition {
            name: "lookup".into(),
            contract_version: 1,
            description: "lookup".into(),
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
        let request = convert_messages(&messages, &[&definition], "claude", 1024, None).unwrap();
        assert_eq!(request.system.as_deref(), Some("system"));
        assert!(matches!(
            request.messages[1].content[0],
            WireContent::ToolUse { .. }
        ));
        assert_eq!(request.messages[2].role, "user");
    }

    #[test]
    fn image_content_uses_anthropic_base64_source_without_a_path() {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(directory.path().join("artifacts"));
        let (artifact, _) = store
            .put(b"image", "image/png", PrincipalId::new())
            .unwrap();
        let messages = [Message {
            role: Role::User,
            content: vec![ContentBlock::Image(ImageRef {
                artifact,
                media_type: "image/png".into(),
                byte_len: 5,
                width: Some(1),
                height: Some(1),
            })],
        }];
        let resolver = MediaResolver::new(store, 64);
        let request = convert_messages(&messages, &[], "claude", 1024, Some(&resolver)).unwrap();
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"type\":\"image\""));
        assert!(encoded.contains("\"data\":\"aW1hZ2U=\""));
        assert!(!encoded.contains(directory.path().to_str().unwrap()));
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
            json!({"type":"message_start","message":{}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"c","name":"lookup","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"x\"}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
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

    #[test]
    fn sse_accumulator_emits_anthropic_usage_without_inventing_total_tokens() {
        struct Sink(std::sync::Mutex<Option<ProviderUsage>>);
        impl DeltaSink for Sink {
            fn text_delta(&self, _: StepId, _: &str) {}

            fn usage(&self, usage: ProviderUsage) {
                *self.0.lock().expect("usage lock") = Some(usage);
            }
        }
        let sink = Sink(std::sync::Mutex::new(None));
        let mut accumulator = AnthropicAccumulator::default();
        let step = StepId::new();
        for event in [
            json!({"type":"message_start","message":{"usage":{"input_tokens":21}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"done"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}),
            json!({"type":"message_stop"}),
        ] {
            accumulator.apply(&event, step, &sink).expect("event");
        }
        let usage = sink.0.lock().expect("usage lock").expect("usage");
        assert_eq!(usage.input_tokens, Some(21));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.total_tokens, None);
    }

    #[test]
    fn truncated_or_out_of_order_stream_never_returns_a_message() {
        struct Sink;
        impl DeltaSink for Sink {
            fn text_delta(&self, _: StepId, _: &str) {}
        }
        let mut accumulator = AnthropicAccumulator::default();
        let error = accumulator
            .apply(
                &json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"early"}}),
                StepId::new(),
                &Sink,
            )
            .unwrap_err();
        assert!(error.contains("outside an active message"));

        let mut accumulator = AnthropicAccumulator::default();
        accumulator
            .apply(
                &json!({"type":"message_start","message":{}}),
                StepId::new(),
                &Sink,
            )
            .unwrap();
        assert!(accumulator.finish().unwrap_err().contains("message_stop"));
    }
}
