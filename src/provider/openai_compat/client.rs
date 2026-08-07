//! Asynchronous streaming HTTP transport and structured adapter errors.

use super::{
    convert::MessageConversionError,
    stream::{SseDecoder, SseItem, StreamAccumulator, StreamError},
    wire::{WireChatRequest, WireMessage, WireStreamResponse, WireToolDefinition},
};
use crate::{
    agent::{ChatError, ChatTransport, DeltaSink},
    identity::StepId,
    message::Message,
    tool::ToolDefinition,
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::Client;
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenAiCompatErrorKind {
    RequestConversion,
    Transport,
    HttpStatus,
    Stream,
}

#[derive(Debug)]
enum OpenAiCompatErrorSource {
    Conversion(MessageConversionError),
    Http(reqwest::Error),
    Stream(StreamError),
}

#[derive(Debug)]
pub(crate) struct OpenAiCompatError {
    pub(crate) kind: OpenAiCompatErrorKind,
    endpoint: String,
    source: OpenAiCompatErrorSource,
}

impl OpenAiCompatError {
    fn conversion(endpoint: &str, source: MessageConversionError) -> Self {
        Self {
            kind: OpenAiCompatErrorKind::RequestConversion,
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

    fn stream(endpoint: &str, source: StreamError) -> Self {
        Self {
            kind: OpenAiCompatErrorKind::Stream,
            endpoint: endpoint.to_owned(),
            source: OpenAiCompatErrorSource::Stream(source),
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
            OpenAiCompatErrorKind::Stream => {
                write!(
                    f,
                    "chat service returned an invalid stream from {}",
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
            OpenAiCompatErrorSource::Stream(source) => Some(source),
        }
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

    async fn stream_message_inner(
        &self,
        messages: &[Message],
        tools: &[&ToolDefinition],
        step_id: StepId,
        deltas: &dyn DeltaSink,
    ) -> Result<Message, OpenAiCompatError> {
        let wire_messages = messages
            .iter()
            .map(WireMessage::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| OpenAiCompatError::conversion(&self.endpoint, source))?;

        let request = WireChatRequest {
            model: &self.model,
            messages: wire_messages,
            stream: true,
            tools: tools
                .iter()
                .map(|definition| WireToolDefinition::from(*definition))
                .collect(),
        };

        let response = self
            .client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|source| {
                OpenAiCompatError::http(OpenAiCompatErrorKind::Transport, &self.endpoint, source)
            })?
            .error_for_status()
            .map_err(|source| {
                OpenAiCompatError::http(OpenAiCompatErrorKind::HttpStatus, &self.endpoint, source)
            })?;

        let mut bytes = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut accumulator = StreamAccumulator::default();
        let mut done = false;

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|source| {
                OpenAiCompatError::http(OpenAiCompatErrorKind::Transport, &self.endpoint, source)
            })?;
            for item in decoder
                .push(&chunk)
                .map_err(|source| OpenAiCompatError::stream(&self.endpoint, source))?
            {
                match item {
                    SseItem::Done => {
                        done = true;
                        break;
                    }
                    SseItem::Data(data) => {
                        let response: WireStreamResponse =
                            serde_json::from_slice(&data).map_err(|source| {
                                OpenAiCompatError::stream(
                                    &self.endpoint,
                                    StreamError::InvalidJson(source),
                                )
                            })?;
                        let choice = response.choices.into_iter().next().ok_or_else(|| {
                            OpenAiCompatError::stream(&self.endpoint, StreamError::MissingChoice)
                        })?;
                        for fragment in accumulator
                            .apply(choice.delta)
                            .map_err(|source| OpenAiCompatError::stream(&self.endpoint, source))?
                        {
                            deltas.text_delta(step_id, &fragment);
                        }
                    }
                }
            }
            if done {
                break;
            }
        }

        if !done {
            decoder
                .finish()
                .map_err(|source| OpenAiCompatError::stream(&self.endpoint, source))?;
        }
        accumulator
            .finish()
            .map_err(|source| OpenAiCompatError::stream(&self.endpoint, source))
    }

    #[cfg(test)]
    pub(crate) async fn send_message(
        &self,
        messages: &[Message],
        tools: &[&ToolDefinition],
    ) -> Result<Message, OpenAiCompatError> {
        struct IgnoreDeltas;
        impl DeltaSink for IgnoreDeltas {
            fn text_delta(&self, _step_id: StepId, _text: &str) {}
        }

        self.stream_message_inner(messages, tools, StepId::new(), &IgnoreDeltas)
            .await
    }
}

impl ChatTransport for OpenAiCompatClient {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ChatError>> {
        Box::pin(async move {
            self.stream_message_inner(messages, tools, step_id, deltas)
                .await
                .map_err(|error| ChatError::new(error.to_string()))
        })
    }
}

pub(super) fn chat_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}
