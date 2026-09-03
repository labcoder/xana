//! Bounded signature and metadata inspection for immutable local artifacts.
//!
//! Inspection happens after source-length admission and before any decoder,
//! renderer, transform, or provider disclosure. It never trusts an extension
//! or declared media type and retains declaration/detection disagreements.

use super::{
    AccessibilityFactsV1, AccessibilitySourceV1, LocalResourcePath, LocalResourcePathError,
    MAX_RESOURCE_SOURCE_BYTES, MediaTypeFactsV1, RESOURCE_SCHEMA_VERSION, ResourceKindV1,
    ResourceMetadataV1, ResourcePolicyV1, ResourceRefV1, ResourceValidationV1, classify_local_path,
};
use crate::artifact::{ArtifactError, ArtifactRecord, ArtifactStore};
use crate::identity::PrincipalId;
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

const HEADER_PROBE_BYTES: usize = 64 * 1024;

pub(crate) struct ResourceInspector {
    store: ArtifactStore,
    policy: ResourcePolicyV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IngestedResource {
    pub(crate) source_path: String,
    pub(crate) source_label: String,
    pub(crate) resource: ResourceRefV1,
}

pub(crate) struct ResourceIngestor {
    inspector: ResourceInspector,
}

impl ResourceIngestor {
    pub(crate) fn new(
        store: ArtifactStore,
        policy: ResourcePolicyV1,
    ) -> Result<Self, ResourceInspectionError> {
        Ok(Self {
            inspector: ResourceInspector::new(store, policy)?,
        })
    }

    pub(crate) fn ingest_path(
        &self,
        workspace_root: &Path,
        source_path: &str,
        owner: PrincipalId,
    ) -> Result<IngestedResource, ResourceIngestError> {
        match classify_local_path(workspace_root, source_path)? {
            LocalResourcePath::Workspace { relative } => {
                let root = workspace_root.canonicalize()?;
                let canonical = root.join(&relative).canonicalize()?;
                if !canonical.starts_with(&root) {
                    return Err(ResourceIngestError::OutsideWorkspace);
                }
                self.ingest_canonical(canonical, relative, owner)
            }
            LocalResourcePath::External { .. } => Err(ResourceIngestError::OutsideWorkspace),
        }
    }

    pub(crate) fn ingest_approved_path(
        &self,
        workspace_root: &Path,
        source_path: &str,
        owner: PrincipalId,
    ) -> Result<IngestedResource, ResourceIngestError> {
        match classify_local_path(workspace_root, source_path)? {
            LocalResourcePath::Workspace { relative } => {
                let root = workspace_root.canonicalize()?;
                let canonical = root.join(&relative).canonicalize()?;
                if !canonical.starts_with(&root) {
                    return Err(ResourceIngestError::OutsideWorkspace);
                }
                self.ingest_canonical(canonical, relative, owner)
            }
            LocalResourcePath::External { canonical } => {
                self.ingest_canonical(canonical, source_path.to_owned(), owner)
            }
        }
    }

    fn ingest_canonical(
        &self,
        canonical: PathBuf,
        source_path: String,
        owner: PrincipalId,
    ) -> Result<IngestedResource, ResourceIngestError> {
        let limit = usize::try_from(self.inspector.policy.max_total_source_bytes)
            .unwrap_or(MAX_RESOURCE_SOURCE_BYTES)
            .min(MAX_RESOURCE_SOURCE_BYTES);
        let declared = declared_media_type(&canonical);
        let (artifact, _) = self
            .inspector
            .store
            .put_file_bounded(&canonical, declared, owner, limit)?;
        let resource = self.inspector.inspect(&artifact)?;
        let source_label = canonical
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("resource")
            .to_owned();
        Ok(IngestedResource {
            source_path,
            source_label,
            resource,
        })
    }
}

#[derive(Debug)]
pub(crate) enum ResourceIngestError {
    OutsideWorkspace,
    Path(LocalResourcePathError),
    Artifact(ArtifactError),
    Inspection(ResourceInspectionError),
    Io(std::io::Error),
}

impl fmt::Display for ResourceIngestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideWorkspace => {
                formatter.write_str("resource resolves outside the launch workspace")
            }
            Self::Path(error) => error.fmt(formatter),
            Self::Artifact(error) => error.fmt(formatter),
            Self::Inspection(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl Error for ResourceIngestError {}

impl From<LocalResourcePathError> for ResourceIngestError {
    fn from(error: LocalResourcePathError) -> Self {
        Self::Path(error)
    }
}

impl From<ArtifactError> for ResourceIngestError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

impl From<ResourceInspectionError> for ResourceIngestError {
    fn from(error: ResourceInspectionError) -> Self {
        Self::Inspection(error)
    }
}

impl From<std::io::Error> for ResourceIngestError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn declared_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("ogg" | "oga") => "audio/ogg",
        Some("webm") => "video/webm",
        Some("mp4" | "m4v") => "video/mp4",
        _ => "application/octet-stream",
    }
}

impl ResourceInspector {
    pub(crate) fn new(
        store: ArtifactStore,
        policy: ResourcePolicyV1,
    ) -> Result<Self, ResourceInspectionError> {
        policy
            .validate()
            .map_err(|error| ResourceInspectionError::Policy(error.to_string()))?;
        Ok(Self { store, policy })
    }

    pub(crate) fn inspect(
        &self,
        artifact: &ArtifactRecord,
    ) -> Result<ResourceRefV1, ResourceInspectionError> {
        if artifact.byte_len > MAX_RESOURCE_SOURCE_BYTES as u64 {
            return Err(ResourceInspectionError::AboveCompiledCeiling {
                actual: artifact.byte_len,
                limit: MAX_RESOURCE_SOURCE_BYTES as u64,
            });
        }
        let declared = normalize_media_type(&artifact.media_type);
        if artifact.byte_len > self.policy.max_total_source_bytes {
            return Ok(resource(
                artifact,
                ResourceKindV1::Binary,
                declared,
                Some("application/octet-stream".into()),
                ResourceMetadataV1::default(),
                ResourceValidationV1::Rejected {
                    code: "resource.turn_source_bytes_exceeded".into(),
                },
            ));
        }
        let probe_limit = usize::try_from(self.policy.max_in_memory_buffer_bytes)
            .unwrap_or(usize::MAX)
            .min(HEADER_PROBE_BYTES);
        let artifact_limit = usize::try_from(self.policy.max_total_source_bytes)
            .unwrap_or(MAX_RESOURCE_SOURCE_BYTES)
            .min(MAX_RESOURCE_SOURCE_BYTES);
        let probe = self
            .store
            .read_verified_range(artifact, 0, probe_limit, artifact_limit)
            .map_err(ResourceInspectionError::Artifact)?;
        let mut detection = detect(&probe.bytes, declared.as_deref());
        apply_policy(&mut detection, artifact.byte_len, &self.policy);
        Ok(resource(
            artifact,
            detection.kind,
            declared,
            Some(detection.media_type.to_owned()),
            detection.metadata,
            detection.validation,
        ))
    }
}

struct Detection {
    kind: ResourceKindV1,
    media_type: &'static str,
    metadata: ResourceMetadataV1,
    validation: ResourceValidationV1,
}

fn detect(bytes: &[u8], declared: Option<&str>) -> Detection {
    if let Some(metadata) = png_metadata(bytes) {
        return detected(ResourceKindV1::StaticRaster, "image/png", metadata);
    }
    if let Some(metadata) = jpeg_metadata(bytes) {
        return detected(ResourceKindV1::StaticRaster, "image/jpeg", metadata);
    }
    if let Some(metadata) = gif_metadata(bytes) {
        return detected(ResourceKindV1::AnimatedRaster, "image/gif", metadata);
    }
    if let Some((animated, metadata)) = webp_metadata(bytes) {
        return detected(
            if animated {
                ResourceKindV1::AnimatedRaster
            } else {
                ResourceKindV1::StaticRaster
            },
            "image/webp",
            metadata,
        );
    }
    if looks_like_svg(bytes) {
        let mut value = detected(
            ResourceKindV1::Svg,
            "image/svg+xml",
            ResourceMetadataV1 {
                structured_items: Some(count_bounded(bytes, b'<')),
                ..ResourceMetadataV1::default()
            },
        );
        value.validation = ResourceValidationV1::Pending;
        return value;
    }
    if let Some(metadata) = wav_metadata(bytes) {
        return detected(ResourceKindV1::Audio, "audio/wav", metadata);
    }
    if bytes.starts_with(b"OggS") {
        return detected(
            if declared.is_some_and(|value| value.starts_with("video/")) {
                ResourceKindV1::Video
            } else {
                ResourceKindV1::Audio
            },
            if declared.is_some_and(|value| value.starts_with("video/")) {
                "video/ogg"
            } else {
                "audio/ogg"
            },
            ResourceMetadataV1::default(),
        );
    }
    if bytes.starts_with(b"ID3")
        || bytes
            .windows(2)
            .next()
            .is_some_and(|header| header[0] == 0xff && header[1] & 0xe0 == 0xe0)
    {
        return detected(
            ResourceKindV1::Audio,
            "audio/mpeg",
            ResourceMetadataV1::default(),
        );
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return detected(
            ResourceKindV1::Video,
            "video/webm",
            ResourceMetadataV1::default(),
        );
    }
    if bytes.get(4..8) == Some(b"ftyp") {
        return detected(
            ResourceKindV1::Video,
            "video/mp4",
            ResourceMetadataV1::default(),
        );
    }
    if declared == Some("application/json") && looks_like_lottie(bytes) {
        let mut value = detected(
            ResourceKindV1::Lottie,
            "application/json",
            ResourceMetadataV1::default(),
        );
        value.validation = ResourceValidationV1::Pending;
        return value;
    }
    Detection {
        kind: ResourceKindV1::Binary,
        media_type: "application/octet-stream",
        metadata: ResourceMetadataV1::default(),
        validation: ResourceValidationV1::Rejected {
            code: "resource.content_unrecognized".into(),
        },
    }
}

fn detected(
    kind: ResourceKindV1,
    media_type: &'static str,
    metadata: ResourceMetadataV1,
) -> Detection {
    Detection {
        kind,
        media_type,
        metadata,
        validation: ResourceValidationV1::Accepted,
    }
}

fn apply_policy(detection: &mut Detection, source_bytes: u64, policy: &ResourcePolicyV1) {
    if source_bytes > policy.max_source_bytes_for(&detection.kind) {
        detection.validation = ResourceValidationV1::Rejected {
            code: "resource.source_bytes_exceeded".into(),
        };
        return;
    }
    let rejected = match detection.kind {
        ResourceKindV1::StaticRaster => dimensions_exceed(
            &detection.metadata,
            policy.static_raster.max_edge,
            policy.static_raster.max_edge,
            policy.static_raster.max_pixels,
        ),
        ResourceKindV1::AnimatedRaster => dimensions_exceed(
            &detection.metadata,
            u32::MAX,
            u32::MAX,
            policy.animated_raster.max_canvas_pixels,
        ),
        ResourceKindV1::Audio => {
            detection
                .metadata
                .sample_rate_hz
                .is_some_and(|value| value > policy.audio.max_sample_rate_hz)
                || detection
                    .metadata
                    .channels
                    .is_some_and(|value| value > policy.audio.max_channels)
                || detection
                    .metadata
                    .duration_millis
                    .is_some_and(|value| value > policy.audio.max_duration_millis)
        }
        ResourceKindV1::Video => dimensions_exceed(
            &detection.metadata,
            policy.video.max_width,
            policy.video.max_height,
            policy.video.max_pixels,
        ),
        ResourceKindV1::Svg
        | ResourceKindV1::Lottie
        | ResourceKindV1::Binary
        | ResourceKindV1::Unknown(_) => false,
    };
    if rejected {
        detection.validation = ResourceValidationV1::Rejected {
            code: "resource.metadata_limit_exceeded".into(),
        };
    }
}

fn dimensions_exceed(
    metadata: &ResourceMetadataV1,
    max_width: u32,
    max_height: u32,
    max_pixels: u64,
) -> bool {
    let (Some(width), Some(height)) = (metadata.width, metadata.height) else {
        return false;
    };
    width > max_width
        || height > max_height
        || u64::from(width)
            .checked_mul(u64::from(height))
            .is_none_or(|pixels| pixels > max_pixels)
}

fn png_metadata(bytes: &[u8]) -> Option<ResourceMetadataV1> {
    (bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR")
        .then(|| ResourceMetadataV1 {
            width: Some(u32::from_be_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19],
            ])),
            height: Some(u32::from_be_bytes([
                bytes[20], bytes[21], bytes[22], bytes[23],
            ])),
            ..ResourceMetadataV1::default()
        })
}

fn gif_metadata(bytes: &[u8]) -> Option<ResourceMetadataV1> {
    (bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))).then(
        || ResourceMetadataV1 {
            width: Some(u16::from_le_bytes([bytes[6], bytes[7]]).into()),
            height: Some(u16::from_le_bytes([bytes[8], bytes[9]]).into()),
            ..ResourceMetadataV1::default()
        },
    )
}

fn jpeg_metadata(bytes: &[u8]) -> Option<ResourceMetadataV1> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut cursor = 2_usize;
    while cursor.saturating_add(4) <= bytes.len() {
        if bytes[cursor] != 0xff {
            cursor += 1;
            continue;
        }
        let marker = bytes[cursor + 1];
        cursor += 2;
        if matches!(marker, 0xd8 | 0xd9) || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]));
        if length < 2 || cursor.saturating_add(length) > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) && length >= 7 {
            return Some(ResourceMetadataV1 {
                width: Some(u16::from_be_bytes([bytes[cursor + 5], bytes[cursor + 6]]).into()),
                height: Some(u16::from_be_bytes([bytes[cursor + 3], bytes[cursor + 4]]).into()),
                ..ResourceMetadataV1::default()
            });
        }
        cursor += length;
    }
    None
}

fn webp_metadata(bytes: &[u8]) -> Option<(bool, ResourceMetadataV1)> {
    if bytes.len() < 30 || !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return None;
    }
    match bytes.get(12..16)? {
        b"VP8X" => {
            let width = 1_u32.checked_add(read_u24_le(&bytes[24..27]))?;
            let height = 1_u32.checked_add(read_u24_le(&bytes[27..30]))?;
            Some((
                bytes[20] & 0x02 != 0,
                ResourceMetadataV1 {
                    width: Some(width),
                    height: Some(height),
                    ..ResourceMetadataV1::default()
                },
            ))
        }
        _ => Some((false, ResourceMetadataV1::default())),
    }
}

fn read_u24_le(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

fn wav_metadata(bytes: &[u8]) -> Option<ResourceMetadataV1> {
    if bytes.len() < 44 || !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return None;
    }
    let channels = u16::from_le_bytes(bytes[22..24].try_into().ok()?);
    let sample_rate = u32::from_le_bytes(bytes[24..28].try_into().ok()?);
    let byte_rate = u32::from_le_bytes(bytes[28..32].try_into().ok()?);
    let data_bytes = u32::from_le_bytes(bytes[40..44].try_into().ok()?);
    let duration_millis = (byte_rate != 0).then(|| {
        u64::from(data_bytes)
            .checked_mul(1_000)
            .map(|value| value / u64::from(byte_rate))
    })??;
    Some(ResourceMetadataV1 {
        duration_millis: Some(duration_millis),
        sample_rate_hz: Some(sample_rate),
        channels: Some(channels),
        codec: Some("pcm".into()),
        ..ResourceMetadataV1::default()
    })
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let sample = std::str::from_utf8(bytes).unwrap_or_default().trim_start();
    sample.starts_with("<svg")
        || (sample.starts_with("<?xml") && sample.get(..4_096).unwrap_or(sample).contains("<svg"))
}

fn looks_like_lottie(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|object| object.contains_key("v") && object.contains_key("layers"))
}

fn count_bounded(bytes: &[u8], needle: u8) -> u32 {
    u32::try_from(bytes.iter().filter(|byte| **byte == needle).count()).unwrap_or(u32::MAX)
}

fn normalize_media_type(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty()
        && value.len() <= 127
        && value.contains('/')
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()))
    .then_some(value)
}

fn resource(
    artifact: &ArtifactRecord,
    kind: ResourceKindV1,
    declared: Option<String>,
    detected: Option<String>,
    metadata: ResourceMetadataV1,
    validation: ResourceValidationV1,
) -> ResourceRefV1 {
    ResourceRefV1 {
        version: RESOURCE_SCHEMA_VERSION,
        artifact: artifact.clone(),
        kind,
        media_type: MediaTypeFactsV1 { declared, detected },
        metadata,
        accessibility: Some(AccessibilityFactsV1 {
            label: None,
            transcript: None,
            source: AccessibilitySourceV1::Unavailable,
        }),
        validation,
        lineage: None,
    }
}

#[derive(Debug)]
pub(crate) enum ResourceInspectionError {
    Policy(String),
    AboveCompiledCeiling { actual: u64, limit: u64 },
    Artifact(ArtifactError),
}

impl fmt::Display for ResourceInspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(reason) => write!(formatter, "resource policy is invalid: {reason}"),
            Self::AboveCompiledCeiling { actual, limit } => write!(
                formatter,
                "resource contains {actual} bytes; compiled ceiling is {limit} bytes"
            ),
            Self::Artifact(error) => error.fmt(formatter),
        }
    }
}

impl Error for ResourceInspectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Artifact(error) => Some(error),
            Self::Policy(_) | Self::AboveCompiledCeiling { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
