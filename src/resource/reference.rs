//! Versioned artifact-backed resource facts.

use super::{MAX_RESOURCE_SOURCE_BYTES, RESOURCE_SCHEMA_VERSION};
use crate::artifact::{ArtifactRecord, ArtifactRef};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

const MAX_MEDIA_TYPE_BYTES: usize = 127;
const MAX_LABEL_BYTES: usize = 4 * 1024;
const MAX_CODEC_BYTES: usize = 96;
const MAX_TRANSFORM_ID_BYTES: usize = 96;
const MAX_TRANSFORM_PARAMS: usize = 32;
const MAX_PARAM_KEY_BYTES: usize = 64;
const MAX_PARAM_VALUE_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceKindV1 {
    StaticRaster,
    AnimatedRaster,
    Svg,
    Lottie,
    Audio,
    Video,
    Binary,
    Unknown(String),
}

impl ResourceKindV1 {
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::StaticRaster => "static_raster",
            Self::AnimatedRaster => "animated_raster",
            Self::Svg => "svg",
            Self::Lottie => "lottie",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Binary => "binary",
            Self::Unknown(code) => code,
        }
    }

    fn parse(code: String) -> Result<Self, ResourceValidationError> {
        validate_code("resource kind", &code, MAX_TRANSFORM_ID_BYTES)?;
        Ok(match code.as_str() {
            "static_raster" => Self::StaticRaster,
            "animated_raster" => Self::AnimatedRaster,
            "svg" => Self::Svg,
            "lottie" => Self::Lottie,
            "audio" => Self::Audio,
            "video" => Self::Video,
            "binary" => Self::Binary,
            _ => Self::Unknown(code),
        })
    }
}

impl Serialize for ResourceKindV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for ResourceKindV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MediaTypeFactsV1 {
    pub(crate) declared: Option<String>,
    pub(crate) detected: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccessibilitySourceV1 {
    User,
    EmbeddedMetadata,
    Provider,
    Derived,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessibilityFactsV1 {
    pub(crate) label: Option<String>,
    pub(crate) transcript: Option<ArtifactRef>,
    pub(crate) source: AccessibilitySourceV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResourceMetadataV1 {
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) frames: Option<u32>,
    pub(crate) duration_millis: Option<u64>,
    pub(crate) sample_rate_hz: Option<u32>,
    pub(crate) channels: Option<u16>,
    pub(crate) tracks: Option<u16>,
    pub(crate) structured_items: Option<u32>,
    pub(crate) embedded_assets: Option<u32>,
    pub(crate) codec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceValidationV1 {
    Pending,
    Accepted,
    Rejected { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum TransformParamV1 {
    Bool(bool),
    Integer(i64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourceLineageV1 {
    pub(crate) source: ArtifactRef,
    pub(crate) transformer: String,
    pub(crate) transformer_version: String,
    pub(crate) parameters: BTreeMap<String, TransformParamV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourceRefV1 {
    pub(crate) version: u16,
    pub(crate) artifact: ArtifactRecord,
    pub(crate) kind: ResourceKindV1,
    pub(crate) media_type: MediaTypeFactsV1,
    pub(crate) metadata: ResourceMetadataV1,
    pub(crate) accessibility: Option<AccessibilityFactsV1>,
    pub(crate) validation: ResourceValidationV1,
    pub(crate) lineage: Option<ResourceLineageV1>,
}

impl ResourceRefV1 {
    pub(crate) fn tool_evidence(artifact: ArtifactRecord) -> Self {
        Self {
            version: RESOURCE_SCHEMA_VERSION,
            media_type: MediaTypeFactsV1 {
                declared: Some(artifact.media_type.clone()),
                detected: None,
            },
            artifact,
            kind: ResourceKindV1::Binary,
            metadata: ResourceMetadataV1::default(),
            accessibility: Some(AccessibilityFactsV1 {
                label: Some("Complete tool output (JSON string)".to_owned()),
                transcript: None,
                source: AccessibilitySourceV1::Derived,
            }),
            validation: ResourceValidationV1::Accepted,
            lineage: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ResourceValidationError> {
        if self.version != RESOURCE_SCHEMA_VERSION {
            return Err(ResourceValidationError::UnsupportedVersion(self.version));
        }
        if self.artifact.byte_len > MAX_RESOURCE_SOURCE_BYTES as u64 {
            return Err(ResourceValidationError::AboveCompiledCeiling {
                field: "artifact.byte_len",
                value: self.artifact.byte_len,
                ceiling: MAX_RESOURCE_SOURCE_BYTES as u64,
            });
        }
        validate_optional_media_type("declared media type", self.media_type.declared.as_deref())?;
        validate_optional_media_type("detected media type", self.media_type.detected.as_deref())?;
        if self.media_type.declared.is_none() && self.media_type.detected.is_none() {
            return Err(ResourceValidationError::InvalidValue(
                "at least one media type must be present",
            ));
        }
        if let Some(codec) = self.metadata.codec.as_deref() {
            validate_text("codec", codec, MAX_CODEC_BYTES)?;
        }
        if let Some(accessibility) = &self.accessibility
            && let Some(label) = accessibility.label.as_deref()
        {
            validate_text("accessibility label", label, MAX_LABEL_BYTES)?;
        }
        if let ResourceValidationV1::Rejected { code } = &self.validation {
            validate_code("validation code", code, MAX_TRANSFORM_ID_BYTES)?;
        }
        if let Some(lineage) = &self.lineage {
            validate_code("transformer", &lineage.transformer, MAX_TRANSFORM_ID_BYTES)?;
            validate_code(
                "transformer version",
                &lineage.transformer_version,
                MAX_TRANSFORM_ID_BYTES,
            )?;
            if lineage.parameters.len() > MAX_TRANSFORM_PARAMS {
                return Err(ResourceValidationError::TooManyValues {
                    field: "lineage.parameters",
                    actual: lineage.parameters.len(),
                    limit: MAX_TRANSFORM_PARAMS,
                });
            }
            for (key, value) in &lineage.parameters {
                validate_code("transform parameter key", key, MAX_PARAM_KEY_BYTES)?;
                if let TransformParamV1::Text(value) = value {
                    validate_text("transform parameter value", value, MAX_PARAM_VALUE_BYTES)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceValidationError {
    UnsupportedVersion(u16),
    InvalidValue(&'static str),
    InvalidText {
        field: &'static str,
        reason: &'static str,
    },
    TooManyValues {
        field: &'static str,
        actual: usize,
        limit: usize,
    },
    AboveCompiledCeiling {
        field: &'static str,
        value: u64,
        ceiling: u64,
    },
}

impl fmt::Display for ResourceValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported resource schema version {version}")
            }
            Self::InvalidValue(reason) => formatter.write_str(reason),
            Self::InvalidText { field, reason } => write!(formatter, "{field} {reason}"),
            Self::TooManyValues {
                field,
                actual,
                limit,
            } => write!(formatter, "{field} has {actual} values; limit is {limit}"),
            Self::AboveCompiledCeiling {
                field,
                value,
                ceiling,
            } => write!(
                formatter,
                "{field} is {value}; compiled ceiling is {ceiling}"
            ),
        }
    }
}

impl Error for ResourceValidationError {}

fn validate_optional_media_type(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), ResourceValidationError> {
    if let Some(value) = value {
        validate_text(field, value, MAX_MEDIA_TYPE_BYTES)?;
        if !value.contains('/') || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(ResourceValidationError::InvalidText {
                field,
                reason: "must be a MIME-style type without whitespace",
            });
        }
    }
    Ok(())
}

fn validate_code(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ResourceValidationError> {
    validate_text(field, value, max_bytes)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-' | b'/')
    }) {
        return Err(ResourceValidationError::InvalidText {
            field,
            reason: "must contain only lowercase ASCII code characters",
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ResourceValidationError> {
    if value.trim().is_empty() {
        return Err(ResourceValidationError::InvalidText {
            field,
            reason: "must not be blank",
        });
    }
    if value.len() > max_bytes {
        return Err(ResourceValidationError::InvalidText {
            field,
            reason: "exceeds its byte limit",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ResourceValidationError::InvalidText {
            field,
            reason: "must not contain control characters",
        });
    }
    Ok(())
}
