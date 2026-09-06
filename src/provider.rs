//! Provider adapter boundary.
//!
//! Provider wire formats remain private below this module. The rest of Xana
//! exchanges only internal messages, tool definitions, and structured errors.

use crate::{identity::StepId, message::Message, tool::ToolDefinition};
use futures::future::BoxFuture;
use std::{error::Error, fmt};

pub(crate) mod anthropic;
pub(crate) mod helper;
pub(crate) mod openai_compat;

/// The provider-neutral generation boundary used by Xana's native agent loop.
///
/// Account management, model discovery, credential storage, and managed agent
/// runtimes deliberately do not belong on this contract.
pub(crate) trait ConversationalProvider: Send + Sync {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>>;

    fn helper_capabilities(&self) -> HelperCapabilities {
        HelperCapabilities::default()
    }

    /// A separate no-tool request contract. An adapter must implement the wire
    /// policy explicitly; falling back to ordinary chat would discard limits.
    fn stream_helper_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _policy: HelperGenerationPolicy<'a>,
        _step_id: StepId,
        _deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async {
            Err(ProviderError::classified(
                ProviderErrorKind::Request,
                "provider does not implement bounded helper generation",
            ))
        })
    }
}

/// Expressible adapter options, not a guarantee that every model accepts them.
/// Unsupported model/parameter responses must propagate without downgrading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HelperCapabilities {
    pub(crate) output_limit: bool,
    pub(crate) structured_output: bool,
    pub(crate) disable_reasoning: bool,
    pub(crate) zero_temperature: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct HelperGenerationPolicy<'a> {
    /// Wire generation ceiling, separate from source-window and byte budgets.
    pub(crate) max_output_tokens: usize,
    pub(crate) json_schema: Option<&'a serde_json::Value>,
    /// Request the adapter's documented disable control, never hide reasoning.
    pub(crate) disable_reasoning: bool,
    /// Request zero-temperature sampling; this does not guarantee determinism.
    pub(crate) zero_temperature: bool,
}

impl HelperGenerationPolicy<'_> {
    pub(crate) fn validate(self, capabilities: HelperCapabilities) -> Result<(), ProviderError> {
        let reason = if self.max_output_tokens == 0 {
            Some("helper output-token limit must be positive")
        } else if !capabilities.output_limit {
            Some("provider does not support a helper output-token limit")
        } else if self.json_schema.is_some() && !capabilities.structured_output {
            Some("provider does not support helper structured output")
        } else if self.json_schema.is_some_and(|schema| !schema.is_object()) {
            Some("helper JSON schema must be an object")
        } else if self.disable_reasoning && !capabilities.disable_reasoning {
            Some("provider does not support disabling helper reasoning")
        } else if self.zero_temperature && !capabilities.zero_temperature {
            Some("provider does not support zero-temperature helper sampling")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(ProviderError::classified(
                ProviderErrorKind::Request,
                reason,
            )),
            None => Ok(()),
        }
    }
}

pub(crate) trait DeltaSink: Send + Sync {
    fn text_delta(&self, step_id: StepId, text: &str);

    fn reasoning_delta(&self, _step_id: StepId, _text: &str) {}

    fn usage(&self, _usage: ProviderUsage) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ProviderUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) cache_write_input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) reasoning_tokens: Option<u64>,
    pub(crate) tool_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) cost_microunits: Option<u64>,
    pub(crate) prompt_bytes: Option<u64>,
    pub(crate) tool_schema_bytes: Option<u64>,
    /// A one-way stable digest of an upstream request/session affinity ID.
    pub(crate) request_affinity: Option<[u8; 16]>,
}

#[derive(Debug)]
pub(crate) struct ProviderError {
    kind: ProviderErrorKind,
    message: String,
    failure: crate::failure::FailureDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderErrorKind {
    Request,
    Transport,
    Rejected,
    InvalidStream,
    Timeout,
    OutputLimit,
    #[cfg(test)]
    Other,
}

impl ProviderError {
    #[cfg(test)]
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self::classified(ProviderErrorKind::Other, message)
    }

    pub(crate) fn classified(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        use crate::failure::{FailureCategory as Category, FailureDetails, FailureStage as Stage};
        let (category, stage) = match kind {
            ProviderErrorKind::Request => (Category::InvalidRequest, Stage::RequestPreparation),
            ProviderErrorKind::Transport => (Category::Transport, Stage::ProviderConnect),
            ProviderErrorKind::Rejected => (Category::ProviderRejected, Stage::ProviderResponse),
            ProviderErrorKind::InvalidStream => (Category::BrokenStream, Stage::ProviderStream),
            ProviderErrorKind::Timeout => (Category::ReadTimeout, Stage::Unknown),
            ProviderErrorKind::OutputLimit => (Category::OutputLimit, Stage::ProviderStream),
            #[cfg(test)]
            ProviderErrorKind::Other => (Category::Unknown, Stage::Unknown),
        };
        Self {
            kind,
            message: message.into(),
            failure: FailureDetails::new(category, stage),
        }
    }

    pub(crate) fn with_failure(mut self, failure: crate::failure::FailureDetails) -> Self {
        self.failure = failure;
        self
    }

    pub(crate) fn failure(&self) -> crate::failure::FailureDetails {
        self.failure
    }

    pub(crate) fn kind(&self) -> ProviderErrorKind {
        self.kind
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ProviderError {}

#[cfg(test)]
mod helper_contract_tests;
