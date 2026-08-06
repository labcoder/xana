//! Blocking HTTP transport and structured adapter errors.

use super::{
    convert::MessageConversionError,
    wire::{WireChatRequest, WireChatResponse, WireMessage, WireToolDefinition},
};
use crate::{message::Message, tool::ToolDefinition};
use reqwest::blocking::Client;
use std::{error::Error, fmt};

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

    pub(crate) fn send_message(
        &self,
        messages: &[Message],
        tools: &[&ToolDefinition],
    ) -> Result<Message, OpenAiCompatError> {
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

pub(super) fn chat_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}
