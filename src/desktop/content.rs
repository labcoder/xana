//! Bounded rich-content projection and verified Desktop preview access.
//!
//! Presentation code receives semantic values and capability-scoped preview
//! bytes. It never receives artifact paths, hashes, principals, or a general
//! artifact-store handle.

use crate::{
    artifact::{ArtifactRecord, ArtifactStore},
    command_catalog::PresentationCapabilities,
    frontend::semantic::{
        AvailabilityV1, CapabilityFactV1, ContentActionV1, ContentPartV1, ContentProjectionTierV1,
        FactSourceV1, ResourceCapabilityContextV1, ResourceOperationV1, normalize_message,
        project_content, project_resource_capabilities,
    },
    message::{Message, Role},
    resource::{ResourceKindV1, ResourcePolicyV1, ResourceRefV1, ResourceValidationV1},
};
use std::{collections::HashMap, fmt, sync::Arc, time::SystemTime};

const MAX_PUBLIC_TEXT_BYTES: usize = 256 * 1024;

/// Presentation-safe conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopMessage {
    pub id: String,
    pub role: DesktopRole,
    pub content: Vec<DesktopContent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRole {
    System,
    User,
    Assistant,
    Tool,
}

/// One typed, bounded semantic part for Desktop presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopContent {
    pub tier: DesktopContentTier,
    pub outcome: String,
    pub fallback_text: String,
    pub value: DesktopContentValue,
    pub actions: Vec<DesktopContentAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopContentTier {
    Rich,
    Text,
    Metadata,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopContentValue {
    Text(String),
    Markdown(String),
    Code {
        language: Option<String>,
        code: String,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Diff(String),
    Math {
        source: String,
        display: bool,
    },
    Link {
        label: String,
        url: String,
    },
    Resource(Box<DesktopResource>),
    Unsupported {
        version: u16,
        kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopContentAction {
    PreviewLink { url: String },
    OpenLink { url: String },
    InspectArtifact { artifact_id: String },
    CopyArtifactReference { artifact_id: String },
    SaveArtifact { artifact_id: String },
    RevealArtifact { artifact_id: String },
    OpenArtifact { artifact_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopResourceKind {
    StaticRaster,
    AnimatedRaster,
    Svg,
    Lottie,
    Audio,
    Video,
    Binary,
    Unknown(String),
}

impl DesktopResourceKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::StaticRaster => "static_raster",
            Self::AnimatedRaster => "animated_raster",
            Self::Svg => "svg",
            Self::Lottie => "lottie",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Binary => "binary",
            Self::Unknown(kind) => kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopResourceValidation {
    Pending,
    Accepted,
    Rejected { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DesktopResourceMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frames: Option<u32>,
    pub duration_millis: Option<u64>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u16>,
    pub tracks: Option<u16>,
    pub structured_items: Option<u32>,
    pub embedded_assets: Option<u32>,
    pub codec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResourceLineage {
    pub source_artifact_id: String,
    pub transformer: String,
    pub transformer_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopResourceOperation {
    Acquire,
    PresentInline,
    Playback,
    OpenExternal,
    ProviderInput,
    FocusedAnalysis,
    Transform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopResourceAvailability {
    Available,
    Stale,
    Unsupported,
    Unavailable { code: String },
    PermissionRequired { code: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopCapabilitySource {
    Runtime,
    Surface,
    Connection,
    Model,
    Route,
    Adapter,
    Provider,
    ManagedRuntime,
    Mcp,
    A2a,
    Measured,
    Estimated,
    Cache,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DesktopResource {
    pub artifact_id: String,
    pub kind: DesktopResourceKind,
    pub byte_len: u64,
    pub declared_media_type: Option<String>,
    pub detected_media_type: Option<String>,
    pub metadata: DesktopResourceMetadata,
    pub accessibility_label: Option<String>,
    pub validation: DesktopResourceValidation,
    pub lineage: Option<DesktopResourceLineage>,
    pub capabilities: Vec<DesktopResourceCapability>,
    artifact: ArtifactRecord,
    preview_max_bytes: u64,
    preview_max_pixels: u64,
    preview_max_edge: u32,
}

impl fmt::Debug for DesktopResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopResource")
            .field("artifact_id", &self.artifact_id)
            .field("kind", &self.kind)
            .field("byte_len", &self.byte_len)
            .field("declared_media_type", &self.declared_media_type)
            .field("detected_media_type", &self.detected_media_type)
            .field("metadata", &self.metadata)
            .field("accessibility_label", &self.accessibility_label)
            .field("validation", &self.validation)
            .field("lineage", &self.lineage)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl DesktopResource {
    pub fn display_name(&self) -> String {
        let short = self.artifact_id.get(..8).unwrap_or(&self.artifact_id);
        format!("{} {short}", self.kind.as_str().replace('_', " "))
    }

    pub fn media_type(&self) -> &str {
        self.detected_media_type
            .as_deref()
            .or(self.declared_media_type.as_deref())
            .unwrap_or("application/octet-stream")
    }

    pub fn suggested_file_name(&self) -> String {
        let short = self.artifact_id.get(..8).unwrap_or(&self.artifact_id);
        let extension = crate::artifact_action::extension_for_media_type(self.media_type());
        format!("xana-{short}.{extension}")
    }

    pub fn supports_inline_preview(&self) -> bool {
        self.kind == DesktopResourceKind::StaticRaster
            && self.validation == DesktopResourceValidation::Accepted
            && self.capabilities.iter().any(|capability| {
                capability.operation == DesktopResourceOperation::PresentInline
                    && capability.availability == DesktopResourceAvailability::Available
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResourceCapability {
    pub operation: DesktopResourceOperation,
    pub availability: DesktopResourceAvailability,
    pub selected: bool,
    pub authorized: bool,
    pub connection: Option<String>,
    pub model: Option<String>,
    pub effective_max_source_bytes: Option<u64>,
    pub reason_code: Option<String>,
    pub source: DesktopCapabilitySource,
    pub observed_at_unix_millis: u64,
    pub max_age_millis: Option<u64>,
}

/// Verified, bounded image bytes with no path or ambient storage authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopImagePreview {
    pub artifact_id: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Arc<[u8]>,
}

/// Capability-scoped reader for immutable Desktop raster previews.
#[derive(Debug, Clone)]
pub struct DesktopArtifactReader {
    store: ArtifactStore,
}

impl DesktopArtifactReader {
    pub(crate) fn new(store: ArtifactStore) -> Self {
        Self { store }
    }

    pub fn read_static_raster(
        &self,
        resource: &DesktopResource,
    ) -> Result<DesktopImagePreview, super::DesktopError> {
        if !resource.supports_inline_preview() {
            return Err(super::DesktopError::new(
                super::DesktopErrorCode::StateInvalid,
                "resource is not eligible for inline raster preview",
            ));
        }
        let media_type = resource.media_type();
        if !matches!(media_type, "image/png" | "image/jpeg" | "image/webp") {
            return Err(super::DesktopError::new(
                super::DesktopErrorCode::StateInvalid,
                format!("{media_type} has no reviewed static Desktop decoder"),
            ));
        }
        let (Some(width), Some(height)) = (resource.metadata.width, resource.metadata.height)
        else {
            return Err(super::DesktopError::new(
                super::DesktopErrorCode::StateInvalid,
                "image dimensions are unavailable",
            ));
        };
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| {
                super::DesktopError::new(
                    super::DesktopErrorCode::StateInvalid,
                    "image dimensions overflow the preview budget",
                )
            })?;
        if width > resource.preview_max_edge
            || height > resource.preview_max_edge
            || pixels > resource.preview_max_pixels
        {
            return Err(super::DesktopError::new(
                super::DesktopErrorCode::StateInvalid,
                "image dimensions exceed the configured Desktop preview budget",
            ));
        }
        let max_bytes = usize::try_from(resource.preview_max_bytes).map_err(|_| {
            super::DesktopError::new(
                super::DesktopErrorCode::StateInvalid,
                "Desktop preview byte limit is unavailable on this platform",
            )
        })?;
        let bytes = self
            .store
            .read_bounded(&resource.artifact, max_bytes)
            .map_err(|error| {
                super::DesktopError::new(
                    super::DesktopErrorCode::StateInvalid,
                    format!("could not verify Desktop image preview: {error}"),
                )
            })?;
        Ok(DesktopImagePreview {
            artifact_id: resource.artifact_id.clone(),
            media_type: media_type.to_owned(),
            width,
            height,
            bytes: bytes.into(),
        })
    }
}

pub(super) fn project_messages(
    session_id: String,
    messages: &[Message],
    policy: &ResourcePolicyV1,
) -> Vec<DesktopMessage> {
    let mut duplicate_counts = HashMap::<String, usize>::new();
    messages
        .iter()
        .map(|message| {
            let encoded = serde_json::to_vec(message).unwrap_or_default();
            let digest = blake3::hash(&encoded).to_hex().to_string();
            let duplicate = duplicate_counts.entry(digest.clone()).or_default();
            let id = format!("{session_id}:{digest}:{duplicate}");
            *duplicate = duplicate.saturating_add(1);
            project_message(id, message, policy)
        })
        .collect()
}

pub(super) fn project_message(
    id: String,
    message: &Message,
    policy: &ResourcePolicyV1,
) -> DesktopMessage {
    let content = normalize_message(message)
        .into_iter()
        .map(|part| project_part(part, policy))
        .collect();
    DesktopMessage {
        id,
        role: match message.role {
            Role::System => DesktopRole::System,
            Role::User => DesktopRole::User,
            Role::Assistant => DesktopRole::Assistant,
            Role::Tool => DesktopRole::Tool,
        },
        content,
    }
}

fn project_part(part: ContentPartV1, policy: &ResourcePolicyV1) -> DesktopContent {
    let projection = project_content(&part, PresentationCapabilities::desktop());
    let actions = projection.actions.into_iter().map(project_action).collect();
    let value = match part {
        ContentPartV1::Text { text } => DesktopContentValue::Text(text),
        ContentPartV1::Markdown { source } => DesktopContentValue::Markdown(source),
        ContentPartV1::Code { language, code } => DesktopContentValue::Code { language, code },
        ContentPartV1::Table { columns, rows } => DesktopContentValue::Table { columns, rows },
        ContentPartV1::Diff { patch } => DesktopContentValue::Diff(patch),
        ContentPartV1::Math { source, display } => DesktopContentValue::Math { source, display },
        ContentPartV1::Link { label, url } => DesktopContentValue::Link { label, url },
        ContentPartV1::Resource(resource) => {
            DesktopContentValue::Resource(Box::new(project_resource(*resource, policy)))
        }
        ContentPartV1::Unknown { version, kind, .. } => {
            DesktopContentValue::Unsupported { version, kind }
        }
    };
    DesktopContent {
        tier: project_tier(projection.tier),
        outcome: bounded_text(projection.outcome.code, MAX_PUBLIC_TEXT_BYTES),
        fallback_text: bounded_text(projection.fallback_text, MAX_PUBLIC_TEXT_BYTES),
        value,
        actions,
    }
}

fn project_resource(resource: ResourceRefV1, policy: &ResourcePolicyV1) -> DesktopResource {
    let observed_at_unix_millis = observed_at_unix_millis();
    let capabilities = match project_resource_capabilities(
        &resource,
        &ResourceCapabilityContextV1 {
            presentation: PresentationCapabilities::desktop(),
            policy: policy.clone(),
            observed_at_unix_millis,
            exact_route_facts: Vec::new(),
        },
    ) {
        Ok(facts) => facts.into_iter().map(project_capability).collect(),
        Err(error) => vec![DesktopResourceCapability {
            operation: DesktopResourceOperation::PresentInline,
            availability: DesktopResourceAvailability::Unavailable {
                code: "resource.projection_invalid".to_owned(),
            },
            selected: false,
            authorized: false,
            connection: None,
            model: None,
            effective_max_source_bytes: None,
            reason_code: Some(bounded_text(error.to_string(), MAX_PUBLIC_TEXT_BYTES)),
            source: DesktopCapabilitySource::Runtime,
            observed_at_unix_millis,
            max_age_millis: None,
        }],
    };
    DesktopResource {
        artifact_id: resource.artifact.reference.id.to_string(),
        kind: project_resource_kind(&resource.kind),
        byte_len: resource.artifact.byte_len,
        declared_media_type: resource.media_type.declared.clone(),
        detected_media_type: resource.media_type.detected.clone(),
        metadata: DesktopResourceMetadata {
            width: resource.metadata.width,
            height: resource.metadata.height,
            frames: resource.metadata.frames,
            duration_millis: resource.metadata.duration_millis,
            sample_rate_hz: resource.metadata.sample_rate_hz,
            channels: resource.metadata.channels,
            tracks: resource.metadata.tracks,
            structured_items: resource.metadata.structured_items,
            embedded_assets: resource.metadata.embedded_assets,
            codec: resource.metadata.codec.clone(),
        },
        accessibility_label: resource
            .accessibility
            .as_ref()
            .and_then(|facts| facts.label.clone()),
        validation: match &resource.validation {
            ResourceValidationV1::Pending => DesktopResourceValidation::Pending,
            ResourceValidationV1::Accepted => DesktopResourceValidation::Accepted,
            ResourceValidationV1::Rejected { code } => {
                DesktopResourceValidation::Rejected { code: code.clone() }
            }
        },
        lineage: resource
            .lineage
            .as_ref()
            .map(|lineage| DesktopResourceLineage {
                source_artifact_id: lineage.source.id.to_string(),
                transformer: lineage.transformer.clone(),
                transformer_version: lineage.transformer_version.clone(),
            }),
        preview_max_bytes: policy
            .static_raster
            .max_source_bytes
            .min(policy.max_in_memory_buffer_bytes),
        preview_max_pixels: policy.static_raster.max_pixels,
        preview_max_edge: policy.static_raster.max_edge,
        artifact: resource.artifact,
        capabilities,
    }
}

fn project_capability(fact: CapabilityFactV1) -> DesktopResourceCapability {
    DesktopResourceCapability {
        operation: match fact.operation {
            ResourceOperationV1::Acquire => DesktopResourceOperation::Acquire,
            ResourceOperationV1::PresentInline => DesktopResourceOperation::PresentInline,
            ResourceOperationV1::Playback => DesktopResourceOperation::Playback,
            ResourceOperationV1::OpenExternal => DesktopResourceOperation::OpenExternal,
            ResourceOperationV1::ProviderInput => DesktopResourceOperation::ProviderInput,
            ResourceOperationV1::FocusedAnalysis => DesktopResourceOperation::FocusedAnalysis,
            ResourceOperationV1::Transform => DesktopResourceOperation::Transform,
        },
        availability: match fact.availability {
            AvailabilityV1::Available => DesktopResourceAvailability::Available,
            AvailabilityV1::Stale => DesktopResourceAvailability::Stale,
            AvailabilityV1::Unsupported => DesktopResourceAvailability::Unsupported,
            AvailabilityV1::Unavailable { code } => {
                DesktopResourceAvailability::Unavailable { code }
            }
            AvailabilityV1::PermissionRequired { code } => {
                DesktopResourceAvailability::PermissionRequired { code }
            }
        },
        selected: fact.selected,
        authorized: fact.authorized,
        connection: fact.connection,
        model: fact.model,
        effective_max_source_bytes: fact.effective_max_source_bytes,
        reason_code: fact.reason_code,
        source: match fact.source {
            FactSourceV1::Runtime => DesktopCapabilitySource::Runtime,
            FactSourceV1::Surface => DesktopCapabilitySource::Surface,
            FactSourceV1::Connection => DesktopCapabilitySource::Connection,
            FactSourceV1::Model => DesktopCapabilitySource::Model,
            FactSourceV1::Route => DesktopCapabilitySource::Route,
            FactSourceV1::Adapter => DesktopCapabilitySource::Adapter,
            FactSourceV1::Provider => DesktopCapabilitySource::Provider,
            FactSourceV1::ManagedRuntime => DesktopCapabilitySource::ManagedRuntime,
            FactSourceV1::Mcp => DesktopCapabilitySource::Mcp,
            FactSourceV1::A2a => DesktopCapabilitySource::A2a,
            FactSourceV1::Measured => DesktopCapabilitySource::Measured,
            FactSourceV1::Estimated => DesktopCapabilitySource::Estimated,
            FactSourceV1::Cache => DesktopCapabilitySource::Cache,
        },
        observed_at_unix_millis: fact.freshness.observed_at_unix_millis,
        max_age_millis: fact.freshness.max_age_millis,
    }
}

fn project_action(action: ContentActionV1) -> DesktopContentAction {
    match action {
        ContentActionV1::PreviewLink { url } => DesktopContentAction::PreviewLink { url },
        ContentActionV1::OpenLink { url } => DesktopContentAction::OpenLink { url },
        ContentActionV1::InspectArtifact { artifact_id } => DesktopContentAction::InspectArtifact {
            artifact_id: artifact_id.to_string(),
        },
        ContentActionV1::CopyArtifactReference { artifact_id } => {
            DesktopContentAction::CopyArtifactReference {
                artifact_id: artifact_id.to_string(),
            }
        }
        ContentActionV1::SaveArtifact { artifact_id } => DesktopContentAction::SaveArtifact {
            artifact_id: artifact_id.to_string(),
        },
        ContentActionV1::RevealArtifact { artifact_id } => DesktopContentAction::RevealArtifact {
            artifact_id: artifact_id.to_string(),
        },
        ContentActionV1::OpenArtifact { artifact_id } => DesktopContentAction::OpenArtifact {
            artifact_id: artifact_id.to_string(),
        },
    }
}

fn project_tier(tier: ContentProjectionTierV1) -> DesktopContentTier {
    match tier {
        ContentProjectionTierV1::Rich => DesktopContentTier::Rich,
        ContentProjectionTierV1::Text => DesktopContentTier::Text,
        ContentProjectionTierV1::Metadata => DesktopContentTier::Metadata,
        ContentProjectionTierV1::Unsupported => DesktopContentTier::Unsupported,
    }
}

fn project_resource_kind(kind: &ResourceKindV1) -> DesktopResourceKind {
    match kind {
        ResourceKindV1::StaticRaster => DesktopResourceKind::StaticRaster,
        ResourceKindV1::AnimatedRaster => DesktopResourceKind::AnimatedRaster,
        ResourceKindV1::Svg => DesktopResourceKind::Svg,
        ResourceKindV1::Lottie => DesktopResourceKind::Lottie,
        ResourceKindV1::Audio => DesktopResourceKind::Audio,
        ResourceKindV1::Video => DesktopResourceKind::Video,
        ResourceKindV1::Binary => DesktopResourceKind::Binary,
        ResourceKindV1::Unknown(kind) => DesktopResourceKind::Unknown(kind.clone()),
    }
}

fn observed_at_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        artifact::ArtifactStore, identity::PrincipalId, message::ContentBlock, vision::ImageRef,
    };
    use tempfile::TempDir;

    #[test]
    fn rich_parts_retain_type_and_safe_fallback() {
        let message = Message::text(Role::Assistant, "```rust\nfn main() {}\n```");
        let projected =
            project_message("answer".to_owned(), &message, &ResourcePolicyV1::default());
        assert!(matches!(
            projected.content[0].value,
            DesktopContentValue::Code {
                language: Some(ref language),
                ..
            } if language == "rust"
        ));
        assert_eq!(projected.content[0].tier, DesktopContentTier::Rich);
        assert!(projected.content[0].fallback_text.contains("fn main"));
    }

    #[test]
    fn resource_projection_keeps_capabilities_and_hides_store_authority() {
        let directory = TempDir::new().unwrap();
        let store = ArtifactStore::new(directory.path().to_owned());
        let bytes = tiny_png();
        let (artifact, _) = store.put(&bytes, "image/png", PrincipalId::new()).unwrap();
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Image(ImageRef {
                artifact,
                media_type: "image/png".to_owned(),
                byte_len: bytes.len() as u64,
                width: Some(1),
                height: Some(1),
            })],
        };
        let projected = project_message("image".to_owned(), &message, &ResourcePolicyV1::default());
        let DesktopContentValue::Resource(resource) = &projected.content[0].value else {
            panic!("expected resource")
        };
        assert!(resource.supports_inline_preview());
        assert!(resource.capabilities.iter().any(|capability| {
            capability.operation == DesktopResourceOperation::PresentInline
                && capability.availability == DesktopResourceAvailability::Available
        }));

        let preview = DesktopArtifactReader::new(store)
            .read_static_raster(resource)
            .unwrap();
        assert_eq!(preview.bytes.as_ref(), bytes);
        assert_eq!(preview.width, 1);
    }

    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 13, b'I', b'D',
            b'A', b'T', 8, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0, 5, 0, 1, 0xFF, 0x89, 0x99,
            0x3D, 0x1D, 0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
        ]
    }
}
