//! Versioned append-only session records and pure restoration.

mod durable;
mod record;
mod reduce;
mod store;

pub(crate) use durable::DurableSession;
pub(crate) use record::{ConversationEntry, RecordEnvelope, SessionRecord};
pub(crate) use reduce::{RestoredSession, reduce};
#[cfg(test)]
pub(crate) use store::TornTailRepair;
pub(crate) use store::{LoadedSession, SessionStore};

#[cfg(test)]
mod tests;
