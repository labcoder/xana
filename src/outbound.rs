//! One bounded authorization gate for data leaving Xana.
//!
//! Transports receive payload bytes only after connection, user, profile, and
//! conversation ceilings have been intersected and an exact recipient/class
//! decision has been resolved. Review and audit values intentionally contain
//! metadata and content digests, never the selected content itself.

#![allow(dead_code)] // The shared gate is consumed incrementally by later M3 transports.

use crate::{
    config::OutboundDataClass,
    identity::OperationId,
    paths::XanaPaths,
    private_state::{
        OutboundAuditDocument, OutboundAuditRecord, OutboundDecisionDocument,
        OutboundDecisionRecord, PrivateStateError, SavedOutboundDecision, UpdateDocumentError,
        ensure_interoperable_records, read_document, update_document,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_ITEMS: usize = 64;
const MAX_ITEM_BYTES: usize = 4 * 1024 * 1024;
// One vision turn may contain several immutable images whose aggregate input
// is bounded separately at 20 MiB. The common egress gate must be able to
// review that complete, already-bounded request rather than forcing a bypass.
const MAX_TOTAL_BYTES: usize = crate::vision::MAX_IMAGE_BYTES_PER_TURN as usize
    + crate::focused_service::MAX_FOCUSED_PROMPT_BYTES;
const MAX_SAVED_DECISIONS: usize = 1024;
const MAX_AUDIT_RECORDS: usize = 512;
const MAX_IDENTITY_MATERIAL_BYTES: usize = 16 * 1024;
const MAX_CONNECTION_BYTES: usize = 128;
const MAX_DESTINATION_BYTES: usize = 2048;
const MAX_LABEL_BYTES: usize = 256;
const MAX_REFERENCE_BYTES: usize = 1024;
const MAX_PROVENANCE_BYTES: usize = 512;
const MAX_PURPOSE_BYTES: usize = 512;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecipientKind {
    McpStdio,
    McpHttp,
    ExternalAgent,
    FocusedService,
}

impl RecipientKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::McpStdio => "mcp_stdio",
            Self::McpHttp => "mcp_http",
            Self::ExternalAgent => "external_agent",
            Self::FocusedService => "focused_service",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecipientIdentity {
    pub(crate) kind: RecipientKind,
    pub(crate) connection: String,
    pub(crate) destination: String,
    pub(crate) identity_digest: String,
}

impl RecipientIdentity {
    pub(crate) fn new(
        kind: RecipientKind,
        connection: impl Into<String>,
        destination: impl Into<String>,
        identity_material: &[u8],
    ) -> Result<Self, OutboundError> {
        let connection = connection.into();
        let destination = destination.into();
        validate_review_text("recipient connection", &connection, MAX_CONNECTION_BYTES)?;
        validate_review_text("recipient destination", &destination, MAX_DESTINATION_BYTES)?;
        if identity_material.is_empty() || identity_material.len() > MAX_IDENTITY_MATERIAL_BYTES {
            return Err(OutboundError::InvalidRequest(
                "recipient identity material must be 1..=16384 bytes",
            ));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(kind.as_str().as_bytes());
        hasher.update(&[0]);
        hasher.update(connection.as_bytes());
        hasher.update(&[0]);
        hasher.update(destination.as_bytes());
        hasher.update(&[0]);
        hasher.update(identity_material);
        Ok(Self {
            kind,
            connection,
            destination,
            identity_digest: hasher.finalize().to_hex().to_string(),
        })
    }
}

impl fmt::Debug for RecipientIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecipientIdentity")
            .field("kind", &self.kind)
            .field("connection", &self.connection)
            .field("destination", &self.destination)
            .field("identity_digest", &self.identity_digest)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct OutboundItem {
    class: OutboundDataClass,
    label: String,
    reference: Option<String>,
    provenance: String,
    content_digest: String,
    bytes: Vec<u8>,
}

impl OutboundItem {
    pub(crate) fn new(
        class: OutboundDataClass,
        label: impl Into<String>,
        reference: Option<String>,
        provenance: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Self, OutboundError> {
        let label = label.into();
        let provenance = provenance.into();
        validate_review_text("outbound item label", &label, MAX_LABEL_BYTES)?;
        validate_review_text(
            "outbound item provenance",
            &provenance,
            MAX_PROVENANCE_BYTES,
        )?;
        if let Some(reference) = &reference {
            validate_review_text("outbound item reference", reference, MAX_REFERENCE_BYTES)?;
        }
        if bytes.len() > MAX_ITEM_BYTES {
            return Err(OutboundError::ItemTooLarge {
                actual: bytes.len(),
                limit: MAX_ITEM_BYTES,
            });
        }
        let content_digest = blake3::hash(&bytes).to_hex().to_string();
        Ok(Self {
            class,
            label,
            reference,
            provenance,
            content_digest,
            bytes,
        })
    }

    pub(crate) const fn class(&self) -> OutboundDataClass {
        self.class
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn reference(&self) -> Option<&str> {
        self.reference.as_deref()
    }

    fn review(&self) -> OutboundItemReview {
        OutboundItemReview {
            class: self.class,
            label: self.label.clone(),
            reference: self.reference.clone(),
            provenance: self.provenance.clone(),
            byte_len: self.bytes.len(),
            content_digest: self.content_digest.clone(),
        }
    }
}

impl fmt::Debug for OutboundItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboundItem")
            .field("class", &self.class)
            .field("label", &self.label)
            .field("reference", &self.reference)
            .field("provenance", &self.provenance)
            .field("content_digest", &self.content_digest)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

pub(crate) struct OutboundRequest {
    pub(crate) operation_id: OperationId,
    pub(crate) recipient: RecipientIdentity,
    pub(crate) purpose: String,
    items: Vec<OutboundItem>,
    total_bytes: usize,
}

impl OutboundRequest {
    pub(crate) fn new(
        operation_id: OperationId,
        recipient: RecipientIdentity,
        purpose: impl Into<String>,
        items: Vec<OutboundItem>,
    ) -> Result<Self, OutboundError> {
        let purpose = purpose.into();
        validate_review_text("outbound purpose", &purpose, MAX_PURPOSE_BYTES)?;
        if items.is_empty() || items.len() > MAX_ITEMS {
            return Err(OutboundError::InvalidItemCount {
                actual: items.len(),
                limit: MAX_ITEMS,
            });
        }
        let total_bytes = items
            .iter()
            .try_fold(0_usize, |total, item| total.checked_add(item.bytes.len()));
        let Some(total_bytes) = total_bytes else {
            return Err(OutboundError::TotalTooLarge {
                actual: usize::MAX,
                limit: MAX_TOTAL_BYTES,
            });
        };
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(OutboundError::TotalTooLarge {
                actual: total_bytes,
                limit: MAX_TOTAL_BYTES,
            });
        }
        Ok(Self {
            operation_id,
            recipient,
            purpose,
            items,
            total_bytes,
        })
    }

    fn classes(&self) -> BTreeSet<OutboundDataClass> {
        self.items.iter().map(OutboundItem::class).collect()
    }

    pub(crate) fn review(&self) -> OutboundApprovalRequest {
        OutboundApprovalRequest {
            operation_id: self.operation_id,
            recipient: self.recipient.clone(),
            purpose: self.purpose.clone(),
            classes: self.classes().into_iter().collect(),
            items: self.items.iter().map(OutboundItem::review).collect(),
            total_bytes: self.total_bytes,
        }
    }
}

impl fmt::Debug for OutboundRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboundRequest")
            .field("operation_id", &self.operation_id)
            .field("recipient", &self.recipient)
            .field("purpose", &self.purpose)
            .field("items", &self.items)
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboundPolicyLayers {
    pub(crate) connection_allowed: BTreeSet<OutboundDataClass>,
    pub(crate) user_ceiling: BTreeSet<OutboundDataClass>,
    pub(crate) profile_allowed: BTreeSet<OutboundDataClass>,
    pub(crate) conversation_allowed: Option<BTreeSet<OutboundDataClass>>,
}

impl OutboundPolicyLayers {
    pub(crate) fn effective(&self) -> BTreeSet<OutboundDataClass> {
        let mut effective = self.connection_allowed.clone();
        effective.retain(|class| self.user_ceiling.contains(class));
        effective.retain(|class| self.profile_allowed.contains(class));
        if let Some(conversation) = &self.conversation_allowed {
            effective.retain(|class| conversation.contains(class));
        }
        effective
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutboundItemReview {
    pub(crate) class: OutboundDataClass,
    pub(crate) label: String,
    pub(crate) reference: Option<String>,
    pub(crate) provenance: String,
    pub(crate) byte_len: usize,
    pub(crate) content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutboundApprovalRequest {
    pub(crate) operation_id: OperationId,
    pub(crate) recipient: RecipientIdentity,
    pub(crate) purpose: String,
    pub(crate) classes: Vec<OutboundDataClass>,
    pub(crate) items: Vec<OutboundItemReview>,
    pub(crate) total_bytes: usize,
}

impl OutboundApprovalRequest {
    pub(crate) fn for_operation(mut self, operation_id: OperationId) -> Self {
        self.operation_id = operation_id;
        self
    }

    pub(crate) fn render(&self) -> String {
        let mut output = format!(
            "Send to {} ({}, {}) for {}:\n",
            self.recipient.connection,
            self.recipient.kind.as_str(),
            self.recipient.destination,
            self.purpose
        );
        for item in &self.items {
            let reference = item
                .reference
                .as_deref()
                .map_or(String::new(), |value| format!(" [{value}]"));
            output.push_str(&format!(
                "- {}: {}{} ({} bytes, source: {}, digest: {})\n",
                item.class.as_str(),
                item.label,
                reference,
                item.byte_len,
                item.provenance,
                item.content_digest
            ));
        }
        output.push_str("Choice: deny once, allow once, save deny, or save allow");
        output
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundApprovalDecision {
    DenyOnce,
    AllowOnce,
    SaveDeny,
    SaveAllow,
    Cancel,
}

/// A user's decision bound to the exact redacted review they approved.
///
/// Adapters must pass this controller to [`OutboundGuard`] instead of carrying
/// a bare decision. If a later request differs from the reviewed recipient,
/// purpose, classes, items, byte counts, or content digests, the controller
/// fails closed before a grant can be persisted or a transport can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewedOutboundApproval {
    review: OutboundApprovalRequest,
    decision: OutboundApprovalDecision,
}

impl ReviewedOutboundApproval {
    pub(crate) fn new(review: OutboundApprovalRequest, decision: OutboundApprovalDecision) -> Self {
        Self { review, decision }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundDisposition {
    SavedAllow,
    SavedDeny,
    ReviewRequired,
}

pub(crate) trait OutboundApprovalController {
    fn decide<'a>(
        &'a mut self,
        request: &'a OutboundApprovalRequest,
    ) -> BoxFuture<'a, OutboundApprovalDecision>;
}

impl OutboundApprovalController for ReviewedOutboundApproval {
    fn decide<'a>(
        &'a mut self,
        request: &'a OutboundApprovalRequest,
    ) -> BoxFuture<'a, OutboundApprovalDecision> {
        let decision = if self.review == *request {
            self.decision
        } else {
            OutboundApprovalDecision::DenyOnce
        };
        Box::pin(async move { decision })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutboundDecisionSource {
    PolicyCeiling,
    Saved,
    UserOnce,
    UserSaved,
    ApprovalUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub(crate) enum OutboundAuditStage {
    Requested,
    Allowed { source: OutboundDecisionSource },
    Denied { source: OutboundDecisionSource },
    Cancelled,
    Sending,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutboundAuditEvent {
    pub(crate) operation_id: OperationId,
    pub(crate) recipient_kind: RecipientKind,
    pub(crate) recipient_connection: String,
    pub(crate) recipient_destination: String,
    pub(crate) recipient_identity_digest: String,
    pub(crate) classes: Vec<OutboundDataClass>,
    pub(crate) item_count: usize,
    pub(crate) reference_count: usize,
    pub(crate) total_bytes: usize,
    pub(crate) stage: OutboundAuditStage,
}

pub(crate) trait OutboundAuditSink {
    fn record(&mut self, event: OutboundAuditEvent) -> Result<(), OutboundError>;
}

impl OutboundAuditSink for Vec<OutboundAuditEvent> {
    fn record(&mut self, event: OutboundAuditEvent) -> Result<(), OutboundError> {
        self.push(event);
        Ok(())
    }
}

pub(crate) trait OutboundAuditObserver: Send + Sync {
    fn observe(&self, event: &OutboundAuditEvent) -> Result<(), PrivateStateError>;
}

#[derive(Debug, Default)]
pub(crate) struct NoopOutboundAuditObserver;

impl OutboundAuditObserver for NoopOutboundAuditObserver {
    fn observe(&self, _event: &OutboundAuditEvent) -> Result<(), PrivateStateError> {
        Ok(())
    }
}

pub(crate) struct ObservedOutboundAudit<'a> {
    observer: &'a dyn OutboundAuditObserver,
}

impl<'a> ObservedOutboundAudit<'a> {
    pub(crate) fn new(observer: &'a dyn OutboundAuditObserver) -> Self {
        Self { observer }
    }
}

impl OutboundAuditSink for ObservedOutboundAudit<'_> {
    fn record(&mut self, event: OutboundAuditEvent) -> Result<(), OutboundError> {
        self.observer.observe(&event).map_err(OutboundError::Audit)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OutboundAuditJournal {
    path: std::path::PathBuf,
}

impl OutboundAuditJournal {
    pub(crate) fn open(paths: &XanaPaths) -> Result<Self, PrivateStateError> {
        ensure_interoperable_records(paths)?;
        Ok(Self {
            path: paths.outbound_audit_file(),
        })
    }

    pub(crate) fn append(&self, event: &OutboundAuditEvent) -> Result<(), PrivateStateError> {
        let record = audit_record(event);
        update_document::<OutboundAuditDocument, _, std::convert::Infallible>(
            &self.path,
            |document| {
                if document.records.len() >= MAX_AUDIT_RECORDS {
                    let remove = document.records.len() - MAX_AUDIT_RECORDS + 1;
                    document.records.drain(..remove);
                }
                document.records.push(record);
                Ok(())
            },
        )
        .map_err(|error| match error {
            UpdateDocumentError::State(error) => error,
            UpdateDocumentError::Update(never) => match never {},
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundTransportFailure {
    Unavailable,
    Rejected,
    TimedOut,
    Cancelled,
    Protocol,
}

pub(crate) trait OutboundTransport {
    type Receipt;

    fn send<'a>(
        &'a mut self,
        recipient: &'a RecipientIdentity,
        items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>>;
}

#[derive(Debug, Clone)]
pub(crate) struct OutboundGuard {
    decisions_file: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedOutboundDecisionView {
    pub(crate) recipient_identity_digest: String,
    pub(crate) data_class: OutboundDataClass,
    pub(crate) decision: SavedOutboundDecision,
    pub(crate) updated_unix_ms: u64,
}

impl OutboundGuard {
    pub(crate) fn open(paths: &XanaPaths) -> Result<Self, OutboundError> {
        ensure_interoperable_records(paths).map_err(OutboundError::State)?;
        Ok(Self {
            decisions_file: paths.outbound_decisions_file(),
        })
    }

    pub(crate) fn revoke(
        &self,
        recipient: &RecipientIdentity,
        class: OutboundDataClass,
    ) -> Result<bool, OutboundError> {
        self.revoke_digest(&recipient.identity_digest, class)
    }

    pub(crate) fn disposition(
        &self,
        recipient: &RecipientIdentity,
        classes: impl IntoIterator<Item = OutboundDataClass>,
    ) -> Result<OutboundDisposition, OutboundError> {
        let classes = classes.into_iter().collect::<BTreeSet<_>>();
        let saved = self.saved_decisions(recipient, &classes)?;
        if saved
            .iter()
            .any(|decision| matches!(decision, Some(SavedOutboundDecision::Deny)))
        {
            Ok(OutboundDisposition::SavedDeny)
        } else if saved
            .iter()
            .all(|decision| matches!(decision, Some(SavedOutboundDecision::Allow)))
        {
            Ok(OutboundDisposition::SavedAllow)
        } else {
            Ok(OutboundDisposition::ReviewRequired)
        }
    }

    pub(crate) fn list_decisions(&self) -> Result<Vec<SavedOutboundDecisionView>, OutboundError> {
        let document = read_document::<OutboundDecisionDocument>(&self.decisions_file)
            .map_err(OutboundError::State)?;
        Ok(document
            .decisions
            .into_values()
            .map(|record| SavedOutboundDecisionView {
                recipient_identity_digest: record.recipient_identity_digest,
                data_class: record.data_class,
                decision: record.decision,
                updated_unix_ms: record.updated_unix_ms,
            })
            .collect())
    }

    pub(crate) fn revoke_digest(
        &self,
        identity_digest: &str,
        class: OutboundDataClass,
    ) -> Result<bool, OutboundError> {
        validate_identity_digest(identity_digest)?;
        let key = decision_key(identity_digest, class);
        update_document::<OutboundDecisionDocument, _, OutboundError>(
            &self.decisions_file,
            |document| Ok(document.decisions.remove(&key).is_some()),
        )
        .map_err(map_update_error)
    }

    pub(crate) async fn dispatch<C, T, A>(
        &self,
        request: OutboundRequest,
        policy: &OutboundPolicyLayers,
        mut controller: Option<&mut C>,
        transport: &mut T,
        audit: &mut A,
    ) -> Result<T::Receipt, OutboundError>
    where
        C: OutboundApprovalController,
        T: OutboundTransport,
        A: OutboundAuditSink,
    {
        let review = request.review();
        audit.record(audit_event(&review, OutboundAuditStage::Requested))?;
        let classes = request.classes();
        if !classes.is_subset(&policy.effective()) {
            audit.record(audit_event(
                &review,
                OutboundAuditStage::Denied {
                    source: OutboundDecisionSource::PolicyCeiling,
                },
            ))?;
            return Err(OutboundError::Denied(OutboundDecisionSource::PolicyCeiling));
        }

        let saved = self.saved_decisions(&request.recipient, &classes)?;
        let mut allowed_already_recorded = false;
        let source = if saved
            .iter()
            .any(|decision| matches!(decision, Some(SavedOutboundDecision::Deny)))
        {
            audit.record(audit_event(
                &review,
                OutboundAuditStage::Denied {
                    source: OutboundDecisionSource::Saved,
                },
            ))?;
            return Err(OutboundError::Denied(OutboundDecisionSource::Saved));
        } else if let Some(controller) = controller.as_mut() {
            match controller.decide(&review).await {
                OutboundApprovalDecision::DenyOnce => {
                    audit.record(audit_event(
                        &review,
                        OutboundAuditStage::Denied {
                            source: OutboundDecisionSource::UserOnce,
                        },
                    ))?;
                    return Err(OutboundError::Denied(OutboundDecisionSource::UserOnce));
                }
                OutboundApprovalDecision::SaveDeny => {
                    audit.record(audit_event(
                        &review,
                        OutboundAuditStage::Denied {
                            source: OutboundDecisionSource::UserSaved,
                        },
                    ))?;
                    if let Err(error) = self.save_decisions(
                        &request.recipient,
                        &classes,
                        SavedOutboundDecision::Deny,
                    ) {
                        let _ = audit.record(audit_event(&review, OutboundAuditStage::Failed));
                        return Err(error);
                    }
                    return Err(OutboundError::Denied(OutboundDecisionSource::UserSaved));
                }
                OutboundApprovalDecision::Cancel => {
                    audit.record(audit_event(&review, OutboundAuditStage::Cancelled))?;
                    return Err(OutboundError::Cancelled);
                }
                OutboundApprovalDecision::AllowOnce => OutboundDecisionSource::UserOnce,
                OutboundApprovalDecision::SaveAllow => {
                    audit.record(audit_event(
                        &review,
                        OutboundAuditStage::Allowed {
                            source: OutboundDecisionSource::UserSaved,
                        },
                    ))?;
                    allowed_already_recorded = true;
                    if let Err(error) = self.save_decisions(
                        &request.recipient,
                        &classes,
                        SavedOutboundDecision::Allow,
                    ) {
                        let _ = audit.record(audit_event(&review, OutboundAuditStage::Failed));
                        return Err(error);
                    }
                    OutboundDecisionSource::UserSaved
                }
            }
        } else if saved
            .iter()
            .all(|decision| matches!(decision, Some(SavedOutboundDecision::Allow)))
        {
            OutboundDecisionSource::Saved
        } else {
            audit.record(audit_event(
                &review,
                OutboundAuditStage::Denied {
                    source: OutboundDecisionSource::ApprovalUnavailable,
                },
            ))?;
            return Err(OutboundError::Denied(
                OutboundDecisionSource::ApprovalUnavailable,
            ));
        };

        if !allowed_already_recorded {
            audit.record(audit_event(&review, OutboundAuditStage::Allowed { source }))?;
        }
        audit.record(audit_event(&review, OutboundAuditStage::Sending))?;
        match transport.send(&request.recipient, &request.items).await {
            Ok(receipt) => {
                // The external effect has committed. Audit degradation must not
                // discard the receipt and invite an unsafe retry.
                let _ = audit.record(audit_event(&review, OutboundAuditStage::Succeeded));
                Ok(receipt)
            }
            Err(failure) => {
                let stage = if failure == OutboundTransportFailure::Cancelled {
                    OutboundAuditStage::Cancelled
                } else {
                    OutboundAuditStage::Failed
                };
                // Preserve the transport outcome even when the completion
                // observation cannot be durably appended.
                let _ = audit.record(audit_event(&review, stage));
                Err(OutboundError::Transport(failure))
            }
        }
    }

    fn saved_decisions(
        &self,
        recipient: &RecipientIdentity,
        classes: &BTreeSet<OutboundDataClass>,
    ) -> Result<Vec<Option<SavedOutboundDecision>>, OutboundError> {
        let document = read_document::<OutboundDecisionDocument>(&self.decisions_file)
            .map_err(OutboundError::State)?;
        Ok(classes
            .iter()
            .map(|class| {
                document
                    .decisions
                    .get(&decision_key(&recipient.identity_digest, *class))
                    .filter(|record| {
                        record.recipient_identity_digest == recipient.identity_digest
                            && record.data_class == *class
                    })
                    .map(|record| record.decision)
            })
            .collect())
    }

    fn save_decisions(
        &self,
        recipient: &RecipientIdentity,
        classes: &BTreeSet<OutboundDataClass>,
        decision: SavedOutboundDecision,
    ) -> Result<(), OutboundError> {
        let updated_unix_ms = now_unix_ms()?;
        update_document::<OutboundDecisionDocument, _, OutboundError>(
            &self.decisions_file,
            |document| {
                let new = classes
                    .iter()
                    .filter(|class| {
                        !document
                            .decisions
                            .contains_key(&decision_key(&recipient.identity_digest, **class))
                    })
                    .count();
                if document.decisions.len().saturating_add(new) > MAX_SAVED_DECISIONS {
                    return Err(OutboundError::SavedDecisionLimit);
                }
                for class in classes {
                    document.decisions.insert(
                        decision_key(&recipient.identity_digest, *class),
                        OutboundDecisionRecord {
                            recipient_identity_digest: recipient.identity_digest.clone(),
                            data_class: *class,
                            decision,
                            updated_unix_ms,
                        },
                    );
                }
                Ok(())
            },
        )
        .map_err(map_update_error)
    }
}

fn validate_identity_digest(identity_digest: &str) -> Result<(), OutboundError> {
    if identity_digest.len() != 64
        || !identity_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(OutboundError::InvalidRequest(
            "recipient identity digest must be exactly 64 lowercase hexadecimal characters",
        ));
    }
    Ok(())
}

fn audit_event(review: &OutboundApprovalRequest, stage: OutboundAuditStage) -> OutboundAuditEvent {
    OutboundAuditEvent {
        operation_id: review.operation_id,
        recipient_kind: review.recipient.kind,
        recipient_connection: review.recipient.connection.clone(),
        recipient_destination: review.recipient.destination.clone(),
        recipient_identity_digest: review.recipient.identity_digest.clone(),
        classes: review.classes.clone(),
        item_count: review.items.len(),
        reference_count: review
            .items
            .iter()
            .filter(|item| item.reference.is_some())
            .count(),
        total_bytes: review.total_bytes,
        stage,
    }
}

fn audit_record(event: &OutboundAuditEvent) -> OutboundAuditRecord {
    let (stage, decision_source) = match event.stage {
        OutboundAuditStage::Requested => ("requested", None),
        OutboundAuditStage::Allowed { source } => ("allowed", Some(source_name(source))),
        OutboundAuditStage::Denied { source } => ("denied", Some(source_name(source))),
        OutboundAuditStage::Cancelled => ("cancelled", None),
        OutboundAuditStage::Sending => ("sending", None),
        OutboundAuditStage::Succeeded => ("succeeded", None),
        OutboundAuditStage::Failed => ("failed", None),
    };
    OutboundAuditRecord {
        recorded_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
        operation_id: event.operation_id.to_string(),
        recipient_kind: event.recipient_kind.as_str().to_owned(),
        recipient_connection: event.recipient_connection.clone(),
        recipient_destination: event.recipient_destination.clone(),
        recipient_identity_digest: event.recipient_identity_digest.clone(),
        data_classes: event.classes.clone(),
        item_count: event.item_count,
        reference_count: event.reference_count,
        total_bytes: event.total_bytes,
        stage: stage.to_owned(),
        decision_source: decision_source.map(str::to_owned),
    }
}

const fn source_name(source: OutboundDecisionSource) -> &'static str {
    match source {
        OutboundDecisionSource::PolicyCeiling => "policy_ceiling",
        OutboundDecisionSource::Saved => "saved",
        OutboundDecisionSource::UserOnce => "user_once",
        OutboundDecisionSource::UserSaved => "user_saved",
        OutboundDecisionSource::ApprovalUnavailable => "approval_unavailable",
    }
}

fn decision_key(identity_digest: &str, class: OutboundDataClass) -> String {
    format!("{identity_digest}:{}", class.as_str())
}

fn validate_review_text(
    field: &'static str,
    value: &str,
    limit: usize,
) -> Result<(), OutboundError> {
    if value.trim().is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(OutboundError::UnsafeReviewText { field, limit });
    }
    Ok(())
}

fn now_unix_ms() -> Result<u64, OutboundError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| OutboundError::Clock)?
        .as_millis();
    u64::try_from(millis).map_err(|_| OutboundError::Clock)
}

fn map_update_error(error: UpdateDocumentError<OutboundError>) -> OutboundError {
    match error {
        UpdateDocumentError::State(error) => OutboundError::State(error),
        UpdateDocumentError::Update(error) => error,
    }
}

#[derive(Debug)]
pub(crate) enum OutboundError {
    InvalidRequest(&'static str),
    UnsafeReviewText { field: &'static str, limit: usize },
    InvalidItemCount { actual: usize, limit: usize },
    ItemTooLarge { actual: usize, limit: usize },
    TotalTooLarge { actual: usize, limit: usize },
    Denied(OutboundDecisionSource),
    Cancelled,
    SavedDecisionLimit,
    Audit(PrivateStateError),
    Clock,
    State(PrivateStateError),
    Transport(OutboundTransportFailure),
}

impl fmt::Display for OutboundError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(reason) => formatter.write_str(reason),
            Self::UnsafeReviewText { field, limit } => write!(
                formatter,
                "{field} must be non-blank, free of control characters, and at most {limit} bytes"
            ),
            Self::InvalidItemCount { actual, limit } => write!(
                formatter,
                "outbound request contains {actual} items; expected 1..={limit}"
            ),
            Self::ItemTooLarge { actual, limit } => write!(
                formatter,
                "outbound item contains {actual} bytes, exceeding the {limit}-byte limit"
            ),
            Self::TotalTooLarge { actual, limit } => write!(
                formatter,
                "outbound request contains {actual} bytes, exceeding the {limit}-byte limit"
            ),
            Self::Denied(source) => write!(formatter, "outbound request was denied ({source:?})"),
            Self::Cancelled => formatter.write_str("outbound request was cancelled"),
            Self::SavedDecisionLimit => {
                formatter.write_str("saved outbound-decision limit has been reached")
            }
            Self::Audit(error) => write!(formatter, "could not commit outbound audit: {error}"),
            Self::Clock => formatter.write_str("system time is unavailable"),
            Self::State(error) => {
                write!(formatter, "outbound policy state is unavailable: {error}")
            }
            Self::Transport(failure) => {
                write!(formatter, "outbound transport failed ({failure:?})")
            }
        }
    }
}

impl Error for OutboundError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Audit(error) | Self::State(error) => Some(error),
            Self::InvalidRequest(_)
            | Self::UnsafeReviewText { .. }
            | Self::InvalidItemCount { .. }
            | Self::ItemTooLarge { .. }
            | Self::TotalTooLarge { .. }
            | Self::Denied(_)
            | Self::Cancelled
            | Self::SavedDecisionLimit
            | Self::Clock
            | Self::Transport(_) => None,
        }
    }
}

#[cfg(test)]
mod tests;
