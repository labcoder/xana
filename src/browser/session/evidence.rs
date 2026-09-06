//! Protected, principal-scoped browser receipts and immutable evidence.
use super::*;
use crate::artifact::ArtifactStore;

impl BrowserOwner {
    pub(super) async fn artifact(
        &self,
        bytes: Vec<u8>,
        media: &'static str,
        bound: usize,
    ) -> Result<ArtifactRecord, BrowserError> {
        let store = ArtifactStore::protected(self.inner.store.clone());
        let principal = self.inner.principal;
        tokio::task::spawn_blocking(move || {
            store
                .put_bounded(&bytes, media, principal, bound)
                .map(|v| v.0)
                .map_err(|_| BrowserError::Storage)
        })
        .await
        .map_err(|_| BrowserError::Storage)?
    }
    pub(super) async fn persist(&self, receipt: &BrowserReceipt) -> Result<(), BrowserError> {
        let store = self.inner.store.clone();
        let name = format!("browser/receipts/{}", receipt.id);
        let index_name = format!("browser/receipt-index/{}", self.inner.principal);
        let receipt_id = receipt.id;
        let bytes = serde_json::to_vec(receipt).map_err(|_| BrowserError::Protocol)?;
        let result = tokio::task::spawn_blocking(move || {
            store
                .set_document(&name, &bytes, 256 * 1024)
                .map_err(|_| BrowserError::LockedStorage)?;
            let mut index: Vec<Uuid> = store
                .document(&index_name, 4096)
                .map_err(|_| BrowserError::Storage)?
                .map(|bytes| serde_json::from_slice(&bytes))
                .transpose()
                .map_err(|_| BrowserError::Storage)?
                .unwrap_or_default();
            if index.len() > 64 {
                return Err(BrowserError::Storage);
            }
            index.retain(|old| *old != receipt_id);
            index.insert(0, receipt_id);
            index.truncate(64);
            let bytes = serde_json::to_vec(&index).map_err(|_| BrowserError::Storage)?;
            store
                .set_document(&index_name, &bytes, 4096)
                .map_err(|_| BrowserError::Storage)
        })
        .await
        .unwrap_or(Err(BrowserError::Storage));
        if result.is_err() {
            self.inner
                .snapshot
                .lock()
                .expect("browser owner")
                .receipt_error = true;
        }
        result
    }
    pub(crate) async fn receipts(&self, limit: usize) -> Result<Vec<BrowserReceipt>, BrowserError> {
        let store = self.inner.store.clone();
        let index_name = format!("browser/receipt-index/{}", self.inner.principal);
        tokio::task::spawn_blocking(move || {
            let index: Vec<Uuid> = store
                .document(&index_name, 4096)
                .map_err(|_| BrowserError::Storage)?
                .map(|bytes| serde_json::from_slice(&bytes))
                .transpose()
                .map_err(|_| BrowserError::Storage)?
                .unwrap_or_default();
            if index.len() > 64 {
                return Err(BrowserError::Storage);
            }
            index
                .iter()
                .take(limit.min(64))
                .map(|id| {
                    let bytes = store
                        .document(&format!("browser/receipts/{id}"), 256 * 1024)
                        .map_err(|_| BrowserError::Storage)?
                        .ok_or(BrowserError::Storage)?;
                    serde_json::from_slice(&bytes).map_err(|_| BrowserError::Storage)
                })
                .collect()
        })
        .await
        .map_err(|_| BrowserError::Storage)?
    }
}
