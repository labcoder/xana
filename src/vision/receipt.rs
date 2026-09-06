//! Bounded vision provenance, shared by durable records and client projections.
//! No image bytes, source paths, credentials, or prompt text belong here.

use crate::identity::{OperationId, SessionId};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisionStatus {
    Dispatching,
    NativeSubmitted,
    AnalysisReady,
    Denied,
    Cancelled,
    ControllerLost,
    Unsupported,
    Unavailable,
    Failed,
    Unknown,
}

/// An ordered immutable source; its digest is content identity, not a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisionSource {
    pub artifact_id: String,
    pub digest: String,
    pub media_type: String,
    pub byte_len: u64,
}

/// Attribution for the exact destination selected before approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisionDestination {
    pub route: Option<String>,
    pub connection: String,
    pub model: String,
    pub adapter: String,
    pub recipient: String,
    pub recipient_digest: String,
}

/// Unknown usage/cost is distinct from a measured zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisionUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
}

/// Immutable evidence for one attempted image turn; this is not replay authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisionReceipt {
    pub version: u16,
    pub conversation_id: String,
    pub operation_id: String,
    pub revision: u64,
    pub plan_digest: String,
    pub prompt_digest: String,
    pub destination: VisionDestination,
    pub sources: Vec<VisionSource>,
    pub status: VisionStatus,
    pub usage: VisionUsage,
    pub derivative: Option<VisionSource>,
    /// Analysis output is always data, never an instruction or authority grant.
    pub untrusted_derivative: bool,
}

impl VisionReceipt {
    pub(crate) fn operation(&self) -> Option<OperationId> {
        self.operation_id.parse().ok()
    }

    pub(crate) fn valid_for(&self, session: SessionId) -> bool {
        self.version == 1
            && self.conversation_id == session.to_string()
            && self.operation().is_some()
            && (1..=2).contains(&self.revision)
            && digest(&self.plan_digest)
            && digest(&self.prompt_digest)
            && (1..=super::MAX_IMAGES_PER_TURN).contains(&self.sources.len())
            && self
                .sources
                .iter()
                .all(|source| valid_source(source) && source.media_type != "text/plain")
            && self.sources.iter().enumerate().all(|(index, source)| {
                !self.sources[..index]
                    .iter()
                    .any(|prior| prior.artifact_id == source.artifact_id)
            })
            && self
                .sources
                .iter()
                .map(|source| source.byte_len)
                .sum::<u64>()
                <= super::MAX_IMAGE_BYTES_PER_TURN
            && self.derivative.as_ref().is_none_or(|source| {
                valid_source(source)
                    && source.media_type == "text/plain"
                    && source.byte_len <= 64 * 1024
            })
            && [
                &self.destination.connection,
                &self.destination.model,
                &self.destination.adapter,
            ]
            .iter()
            .all(|text| {
                !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control)
            })
            && self.destination.route.as_ref().is_none_or(|route| {
                !route.is_empty() && route.len() <= 256 && !route.chars().any(char::is_control)
            })
            && self.destination.recipient.len() <= 2048
            && !self.destination.recipient.chars().any(char::is_control)
            && digest(&self.destination.recipient_digest)
            && self.untrusted_derivative
            && match self.status {
                VisionStatus::AnalysisReady => {
                    self.revision == 2
                        && self.destination.route.is_some()
                        && self.derivative.is_some()
                }
                VisionStatus::NativeSubmitted => {
                    self.revision == 2
                        && self.destination.route.is_none()
                        && self.derivative.is_none()
                        && self.usage == VisionUsage::default()
                }
                VisionStatus::Dispatching => {
                    self.revision == 1
                        && self.derivative.is_none()
                        && self.usage == VisionUsage::default()
                }
                VisionStatus::Unknown => false, // read projection only, never a persisted outcome
                _ => self.derivative.is_none() && self.usage == VisionUsage::default(),
            }
    }
}

/// One immutable attempt has at most an intent and one terminal receipt. Shared
/// by the writer, offline verification and indexed adapter reads.
pub(crate) fn valid_transition(
    previous: Option<&VisionReceipt>,
    next: &VisionReceipt,
    session: SessionId,
) -> bool {
    if !next.valid_for(session) {
        return false;
    }
    let Some(previous) = previous else {
        return next.revision == 1
            && matches!(
                next.status,
                VisionStatus::Dispatching | VisionStatus::Denied | VisionStatus::Cancelled
            );
    };
    if !previous.valid_for(session)
        || previous.revision != 1
        || previous.status != VisionStatus::Dispatching
        || next.revision != 2
        || next.status == VisionStatus::Dispatching
    {
        return false;
    }
    let mut identity = next.clone();
    identity.revision = previous.revision;
    identity.status = previous.status;
    identity.usage = previous.usage.clone();
    identity.derivative = previous.derivative.clone();
    identity == *previous
}

fn valid_source(source: &VisionSource) -> bool {
    source.artifact_id.parse::<uuid::Uuid>().is_ok()
        && digest(&source.digest)
        && source.byte_len <= crate::artifact::MAX_ARTIFACT_BYTES as u64
        && matches!(
            source.media_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "text/plain"
        )
}

fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
