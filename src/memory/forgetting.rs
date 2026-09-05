//! Explicit forgetting and separately reviewed source deletion.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDeletionPreview {
    pub conversation: Uuid,
    pub records: u64,
    pub bytes: u64,
    pub review: String,
    pub notice: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDeletionReceipt {
    pub conversation: Uuid,
    pub receipt: Uuid,
    pub deleted_records: u64,
    pub at_unix_seconds: u64,
    pub artifacts_retained: bool,
}

impl MemoryOwner {
    pub(crate) fn deletion_preview(&self, conversation: Uuid) -> Result<SourceDeletionPreview> {
        self.store.source_deletion_preview(conversation)
    }

    pub(crate) fn delete_source(
        &self,
        conversation: Uuid,
        review: &str,
    ) -> Result<SourceDeletionReceipt> {
        self.store
            .delete_source_history(conversation, review, now()?)
    }
}
