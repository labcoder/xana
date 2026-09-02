//! Deterministic, inert projections from runtime content into shared semantics.
//!
//! This module does not render markup, fetch links, read artifacts, or invoke a
//! model. It normalizes bounded content and tells a surface which honest
//! fallback tier it can present from capabilities it already owns.

use super::{
    AvailabilityV1, ContentPartV1, FactSourceV1, FreshnessV1, MAX_SAFE_TEXT_BYTES, SemanticCodeV1,
    SemanticError,
    content::{CapabilityFactV1, ResourceOperationV1},
    validate_code, validate_text,
};
use crate::{
    artifact::ArtifactRecord,
    command_catalog::PresentationCapabilities,
    identity::ArtifactId,
    message::{ContentBlock, Message},
    resource::{ResourceKindV1, ResourcePolicyV1, ResourceRefV1, ResourceValidationV1},
    session::CompactionCheckpoint,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};

const TRUNCATION_LABEL: &str = "\n[content truncated at Xana's semantic boundary]";
const MAX_LINK_PREVIEW_URL_BYTES: usize = 4 * 1024;
const MAX_LINK_PREVIEW_TITLE_BYTES: usize = 512;
const MAX_LINK_PREVIEW_SITE_BYTES: usize = 256;
const MAX_LINK_PREVIEW_TEXT_BYTES: usize = 24 * 1024;
const MAX_LINK_PREVIEW_REDIRECTS: usize = 3;
const MAX_LINK_PREVIEW_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinkPreviewCacheStatusV1 {
    FreshNotCached,
}

/// A runtime-resolved, non-executable card for one explicitly requested link.
///
/// The retained text is evidence for the model and a readable fallback for
/// clients. It is never HTML and does not grant navigation or another fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkPreviewCardV1 {
    pub(crate) requested_url: String,
    pub(crate) final_url: String,
    pub(crate) site_name: String,
    pub(crate) title: Option<String>,
    pub(crate) fetched_unix_ms: u64,
    pub(crate) media_type: String,
    pub(crate) response_bytes: u64,
    pub(crate) content_digest: String,
    pub(crate) redirects: Vec<String>,
    pub(crate) text: String,
    pub(crate) text_truncated: bool,
    pub(crate) untrusted: bool,
    pub(crate) cache_status: LinkPreviewCacheStatusV1,
    pub(crate) artifact: Option<ArtifactRecord>,
}

impl LinkPreviewCardV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        validate_preview_url("requested link-preview URL", &self.requested_url)?;
        validate_preview_url("resolved link-preview URL", &self.final_url)?;
        for redirect in &self.redirects {
            validate_preview_url("link-preview redirect URL", redirect)?;
        }
        if self.redirects.len() > MAX_LINK_PREVIEW_REDIRECTS {
            return Err(SemanticError::TooManyValues {
                field: "link-preview redirects",
                actual: self.redirects.len(),
                limit: MAX_LINK_PREVIEW_REDIRECTS,
            });
        }
        validate_text(
            "link-preview site name",
            &self.site_name,
            MAX_LINK_PREVIEW_SITE_BYTES,
        )?;
        if self.site_name.trim().is_empty() {
            return Err(SemanticError::InvalidStructure {
                field: "link-preview site name",
                reason: "must not be blank",
            });
        }
        if let Some(title) = &self.title {
            validate_text("link-preview title", title, MAX_LINK_PREVIEW_TITLE_BYTES)?;
        }
        validate_text("link-preview text", &self.text, MAX_LINK_PREVIEW_TEXT_BYTES)?;
        validate_code("link-preview media type", &self.media_type, 256)?;
        if self.response_bytes > MAX_LINK_PREVIEW_RESPONSE_BYTES {
            return Err(SemanticError::InvalidStructure {
                field: "link-preview response bytes",
                reason: "exceeds the compiled semantic ceiling",
            });
        }
        if self.content_digest.len() != 64
            || !self
                .content_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(SemanticError::InvalidStructure {
                field: "link-preview content digest",
                reason: "must be a 64-character hexadecimal digest",
            });
        }
        if !self.untrusted {
            return Err(SemanticError::InvalidStructure {
                field: "link-preview trust",
                reason: "remote preview content must remain untrusted",
            });
        }
        if let Some(artifact) = &self.artifact
            && artifact.byte_len != self.response_bytes
        {
            return Err(SemanticError::InvalidStructure {
                field: "link-preview artifact",
                reason: "must retain the complete resolved response",
            });
        }
        Ok(())
    }
}

fn validate_preview_url(field: &'static str, value: &str) -> Result<(), SemanticError> {
    validate_text(field, value, MAX_LINK_PREVIEW_URL_BYTES)?;
    let url = Url::parse(value).map_err(|_| SemanticError::InvalidStructure {
        field,
        reason: "must be an absolute HTTP or HTTPS URL",
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(SemanticError::InvalidStructure {
            field,
            reason: "must be HTTP(S) without credentials or a fragment",
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContentProjectionTierV1 {
    Rich,
    Text,
    Metadata,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ContentActionV1 {
    PreviewLink { url: String },
    OpenLink { url: String },
    InspectArtifact { artifact_id: ArtifactId },
    CopyArtifactReference { artifact_id: ArtifactId },
    SaveArtifact { artifact_id: ArtifactId },
    RevealArtifact { artifact_id: ArtifactId },
    OpenArtifact { artifact_id: ArtifactId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentProjectionV1 {
    pub(crate) tier: ContentProjectionTierV1,
    pub(crate) outcome: SemanticCodeV1,
    /// Sanitized, bounded text that remains useful when specialized rendering
    /// is unavailable. This is never executable markup.
    pub(crate) fallback_text: String,
    pub(crate) actions: Vec<ContentActionV1>,
}

/// Convert one provider-neutral runtime message into bounded semantic parts.
///
/// Text is recognized only when the entire block unambiguously represents one
/// fenced block, display-math block, pipe table, or Markdown link. Mixed or
/// malformed input remains inert Markdown/text for a renderer to parse safely.
pub(crate) fn normalize_message(message: &Message) -> Vec<ContentPartV1> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => normalize_text(text),
            ContentBlock::Image(image) => Some(ContentPartV1::Resource(Box::new(
                ResourceRefV1::from(image),
            ))),
            ContentBlock::ToolCall(call) => Some(ContentPartV1::Text {
                text: sanitize_and_bound(&format!("[tool call: {}]", call.name)),
            }),
            ContentBlock::ToolResult(result) => Some(ContentPartV1::Code {
                language: Some("text".to_owned()),
                code: sanitize_and_bound(&result.output),
            }),
        })
        .collect()
}

fn normalize_text(source: &str) -> Option<ContentPartV1> {
    let source = sanitize_and_bound(source);
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((language, code)) = fenced_block(trimmed) {
        return Some(if language.as_deref() == Some("diff") {
            ContentPartV1::Diff { patch: code }
        } else {
            ContentPartV1::Code { language, code }
        });
    }
    if let Some(math) = trimmed
        .strip_prefix("$$")
        .and_then(|value| value.strip_suffix("$$"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(ContentPartV1::Math {
            source: math.to_owned(),
            display: true,
        });
    }
    if let Some((columns, rows)) = pipe_table(trimmed) {
        return Some(ContentPartV1::Table { columns, rows });
    }
    if let Some((label, url)) = standalone_markdown_link(trimmed) {
        let link = ContentPartV1::Link { label, url };
        if link.validate().is_ok() {
            return Some(link);
        }
    }
    if looks_like_markdown(trimmed) {
        Some(ContentPartV1::Markdown { source })
    } else {
        Some(ContentPartV1::Text { text: source })
    }
}

fn fenced_block(source: &str) -> Option<(Option<String>, String)> {
    let after_fence = source.strip_prefix("```")?;
    let (language, remainder) = after_fence.split_once('\n')?;
    let code = remainder.strip_suffix("```")?;
    let code = code.strip_suffix('\n').unwrap_or(code);
    let language = language.trim();
    let language = (!language.is_empty()
        && language.len() <= 64
        && language.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'+' | b'#' | b'.' | b'_' | b'-')
        }))
    .then(|| language.to_owned());
    Some((language, code.to_owned()))
}

fn pipe_table(source: &str) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let lines = source.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return None;
    }
    let columns = pipe_cells(lines[0]);
    let separators = pipe_cells(lines[1]);
    if columns.is_empty()
        || columns.len() != separators.len()
        || !separators.iter().all(|cell| {
            let cell = cell.trim_matches(':');
            cell.len() >= 3 && cell.bytes().all(|byte| byte == b'-')
        })
    {
        return None;
    }
    let rows = lines[2..]
        .iter()
        .map(|line| pipe_cells(line))
        .collect::<Vec<_>>();
    if rows.iter().any(|row| row.len() != columns.len()) {
        return None;
    }
    Some((columns, rows))
}

fn pipe_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn standalone_markdown_link(source: &str) -> Option<(String, String)> {
    let after_open = source.strip_prefix('[')?;
    let (label, after_label) = after_open.split_once("](")?;
    let url = after_label.strip_suffix(')')?;
    (!label.trim().is_empty() && !url.trim().is_empty())
        .then(|| (label.trim().to_owned(), url.trim().to_owned()))
}

fn looks_like_markdown(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with(['#', '>', '-', '*'])
            || line.starts_with("1. ")
            || line.contains("**")
            || line.contains('`')
            || line.contains("](")
            || line.contains("<")
    })
}

pub(crate) fn project_content(
    part: &ContentPartV1,
    capabilities: PresentationCapabilities,
) -> ContentProjectionV1 {
    match part {
        ContentPartV1::Text { text } => projection(
            ContentProjectionTierV1::Rich,
            "content.text.rich",
            text.clone(),
            Vec::new(),
        ),
        ContentPartV1::Markdown { source } => projection(
            if capabilities.rich_markdown {
                ContentProjectionTierV1::Rich
            } else {
                ContentProjectionTierV1::Text
            },
            if capabilities.rich_markdown {
                "content.markdown.rich"
            } else {
                "content.markdown.text_fallback"
            },
            source.clone(),
            Vec::new(),
        ),
        ContentPartV1::Code { language, code } => projection(
            if capabilities.rich_markdown {
                ContentProjectionTierV1::Rich
            } else {
                ContentProjectionTierV1::Text
            },
            "content.code.text_fallback",
            language
                .as_ref()
                .map_or_else(|| code.clone(), |language| format!("[{language}]\n{code}")),
            Vec::new(),
        ),
        ContentPartV1::Table { columns, rows } => projection(
            if capabilities.rich_markdown {
                ContentProjectionTierV1::Rich
            } else {
                ContentProjectionTierV1::Text
            },
            "content.table.text_fallback",
            render_table(columns, rows),
            Vec::new(),
        ),
        ContentPartV1::Diff { patch } => projection(
            if capabilities.rich_markdown {
                ContentProjectionTierV1::Rich
            } else {
                ContentProjectionTierV1::Text
            },
            "content.diff.text_fallback",
            patch.clone(),
            Vec::new(),
        ),
        ContentPartV1::Math { source, .. } => projection(
            if capabilities.math {
                ContentProjectionTierV1::Rich
            } else {
                ContentProjectionTierV1::Text
            },
            if capabilities.math {
                "content.math.rich"
            } else {
                "content.math.source_fallback"
            },
            source.clone(),
            Vec::new(),
        ),
        ContentPartV1::Link { label, url } => {
            let mut actions = vec![ContentActionV1::PreviewLink { url: url.clone() }];
            if capabilities.safe_link_open {
                actions.push(ContentActionV1::OpenLink { url: url.clone() });
            }
            projection(
                if capabilities.safe_link_open {
                    ContentProjectionTierV1::Rich
                } else {
                    ContentProjectionTierV1::Text
                },
                "content.link.explicit_actions",
                format!("{label}: {url}"),
                actions,
            )
        }
        ContentPartV1::Resource(resource) => project_resource(resource, capabilities),
        ContentPartV1::Unknown { version, kind, .. } => projection(
            ContentProjectionTierV1::Unsupported,
            "content.unknown.unsupported",
            format!("Unsupported content: {kind} (schema version {version})"),
            Vec::new(),
        ),
    }
}

fn project_resource(
    resource: &ResourceRefV1,
    capabilities: PresentationCapabilities,
) -> ContentProjectionV1 {
    let rich = match resource.kind {
        ResourceKindV1::StaticRaster | ResourceKindV1::AnimatedRaster => capabilities.inline_images,
        ResourceKindV1::Audio | ResourceKindV1::Video => capabilities.inline_audio_video,
        ResourceKindV1::Svg
        | ResourceKindV1::Lottie
        | ResourceKindV1::Binary
        | ResourceKindV1::Unknown(_) => false,
    };
    let id = resource.artifact.reference.id;
    let actions = vec![
        ContentActionV1::InspectArtifact { artifact_id: id },
        ContentActionV1::CopyArtifactReference { artifact_id: id },
        ContentActionV1::SaveArtifact { artifact_id: id },
        ContentActionV1::RevealArtifact { artifact_id: id },
        ContentActionV1::OpenArtifact { artifact_id: id },
    ];
    let declared = resource.media_type.declared.as_deref().unwrap_or("unknown");
    let detected = resource.media_type.detected.as_deref().unwrap_or("unknown");
    let metadata = format!(
        "{} artifact:{} · {} bytes · declared {} · detected {}",
        resource.kind.code(),
        id,
        resource.artifact.byte_len,
        declared,
        detected
    );
    projection(
        if rich {
            ContentProjectionTierV1::Rich
        } else {
            ContentProjectionTierV1::Metadata
        },
        if rich {
            "content.resource.rich"
        } else {
            "content.resource.metadata_fallback"
        },
        metadata,
        actions,
    )
}

#[derive(Debug, Clone)]
pub(crate) struct ResourceCapabilityContextV1 {
    pub(crate) presentation: PresentationCapabilities,
    pub(crate) policy: ResourcePolicyV1,
    pub(crate) observed_at_unix_millis: u64,
    /// Exact connection/model/route facts supplied by their authoritative
    /// adapters. Absence is unsupported/unknown, never inferred from media.
    pub(crate) exact_route_facts: Vec<CapabilityFactV1>,
}

pub(crate) fn project_resource_capabilities(
    resource: &ResourceRefV1,
    context: &ResourceCapabilityContextV1,
) -> Result<Vec<CapabilityFactV1>, SemanticError> {
    resource
        .validate()
        .map_err(|error| SemanticError::Resource(error.to_string()))?;
    context
        .policy
        .validate()
        .map_err(|error| SemanticError::Resource(error.to_string()))?;
    let max_source_bytes = context.policy.max_source_bytes_for(&resource.kind);
    let admitted = resource.artifact.byte_len <= max_source_bytes
        && matches!(resource.validation, ResourceValidationV1::Accepted);
    let freshness = FreshnessV1 {
        observed_at_unix_millis: context.observed_at_unix_millis,
        max_age_millis: None,
    };
    let local = |operation, availability, authorized, reason_code: Option<&str>| CapabilityFactV1 {
        operation,
        availability,
        selected: false,
        authorized,
        connection: None,
        model: None,
        effective_max_source_bytes: Some(max_source_bytes),
        reason_code: reason_code.map(str::to_owned),
        source: FactSourceV1::Runtime,
        freshness: freshness.clone(),
    };
    let presentation_available = admitted
        && match resource.kind {
            ResourceKindV1::StaticRaster | ResourceKindV1::AnimatedRaster => {
                context.presentation.inline_images
            }
            ResourceKindV1::Audio | ResourceKindV1::Video => {
                context.presentation.inline_audio_video
            }
            ResourceKindV1::Svg
            | ResourceKindV1::Lottie
            | ResourceKindV1::Binary
            | ResourceKindV1::Unknown(_) => false,
        };
    let playback_available = admitted
        && matches!(resource.kind, ResourceKindV1::Audio | ResourceKindV1::Video)
        && context.presentation.inline_audio_video;
    let mut facts = vec![
        local(
            ResourceOperationV1::Acquire,
            if admitted {
                AvailabilityV1::Available
            } else {
                AvailabilityV1::Unavailable {
                    code: "resource.admission_rejected".into(),
                }
            },
            admitted,
            (!admitted).then_some("resource.admission_rejected"),
        ),
        local(
            ResourceOperationV1::PresentInline,
            if presentation_available {
                AvailabilityV1::Available
            } else {
                AvailabilityV1::Unsupported
            },
            presentation_available,
            (!presentation_available).then_some("resource.presentation_fallback"),
        ),
        local(
            ResourceOperationV1::Playback,
            if playback_available {
                AvailabilityV1::Available
            } else {
                AvailabilityV1::Unsupported
            },
            playback_available,
            (!playback_available).then_some("resource.playback_unavailable"),
        ),
        local(
            ResourceOperationV1::OpenExternal,
            AvailabilityV1::PermissionRequired {
                code: "resource.external_open_requires_permission".into(),
            },
            false,
            Some("resource.external_open_requires_permission"),
        ),
    ];
    for operation in [
        ResourceOperationV1::ProviderInput,
        ResourceOperationV1::FocusedAnalysis,
        ResourceOperationV1::Transform,
    ] {
        let matching = context
            .exact_route_facts
            .iter()
            .filter(|fact| fact.operation == operation)
            .collect::<Vec<_>>();
        if matching.len() > 1 {
            return Err(SemanticError::InvalidStructure {
                field: "resource route capability",
                reason: "must contain at most one exact fact per operation",
            });
        }
        if let Some(fact) = matching.first() {
            fact.validate()?;
            facts.push((*fact).clone());
        } else {
            facts.push(local(
                operation,
                AvailabilityV1::Unsupported,
                false,
                Some("resource.route_capability_unobserved"),
            ));
        }
    }
    facts.sort_by_key(|fact| fact.operation);
    Ok(facts)
}

fn projection(
    tier: ContentProjectionTierV1,
    code: &str,
    fallback_text: String,
    actions: Vec<ContentActionV1>,
) -> ContentProjectionV1 {
    ContentProjectionV1 {
        tier,
        outcome: SemanticCodeV1::new(code),
        fallback_text: sanitize_and_bound(&fallback_text),
        actions,
    }
}

fn render_table(columns: &[String], rows: &[Vec<String>]) -> String {
    std::iter::once(columns.join(" | "))
        .chain(rows.iter().map(|row| row.join(" | ")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SummarySourceV1 {
    RuntimeFacts,
    Compaction,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SummaryUsageEffectV1 {
    NoModelRequest,
    ExistingProviderRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttributedSummaryV1 {
    pub(crate) text: String,
    pub(crate) source: SummarySourceV1,
    pub(crate) connection: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) freshness: FreshnessV1,
    pub(crate) usage_effect: SummaryUsageEffectV1,
}

impl AttributedSummaryV1 {
    pub(crate) fn from_compaction(
        checkpoint: &CompactionCheckpoint,
        observed_at_unix_millis: u64,
    ) -> Self {
        Self {
            text: sanitize_and_bound(&checkpoint.summary.render()),
            source: SummarySourceV1::Compaction,
            connection: None,
            model: None,
            freshness: FreshnessV1 {
                observed_at_unix_millis,
                max_age_millis: None,
            },
            usage_effect: SummaryUsageEffectV1::NoModelRequest,
        }
    }

    pub(crate) fn provider_existing(
        text: &str,
        connection: &str,
        model: &str,
        freshness: FreshnessV1,
    ) -> Result<Self, SemanticError> {
        let summary = Self {
            text: sanitize_and_bound(text),
            source: SummarySourceV1::Provider,
            connection: Some(sanitize_and_bound(connection)),
            model: Some(sanitize_and_bound(model)),
            freshness,
            usage_effect: SummaryUsageEffectV1::ExistingProviderRequest,
        };
        if summary.text.trim().is_empty() {
            return Err(SemanticError::InvalidStructure {
                field: "summary",
                reason: "must not be blank",
            });
        }
        Ok(summary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FreshRecapRequestV1 {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) outcome: SemanticCodeV1,
}

impl FreshRecapRequestV1 {
    /// Construct explicit intent only. Calling this never performs model I/O.
    pub(crate) fn new(connection: &str, model: &str) -> Result<Self, SemanticError> {
        let connection = sanitize_and_bound(connection);
        let model = sanitize_and_bound(model);
        if connection.trim().is_empty() || model.trim().is_empty() {
            return Err(SemanticError::InvalidStructure {
                field: "fresh recap route",
                reason: "must name an exact connection and model",
            });
        }
        Ok(Self {
            connection,
            model,
            outcome: SemanticCodeV1::new("summary.fresh_recap.requires_model_request"),
        })
    }
}

fn sanitize_and_bound(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut safe = normalized
        .chars()
        .filter(|character| {
            matches!(character, '\n' | '\t')
                || (!character.is_control() && !is_bidi_control(*character))
        })
        .collect::<String>();
    if safe.len() <= MAX_SAFE_TEXT_BYTES {
        return safe;
    }
    let limit = MAX_SAFE_TEXT_BYTES.saturating_sub(TRUNCATION_LABEL.len());
    let mut end = limit;
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    safe.truncate(end);
    safe.push_str(TRUNCATION_LABEL);
    safe
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests;
