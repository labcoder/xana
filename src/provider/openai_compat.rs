use crate::message::{ContentBlock, Message, Role, ToolCall, ToolResultStatus};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireToolKind {
    Function,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: WireToolKind,
    function: WireFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireMessage {
    role: WireRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct WireChatRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct WireChatResponse {
    choices: Vec<WireChatChoice>,
}

#[derive(Debug, Deserialize)]
struct WireChatChoice {
    message: WireMessage,
}

#[derive(Debug)]
enum MessageConversionError {
    TextAfterToolCall,
    InvalidToolArguments {
        tool_call_id: String,
        source: serde_json::Error,
    },
    UnexpectedResponseRole(WireRole),
    InvalidMessageShape {
        role: Role,
        detail: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenAiCompatErrorKind {
    RequestConversion,
    Transport,
    HttpStatus,
    Decode,
    MissingChoice,
    ResponseConversion,
}

#[derive(Debug)]
enum OpenAiCompatErrorSource {
    None,
    Conversion(MessageConversionError),
    Http(reqwest::Error),
}

#[derive(Debug)]
pub(crate) struct OpenAiCompatError {
    pub(crate) kind: OpenAiCompatErrorKind,
    endpoint: String,
    source: OpenAiCompatErrorSource,
}

impl OpenAiCompatError {
    fn conversion(
        kind: OpenAiCompatErrorKind,
        endpoint: &str,
        source: MessageConversionError,
    ) -> Self {
        Self {
            kind,
            endpoint: endpoint.to_owned(),
            source: OpenAiCompatErrorSource::Conversion(source),
        }
    }

    fn http(kind: OpenAiCompatErrorKind, endpoint: &str, source: reqwest::Error) -> Self {
        Self {
            kind,
            endpoint: endpoint.to_owned(),
            source: OpenAiCompatErrorSource::Http(source),
        }
    }

    fn missing_choice(endpoint: &str) -> Self {
        Self {
            kind: OpenAiCompatErrorKind::MissingChoice,
            endpoint: endpoint.to_owned(),
            source: OpenAiCompatErrorSource::None,
        }
    }
}

impl fmt::Display for OpenAiCompatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            OpenAiCompatErrorKind::RequestConversion => {
                write!(f, "could not encode chat request for {}", self.endpoint)
            }
            OpenAiCompatErrorKind::Transport => {
                write!(f, "could not reach chat service at {}", self.endpoint)
            }
            OpenAiCompatErrorKind::HttpStatus => {
                write!(f, "chat service rejected request at {}", self.endpoint)
            }
            OpenAiCompatErrorKind::Decode => {
                write!(
                    f,
                    "chat service returned invalid JSON from {}",
                    self.endpoint
                )
            }
            OpenAiCompatErrorKind::MissingChoice => {
                write!(f, "chat service returned no choices from {}", self.endpoint)
            }
            OpenAiCompatErrorKind::ResponseConversion => {
                write!(
                    f,
                    "chat service returned an unsupported message from {}",
                    self.endpoint
                )
            }
        }
    }
}

impl Error for OpenAiCompatError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.source {
            OpenAiCompatErrorSource::Conversion(source) => Some(source),
            OpenAiCompatErrorSource::Http(source) => Some(source),
            OpenAiCompatErrorSource::None => None,
        }
    }
}

impl fmt::Display for MessageConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextAfterToolCall => {
                write!(
                    f,
                    "text after a tool call cannot be represented by this adapter"
                )
            }
            Self::InvalidToolArguments {
                tool_call_id,
                source,
            } => {
                write!(
                    f,
                    "tool call {tool_call_id:?} contained invalid JSON arguments: {source}"
                )
            }
            Self::UnexpectedResponseRole(role) => {
                write!(f, "provider returned unexpected response role {role:?}")
            }
            Self::InvalidMessageShape { role, detail } => {
                write!(f, "internal {role:?} message is invalid: {detail}")
            }
        }
    }
}

impl Error for MessageConversionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidToolArguments { source, .. } => Some(source),
            Self::TextAfterToolCall
            | Self::UnexpectedResponseRole(_)
            | Self::InvalidMessageShape { .. } => None,
        }
    }
}

impl From<Role> for WireRole {
    fn from(role: Role) -> Self {
        match role {
            Role::User => Self::User,
            Role::Assistant => Self::Assistant,
            Role::Tool => Self::Tool,
        }
    }
}

impl From<&ToolCall> for WireToolCall {
    fn from(tool_call: &ToolCall) -> Self {
        Self {
            id: tool_call.id.clone(),
            kind: WireToolKind::Function,
            function: WireFunctionCall {
                name: tool_call.name.clone(),
                arguments: tool_call.arguments.to_string(),
            },
        }
    }
}

impl TryFrom<WireToolCall> for ToolCall {
    type Error = MessageConversionError;

    fn try_from(wire: WireToolCall) -> Result<Self, Self::Error> {
        let WireToolCall {
            id,
            kind: _,
            function,
        } = wire;

        let arguments = serde_json::from_str(&function.arguments).map_err(|source| {
            MessageConversionError::InvalidToolArguments {
                tool_call_id: id.clone(),
                source,
            }
        })?;

        Ok(Self {
            id,
            name: function.name,
            arguments,
        })
    }
}

impl TryFrom<WireMessage> for Message {
    type Error = MessageConversionError;

    fn try_from(wire: WireMessage) -> Result<Self, Self::Error> {
        if wire.role != WireRole::Assistant {
            return Err(MessageConversionError::UnexpectedResponseRole(wire.role));
        }

        let mut content = Vec::new();

        if let Some(text) = wire.content {
            content.push(ContentBlock::Text(text));
        }

        for tool_call in wire.tool_calls.unwrap_or_default() {
            content.push(ContentBlock::ToolCall(ToolCall::try_from(tool_call)?));
        }

        Ok(Self {
            role: Role::Assistant,
            content,
        })
    }
}

impl TryFrom<&Message> for WireMessage {
    type Error = MessageConversionError;

    fn try_from(message: &Message) -> Result<Self, Self::Error> {
        if message.role == Role::Tool {
            return match message.content.as_slice() {
                [ContentBlock::ToolResult(result)] => {
                    let content = match result.status {
                        ToolResultStatus::Success => result.output.clone(),
                        ToolResultStatus::Error => format!("ERROR: {}", result.output),
                    };

                    Ok(Self {
                        role: WireRole::Tool,
                        content: Some(content),
                        tool_calls: None,
                        tool_call_id: Some(result.call_id.clone()),
                    })
                }
                _ => Err(MessageConversionError::InvalidMessageShape {
                    role: message.role,
                    detail: "tool messages require exactly one tool-result block",
                }),
            };
        }

        let mut text: Option<String> = None;
        let mut tool_calls = Vec::new();
        let mut saw_tool_call = false;

        for block in &message.content {
            match block {
                ContentBlock::Text(part) => {
                    if saw_tool_call {
                        return Err(MessageConversionError::TextAfterToolCall);
                    }

                    match &mut text {
                        Some(existing) => existing.push_str(part),
                        None => text = Some(part.clone()),
                    }
                }
                ContentBlock::ToolCall(tool_call) => {
                    saw_tool_call = true;
                    tool_calls.push(WireToolCall::from(tool_call));
                }
                ContentBlock::ToolResult(_) => {
                    return Err(MessageConversionError::InvalidMessageShape {
                        role: message.role,
                        detail: "user and assistant messages cannot contain tool results",
                    });
                }
            }
        }

        Ok(Self {
            role: message.role.into(),
            content: text,
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            tool_call_id: None,
        })
    }
}

pub(crate) struct OpenAiCompatClient {
    client: Client,
    endpoint: String,
    model: String,
}

impl OpenAiCompatClient {
    pub(crate) fn new(base_url: String, model: String) -> Self {
        let endpoint = chat_endpoint(&base_url);

        Self {
            client: Client::new(),
            endpoint,
            model,
        }
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn send_message(&self, messages: &[Message]) -> Result<Message, OpenAiCompatError> {
        let wire_messages = messages
            .iter()
            .map(WireMessage::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| {
                OpenAiCompatError::conversion(
                    OpenAiCompatErrorKind::RequestConversion,
                    &self.endpoint,
                    source,
                )
            })?;

        let request = WireChatRequest {
            model: &self.model,
            messages: wire_messages,
            stream: false,
        };

        let response = self
            .client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .map_err(|source| {
                OpenAiCompatError::http(OpenAiCompatErrorKind::Transport, &self.endpoint, source)
            })?;

        let response = response.error_for_status().map_err(|source| {
            OpenAiCompatError::http(OpenAiCompatErrorKind::HttpStatus, &self.endpoint, source)
        })?;

        let response = response.json::<WireChatResponse>().map_err(|source| {
            OpenAiCompatError::http(OpenAiCompatErrorKind::Decode, &self.endpoint, source)
        })?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| OpenAiCompatError::missing_choice(&self.endpoint))?;

        Message::try_from(choice.message).map_err(|source| {
            OpenAiCompatError::conversion(
                OpenAiCompatErrorKind::ResponseConversion,
                &self.endpoint,
                source,
            )
        })
    }
}

fn chat_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolResult;

    fn first_fixture_message(json: &str) -> WireMessage {
        let response = match serde_json::from_str::<WireChatResponse>(json) {
            Ok(response) => response,
            Err(error) => panic!("expected captured response to decode: {error}"),
        };

        match response.choices.into_iter().next() {
            Some(choice) => choice.message,
            None => panic!("expected captured response to contain one choice"),
        }
    }

    fn assert_wire_messages_match(expected: &WireMessage, actual: &WireMessage) {
        assert_eq!(actual.role, expected.role);
        assert_eq!(actual.content, expected.content);
        assert_eq!(actual.tool_call_id, expected.tool_call_id);
        let expected_calls = expected.tool_calls.as_deref().unwrap_or(&[]);
        let actual_calls = actual.tool_calls.as_deref().unwrap_or(&[]);

        assert_eq!(actual_calls.len(), expected_calls.len());

        for (expected_call, actual_call) in expected_calls.iter().zip(actual_calls) {
            assert_eq!(actual_call.id, expected_call.id);
            assert_eq!(actual_call.kind, expected_call.kind);
            assert_eq!(actual_call.function.name, expected_call.function.name);

            let expected_arguments = match serde_json::from_str::<serde_json::Value>(
                &expected_call.function.arguments,
            ) {
                Ok(arguments) => arguments,
                Err(error) => {
                    panic!("expected fixture tool arguments to be JSON: {error}")
                }
            };

            let actual_arguments =
                match serde_json::from_str::<serde_json::Value>(&actual_call.function.arguments) {
                    Ok(arguments) => arguments,
                    Err(error) => {
                        panic!("expected round-trip tool arguments to be JSON: {error}")
                    }
                };

            assert_eq!(actual_arguments, expected_arguments);
        }
    }

    #[test]
    fn roles_convert_to_wire() {
        assert_eq!(WireRole::from(Role::User), WireRole::User);
        assert_eq!(WireRole::from(Role::Assistant), WireRole::Assistant);
        assert_eq!(WireRole::from(Role::Tool), WireRole::Tool);
    }

    #[test]
    fn tool_results_serialize_status_and_original_call_id() {
        let success = Message::tool_result(ToolResult::success("call-ok", "contents"));
        let error = Message::tool_result(ToolResult::error("call-err", "missing file"));

        let success_wire = WireMessage::try_from(&success).expect("success wire message");
        let error_wire = WireMessage::try_from(&error).expect("error wire message");

        assert_eq!(success_wire.role, WireRole::Tool);
        assert_eq!(success_wire.content.as_deref(), Some("contents"));
        assert_eq!(success_wire.tool_call_id.as_deref(), Some("call-ok"));
        assert_eq!(error_wire.content.as_deref(), Some("ERROR: missing file"));
        assert_eq!(error_wire.tool_call_id.as_deref(), Some("call-err"));
    }

    #[test]
    fn tool_role_from_provider_is_rejected_as_a_response() {
        let wire = WireMessage {
            role: WireRole::Tool,
            content: Some("contents".to_owned()),
            tool_calls: None,
            tool_call_id: Some("call-1".to_owned()),
        };

        assert!(matches!(
            Message::try_from(wire),
            Err(MessageConversionError::UnexpectedResponseRole(
                WireRole::Tool
            ))
        ));
    }

    #[test]
    fn invalid_tool_message_fails_before_http() {
        let client =
            OpenAiCompatClient::new("http://127.0.0.1:9/v1".to_owned(), "test-model".to_owned());
        let message = Message::text(Role::Tool, "tool messages require a tool-result block");

        let result = client.send_message(&[message]);

        match result {
            Err(error) => {
                assert_eq!(error.kind, OpenAiCompatErrorKind::RequestConversion);
                assert!(error.source().is_some());
            }
            Ok(_) => panic!("expected request conversion to fail"),
        }
    }

    #[test]
    fn tool_call_arguments_cross_as_structured_json() {
        let original = ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            arguments: serde_json::json!({"path": "README.md"}),
        };

        let wire = WireToolCall::from(&original);

        assert_eq!(wire.function.arguments, r#"{"path":"README.md"}"#);

        let round_tripped = match ToolCall::try_from(wire) {
            Ok(tool_call) => tool_call,
            Err(error) => panic!("expected tool call to convert: {error}"),
        };

        assert_eq!(round_tripped, original);
    }

    #[test]
    fn invalid_tool_arguments_return_the_call_id() {
        let wire = WireToolCall {
            id: "call-bad-json".to_owned(),
            kind: WireToolKind::Function,
            function: WireFunctionCall {
                name: "read_file".to_owned(),
                arguments: "not json".to_owned(),
            },
        };

        let result = ToolCall::try_from(wire);

        assert!(matches!(
            result,
            Err(MessageConversionError::InvalidToolArguments {
                tool_call_id,
                ..
            }) if tool_call_id == "call-bad-json"
        ));
    }

    #[test]
    fn null_tool_calls_decode_as_absent() {
        let json = r#"{
            "role": "assistant",
            "content": "hello",
            "tool_calls": null
        }"#;

        let wire = match serde_json::from_str::<WireMessage>(json) {
            Ok(wire) => wire,
            Err(error) => panic!("expected wire message to decode: {error}"),
        };

        assert!(wire.tool_calls.is_none());

        let message = match Message::try_from(wire) {
            Ok(message) => message,
            Err(error) => panic!("expected wire message to convert: {error}"),
        };

        assert!(matches!(
            message.content.as_slice(),
            [ContentBlock::Text(text)] if text == "hello"
        ));
    }

    #[test]
    fn text_and_tool_calls_use_the_wire_canonical_order() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("first".to_owned()),
                ContentBlock::Text(" second".to_owned()),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "README.md"}),
                }),
            ],
        };

        let wire = match WireMessage::try_from(&message) {
            Ok(wire) => wire,
            Err(error) => panic!("expected message to convert: {error}"),
        };

        assert_eq!(wire.content.as_deref(), Some("first second"));
        assert!(matches!(
            wire.tool_calls.as_deref(),
            Some([tool_call])
                if tool_call.id == "call-1"
                    && tool_call.function.name == "read_file"
        ));
    }

    #[test]
    fn text_after_a_tool_call_is_rejected() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolCall(ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "README.md"}),
                }),
                ContentBlock::Text("this cannot remain after the call".to_owned()),
            ],
        };

        let result = WireMessage::try_from(&message);

        assert!(matches!(
            result,
            Err(MessageConversionError::TextAfterToolCall)
        ));
    }

    #[test]
    fn unrepresentable_history_fails_before_http() {
        let client =
            OpenAiCompatClient::new("http://127.0.0.1:9/v1".to_owned(), "test-model".to_owned());
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolCall(ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "README.md"}),
                }),
                ContentBlock::Text("not representable afterward".to_owned()),
            ],
        };

        let result = client.send_message(&[message]);

        match result {
            Err(error) => {
                assert_eq!(error.kind, OpenAiCompatErrorKind::RequestConversion);
                assert!(error.source().is_some());
            }
            Ok(_) => panic!("expected request conversion to fail"),
        }
    }

    #[test]
    fn captured_text_response_round_trips_through_internal_message() {
        let original = first_fixture_message(include_str!("fixtures/chat_text_response.json"));

        let internal = match Message::try_from(original.clone()) {
            Ok(message) => message,
            Err(error) => panic!("expected text fixture to convert: {error}"),
        };

        assert_eq!(internal.role, Role::Assistant);
        assert!(
            internal
                .content
                .iter()
                .any(|block| { matches!(block, ContentBlock::Text(text) if !text.is_empty()) })
        );

        let round_tripped = match WireMessage::try_from(&internal) {
            Ok(message) => message,
            Err(error) => panic!("expected internal message to convert: {error}"),
        };

        assert_wire_messages_match(&original, &round_tripped);
    }

    #[test]
    fn captured_tool_call_response_round_trips_through_internal_message() {
        let original = first_fixture_message(include_str!("fixtures/chat_tool_call_response.json"));

        let internal = match Message::try_from(original.clone()) {
            Ok(message) => message,
            Err(error) => panic!("expected tool fixture to convert: {error}"),
        };

        assert!(internal.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolCall(tool_call)
                    if tool_call.name == "lookup_weather"
                        && tool_call
                            .arguments
                            .get("city")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|city| city.to_lowercase().contains("paris"))
            )
        }));

        let round_tripped = match WireMessage::try_from(&internal) {
            Ok(message) => message,
            Err(error) => panic!("expected internal message to convert: {error}"),
        };

        assert_wire_messages_match(&original, &round_tripped);
    }

    #[test]
    fn endpoint_appends_chat_route_without_double_slash() {
        let without_trailing_slash = chat_endpoint("http://localhost:11434/v1");
        let with_trailing_slash = chat_endpoint("http://localhost:11434/v1/");

        assert_eq!(
            without_trailing_slash,
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            with_trailing_slash,
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn request_serializes_internal_history_at_the_wire_edge() {
        let messages = [
            Message::text(Role::User, "Hello"),
            Message::text(Role::Assistant, "Hi there"),
        ];

        let wire_messages = match messages
            .iter()
            .map(WireMessage::try_from)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(messages) => messages,
            Err(error) => panic!("expected history to convert: {error}"),
        };

        let request = WireChatRequest {
            model: "qwen2.5-coder:7b",
            messages: wire_messages,
            stream: false,
        };

        let value = match serde_json::to_value(&request) {
            Ok(value) => value,
            Err(error) => panic!("expected chat request to serialize: {error}"),
        };

        assert_eq!(value["model"], "qwen2.5-coder:7b");
        assert_eq!(value["stream"], false);
        assert_eq!(
            value["messages"].as_array().map(|messages| messages.len()),
            Some(2)
        );
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][0]["content"], "Hello");
        assert_eq!(value["messages"][1]["role"], "assistant");
        assert_eq!(value["messages"][1]["content"], "Hi there");
    }

    #[test]
    fn adjacent_text_blocks_normalize_to_one_wire_string() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("one".to_owned()),
                ContentBlock::Text(" two".to_owned()),
                ContentBlock::Text("\nthree".to_owned()),
            ],
        };

        let wire = match WireMessage::try_from(&message) {
            Ok(message) => message,
            Err(error) => panic!("expected adjacent text to convert: {error}"),
        };

        assert_eq!(wire.content.as_deref(), Some("one two\nthree"));
        assert!(wire.tool_calls.is_none());

        let round_tripped = match Message::try_from(wire) {
            Ok(message) => message,
            Err(error) => panic!("expected wire text to convert: {error}"),
        };

        assert!(matches!(
            round_tripped.content.as_slice(),
            [ContentBlock::Text(text)] if text == "one two\nthree"
        ));
    }
}
