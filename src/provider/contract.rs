//! Focused provider contract and connection/model descriptors.
//!
//! Provider wire formats stay private. This module exposes only normalized
//! request/response semantics to the runtime agent and composition root.

#![allow(dead_code)]

use super::openai_compat::OpenAiCompatClient;
use crate::{agent::DeltaSink, identity::StepId, message::Message, tool::ToolDefinition};
use futures::future::BoxFuture;
use std::{fmt, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    Environment(String),
    Stored(String),
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(REDACTED)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderProtocol {
    OpenAiCompatible,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputModality {
    Text,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDescriptor {
    pub id: String,
    pub input_modalities: Vec<InputModality>,
    pub tools: bool,
    pub reasoning: bool,
    pub context_tokens: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionDescriptor {
    pub id: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub credential: Option<CredentialRef>,
    pub models: Vec<ModelDescriptor>,
}

#[derive(Clone, Copy)]
pub struct ProviderRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [&'a ToolDefinition],
    pub max_output_tokens: Option<usize>,
    pub step_id: StepId,
    pub deltas: &'a dyn DeltaSink,
}

pub trait ConversationalProvider: Send + Sync {
    fn descriptor(&self) -> &ConnectionDescriptor;
    fn generate<'a>(
        &'a self,
        request: ProviderRequest<'a>,
    ) -> BoxFuture<'a, Result<Message, ProviderError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub message: String,
    pub retryable: bool,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ProviderError {}

pub struct OpenRouterProvider {
    descriptor: ConnectionDescriptor,
    client: Arc<OpenAiCompatClient>,
}

impl OpenRouterProvider {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, token: SecretValue) -> Self {
        Self::with_attribution(base_url, model, token, None, None)
    }

    pub fn with_attribution(
        base_url: impl Into<String>,
        model: impl Into<String>,
        token: SecretValue,
        referer: Option<String>,
        title: Option<String>,
    ) -> Self {
        let base_url = base_url.into();
        let model = model.into();
        let client = OpenAiCompatClient::with_bearer_and_attribution(
            base_url.clone(),
            model.clone(),
            token.expose().to_owned(),
            referer,
            title,
        );
        Self {
            descriptor: ConnectionDescriptor {
                id: "openrouter".to_owned(),
                protocol: ProviderProtocol::OpenAiCompatible,
                base_url,
                credential: Some(CredentialRef::Stored("openrouter".to_owned())),
                models: vec![ModelDescriptor {
                    id: model,
                    input_modalities: vec![InputModality::Text],
                    tools: true,
                    reasoning: false,
                    context_tokens: None,
                }],
            },
            client: Arc::new(client),
        }
    }
}

impl ConversationalProvider for OpenRouterProvider {
    fn descriptor(&self) -> &ConnectionDescriptor {
        &self.descriptor
    }

    fn generate<'a>(
        &'a self,
        request: ProviderRequest<'a>,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            self.client
                .stream_message_with_options(
                    request.messages,
                    request.tools,
                    request.max_output_tokens,
                    request.step_id,
                    request.deltas,
                )
                .await
                .map_err(|error| ProviderError {
                    message: error.to_string(),
                    retryable: false,
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        assert_eq!(
            format!("{:?}", SecretValue::new("top-secret")),
            "SecretValue(REDACTED)"
        );
    }

    #[test]
    fn openrouter_descriptor_reuses_openai_compatible_protocol() {
        let provider = OpenRouterProvider::new(
            "https://openrouter.ai/api/v1",
            "openai/gpt-4o-mini",
            SecretValue::new("token"),
        );
        assert_eq!(
            provider.descriptor().protocol,
            ProviderProtocol::OpenAiCompatible
        );
        assert_eq!(provider.descriptor().id, "openrouter");
        assert!(provider.descriptor().base_url.ends_with("/api/v1"));
    }
}
