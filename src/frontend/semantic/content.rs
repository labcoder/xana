use super::{
    AvailabilityV1, FactSourceV1, FreshnessV1, MAX_SAFE_TEXT_BYTES, SemanticError, validate_code,
    validate_text,
};
use crate::{
    identity::ArtifactId,
    resource::{MAX_RESOURCE_SOURCE_BYTES, ResourcePolicyV1, ResourceRefV1},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const CONTENT_VERSION: u16 = 1;
const MAX_CONTENT_PART_BYTES: usize = 768 * 1024;
const MAX_CODE_BYTES: usize = 512 * 1024;
const MAX_TABLE_COLUMNS: usize = 64;
const MAX_TABLE_ROWS: usize = 1_000;
const MAX_TABLE_CELL_BYTES: usize = 16 * 1024;
const MAX_LINK_BYTES: usize = 4 * 1024;
const MAX_SOURCE_LABEL_BYTES: usize = 256;
const MAX_DESTINATION_BYTES: usize = 256;
const MAX_CAPABILITY_FACTS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContentPartV1 {
    Text {
        text: String,
    },
    Markdown {
        source: String,
    },
    Code {
        language: Option<String>,
        code: String,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Diff {
        patch: String,
    },
    Math {
        source: String,
        display: bool,
    },
    Link {
        label: String,
        url: String,
    },
    Resource(Box<ResourceRefV1>),
    Unknown {
        version: u16,
        kind: String,
        payload: Value,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContentPart {
    #[serde(default = "content_version")]
    version: u16,
    kind: String,
    payload: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TextPayload {
    text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkdownPayload {
    source: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodePayload {
    language: Option<String>,
    code: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TablePayload {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffPayload {
    patch: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MathPayload {
    source: String,
    display: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkPayload {
    label: String,
    url: String,
}

const fn content_version() -> u16 {
    CONTENT_VERSION
}

impl Serialize for ContentPartV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (version, kind, payload) = match self {
            Self::Text { text } => (
                CONTENT_VERSION,
                "text",
                serde_json::to_value(TextPayload { text: text.clone() }),
            ),
            Self::Markdown { source } => (
                CONTENT_VERSION,
                "markdown",
                serde_json::to_value(MarkdownPayload {
                    source: source.clone(),
                }),
            ),
            Self::Code { language, code } => (
                CONTENT_VERSION,
                "code",
                serde_json::to_value(CodePayload {
                    language: language.clone(),
                    code: code.clone(),
                }),
            ),
            Self::Table { columns, rows } => (
                CONTENT_VERSION,
                "table",
                serde_json::to_value(TablePayload {
                    columns: columns.clone(),
                    rows: rows.clone(),
                }),
            ),
            Self::Diff { patch } => (
                CONTENT_VERSION,
                "diff",
                serde_json::to_value(DiffPayload {
                    patch: patch.clone(),
                }),
            ),
            Self::Math { source, display } => (
                CONTENT_VERSION,
                "math",
                serde_json::to_value(MathPayload {
                    source: source.clone(),
                    display: *display,
                }),
            ),
            Self::Link { label, url } => (
                CONTENT_VERSION,
                "link",
                serde_json::to_value(LinkPayload {
                    label: label.clone(),
                    url: url.clone(),
                }),
            ),
            Self::Resource(resource) => {
                (CONTENT_VERSION, "resource", serde_json::to_value(resource))
            }
            Self::Unknown {
                version,
                kind,
                payload,
            } => (*version, kind.as_str(), Ok(payload.clone())),
        };
        RawContentPart {
            version,
            kind: kind.to_owned(),
            payload: payload.map_err(serde::ser::Error::custom)?,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ContentPartV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawContentPart::deserialize(deserializer)?;
        if raw.version != CONTENT_VERSION {
            return Ok(Self::Unknown {
                version: raw.version,
                kind: raw.kind,
                payload: raw.payload,
            });
        }
        macro_rules! payload {
            ($type:ty) => {
                serde_json::from_value::<$type>(raw.payload).map_err(serde::de::Error::custom)?
            };
        }
        Ok(match raw.kind.as_str() {
            "text" => {
                let value = payload!(TextPayload);
                Self::Text { text: value.text }
            }
            "markdown" => {
                let value = payload!(MarkdownPayload);
                Self::Markdown {
                    source: value.source,
                }
            }
            "code" => {
                let value = payload!(CodePayload);
                Self::Code {
                    language: value.language,
                    code: value.code,
                }
            }
            "table" => {
                let value = payload!(TablePayload);
                Self::Table {
                    columns: value.columns,
                    rows: value.rows,
                }
            }
            "diff" => {
                let value = payload!(DiffPayload);
                Self::Diff { patch: value.patch }
            }
            "math" => {
                let value = payload!(MathPayload);
                Self::Math {
                    source: value.source,
                    display: value.display,
                }
            }
            "link" => {
                let value = payload!(LinkPayload);
                Self::Link {
                    label: value.label,
                    url: value.url,
                }
            }
            "resource" => Self::Resource(Box::new(payload!(ResourceRefV1))),
            _ => Self::Unknown {
                version: raw.version,
                kind: raw.kind,
                payload: raw.payload,
            },
        })
    }
}

impl ContentPartV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        match self {
            Self::Text { text } | Self::Markdown { source: text } => {
                validate_text("content text", text, MAX_SAFE_TEXT_BYTES)?;
            }
            Self::Code { language, code } => {
                validate_text("code", code, MAX_CODE_BYTES)?;
                if let Some(language) = language {
                    validate_code("code language", language, 64)?;
                }
            }
            Self::Table { columns, rows } => {
                if columns.is_empty() || columns.len() > MAX_TABLE_COLUMNS {
                    return Err(SemanticError::InvalidStructure {
                        field: "table columns",
                        reason: "must contain between 1 and 64 columns",
                    });
                }
                if rows.len() > MAX_TABLE_ROWS {
                    return Err(SemanticError::TooManyValues {
                        field: "table rows",
                        actual: rows.len(),
                        limit: MAX_TABLE_ROWS,
                    });
                }
                for cell in columns.iter().chain(rows.iter().flatten()) {
                    validate_text("table cell", cell, MAX_TABLE_CELL_BYTES)?;
                }
                if rows.iter().any(|row| row.len() != columns.len()) {
                    return Err(SemanticError::InvalidStructure {
                        field: "table row",
                        reason: "must contain exactly one cell per column",
                    });
                }
            }
            Self::Diff { patch } => validate_text("diff", patch, MAX_CODE_BYTES)?,
            Self::Math { source, .. } => validate_text("math", source, MAX_SAFE_TEXT_BYTES)?,
            Self::Link { label, url } => {
                validate_text("link label", label, MAX_LINK_BYTES)?;
                validate_text("link URL", url, MAX_LINK_BYTES)?;
                let parsed = Url::parse(url).map_err(|_| SemanticError::InvalidStructure {
                    field: "link URL",
                    reason: "must be an absolute HTTP or HTTPS URL",
                })?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.username() != ""
                    || parsed.password().is_some()
                {
                    return Err(SemanticError::InvalidStructure {
                        field: "link URL",
                        reason: "must be HTTP(S) and must not contain credentials",
                    });
                }
            }
            Self::Resource(resource) => resource
                .validate()
                .map_err(|error| SemanticError::Resource(error.to_string()))?,
            Self::Unknown {
                version,
                kind,
                payload,
            } => {
                validate_code("unknown content kind", kind, 96)?;
                validate_encoded(payload, MAX_CONTENT_PART_BYTES)?;
                if *version == 0 {
                    return Err(SemanticError::UnsupportedVersion {
                        family: "content",
                        version: *version,
                    });
                }
            }
        }
        validate_encoded(self, MAX_CONTENT_PART_BYTES)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttachmentProvenanceV1 {
    UserSelected,
    DragAndDrop,
    Clipboard,
    Tool,
    ExternalAgent,
    Derived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceOperationV1 {
    Acquire,
    PresentInline,
    Playback,
    OpenExternal,
    ProviderInput,
    FocusedAnalysis,
    Transform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapabilityFactV1 {
    pub(crate) operation: ResourceOperationV1,
    pub(crate) availability: AvailabilityV1,
    #[serde(default)]
    pub(crate) selected: bool,
    #[serde(default)]
    pub(crate) authorized: bool,
    #[serde(default)]
    pub(crate) connection: Option<String>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) effective_max_source_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) reason_code: Option<String>,
    pub(crate) source: FactSourceV1,
    pub(crate) freshness: FreshnessV1,
}

impl CapabilityFactV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        for (field, value) in [
            ("capability connection", self.connection.as_deref()),
            ("capability model", self.model.as_deref()),
        ] {
            if let Some(value) = value {
                validate_text(field, value, MAX_DESTINATION_BYTES)?;
            }
        }
        if let Some(code) = self.reason_code.as_deref() {
            validate_code("capability reason", code, 96)?;
        }
        if self.effective_max_source_bytes == Some(0) {
            return Err(SemanticError::InvalidStructure {
                field: "capability source-byte limit",
                reason: "must not be zero",
            });
        }
        if self
            .effective_max_source_bytes
            .is_some_and(|limit| limit > MAX_RESOURCE_SOURCE_BYTES as u64)
        {
            return Err(SemanticError::InvalidStructure {
                field: "capability source-byte limit",
                reason: "exceeds the compiled resource ceiling",
            });
        }
        if self.selected && (self.connection.is_none() || self.model.is_none()) {
            return Err(SemanticError::InvalidStructure {
                field: "selected resource capability",
                reason: "must name its exact connection and model",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachmentV1 {
    pub(crate) id: Uuid,
    pub(crate) resource: ResourceRefV1,
    pub(crate) provenance: AttachmentProvenanceV1,
    /// A display-only basename or source class, never an ambient path.
    pub(crate) source_label: Option<String>,
    pub(crate) capabilities: Vec<CapabilityFactV1>,
}

impl AttachmentV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        self.resource
            .validate()
            .map_err(|error| SemanticError::Resource(error.to_string()))?;
        if let Some(label) = self.source_label.as_deref() {
            validate_text("attachment source label", label, MAX_SOURCE_LABEL_BYTES)?;
            if label.contains(['/', '\\']) {
                return Err(SemanticError::InvalidStructure {
                    field: "attachment source label",
                    reason: "must not contain an ambient path",
                });
            }
        }
        if self.capabilities.len() > MAX_CAPABILITY_FACTS {
            return Err(SemanticError::TooManyValues {
                field: "attachment capabilities",
                actual: self.capabilities.len(),
                limit: MAX_CAPABILITY_FACTS,
            });
        }
        for capability in &self.capabilities {
            capability.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DisclosureDecisionV1 {
    Approved,
    Denied,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisclosureReceiptV1 {
    pub(crate) id: Uuid,
    pub(crate) resource_id: ArtifactId,
    pub(crate) operation: ResourceOperationV1,
    pub(crate) destination: String,
    pub(crate) connection: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) source_bytes: u64,
    pub(crate) derivative: Option<ArtifactId>,
    pub(crate) decision: DisclosureDecisionV1,
    pub(crate) decided_at_unix_millis: u64,
}

impl DisclosureReceiptV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        validate_code(
            "disclosure destination",
            &self.destination,
            MAX_DESTINATION_BYTES,
        )?;
        for (field, value) in [
            ("disclosure connection", self.connection.as_deref()),
            ("disclosure model", self.model.as_deref()),
        ] {
            if let Some(value) = value {
                validate_text(field, value, MAX_DESTINATION_BYTES)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachmentPolicySnapshotV1 {
    pub(crate) version: u16,
    pub(crate) configured: ResourcePolicyV1,
    pub(crate) route_limit: Option<ResourcePolicyV1>,
}

impl Default for AttachmentPolicySnapshotV1 {
    fn default() -> Self {
        Self {
            version: CONTENT_VERSION,
            configured: ResourcePolicyV1::default(),
            route_limit: None,
        }
    }
}

impl AttachmentPolicySnapshotV1 {
    pub(crate) fn effective(&self) -> Result<ResourcePolicyV1, SemanticError> {
        if self.version != CONTENT_VERSION {
            return Err(SemanticError::UnsupportedVersion {
                family: "attachment policy",
                version: self.version,
            });
        }
        self.configured
            .validate()
            .map_err(|error| SemanticError::Resource(error.to_string()))?;
        match &self.route_limit {
            Some(route) => self
                .configured
                .effective_with(route)
                .map_err(|error| SemanticError::Resource(error.to_string())),
            None => Ok(self.configured.clone()),
        }
    }

    pub(crate) fn validate_attachments(
        &self,
        attachments: &[AttachmentV1],
    ) -> Result<(), SemanticError> {
        for attachment in attachments {
            attachment.validate()?;
        }
        self.effective()?
            .admit_source_lengths(
                attachments
                    .iter()
                    .map(|attachment| attachment.resource.artifact.byte_len),
            )
            .map_err(|error| SemanticError::Resource(error.to_string()))
    }
}

fn validate_encoded<T: Serialize>(value: &T, limit: usize) -> Result<(), SemanticError> {
    let actual = serde_json::to_vec(value)
        .map_err(|_| SemanticError::InvalidStructure {
            field: "semantic value",
            reason: "could not be encoded",
        })?
        .len();
    if actual > limit {
        return Err(SemanticError::PayloadTooLarge { actual, limit });
    }
    Ok(())
}
