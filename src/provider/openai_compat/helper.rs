//! Explicit Chat Completions helper dialects; never inferred from a URL/model.

use crate::provider::{HelperCapabilities, HelperGenerationPolicy};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum HelperDialect {
    #[default]
    Generic,
    Ollama,
    OpenAi,
    OpenRouter,
}

impl HelperDialect {
    pub(super) fn capabilities(self) -> HelperCapabilities {
        HelperCapabilities {
            output_limit: true,
            structured_output: self != Self::Generic,
            // OpenAI/OpenRouter reasoning controls depend on per-model metadata
            // that this adapter does not own. Do not guess from model IDs.
            disable_reasoning: self == Self::Ollama,
            zero_temperature: self == Self::Ollama,
        }
    }

    pub(super) fn wire_options(self, policy: HelperGenerationPolicy<'_>) -> WireHelperOptions<'_> {
        // Callers validate capabilities before constructing a request.
        // Official Chat APIs: OpenAI uses max_completion_tokens (visible plus
        // reasoning); Ollama/OpenRouter/generic Chat compatibility uses max_tokens.
        // https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create
        // https://docs.ollama.com/api/openai-compatibility
        // https://openrouter.ai/docs/api_reference/parameters
        WireHelperOptions {
            max_completion_tokens: (self == Self::OpenAi).then_some(policy.max_output_tokens),
            max_tokens: (self != Self::OpenAi).then_some(policy.max_output_tokens),
            response_format: policy.json_schema.map(|schema| WireResponseFormat {
                kind: "json_schema",
                json_schema: WireJsonSchema {
                    name: "xana_helper",
                    strict: true,
                    schema,
                },
            }),
            reasoning_effort: policy.disable_reasoning.then_some("none"),
            temperature: policy.zero_temperature.then_some(0),
            // OpenRouter otherwise permits routing to providers that ignore
            // unsupported parameters. Keep cap/schema requests fail-closed.
            // https://openrouter.ai/docs/guides/routing/provider-selection
            provider: (self == Self::OpenRouter).then_some(WireProviderPreferences {
                require_parameters: true,
            }),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub(super) struct WireHelperOptions<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<WireResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<WireProviderPreferences>,
}

#[derive(Debug, Serialize)]
struct WireResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    json_schema: WireJsonSchema<'a>,
}

#[derive(Debug, Serialize)]
struct WireJsonSchema<'a> {
    name: &'static str,
    strict: bool,
    schema: &'a serde_json::Value,
}

#[derive(Debug, Serialize)]
struct WireProviderPreferences {
    require_parameters: bool,
}
