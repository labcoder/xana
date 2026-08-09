//! Bounded prompt-context planning.
//!
//! Prompt inputs are transient materializations of separately persisted
//! artifact/context identities. Selection and provenance remain explicit.

pub(crate) mod persisted;
mod preview;
mod source;

use std::{collections::HashSet, error::Error, fmt, io, path::PathBuf, string::FromUtf8Error};

pub(crate) use preview::{canonical_text, estimate_tokens, preview};
#[cfg(test)]
pub(crate) use source::load_project_sources;
pub(crate) use source::{
    MAX_PROJECT_SOURCE_BYTES, PROJECT_INSTRUCTIONS, read_project_instructions,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TransientSourceId(String);

impl TransientSourceId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceOrigin {
    BuiltIn,
    ToolRegistry,
    RuntimeEnvironment,
    ProductDocumentation,
    ProjectFile,
    ParentHandoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustClass {
    Xana,
    Runtime,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProvenance {
    pub(crate) display_name: String,
    pub(crate) path: Option<PathBuf>,
    pub(crate) origin: SourceOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextSource {
    pub(crate) id: TransientSourceId,
    pub(crate) provenance: SourceProvenance,
    pub(crate) trust: TrustClass,
    pub(crate) content: String,
    pub(crate) max_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Explicit preview operations are invoked by later context capabilities.
pub(crate) enum PreviewSelector {
    Head,
    Lines { start: usize, end: usize },
    LiteralSearch { query: String, max_matches: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextPreview {
    pub(crate) source_id: TransientSourceId,
    pub(crate) provenance: SourceProvenance,
    pub(crate) trust: TrustClass,
    pub(crate) selector: PreviewSelector,
    pub(crate) text: String,
    pub(crate) estimated_tokens: usize,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextBudget {
    pub(crate) total_tokens: usize,
    pub(crate) conversation_reserve_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextPlan {
    pub(crate) selected: Vec<ContextPreview>,
    pub(crate) used_tokens: usize,
    pub(crate) omitted_sources: Vec<TransientSourceId>,
}

pub(crate) struct ContextPlanner {
    sources: Vec<ContextSource>,
    budget: ContextBudget,
}

#[derive(Debug)]
#[allow(dead_code)] // Source-loading variants remain covered by compatibility tests.
pub(crate) enum ContextError {
    DuplicateSourceId {
        id: TransientSourceId,
    },
    InvalidSourceId {
        id: TransientSourceId,
    },
    ReservedSourceId {
        id: TransientSourceId,
    },
    InvalidSourceBudget {
        id: TransientSourceId,
    },
    InvalidBudget {
        total: usize,
        reserve: usize,
    },
    ReservationExceedsBudget {
        reserved: usize,
        available: usize,
    },
    InvalidLineRange {
        start: usize,
        end: usize,
    },
    InvalidSearch {
        reason: &'static str,
    },
    SourceMetadata {
        path: PathBuf,
        source: io::Error,
    },
    InvalidSourceKind {
        path: PathBuf,
    },
    SourceRead {
        path: PathBuf,
        source: io::Error,
    },
    SourceTooLarge {
        path: PathBuf,
        limit: usize,
    },
    InvalidUtf8 {
        path: PathBuf,
        source: FromUtf8Error,
    },
}

impl ContextPlanner {
    pub(crate) fn new(
        sources: Vec<ContextSource>,
        budget: ContextBudget,
    ) -> Result<Self, ContextError> {
        if budget.total_tokens < budget.conversation_reserve_tokens {
            return Err(ContextError::InvalidBudget {
                total: budget.total_tokens,
                reserve: budget.conversation_reserve_tokens,
            });
        }

        let mut ids = HashSet::new();
        for source in &sources {
            if source.id.as_str().trim().is_empty() {
                return Err(ContextError::InvalidSourceId {
                    id: source.id.clone(),
                });
            }
            if is_reserved_project_id(source.id.as_str()) {
                return Err(ContextError::ReservedSourceId {
                    id: source.id.clone(),
                });
            }
            if !ids.insert(source.id.clone()) {
                return Err(ContextError::DuplicateSourceId {
                    id: source.id.clone(),
                });
            }
            if source.max_tokens == 0 {
                return Err(ContextError::InvalidSourceBudget {
                    id: source.id.clone(),
                });
            }
        }

        Ok(Self { sources, budget })
    }

    pub(crate) fn plan(&self, reserved_tokens: usize) -> Result<ContextPlan, ContextError> {
        let available = self
            .budget
            .total_tokens
            .saturating_sub(self.budget.conversation_reserve_tokens);
        if reserved_tokens > available {
            return Err(ContextError::ReservationExceedsBudget {
                reserved: reserved_tokens,
                available,
            });
        }

        let mut remaining = available - reserved_tokens;
        let mut used_tokens = 0;
        let mut selected = Vec::new();
        let mut omitted_sources = Vec::new();

        for source in &self.sources {
            let candidate = preview(source, PreviewSelector::Head)?;
            if candidate.estimated_tokens <= remaining {
                remaining -= candidate.estimated_tokens;
                used_tokens += candidate.estimated_tokens;
                selected.push(candidate);
            } else {
                omitted_sources.push(source.id.clone());
            }
        }

        Ok(ContextPlan {
            used_tokens,
            selected,
            omitted_sources,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextPlanReport(String);

impl ContextPlanReport {
    pub(crate) fn render(plan: &ContextPlan) -> Self {
        let mut lines = vec![format!("estimated selected tokens: {}", plan.used_tokens)];
        lines.extend(plan.selected.iter().map(|preview| {
            format!(
                "selected {} ({} estimated tokens, truncated={})",
                preview.source_id.as_str(),
                preview.estimated_tokens,
                preview.truncated
            )
        }));
        lines.extend(
            plan.omitted_sources
                .iter()
                .map(|id| format!("omitted {}", id.as_str())),
        );
        Self(lines.join("\n"))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_reserved_project_id(id: &str) -> bool {
    ["builtin:", "runtime:", "product:", "tools:"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSourceId { id } => {
                write!(f, "context source id {:?} is duplicated", id.as_str())
            }
            Self::InvalidSourceId { id } => {
                write!(f, "context source id {:?} is invalid", id.as_str())
            }
            Self::ReservedSourceId { id } => write!(
                f,
                "project context source id {:?} uses a reserved namespace",
                id.as_str()
            ),
            Self::InvalidSourceBudget { id } => write!(
                f,
                "context source {:?} must have a nonzero token budget",
                id.as_str()
            ),
            Self::InvalidBudget { total, reserve } => write!(
                f,
                "context budget total {total} is smaller than conversation reserve {reserve}"
            ),
            Self::ReservationExceedsBudget {
                reserved,
                available,
            } => write!(
                f,
                "fixed prompt reservation {reserved} exceeds available context budget {available}"
            ),
            Self::InvalidLineRange { start, end } => {
                write!(f, "invalid inclusive line range {start}..={end}")
            }
            Self::InvalidSearch { reason } => write!(f, "invalid context search: {reason}"),
            Self::SourceMetadata { path, source } => {
                write!(
                    f,
                    "could not inspect context source {}: {source}",
                    path.display()
                )
            }
            Self::InvalidSourceKind { path } => write!(
                f,
                "context source {} must be a regular file and not a symlink",
                path.display()
            ),
            Self::SourceRead { path, source } => {
                write!(
                    f,
                    "could not read context source {}: {source}",
                    path.display()
                )
            }
            Self::SourceTooLarge { path, limit } => write!(
                f,
                "context source {} exceeds the {limit}-byte limit",
                path.display()
            ),
            Self::InvalidUtf8 { path, .. } => {
                write!(f, "context source {} is not valid UTF-8", path.display())
            }
        }
    }
}

impl Error for ContextError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceMetadata { source, .. } | Self::SourceRead { source, .. } => Some(source),
            Self::InvalidUtf8 { source, .. } => Some(source),
            Self::DuplicateSourceId { .. }
            | Self::InvalidSourceId { .. }
            | Self::ReservedSourceId { .. }
            | Self::InvalidSourceBudget { .. }
            | Self::InvalidBudget { .. }
            | Self::ReservationExceedsBudget { .. }
            | Self::InvalidLineRange { .. }
            | Self::InvalidSearch { .. }
            | Self::InvalidSourceKind { .. }
            | Self::SourceTooLarge { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
