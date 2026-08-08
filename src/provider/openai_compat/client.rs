//! Asynchronous streaming HTTP transport and structured adapter errors.

use super::{
    convert::{MessageConversionError, convert_message},
    stream::{SseDecoder, SseItem, StreamAccumulator, StreamError},
    wire::{WireChatRequest, WireMessage, WireStreamResponse, WireToolDefinition},
};
use crate::{
    credential::SecretString,
    identity::StepId,
    message::Message,
    provider::{ConversationalProvider, DeltaSink, ProviderError},
    tool::ToolDefinition,
    vision::MediaResolver,
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
    bearer_token: Option<SecretString>,
    attribution: Vec<(String, String)>,
    media: Option<MediaResolver>,
}

impl OpenAiCompatClient {
    pub(crate) fn new(base_url: String, model: String) -> Self {
        let endpoint = chat_endpoint(&base_url);

        Self {
            client: Client::new(),
            endpoint,
            model,
            bearer_token: None,
            attribution: Vec::new(),
            media: None,
        }
    }

    pub(crate) fn with_bearer_and_attribution(
        base_url: String,
        model: String,
        bearer_token: SecretString,
        referer: Option<String>,
        title: Option<String>,
    ) -> Self {
        let mut client = Self::new(base_url, model);
        client.bearer_token = Some(bearer_token);
        if let Some(referer) = referer {
            client
                .attribution
                .push(("HTTP-Referer".to_owned(), referer));
        }
        if let Some(title) = title {
            client.attribution.push(("X-Title".to_owned(), title));
        }
        client
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
        messages: &[Message],
        tools: &[&ToolDefinition],
        max_output_tokens: Option<usize>,
        step_id: StepId,
        deltas: &dyn DeltaSink,
    ) -> Result<Message, OpenAiCompatError> {
        let wire_messages = messages
            .iter()
            .map(|message| {
                if message
                    .content
                    .iter()
                    .any(|block| matches!(block, crate::message::ContentBlock::Image(_)))
                {
                    convert_message(message, self.media.as_ref())
                } else {
                    WireMessage::try_from(message)
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| OpenAiCompatError::conversion(&self.endpoint, source))?;

        let request = WireChatRequest {
            model: &self.model,
            messages: wire_messages,
            stream: true,
            max_output_tokens,
            tools: tools
                .iter()
                .map(|definition| WireToolDefinition::from(*definition))
                .collect(),
        };

        let mut builder = self.client.post(&self.endpoint);
        if let Some(token) = &self.bearer_token {
            builder = builder.bearer_auth(token.expose());
        }
        for (name, value) in &self.attribution {
            builder = builder.header(name, value);
        }
        let response = builder
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

        decoder
            .finish()
            .map_err(|source| OpenAiCompatError::stream(&self.endpoint, source))?;
        if !done {
            return Err(OpenAiCompatError::stream(
                &self.endpoint,
                StreamError::MissingDone,
            ));
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

        self.stream_message_inner(messages, tools, None, StepId::new(), &IgnoreDeltas)
            .await
    }
}

impl ConversationalProvider for OpenAiCompatClient {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            self.stream_message_inner(messages, tools, None, step_id, deltas)
                .await
                .map_err(|error| ProviderError::new(error.to_string()))
        })
    }
}

pub(super) fn chat_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}
