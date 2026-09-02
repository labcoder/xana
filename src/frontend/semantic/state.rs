//! Bounded semantic snapshots and deterministic delta replication.

use super::{
    ActivityItemV1, ApprovalV1, AttachmentPolicySnapshotV1, AttachmentV1, AttentionStateV1,
    CompletionReceiptV1, DisclosureReceiptV1, ExecutionFactsV1, MAX_SAFE_TEXT_BYTES,
    SEMANTIC_PROTOCOL_VERSION, SemanticError, UsageLedgerV1, UsageObservationV1,
    activity::validate_activity_tree,
    event::{
        ConversationLineageV1, DecodedSemanticEventV1, SemanticEventEnvelopeV1, SemanticEventV1,
        SurfaceCapabilityV1, UnknownSemanticV1, validate_content,
    },
    validate_code,
};
use crate::identity::{ConversationId, OperationId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_SEMANTIC_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 32;
const MAX_ATTENTION_ITEMS: usize = 256;
const MAX_APPROVALS: usize = 128;
const MAX_EXECUTION_FACTS: usize = 128;
const MAX_COMPLETION_RECEIPTS: usize = 128;
const MAX_CAPABILITY_FACTS: usize = 512;
const MAX_DISCLOSURE_RECEIPTS: usize = 256;
const MAX_UNKNOWN_ITEMS: usize = 64;
const MAX_UNKNOWN_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_USAGE_OBSERVATIONS: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticDeltaV1 {
    pub(crate) sequence: u64,
    pub(crate) event: SemanticEventEnvelopeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SemanticSnapshotV1 {
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) conversation_id: Option<ConversationId>,
    pub(crate) lineage: Option<ConversationLineageV1>,
    pub(crate) content: Vec<super::ContentPartV1>,
    pub(crate) authoritative_finals: BTreeMap<OperationId, Vec<super::ContentPartV1>>,
    pub(crate) attachments: Vec<AttachmentV1>,
    pub(crate) attachment_policy: AttachmentPolicySnapshotV1,
    pub(crate) activity: Vec<ActivityItemV1>,
    pub(crate) attention: AttentionStateV1,
    pub(crate) usage: Vec<UsageObservationV1>,
    pub(crate) approvals: Vec<ApprovalV1>,
    pub(crate) execution_facts: Vec<ExecutionFactsV1>,
    pub(crate) completion_receipts: Vec<CompletionReceiptV1>,
    pub(crate) capabilities: Vec<SurfaceCapabilityV1>,
    pub(crate) disclosures: Vec<DisclosureReceiptV1>,
    pub(crate) unknown: Vec<UnknownSemanticV1>,
}

impl Default for SemanticSnapshotV1 {
    fn default() -> Self {
        Self {
            version: SEMANTIC_PROTOCOL_VERSION,
            sequence: 0,
            conversation_id: None,
            lineage: None,
            content: Vec::new(),
            authoritative_finals: BTreeMap::new(),
            attachments: Vec::new(),
            attachment_policy: AttachmentPolicySnapshotV1::default(),
            activity: Vec::new(),
            attention: AttentionStateV1::default(),
            usage: Vec::new(),
            approvals: Vec::new(),
            execution_facts: Vec::new(),
            completion_receipts: Vec::new(),
            capabilities: Vec::new(),
            disclosures: Vec::new(),
            unknown: Vec::new(),
        }
    }
}

impl SemanticSnapshotV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        if self.version != SEMANTIC_PROTOCOL_VERSION {
            return Err(SemanticError::UnsupportedVersion {
                family: "semantic snapshot",
                version: self.version,
            });
        }
        if let Some(lineage) = &self.lineage {
            lineage.validate()?;
        }
        validate_content(&self.content)?;
        for content in self.authoritative_finals.values() {
            validate_content(content)?;
        }
        check_count("attachments", self.attachments.len(), MAX_ATTACHMENTS)?;
        self.attachment_policy
            .validate_attachments(&self.attachments)?;
        validate_activity_tree(&self.activity)?;
        check_count(
            "attention items",
            self.attention.values().count(),
            MAX_ATTENTION_ITEMS,
        )?;
        for attention in self.attention.values() {
            attention.message.validate()?;
        }
        check_count(
            "usage observations",
            self.usage.len(),
            MAX_USAGE_OBSERVATIONS,
        )?;
        let mut ledger = UsageLedgerV1::default();
        for usage in &self.usage {
            ledger.observe(usage.clone())?;
        }
        check_count("approvals", self.approvals.len(), MAX_APPROVALS)?;
        for approval in &self.approvals {
            approval.validate()?;
        }
        check_count(
            "execution facts",
            self.execution_facts.len(),
            MAX_EXECUTION_FACTS,
        )?;
        for facts in &self.execution_facts {
            facts.validate()?;
        }
        check_count(
            "completion receipts",
            self.completion_receipts.len(),
            MAX_COMPLETION_RECEIPTS,
        )?;
        for receipt in &self.completion_receipts {
            receipt.validate()?;
        }
        check_count(
            "capabilities",
            self.capabilities.len(),
            MAX_CAPABILITY_FACTS,
        )?;
        for capability in &self.capabilities {
            capability.validate()?;
        }
        check_count(
            "disclosure receipts",
            self.disclosures.len(),
            MAX_DISCLOSURE_RECEIPTS,
        )?;
        for disclosure in &self.disclosures {
            disclosure.validate()?;
        }
        check_count("unknown events", self.unknown.len(), MAX_UNKNOWN_ITEMS)?;
        for unknown in &self.unknown {
            validate_code("unknown event kind", &unknown.kind, 96)?;
            let bytes = serde_json::to_vec(&unknown.payload).map_err(|_| {
                SemanticError::InvalidStructure {
                    field: "unknown event payload",
                    reason: "could not be encoded",
                }
            })?;
            if bytes.len() > MAX_UNKNOWN_PAYLOAD_BYTES {
                return Err(SemanticError::PayloadTooLarge {
                    actual: bytes.len(),
                    limit: MAX_UNKNOWN_PAYLOAD_BYTES,
                });
            }
        }
        let bytes = serde_json::to_vec(self).map_err(|_| SemanticError::InvalidStructure {
            field: "semantic snapshot",
            reason: "could not be encoded",
        })?;
        if bytes.len() > MAX_SEMANTIC_SNAPSHOT_BYTES {
            return Err(SemanticError::PayloadTooLarge {
                actual: bytes.len(),
                limit: MAX_SEMANTIC_SNAPSHOT_BYTES,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticReplicaV1 {
    snapshot: SemanticSnapshotV1,
    usage: UsageLedgerV1,
    needs_fresh_snapshot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyDeltaResult {
    Applied,
    Duplicate,
    NeedsFreshSnapshot,
}

impl SemanticReplicaV1 {
    pub(crate) fn from_snapshot(snapshot: SemanticSnapshotV1) -> Result<Self, SemanticError> {
        snapshot.validate()?;
        let mut usage = UsageLedgerV1::default();
        for observation in &snapshot.usage {
            usage.observe(observation.clone())?;
        }
        Ok(Self {
            snapshot,
            usage,
            needs_fresh_snapshot: false,
        })
    }

    pub(crate) fn snapshot(&self) -> &SemanticSnapshotV1 {
        &self.snapshot
    }

    pub(crate) fn install_snapshot(
        &mut self,
        snapshot: SemanticSnapshotV1,
    ) -> Result<(), SemanticError> {
        *self = Self::from_snapshot(snapshot)?;
        Ok(())
    }

    pub(crate) fn apply(
        &mut self,
        delta: SemanticDeltaV1,
    ) -> Result<ApplyDeltaResult, SemanticError> {
        if delta.sequence <= self.snapshot.sequence {
            return Ok(ApplyDeltaResult::Duplicate);
        }
        if self.needs_fresh_snapshot {
            return Ok(ApplyDeltaResult::NeedsFreshSnapshot);
        }
        let expected = self.snapshot.sequence.saturating_add(1);
        if delta.sequence != expected {
            self.needs_fresh_snapshot = true;
            return Ok(ApplyDeltaResult::NeedsFreshSnapshot);
        }
        match delta.event.decode()? {
            DecodedSemanticEventV1::Known(event) => self.apply_event(*event)?,
            DecodedSemanticEventV1::Unknown(unknown) => {
                if self.snapshot.unknown.len() == MAX_UNKNOWN_ITEMS {
                    self.snapshot.unknown.remove(0);
                }
                self.snapshot.unknown.push(unknown);
            }
        }
        self.snapshot.sequence = delta.sequence;
        self.snapshot.validate()?;
        Ok(ApplyDeltaResult::Applied)
    }

    fn apply_event(&mut self, event: SemanticEventV1) -> Result<(), SemanticError> {
        match event {
            SemanticEventV1::ContentAppended { parts, .. } => {
                self.snapshot.content.extend(parts);
            }
            SemanticEventV1::FinalContent { run_id, parts } => {
                self.snapshot.authoritative_finals.insert(run_id, parts);
            }
            SemanticEventV1::AttachmentUpserted { attachment } => {
                upsert_by(&mut self.snapshot.attachments, attachment, |value| value.id)
            }
            SemanticEventV1::ActivityUpserted { activity } => {
                upsert_by(&mut self.snapshot.activity, activity, |value| value.id);
            }
            SemanticEventV1::ProgressTextDelta { activity_id, delta } => {
                let activity = self
                    .snapshot
                    .activity
                    .iter_mut()
                    .find(|activity| activity.id == activity_id)
                    .ok_or(SemanticError::InvalidStructure {
                        field: "progress activity",
                        reason: "must refer to an existing activity item",
                    })?;
                let text = activity.disclosed_text.get_or_insert_with(String::new);
                if text.len().saturating_add(delta.len()) > MAX_SAFE_TEXT_BYTES {
                    return Err(SemanticError::PayloadTooLarge {
                        actual: text.len().saturating_add(delta.len()),
                        limit: MAX_SAFE_TEXT_BYTES,
                    });
                }
                text.push_str(&delta);
            }
            SemanticEventV1::AttentionUpserted { attention } => {
                self.snapshot.attention.upsert(attention)?;
            }
            SemanticEventV1::AttentionAcknowledged {
                attention_id,
                at_unix_millis,
            } => {
                if !self
                    .snapshot
                    .attention
                    .acknowledge(attention_id, at_unix_millis)
                {
                    return Err(SemanticError::InvalidStructure {
                        field: "attention acknowledgement",
                        reason: "must refer to an existing attention item",
                    });
                }
            }
            SemanticEventV1::UsageObserved { observation } => {
                if self.usage.observe(observation.clone())? {
                    self.snapshot.usage.push(observation);
                }
            }
            SemanticEventV1::ApprovalUpserted { approval } => {
                upsert_by(&mut self.snapshot.approvals, approval, |value| {
                    value.invocation_id
                })
            }
            SemanticEventV1::ExecutionFactsUpserted { facts } => {
                upsert_by(&mut self.snapshot.execution_facts, facts, |value| {
                    value.run_id
                })
            }
            SemanticEventV1::CompletionUpserted { receipt } => {
                upsert_by(&mut self.snapshot.completion_receipts, receipt, |value| {
                    value.id
                })
            }
            SemanticEventV1::CapabilityCatalogReplaced { mut capabilities } => {
                capabilities.sort_by(|left, right| left.id.cmp(&right.id));
                capabilities.dedup_by(|left, right| left.id == right.id);
                self.snapshot.capabilities = capabilities;
            }
            SemanticEventV1::DisclosureRecorded { receipt } => {
                upsert_by(&mut self.snapshot.disclosures, receipt, |value| value.id)
            }
        }
        Ok(())
    }
}

fn check_count(field: &'static str, actual: usize, limit: usize) -> Result<(), SemanticError> {
    if actual > limit {
        return Err(SemanticError::TooManyValues {
            field,
            actual,
            limit,
        });
    }
    Ok(())
}

fn upsert_by<T, K>(values: &mut Vec<T>, value: T, key: impl Fn(&T) -> K)
where
    K: PartialEq,
{
    if let Some(existing) = values
        .iter_mut()
        .find(|existing| key(existing) == key(&value))
    {
        *existing = value;
    } else {
        values.push(value);
    }
}
