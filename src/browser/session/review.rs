//! Durable consequential-intent fence. Process cleanup is not reconciliation;
//! only a definite receipt or exact owner review restores consequential authority.
use super::*;

const MAX_STATE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserReview {
    pub(crate) receipt: Uuid,
    pub(crate) revision: u64,
    pub(crate) task: Option<Uuid>,
    pub(crate) operation: OperationId,
    pub(crate) action: String,
    pub(crate) purpose: String,
    pub(crate) label: String,
    pub(crate) recipient: String,
    pub(crate) evidence: ArtifactRecord,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewState {
    revision: u64,
    pending: Option<BrowserReview>,
}

impl BrowserOwner {
    pub(crate) async fn pending_review(&self) -> Result<Option<BrowserReview>, BrowserError> {
        Ok(self.read_review().await?.1.pending)
    }

    pub(super) async fn require_review_clear(&self) -> Result<(), BrowserError> {
        if self.pending_review().await?.is_some() {
            Err(BrowserError::ReviewRequired)
        } else {
            Ok(())
        }
    }

    pub(super) async fn begin_review(
        &self,
        receipt: &BrowserReceipt,
        plan: &BrowserPlan,
    ) -> Result<BrowserReview, BrowserError> {
        let BrowserRequest::Act {
            effect, purpose, ..
        } = &plan.request
        else {
            return Err(BrowserError::InvalidInput);
        };
        let (previous, mut state) = self.read_review().await?;
        if state.pending.is_some() {
            return Err(BrowserError::ReviewRequired);
        }
        state.revision = state.revision.checked_add(1).ok_or(BrowserError::Limit)?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "request":plan.request,"observed_target":plan.review,"receipt":receipt.id,
            "operation":receipt.operation,"task":receipt.task,
        }))
        .map_err(|_| BrowserError::Protocol)?;
        let evidence = self.artifact(bytes, "application/json", 32 * 1024).await?;
        let target = plan.review.as_ref();
        let recipient = target
            .and_then(|target| {
                if matches!(effect, BrowserEffect::Click {}) {
                    target["element"]["form"]["action"]
                        .as_str()
                        .or_else(|| target["element"]["destination"].as_str())
                        .or_else(|| target["url"].as_str())
                } else {
                    target["url"].as_str()
                }
            })
            .and_then(|url| reqwest::Url::parse(url).ok())
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_default();
        let review = BrowserReview {
            receipt: receipt.id,
            revision: state.revision,
            task: receipt.task,
            operation: receipt.operation,
            action: match effect {
                BrowserEffect::Click {} => "click",
                BrowserEffect::Fill { .. } => "fill",
            }
            .into(),
            purpose: purpose.chars().take(128).collect(),
            label: target
                .and_then(|target| target["label"].as_str())
                .unwrap_or("")
                .chars()
                .take(128)
                .collect(),
            recipient,
            evidence,
        };
        state.pending = Some(review.clone());
        self.replace_review(previous, state).await?;
        Ok(review)
    }

    pub(super) async fn settle_review(&self, expected: &BrowserReview) -> Result<(), BrowserError> {
        let (previous, mut state) = self.read_review().await?;
        if !state.pending.as_ref().is_some_and(|pending| {
            pending.receipt == expected.receipt && pending.revision == expected.revision
        }) {
            return Err(BrowserError::Stale);
        }
        state.revision = state.revision.checked_add(1).ok_or(BrowserError::Limit)?;
        state.pending = None;
        self.replace_review(previous, state).await
    }

    /// Only client/controller dispatch calls this. It is deliberately absent
    /// from BrowserRequest and cannot be invoked by the model or page.
    pub(crate) async fn resolve(
        &self,
        receipt: Uuid,
        revision: u64,
        outcome: BrowserResolution,
    ) -> Result<BrowserReceipt, BrowserError> {
        let _slot = self
            .inner
            .session
            .try_lock()
            .map_err(|_| BrowserError::Busy)?;
        let pending = self.pending_review().await?.ok_or(BrowserError::Stale)?;
        if pending.receipt != receipt || pending.revision != revision {
            return Err(BrowserError::Stale);
        }
        let record = BrowserReceipt {
            id: Uuid::new_v4(),
            task: pending.task,
            operation: OperationId::new(),
            outcome: format!(
                "owner reviewed receipt {receipt} revision {revision}: {outcome:?}; no automatic retry"
            ),
            acknowledged: true,
            snapshot: self.snapshot(),
            evidence: Some(pending.evidence.clone()),
            observation: None,
        };
        // A durable audit precedes clearing the fence. A crash in between is
        // conservative: the same unresolved effect still requires owner review.
        self.persist(&record).await?;
        self.settle_review(&pending).await?;
        Ok(record)
    }

    async fn read_review(&self) -> Result<(Option<Vec<u8>>, ReviewState), BrowserError> {
        let store = self.inner.store.clone();
        let name = format!("browser/review-state/{}", self.inner.conversation);
        tokio::task::spawn_blocking(move || {
            let previous = store
                .document(&name, MAX_STATE_BYTES)
                .map_err(|_| BrowserError::Storage)?;
            let state: ReviewState = previous
                .as_ref()
                .map(|bytes| serde_json::from_slice(bytes))
                .transpose()
                .map_err(|_| BrowserError::Storage)?
                .unwrap_or_default();
            if state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.revision == 0 || pending.revision != state.revision)
            {
                return Err(BrowserError::Storage);
            }
            Ok((previous, state))
        })
        .await
        .map_err(|_| BrowserError::Storage)?
    }

    async fn replace_review(
        &self,
        previous: Option<Vec<u8>>,
        state: ReviewState,
    ) -> Result<(), BrowserError> {
        let store = self.inner.store.clone();
        let name = format!("browser/review-state/{}", self.inner.conversation);
        let replacement = serde_json::to_vec(&state).map_err(|_| BrowserError::Storage)?;
        tokio::task::spawn_blocking(move || {
            if store
                .compare_exchange_document(
                    &name,
                    previous.as_deref(),
                    &replacement,
                    MAX_STATE_BYTES,
                )
                .map_err(|_| BrowserError::Storage)?
            {
                Ok(())
            } else {
                Err(BrowserError::Stale)
            }
        })
        .await
        .map_err(|_| BrowserError::Storage)?
    }
}

#[cfg(test)]
mod tests;
