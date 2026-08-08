//! Explicit, bounded documentation catalog for Xana itself.
//!
//! The catalog is compiled from known files. It never walks the checkout or
//! treats a user path as a logical document id.

use std::{fmt, ops::Range};

use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::json;
use xana_core::{LogicalToolId, Tool, ToolDefinition};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocAudience {
    User,
    Contributor,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocAuthority {
    Descriptive,
    Prescriptive,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DocStatus {
    Shipped,
    Accepted,
    Proposed,
    Historical,
}

#[derive(Debug, Clone, Copy)]
pub struct BundledDoc {
    pub id: &'static str,
    pub title: &'static str,
    pub audience: &'static [DocAudience],
    pub authority: DocAuthority,
    pub status: DocStatus,
    pub topics: &'static [&'static str],
    pub body: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocSummary {
    pub id: &'static str,
    pub title: &'static str,
    pub status: DocStatus,
    pub topics: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocRange {
    pub start: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedDoc {
    pub id: &'static str,
    pub text: String,
    pub range: Range<usize>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocReadError {
    UnknownId,
    InvalidRange,
    NotUtf8Boundary,
    OutputLimit,
}

impl fmt::Display for DocReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownId => f.write_str("unknown Xana document id"),
            Self::InvalidRange => f.write_str("document range is invalid"),
            Self::NotUtf8Boundary => f.write_str("document range must use UTF-8 boundaries"),
            Self::OutputLimit => f.write_str("document read exceeds the catalog output limit"),
        }
    }
}
impl std::error::Error for DocReadError {}

#[derive(Debug, Clone, Copy)]
pub struct ProductDocCatalog {
    product_version: &'static str,
    entries: &'static [BundledDoc],
    max_read_bytes: usize,
}

impl ProductDocCatalog {
    pub const fn new(
        product_version: &'static str,
        entries: &'static [BundledDoc],
        max_read_bytes: usize,
    ) -> Self {
        Self {
            product_version,
            entries,
            max_read_bytes,
        }
    }
    pub fn product_version(&self) -> &str {
        self.product_version
    }
    pub fn entries(&self) -> &[BundledDoc] {
        self.entries
    }
    pub fn list(&self, topic: Option<&str>) -> Vec<DocSummary> {
        let mut entries = self
            .entries
            .iter()
            .filter(|entry| topic.is_none_or(|topic| entry.topics.contains(&topic)))
            .map(|entry| DocSummary {
                id: entry.id,
                title: entry.title,
                status: entry.status,
                topics: entry.topics,
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.id);
        entries
    }
    pub fn read(&self, id: &str, range: Option<DocRange>) -> Result<BoundedDoc, DocReadError> {
        if id.starts_with('/') || id.contains("..") || id.contains('\\') {
            return Err(DocReadError::UnknownId);
        }
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or(DocReadError::UnknownId)?;
        let requested = range.unwrap_or(DocRange {
            start: 0,
            max_bytes: self.max_read_bytes,
        });
        if requested.max_bytes == 0 || requested.max_bytes > self.max_read_bytes {
            return Err(DocReadError::OutputLimit);
        }
        if requested.start > entry.body.len() {
            return Err(DocReadError::InvalidRange);
        }
        if !entry.body.is_char_boundary(requested.start) {
            return Err(DocReadError::NotUtf8Boundary);
        }
        let end = requested
            .start
            .saturating_add(requested.max_bytes)
            .min(entry.body.len());
        let end = (requested.start..=end)
            .rev()
            .find(|position| entry.body.is_char_boundary(*position))
            .ok_or(DocReadError::NotUtf8Boundary)?;
        let text = entry.body[requested.start..end].to_owned();
        Ok(BoundedDoc {
            id: entry.id,
            text,
            range: requested.start..end,
            truncated: end < entry.body.len(),
        })
    }
}

static USER_AUDIENCE: &[DocAudience] = &[DocAudience::User, DocAudience::Agent];
static CONTRIBUTOR_AUDIENCE: &[DocAudience] = &[DocAudience::Contributor, DocAudience::Agent];
static USER_TOPICS: &[&str] = &["configuration", "installation", "usage"];
static ARCH_TOPICS: &[&str] = &["architecture", "runtime", "boundaries"];
static PRINCIPLES_TOPICS: &[&str] = &["principles", "safety", "architecture"];
static PROPOSAL_TOPICS: &[&str] = &["proposal", "future"];

static ENTRIES: &[BundledDoc] = &[
    BundledDoc {
        id: "architecture.overview",
        title: "Xana architecture",
        audience: CONTRIBUTOR_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: ARCH_TOPICS,
        body: include_str!("../docs/architecture/README.md"),
    },
    BundledDoc {
        id: "engineering.principles",
        title: "Xana design principles",
        audience: CONTRIBUTOR_AUDIENCE,
        authority: DocAuthority::Prescriptive,
        status: DocStatus::Shipped,
        topics: PRINCIPLES_TOPICS,
        body: include_str!("../docs/principles.md"),
    },
    BundledDoc {
        id: "user.configuration",
        title: "Configuration",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: USER_TOPICS,
        body: include_str!("../docs/user/configuration.md"),
    },
    BundledDoc {
        id: "user.installation",
        title: "Installation",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: &["installation", "updates"],
        body: include_str!("../docs/user/installation.md"),
    },
    BundledDoc {
        id: "proposal.media",
        title: "Media and document services",
        audience: CONTRIBUTOR_AUDIENCE,
        authority: DocAuthority::None,
        status: DocStatus::Proposed,
        topics: PROPOSAL_TOPICS,
        body: include_str!("../docs/proposals/0006-media-and-document-services.md"),
    },
];

pub fn default_catalog() -> ProductDocCatalog {
    ProductDocCatalog::new(env!("CARGO_PKG_VERSION"), ENTRIES, 32 * 1024)
}

/// One bounded model-facing tool for the explicit product catalog.
#[derive(Debug, Clone, Copy)]
pub struct XanaDocsTool {
    catalog: ProductDocCatalog,
}

impl XanaDocsTool {
    pub fn new(catalog: ProductDocCatalog) -> Self {
        Self { catalog }
    }
}

impl Tool for XanaDocsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: LogicalToolId::parse("xana_docs").expect("static tool id"),
            contract_version: 1,
            description: "Read bounded documentation about Xana itself; this is not the user's project documentation.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["list", "read"] },
                    "id": { "type": "string" },
                    "topic": { "type": "string" },
                    "start": { "type": "integer", "minimum": 0 },
                    "max_bytes": { "type": "integer", "minimum": 1 }
                },
                "required": ["op"]
            }),
            effect_class: xana_core::EffectClass::Read,
            replay_safety: xana_core::ReplaySafety::Safe,
        }
    }

    fn invoke<'a>(
        &'a self,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let op = arguments
                .get("op")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "xana_docs requires op=list or op=read".to_owned())?;
            match op {
                "list" => serde_json::to_string(
                    &self
                        .catalog
                        .list(arguments.get("topic").and_then(serde_json::Value::as_str)),
                )
                .map_err(|error| error.to_string()),
                "read" => {
                    let id = arguments
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| "xana_docs read requires id".to_owned())?;
                    let range = arguments
                        .get("start")
                        .or_else(|| arguments.get("max_bytes"))
                        .map(|_| DocRange {
                            start: arguments
                                .get("start")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0) as usize,
                            max_bytes: arguments
                                .get("max_bytes")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(32 * 1024)
                                as usize,
                        });
                    let document = self
                        .catalog
                        .read(id, range)
                        .map_err(|error| error.to_string())?;
                    serde_json::to_string(&json!({ "id": document.id, "text": document.text, "start": document.range.start, "end": document.range.end, "truncated": document.truncated })).map_err(|error| error.to_string())
                }
                _ => Err(format!("unsupported xana_docs operation {op:?}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_explicit_sorted_and_versioned() {
        let catalog = default_catalog();
        let ids = catalog
            .list(None)
            .into_iter()
            .map(|summary| summary.id)
            .collect::<Vec<_>>();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(catalog.product_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn proposed_entry_has_no_prescriptive_authority() {
        let catalog = default_catalog();
        let proposal = catalog
            .entries()
            .iter()
            .find(|entry| entry.status == DocStatus::Proposed)
            .unwrap();
        assert_eq!(proposal.authority, DocAuthority::None);
    }

    #[test]
    fn reads_are_bounded_and_traversal_free() {
        let catalog = default_catalog();
        let doc = catalog
            .read(
                "user.configuration",
                Some(DocRange {
                    start: 0,
                    max_bytes: 32,
                }),
            )
            .unwrap();
        assert!(doc.truncated);
        for id in ["../README.md", "/tmp/x", "unknown"] {
            assert_eq!(catalog.read(id, None), Err(DocReadError::UnknownId));
        }
    }
}
