//! Versioned append-only session records and pure restoration.

pub(crate) mod compaction;
mod durable;
pub(crate) mod hydration;
mod record;
mod reduce;
mod store;

pub(crate) use compaction::{
    COMPACTION_CHECKPOINT_VERSION, CompactionCandidate, CompactionCheckpoint, CompactionError,
    CompactionReason, CompactionSummary, PromptContinuation,
};
pub(crate) use durable::{DurableSession, NativeConversationHandle};
pub(crate) use record::SESSION_RECORD_VERSION;
pub(crate) use record::{ConversationEntry, NativeBranchLineage, RecordEnvelope, SessionRecord};
pub(crate) use reduce::{
    RestoredOperation, RestoredSession, apply_validated, reduce, validate_envelope,
    validate_envelope_with_compaction_proof,
};
#[cfg(test)]
pub(crate) use store::TornTailRepair;
pub(crate) use store::{ConversationPage, LoadedSession, SessionStore};
pub(crate) use store::{MAX_RECORD_BYTES, MAX_SESSION_BYTES, MAX_SESSION_RECORDS};

#[cfg(test)]
mod tests;
