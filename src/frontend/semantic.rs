//! Frontend-neutral semantic values layered over the transport protocol.
//!
//! These values describe what clients may present without granting them
//! runtime authority. Every family is bounded, versioned, provenance-bearing,
//! and safe to inspect when a newer peer sends an unknown kind.

mod activity;
mod content;
mod event;
mod state;
mod usage;

pub(crate) use activity::{
    ActivityItemV1, ApprovalV1, AttentionItemV1, AttentionStateV1, CompletionReceiptV1,
    ExecutionFactsV1,
};
pub(crate) use content::{
    AttachmentPolicySnapshotV1, AttachmentV1, ContentPartV1, DisclosureReceiptV1,
};
pub(crate) use event::SemanticEventEnvelopeV1;
pub(crate) use state::{SemanticDeltaV1, SemanticReplicaV1, SemanticSnapshotV1};
pub(crate) use usage::{
    ContextOccupancyV1, CreditBalanceV1, LimitObservationV1, UsageAccountingV1, UsageAggregateV1,
    UsageAmountsV1, UsageLedgerV1, UsageObservationV1, UsageScopeV1,
};

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

pub(crate) const SEMANTIC_PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_SEMANTIC_CODE_BYTES: usize = 96;
pub(crate) const MAX_SEMANTIC_PARAMETERS: usize = 16;
pub(crate) const MAX_SEMANTIC_PARAMETER_BYTES: usize = 512;
pub(crate) const MAX_SAFE_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FactSourceV1 {
    Runtime,
    Provider,
    ManagedRuntime,
    Mcp,
    A2a,
    Measured,
    Estimated,
    Cache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FactAuthorityV1 {
    Authoritative,
    ProviderReported,
    Measured,
    Estimated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FreshnessV1 {
    pub(crate) observed_at_unix_millis: u64,
    pub(crate) max_age_millis: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AvailabilityV1 {
    Available,
    Stale,
    Unsupported,
    Unavailable { code: String },
    PermissionRequired { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SemanticParamV1 {
    Bool(bool),
    Integer(i64),
    Unsigned(u64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticCodeV1 {
    pub(crate) code: String,
    pub(crate) parameters: BTreeMap<String, SemanticParamV1>,
}

impl SemanticCodeV1 {
    pub(crate) fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            parameters: BTreeMap::new(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        validate_code("semantic code", &self.code, MAX_SEMANTIC_CODE_BYTES)?;
        if self.parameters.len() > MAX_SEMANTIC_PARAMETERS {
            return Err(SemanticError::TooManyValues {
                field: "semantic parameters",
                actual: self.parameters.len(),
                limit: MAX_SEMANTIC_PARAMETERS,
            });
        }
        for (key, value) in &self.parameters {
            validate_code("semantic parameter key", key, MAX_SEMANTIC_CODE_BYTES)?;
            if let SemanticParamV1::Text(value) = value {
                validate_text(
                    "semantic parameter value",
                    value,
                    MAX_SEMANTIC_PARAMETER_BYTES,
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticError {
    UnsupportedVersion {
        family: &'static str,
        version: u16,
    },
    InvalidText {
        field: &'static str,
        reason: &'static str,
    },
    TooManyValues {
        field: &'static str,
        actual: usize,
        limit: usize,
    },
    PayloadTooLarge {
        actual: usize,
        limit: usize,
    },
    InvalidStructure {
        field: &'static str,
        reason: &'static str,
    },
    Resource(String),
    SequenceGap {
        expected: u64,
        actual: u64,
    },
    ArithmeticOverflow(&'static str),
}

impl fmt::Display for SemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { family, version } => {
                write!(formatter, "unsupported {family} version {version}")
            }
            Self::InvalidText { field, reason } => write!(formatter, "{field} {reason}"),
            Self::TooManyValues {
                field,
                actual,
                limit,
            } => write!(formatter, "{field} has {actual} values; limit is {limit}"),
            Self::PayloadTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "semantic payload is {actual} bytes; limit is {limit}"
                )
            }
            Self::InvalidStructure { field, reason } => write!(formatter, "{field} {reason}"),
            Self::Resource(reason) => write!(formatter, "resource is invalid: {reason}"),
            Self::SequenceGap { expected, actual } => write!(
                formatter,
                "semantic sequence gap: expected {expected}, received {actual}"
            ),
            Self::ArithmeticOverflow(field) => write!(formatter, "{field} overflowed"),
        }
    }
}

impl Error for SemanticError {}

pub(super) fn validate_code(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), SemanticError> {
    validate_text(field, value, max_bytes)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-' | b'/')
    }) {
        return Err(SemanticError::InvalidText {
            field,
            reason: "must contain only lowercase ASCII code characters",
        });
    }
    Ok(())
}

pub(super) fn validate_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), SemanticError> {
    if value.trim().is_empty() {
        return Err(SemanticError::InvalidText {
            field,
            reason: "must not be blank",
        });
    }
    if value.len() > max_bytes {
        return Err(SemanticError::InvalidText {
            field,
            reason: "exceeds its byte limit",
        });
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(SemanticError::InvalidText {
            field,
            reason: "contains unsafe control characters",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
