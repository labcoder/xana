//! Helper-only Messages request controls; ordinary chat keeps its existing body.

use super::{AnthropicConversionError, WireRequest, convert_messages};
use crate::{
    message::Message,
    provider::{HelperCapabilities, HelperGenerationPolicy},
    vision::MediaResolver,
};
use serde::Serialize;
use serde_json::Value;

pub(super) const CAPABILITIES: HelperCapabilities = HelperCapabilities {
    output_limit: true,
    structured_output: true,
    disable_reasoning: true,
    // Temperature support varies by model; this adapter has no capability
    // metadata that could safely promise the zero-temperature option.
    zero_temperature: false,
};

pub(super) fn request<'a>(
    messages: &[Message],
    model: &'a str,
    policy: HelperGenerationPolicy<'a>,
    media: Option<&MediaResolver>,
) -> Result<WireRequest<'a>, AnthropicConversionError> {
    let mut request = convert_messages(messages, &[], model, policy.max_output_tokens, media)?;
    // Current Messages API, not the retired output_format beta shape:
    // https://platform.claude.com/docs/en/build-with-claude/structured-outputs
    // https://platform.claude.com/docs/en/api/http/messages/create
    request.output_config = policy.json_schema.map(|schema| WireOutputConfig {
        format: WireOutputFormat {
            kind: "json_schema",
            schema,
        },
    });
    // Per-model incompatibility remains a server rejection, never a retry with
    // thinking enabled or a model-name guess. No effort setting is injected.
    request.thinking = policy
        .disable_reasoning
        .then_some(WireThinking { kind: "disabled" });
    Ok(request)
}

#[derive(Debug, Serialize)]
pub(super) struct WireOutputConfig<'a> {
    format: WireOutputFormat<'a>,
}

#[derive(Debug, Serialize)]
struct WireOutputFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    schema: &'a Value,
}

#[derive(Debug, Serialize)]
pub(super) struct WireThinking {
    #[serde(rename = "type")]
    kind: &'static str,
}
