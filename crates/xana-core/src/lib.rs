//! Headless Xana capability contracts.
//!
//! This crate deliberately has no filesystem, terminal, HTTP, process, or
//! provider-wire dependencies. It owns stable capability identities and the
//! immutable metadata snapshot consumed by runtime composition.

use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityId(String);

impl CapabilityId {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || byte == b'-'
                            || byte == b'_'
                    })
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(IdError(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalToolId(String);

impl LogicalToolId {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdError> {
        CapabilityId::parse(value.into()).map(|id| Self(id.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LogicalToolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError(String);

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid stable id {:?}", self.0)
    }
}

impl std::error::Error for IdError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCapabilitySnapshot {
    capabilities: BTreeSet<CapabilityId>,
    tools: BTreeSet<LogicalToolId>,
}

impl AgentCapabilitySnapshot {
    pub fn new(capabilities: BTreeSet<CapabilityId>, tools: BTreeSet<LogicalToolId>) -> Self {
        Self {
            capabilities,
            tools,
        }
    }

    pub fn contains(&self, id: &CapabilityId) -> bool {
        self.capabilities.contains(id)
    }

    pub fn capabilities(&self) -> &BTreeSet<CapabilityId> {
        &self.capabilities
    }

    pub fn tool_ids(&self) -> &BTreeSet<LogicalToolId> {
        &self.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_ambiguous_or_unstable_spellings() {
        for invalid in ["", ".read", "fs..read", "FS.read", "fs/read", "fs read"] {
            assert!(
                CapabilityId::parse(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert_eq!(
            CapabilityId::parse("fs.read").expect("valid id").as_str(),
            "fs.read"
        );
        assert_eq!(
            LogicalToolId::parse("read_file")
                .expect("valid tool id")
                .as_str(),
            "read_file"
        );
    }

    #[test]
    fn snapshot_keeps_capability_and_tool_metadata_distinct() {
        let capability = CapabilityId::parse("fs.read").expect("valid capability");
        let tool = LogicalToolId::parse("read_file").expect("valid tool");
        let snapshot = AgentCapabilitySnapshot::new(
            [capability.clone()].into_iter().collect(),
            [tool.clone()].into_iter().collect(),
        );

        assert!(snapshot.contains(&capability));
        assert_eq!(snapshot.tool_ids(), &[tool].into_iter().collect());
    }
}
