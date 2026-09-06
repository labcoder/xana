//! Bounded browser review projection; the runtime still owns reconciliation.
use crate::browser::{BrowserControl, BrowserResolution};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone)]
pub struct DesktopBrowserReview {
    receipt: Uuid,
    revision: u64,
    pub summary: String,
}

impl DesktopBrowserReview {
    /// Decode only the existing runtime-owned status detail, not page content.
    /// Call on explicit review, rather than parsing activity on every paint.
    pub fn from_status_detail(detail: &str) -> Option<Self> {
        if detail.len() > 16 * 1024 {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(detail).ok()?;
        let pending = value.get("pending_review")?;
        #[derive(Deserialize)]
        struct Identity {
            receipt: Uuid,
            revision: u64,
        }
        let identity: Identity = serde_json::from_value(pending.clone()).ok()?;
        Some(Self {
            receipt: identity.receipt,
            revision: identity.revision,
            summary: pending.to_string(),
        })
    }

    pub fn resolve(&self, outcome: BrowserResolution) -> BrowserControl {
        BrowserControl::Resolve {
            receipt: self.receipt,
            revision: self.revision,
            outcome,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_preserves_exact_receipt_and_revision_and_refuses_invalid_status() {
        let receipt = Uuid::new_v4();
        let detail = serde_json::json!({"pending_review":{
            "receipt":receipt,"revision":3,"purpose":"One controlled form"
        }})
        .to_string();
        let review = DesktopBrowserReview::from_status_detail(&detail).unwrap();
        assert_eq!(
            review.resolve(BrowserResolution::NotApplied),
            BrowserControl::Resolve {
                receipt,
                revision: 3,
                outcome: BrowserResolution::NotApplied
            }
        );
        for invalid in [
            "{}".to_owned(),
            "{\"pending_review\":null}".to_owned(),
            "{\"pending_review\":{\"receipt\":\"not-an-id\",\"revision\":3}}".to_owned(),
            " ".repeat(16 * 1024 + 1),
        ] {
            assert!(DesktopBrowserReview::from_status_detail(&invalid).is_none());
        }
    }
}
