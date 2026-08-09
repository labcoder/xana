//! Explicit, bounded documentation catalog for Xana itself.
//!
//! The catalog is compiled from known files. It never walks the checkout or
//! treats a user path as a logical document id.

use std::{fmt, ops::Range};

use serde::Serialize;

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
static AUTOMATION_TOPICS: &[&str] = &["usage", "cli", "automation", "json", "sessions"];
static TUI_TOPICS: &[&str] = &[
    "usage",
    "cli",
    "tui",
    "commands",
    "composer",
    "accessibility",
];
static ARCH_TOPICS: &[&str] = &["architecture", "runtime", "boundaries"];
static MODEL_TOPICS: &[&str] = &["models", "providers", "credentials", "codex"];
static ORCHESTRATION_TOPICS: &[&str] = &["agents", "delegation", "orchestration", "routes"];
static PRINCIPLES_TOPICS: &[&str] = &["principles", "safety", "architecture"];
static PROPOSAL_TOPICS: &[&str] = &["proposal", "future"];

static ENTRIES: &[BundledDoc] = &[
    BundledDoc {
        id: "architecture.connections",
        title: "Connections, models, and managed runtimes",
        audience: CONTRIBUTOR_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: MODEL_TOPICS,
        body: include_str!("../docs/architecture/models-and-managed-runtimes.md"),
    },
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
        id: "architecture.providers",
        title: "Provider contracts",
        audience: CONTRIBUTOR_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: MODEL_TOPICS,
        body: include_str!("../docs/architecture/providers.md"),
    },
    BundledDoc {
        id: "architecture.vision",
        title: "Image input and media resolution",
        audience: CONTRIBUTOR_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: &["images", "artifacts", "providers"],
        body: include_str!("../docs/architecture/vision.md"),
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
        id: "user.automation",
        title: "Terminal and one-shot modes",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: AUTOMATION_TOPICS,
        body: include_str!("../docs/user/automation.md"),
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
        id: "user.orchestration",
        title: "Child orchestration",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: ORCHESTRATION_TOPICS,
        body: include_str!("../docs/user/orchestration.md"),
    },
    BundledDoc {
        id: "user.orchestration-plans",
        title: "Orchestration plans",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: ORCHESTRATION_TOPICS,
        body: include_str!("../docs/user/orchestration-plans.md"),
    },
    BundledDoc {
        id: "user.presentation",
        title: "Terminal presentation",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: &["terminal", "theme", "color", "accessibility", "preferences"],
        body: include_str!("../docs/user/presentation.md"),
    },
    BundledDoc {
        id: "user.rich-content",
        title: "Rich terminal content and artifacts",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: &["tui", "markdown", "artifacts", "images", "links", "safety"],
        body: include_str!("../docs/user/rich-content.md"),
    },
    BundledDoc {
        id: "user.sessions",
        title: "Sessions",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: &["sessions", "resume", "workspace", "tui", "navigation"],
        body: include_str!("../docs/user/sessions.md"),
    },
    BundledDoc {
        id: "user.tui",
        title: "Full-screen terminal UI",
        audience: USER_AUDIENCE,
        authority: DocAuthority::Descriptive,
        status: DocStatus::Shipped,
        topics: TUI_TOPICS,
        body: include_str!("../docs/user/tui.md"),
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
