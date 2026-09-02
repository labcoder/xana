//! Versioned semantic event envelopes and forward-compatible decoding.

use super::{
    ActivityItemV1, ApprovalV1, AttachmentV1, AttentionItemV1, AvailabilityV1, CompletionReceiptV1,
    DisclosureReceiptV1, ExecutionFactsV1, FactSourceV1, FreshnessV1, SEMANTIC_PROTOCOL_VERSION,
    SemanticError, UsageObservationV1, validate_code, validate_text,
};
use crate::identity::{ConversationEntryId, ConversationId, OperationId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const MAX_SEMANTIC_EVENT_BYTES: usize = 1024 * 1024;
const MAX_CONTENT_PARTS: usize = 512;
const MAX_CAPABILITY_FACTS: usize = 512;
const MAX_UNKNOWN_PAYLOAD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SubmissionOriginV1 {
    Interactive,
    Automation { request_id: Uuid },
    VoiceAdapter { adapter: String, request_id: Uuid },
}

impl SubmissionOriginV1 {
    pub(super) fn validate(&self) -> Result<(), SemanticError> {
        if let Self::VoiceAdapter { adapter, .. } = self {
            validate_code("voice adapter", adapter, 96)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationLineageV1 {
    pub(crate) source_conversation_id: ConversationId,
    pub(crate) source_entry_id: Option<ConversationEntryId>,
    pub(crate) continuation: String,
}

impl ConversationLineageV1 {
    pub(super) fn validate(&self) -> Result<(), SemanticError> {
        validate_code("conversation continuation", &self.continuation, 96)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SurfaceCapabilityV1 {
    pub(crate) id: String,
    pub(crate) availability: AvailabilityV1,
    pub(crate) selected: bool,
    pub(crate) authorized: bool,
    pub(crate) source: FactSourceV1,
    pub(crate) freshness: FreshnessV1,
}

impl SurfaceCapabilityV1 {
    pub(super) fn validate(&self) -> Result<(), SemanticError> {
        validate_code("surface capability", &self.id, 128)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SemanticEventV1 {
    ContentAppended {
        parts: Vec<super::ContentPartV1>,
        origin: SubmissionOriginV1,
    },
    FinalContent {
        run_id: OperationId,
        parts: Vec<super::ContentPartV1>,
    },
    AttachmentUpserted {
        attachment: AttachmentV1,
    },
    ActivityUpserted {
        activity: ActivityItemV1,
    },
    ProgressTextDelta {
        activity_id: Uuid,
        delta: String,
    },
    AttentionUpserted {
        attention: AttentionItemV1,
    },
    AttentionAcknowledged {
        attention_id: Uuid,
        at_unix_millis: u64,
    },
    UsageObserved {
        observation: UsageObservationV1,
    },
    ApprovalUpserted {
        approval: ApprovalV1,
    },
    ExecutionFactsUpserted {
        facts: ExecutionFactsV1,
    },
    CompletionUpserted {
        receipt: CompletionReceiptV1,
    },
    CapabilityCatalogReplaced {
        capabilities: Vec<SurfaceCapabilityV1>,
    },
    DisclosureRecorded {
        receipt: DisclosureReceiptV1,
    },
}

impl SemanticEventV1 {
    fn kind(&self) -> &'static str {
        match self {
            Self::ContentAppended { .. } => "content_appended",
            Self::FinalContent { .. } => "final_content",
            Self::AttachmentUpserted { .. } => "attachment_upserted",
            Self::ActivityUpserted { .. } => "activity_upserted",
            Self::ProgressTextDelta { .. } => "progress_text_delta",
            Self::AttentionUpserted { .. } => "attention_upserted",
            Self::AttentionAcknowledged { .. } => "attention_acknowledged",
            Self::UsageObserved { .. } => "usage_observed",
            Self::ApprovalUpserted { .. } => "approval_upserted",
            Self::ExecutionFactsUpserted { .. } => "execution_facts_upserted",
            Self::CompletionUpserted { .. } => "completion_upserted",
            Self::CapabilityCatalogReplaced { .. } => "capability_catalog_replaced",
            Self::DisclosureRecorded { .. } => "disclosure_recorded",
        }
    }

    fn validate(&self) -> Result<(), SemanticError> {
        match self {
            Self::ContentAppended { parts, origin } => {
                validate_content(parts)?;
                origin.validate()?;
            }
            Self::FinalContent { parts, .. } => validate_content(parts)?,
            Self::AttachmentUpserted { attachment } => attachment.validate()?,
            Self::ActivityUpserted { activity } => activity.validate()?,
            Self::ProgressTextDelta { delta, .. } => {
                validate_text("progress delta", delta, 16 * 1024)?;
            }
            Self::AttentionUpserted { attention } => attention.message.validate()?,
            Self::UsageObserved { observation } => observation.validate()?,
            Self::ApprovalUpserted { approval } => approval.validate()?,
            Self::ExecutionFactsUpserted { facts } => facts.validate()?,
            Self::CompletionUpserted { receipt } => receipt.validate()?,
            Self::CapabilityCatalogReplaced { capabilities } => {
                if capabilities.len() > MAX_CAPABILITY_FACTS {
                    return Err(SemanticError::TooManyValues {
                        field: "surface capabilities",
                        actual: capabilities.len(),
                        limit: MAX_CAPABILITY_FACTS,
                    });
                }
                for capability in capabilities {
                    capability.validate()?;
                }
            }
            Self::DisclosureRecorded { receipt } => receipt.validate()?,
            Self::AttentionAcknowledged { .. } => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticEventEnvelopeV1 {
    pub(crate) version: u16,
    pub(crate) kind: String,
    pub(crate) payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnknownSemanticV1 {
    pub(crate) version: u16,
    pub(crate) kind: String,
    pub(crate) payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodedSemanticEventV1 {
    Known(Box<SemanticEventV1>),
    Unknown(UnknownSemanticV1),
}

impl SemanticEventEnvelopeV1 {
    pub(crate) fn encode(event: SemanticEventV1) -> Result<Self, SemanticError> {
        event.validate()?;
        let envelope = Self {
            version: SEMANTIC_PROTOCOL_VERSION,
            kind: event.kind().to_owned(),
            payload: serde_json::to_value(event).map_err(|_| SemanticError::InvalidStructure {
                field: "semantic event",
                reason: "could not be encoded",
            })?,
        };
        envelope.validate_size()?;
        Ok(envelope)
    }

    pub(crate) fn decode(&self) -> Result<DecodedSemanticEventV1, SemanticError> {
        self.validate_size()?;
        validate_code("semantic event kind", &self.kind, 96)?;
        if self.version != SEMANTIC_PROTOCOL_VERSION || !is_known_kind(&self.kind) {
            if serde_json::to_vec(&self.payload)
                .map_or(true, |bytes| bytes.len() > MAX_UNKNOWN_PAYLOAD_BYTES)
            {
                return Err(SemanticError::PayloadTooLarge {
                    actual: serde_json::to_vec(&self.payload)
                        .map(|bytes| bytes.len())
                        .unwrap_or(usize::MAX),
                    limit: MAX_UNKNOWN_PAYLOAD_BYTES,
                });
            }
            return Ok(DecodedSemanticEventV1::Unknown(UnknownSemanticV1 {
                version: self.version,
                kind: self.kind.clone(),
                payload: self.payload.clone(),
            }));
        }
        let event =
            serde_json::from_value::<SemanticEventV1>(self.payload.clone()).map_err(|_| {
                SemanticError::InvalidStructure {
                    field: "semantic event payload",
                    reason: "does not match its known kind",
                }
            })?;
        if event.kind() != self.kind {
            return Err(SemanticError::InvalidStructure {
                field: "semantic event kind",
                reason: "does not match the payload",
            });
        }
        event.validate()?;
        Ok(DecodedSemanticEventV1::Known(Box::new(event)))
    }

    fn validate_size(&self) -> Result<(), SemanticError> {
        let actual = serde_json::to_vec(self)
            .map_err(|_| SemanticError::InvalidStructure {
                field: "semantic event",
                reason: "could not be encoded",
            })?
            .len();
        if actual > MAX_SEMANTIC_EVENT_BYTES {
            return Err(SemanticError::PayloadTooLarge {
                actual,
                limit: MAX_SEMANTIC_EVENT_BYTES,
            });
        }
        Ok(())
    }
}

fn is_known_kind(kind: &str) -> bool {
    matches!(
        kind,
        "content_appended"
            | "final_content"
            | "attachment_upserted"
            | "activity_upserted"
            | "progress_text_delta"
            | "attention_upserted"
            | "attention_acknowledged"
            | "usage_observed"
            | "approval_upserted"
            | "execution_facts_upserted"
            | "completion_upserted"
            | "capability_catalog_replaced"
            | "disclosure_recorded"
    )
}

pub(super) fn validate_content(parts: &[super::ContentPartV1]) -> Result<(), SemanticError> {
    if parts.len() > MAX_CONTENT_PARTS {
        return Err(SemanticError::TooManyValues {
            field: "content parts",
            actual: parts.len(),
            limit: MAX_CONTENT_PARTS,
        });
    }
    for part in parts {
        part.validate()?;
    }
    Ok(())
}
