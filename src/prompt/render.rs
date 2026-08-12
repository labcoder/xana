//! Canonical prompt rendering and conservative input-cost estimation.

use super::{PromptLayer, PromptLayerKind};
use crate::{
    context::{SourceOrigin, TrustClass, estimate_tokens},
    message::{ContentBlock, Message, Role, ToolResultStatus},
    tool::ToolDefinition,
};

pub(super) fn estimate_tool_schema_tokens(definitions: &[&ToolDefinition]) -> usize {
    definitions
        .iter()
        .map(|definition| {
            let value = serde_json::json!({
                "type": "function",
                "function": {
                    "name": definition.name,
                    "description": definition.description,
                    "parameters": definition.parameters,
                }
            });
            estimate_tokens(&value.to_string())
        })
        .sum()
}

pub(super) fn estimate_message_tokens(message: &Message) -> usize {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut tokens = estimate_tokens(role);
    for block in &message.content {
        tokens += match block {
            ContentBlock::Text(text) => estimate_tokens(text),
            ContentBlock::Image(image) => estimate_image_tokens(image),
            ContentBlock::ToolCall(call) => {
                estimate_tokens(&call.id)
                    + estimate_tokens(&call.name)
                    + estimate_tokens(&call.arguments.to_string())
            }
            ContentBlock::ToolResult(result) => {
                let status = match result.status {
                    ToolResultStatus::Success => "success",
                    ToolResultStatus::Error => "error",
                };
                estimate_tokens(&result.call_id)
                    + estimate_tokens(status)
                    + estimate_tokens(&result.output)
            }
        };
    }
    tokens
}

fn estimate_image_tokens(image: &crate::vision::ImageRef) -> usize {
    // Vision pricing/tokenization is provider-specific. Reserving roughly one
    // token per 750 pixels is deliberately conservative for current native
    // providers and prevents an image from being counted as a tiny filename-
    // sized placeholder. Byte length is the fallback for legacy metadata.
    let units = image
        .width
        .zip(image.height)
        .map(|(width, height)| u64::from(width).saturating_mul(u64::from(height)))
        .unwrap_or(image.byte_len);
    usize::try_from(units.saturating_add(749) / 750)
        .unwrap_or(usize::MAX)
        .max(256)
}

pub(super) fn refresh_layer_costs(layers: &mut [PromptLayer]) {
    for layer in layers {
        layer.estimated_tokens = estimate_tokens(&render_layer(layer));
    }
}

pub(super) fn render_layers(layers: &[PromptLayer]) -> String {
    layers
        .iter()
        .map(render_layer)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_layer(layer: &PromptLayer) -> String {
    let path = layer
        .provenance
        .path
        .as_ref()
        .map(|path| format!(" path=\"{}\"", escape_attribute(&path.to_string_lossy())))
        .unwrap_or_default();
    format!(
        "<xana-source id=\"{}\" name=\"{}\" kind=\"{}\" trust=\"{}\" origin=\"{}\" truncated=\"{}\"{}>\n{}\n</xana-source>",
        escape_attribute(&layer.source_id),
        escape_attribute(&layer.provenance.display_name),
        layer_kind_name(layer.kind),
        trust_name(layer.trust),
        origin_name(layer.provenance.origin),
        layer.truncated,
        path,
        escape_body(&layer.text)
    )
}

pub(super) fn trim_outer_blank_lines(text: &str) -> String {
    let canonical = crate::context::canonical_text(text);
    let lines = canonical.split('\n').collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(start, |index| index + 1);
    lines[start..end].join("\n")
}

fn escape_body(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attribute(text: &str) -> String {
    escape_body(text)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn layer_kind_name(kind: PromptLayerKind) -> &'static str {
    match kind {
        PromptLayerKind::Identity => "identity",
        PromptLayerKind::Guidelines => "guidelines",
        PromptLayerKind::ProductDocumentation => "product_documentation",
        PromptLayerKind::ToolCatalog => "tool_catalog",
        PromptLayerKind::Environment => "environment",
        PromptLayerKind::Surface => "surface",
        PromptLayerKind::ProjectInstructions => "project_instructions",
        PromptLayerKind::SkillInstructions => "skill_instructions",
    }
}

fn trust_name(trust: TrustClass) -> &'static str {
    match trust {
        TrustClass::Xana => "xana",
        TrustClass::Runtime => "runtime",
        TrustClass::Project => "project",
        TrustClass::Skill => "skill",
    }
}

fn origin_name(origin: SourceOrigin) -> &'static str {
    match origin {
        SourceOrigin::BuiltIn => "built_in",
        SourceOrigin::ToolRegistry => "tool_registry",
        SourceOrigin::RuntimeEnvironment => "runtime_environment",
        SourceOrigin::ProductDocumentation => "product_documentation",
        SourceOrigin::ProjectFile => "project_file",
        SourceOrigin::Skill => "skill",
        SourceOrigin::ParentHandoff => "parent_handoff",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        artifact::{ArtifactRecord, ArtifactRef, ContentHash},
        identity::{ArtifactId, PrincipalId},
        vision::ImageRef,
    };

    #[test]
    fn image_cost_uses_pixels_instead_of_a_tiny_text_placeholder() {
        let image = ImageRef {
            artifact: ArtifactRecord {
                reference: ArtifactRef {
                    id: ArtifactId::new(),
                    content_hash: ContentHash::for_bytes(b"image"),
                },
                media_type: "image/png".into(),
                byte_len: 5,
                owner: PrincipalId::new(),
            },
            media_type: "image/png".into(),
            byte_len: 5,
            width: Some(1500),
            height: Some(1000),
        };

        assert_eq!(estimate_image_tokens(&image), 2000);
    }
}
