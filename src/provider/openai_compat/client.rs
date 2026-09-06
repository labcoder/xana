//! Asynchronous streaming HTTP transport and structured adapter errors.

use super::{
    convert::{MessageConversionError, convert_message},
    helper::HelperDialect,
    stream::{SseDecoder, SseItem, StreamAccumulator, StreamError},
    wire::{
        WireChatRequest, WireMessage, WireStreamOptions, WireStreamResponse, WireToolDefinition,
    },
};
use crate::{
    credential::SecretString,
    identity::StepId,
    message::Message,
    provider::{
        ConversationalProvider, DeltaSink, HelperCapabilities, HelperGenerationPolicy,
        ProviderError, ProviderErrorKind, ProviderUsage,
    },
    tool::ToolDefinition,
    vision::MediaResolver,
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::Client;
use std::{error::Error, fmt, time::Duration};

const RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(120);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenAiCompatErrorKind {
    RequestConversion,
    Transport,
    HttpStatus,
    Stream,
    Timeout,
    OutputLimit,
}

#[derive(Debug)]
enum OpenAiCompatErrorSource {
    Conversion(MessageConversionError),
    Http(reqwest::Error),
    Stream(StreamError),
    Timeout(&'static str),
    OutputLimit,
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

    fn timeout(endpoint: &str, phase: &'static str) -> Self {
        Self {
            kind: OpenAiCompatErrorKind::Timeout,
            endpoint: endpoint.to_owned(),
            source: OpenAiCompatErrorSource::Timeout(phase),
        }
    }

    fn detail(&self) -> String {
        match &self.source {
            OpenAiCompatErrorSource::Conversion(source) => source.to_string(),
            OpenAiCompatErrorSource::Http(source) => source.to_string(),
            OpenAiCompatErrorSource::Stream(source) => source.to_string(),
            OpenAiCompatErrorSource::Timeout(phase) => format!("timed out during {phase}"),
            OpenAiCompatErrorSource::OutputLimit => "helper reached its output-token limit".into(),
        }
    }
}

impl fmt::Display for OpenAiCompatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            OpenAiCompatErrorKind::RequestConversion => {
                write!(
                    f,
                    "could not encode chat request for {}: {}",
                    self.endpoint,
                    self.detail()
                )
            }
            OpenAiCompatErrorKind::Transport => {
                write!(
                    f,
                    "could not reach chat service at {}: {}",
                    self.endpoint,
                    self.detail()
                )
            }
            OpenAiCompatErrorKind::HttpStatus => {
                write!(
                    f,
                    "chat service rejected request at {}: {}",
                    self.endpoint,
                    self.detail()
                )
            }
            OpenAiCompatErrorKind::Stream => {
                write!(
                    f,
                    "chat service returned an invalid stream from {}: {}",
                    self.endpoint,
                    self.detail()
                )
            }
            OpenAiCompatErrorKind::Timeout => write!(
                f,
                "chat service timed out at {}: {}",
                self.endpoint,
                self.detail()
            ),
            OpenAiCompatErrorKind::OutputLimit => {
                f.write_str("helper reached its output-token limit")
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
            OpenAiCompatErrorSource::Timeout(_) | OpenAiCompatErrorSource::OutputLimit => None,
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
    include_usage: bool,
    helper_dialect: HelperDialect,
}

impl OpenAiCompatClient {
    pub(crate) fn new(base_url: String, model: String) -> Self {
        Self::new_with_redirect_policy(base_url, model, reqwest::redirect::Policy::limited(10))
    }

    fn new_with_redirect_policy(
        base_url: String,
        model: String,
        redirect_policy: reqwest::redirect::Policy,
    ) -> Self {
        let endpoint = chat_endpoint(&base_url);

        Self {
            client: crate::http_client::builder()
                .connect_timeout(Duration::from_secs(5))
                .redirect(redirect_policy)
                .build()
                .expect("static HTTP client configuration is valid"),
            endpoint,
            model,
            bearer_token: None,
            attribution: Vec::new(),
            media: None,
            include_usage: false,
            helper_dialect: HelperDialect::Generic,
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

    /// Constructs a credential-bearing client whose request destination cannot
    /// be changed by an HTTP redirect. Focused services use this after Xana has
    /// authorized one exact recipient through the outbound guard.
    pub(crate) fn with_bearer_and_attribution_no_redirects(
        base_url: String,
        model: String,
        bearer_token: SecretString,
        referer: Option<String>,
        title: Option<String>,
    ) -> Self {
        let mut client =
            Self::new_with_redirect_policy(base_url, model, reqwest::redirect::Policy::none());
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

    pub(crate) fn with_usage(mut self) -> Self {
        self.include_usage = true;
        self
    }

    pub(crate) fn with_helper_dialect(mut self, dialect: HelperDialect) -> Self {
        self.helper_dialect = dialect;
        self
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn stream_message_inner(
        &self,
        messages: &[Message],
        tools: &[&ToolDefinition],
        helper: Option<HelperGenerationPolicy<'_>>,
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
            stream_options: (self.include_usage || helper.is_some()).then_some(WireStreamOptions {
                include_usage: true,
            }),
            helper: helper
                .map(|policy| self.helper_dialect.wire_options(policy))
                .unwrap_or_default(),
            tools: tools
                .iter()
                .map(|definition| WireToolDefinition::from(*definition))
                .collect(),
        };
        let prompt_bytes = encoded_len(&request.messages);
        let tool_schema_bytes = encoded_len(&request.tools);

        let mut builder = self.client.post(&self.endpoint);
        if let Some(token) = &self.bearer_token {
            builder = builder.bearer_auth(token.expose());
        }
        for (name, value) in &self.attribution {
            builder = builder.header(name, value);
        }
        let response = tokio::time::timeout(RESPONSE_START_TIMEOUT, builder.json(&request).send())
            .await
            .map_err(|_| OpenAiCompatError::timeout(&self.endpoint, "response start"))?
            .map_err(|source| {
                OpenAiCompatError::http(OpenAiCompatErrorKind::Transport, &self.endpoint, source)
            })?
            .error_for_status()
            .map_err(|source| {
                OpenAiCompatError::http(OpenAiCompatErrorKind::HttpStatus, &self.endpoint, source)
            })?;
        let request_affinity = request_affinity(&response);

        let mut bytes = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut accumulator = StreamAccumulator::default();
        let mut done = false;
        let mut output_limited = false;
        let mut unexpected_reasoning = false;

        while let Some(chunk) = tokio::time::timeout(STREAM_IDLE_TIMEOUT, bytes.next())
            .await
            .map_err(|_| OpenAiCompatError::timeout(&self.endpoint, "stream idle"))?
        {
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
                        let has_usage = response.usage.is_some();
                        if let Some(usage) = response.usage {
                            let prompt_details = usage.prompt_tokens_details.unwrap_or_default();
                            let completion_details =
                                usage.completion_tokens_details.unwrap_or_default();
                            deltas.usage(ProviderUsage {
                                input_tokens: usage.prompt_tokens,
                                cached_input_tokens: prompt_details.cached_tokens,
                                cache_write_input_tokens: prompt_details.cache_write_tokens,
                                output_tokens: usage.completion_tokens,
                                reasoning_tokens: completion_details.reasoning_tokens,
                                tool_tokens: None,
                                total_tokens: usage.total_tokens,
                                cost_microunits: usage.cost.and_then(usd_microunits),
                                prompt_bytes,
                                tool_schema_bytes,
                                request_affinity,
                            });
                        }
                        let Some(choice) = response.choices.into_iter().next() else {
                            if has_usage {
                                continue;
                            }
                            return Err(OpenAiCompatError::stream(
                                &self.endpoint,
                                StreamError::MissingChoice,
                            ));
                        };
                        output_limited |= choice.finish_reason.as_deref() == Some("length");
                        let mut delta = choice.delta;
                        if let Some(reasoning) =
                            delta.reasoning.take().filter(|text| !text.is_empty())
                        {
                            unexpected_reasoning |=
                                helper.is_some_and(|policy| policy.disable_reasoning);
                            deltas.reasoning_delta(step_id, &reasoning);
                        }
                        for fragment in accumulator
                            .apply(delta)
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
        // Read through the terminal usage event before rejecting truncation.
        // Even syntactically valid JSON at the cap is not a completed helper.
        if helper.is_some() && output_limited {
            return Err(OpenAiCompatError {
                kind: OpenAiCompatErrorKind::OutputLimit,
                endpoint: self.endpoint.clone(),
                source: OpenAiCompatErrorSource::OutputLimit,
            });
        }
        if unexpected_reasoning {
            return Err(OpenAiCompatError::stream(
                &self.endpoint,
                StreamError::UnexpectedHelperReasoning,
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

fn encoded_len(value: &impl serde::Serialize) -> Option<u64> {
    serde_json::to_vec(value)
        .ok()
        .and_then(|encoded| u64::try_from(encoded.len()).ok())
}

fn request_affinity(response: &reqwest::Response) -> Option<[u8; 16]> {
    ["x-request-id", "x-openrouter-generation-id"]
        .into_iter()
        .find_map(|name| response.headers().get(name))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(|value| {
            let digest = blake3::hash(value.as_bytes());
            let mut truncated = [0_u8; 16];
            truncated.copy_from_slice(&digest.as_bytes()[..16]);
            truncated
        })
}

fn usd_microunits(value: f64) -> Option<u64> {
    let scaled = value * 1_000_000.0;
    (scaled.is_finite() && scaled >= 0.0 && scaled <= u64::MAX as f64)
        .then(|| scaled.round() as u64)
}

impl ConversationalProvider for OpenAiCompatClient {
    fn helper_capabilities(&self) -> HelperCapabilities {
        self.helper_dialect.capabilities()
    }

    fn stream_helper_message<'a>(
        &'a self,
        messages: &'a [Message],
        policy: HelperGenerationPolicy<'a>,
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            policy.validate(self.helper_capabilities())?;
            self.stream_message_inner(messages, &[], Some(policy), step_id, deltas)
                .await
                .map_err(provider_error)
        })
    }

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
                .map_err(provider_error)
        })
    }
}

fn provider_error(error: OpenAiCompatError) -> ProviderError {
    let kind = match error.kind {
        OpenAiCompatErrorKind::RequestConversion => ProviderErrorKind::Request,
        OpenAiCompatErrorKind::Transport => ProviderErrorKind::Transport,
        OpenAiCompatErrorKind::HttpStatus => ProviderErrorKind::Rejected,
        OpenAiCompatErrorKind::Stream => ProviderErrorKind::InvalidStream,
        OpenAiCompatErrorKind::Timeout => ProviderErrorKind::Timeout,
        OpenAiCompatErrorKind::OutputLimit => ProviderErrorKind::OutputLimit,
    };
    ProviderError::classified(kind, error.to_string())
}

pub(super) fn chat_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}
