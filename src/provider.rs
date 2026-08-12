//! Provider adapter boundary.
//!
//! Provider wire formats remain private below this module. The rest of Xana
//! exchanges only internal messages, tool definitions, and structured errors.

use crate::{identity::StepId, message::Message, tool::ToolDefinition};
use futures::future::BoxFuture;
use std::{error::Error, fmt};

pub(crate) mod anthropic;
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
}

pub(crate) trait DeltaSink: Send + Sync {
    fn text_delta(&self, step_id: StepId, text: &str);

    fn usage(&self, _usage: ProviderUsage) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ProviderError {
    message: String,
}

impl ProviderError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ProviderError {}
