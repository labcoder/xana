//! Versioned append-only session records and pure restoration.

mod compaction;
mod durable;
mod record;
mod reduce;
mod store;

pub(crate) use compaction::{
    COMPACTION_CHECKPOINT_VERSION, CompactionCheckpoint, CompactionError, CompactionReason,
    CompactionSummary, PromptContinuation,
};
pub(crate) use durable::{DurableSession, NativeConversationHandle};
pub(crate) use record::{ConversationEntry, NativeBranchLineage, RecordEnvelope, SessionRecord};
pub(crate) use reduce::{
    RestoredOperation, RestoredSession, apply_validated, reduce, validate_envelope,
};
#[cfg(test)]
pub(crate) use store::TornTailRepair;
pub(crate) use store::{ConversationPage, LoadedSession, SessionStore};

#[cfg(test)]
mod tests;
