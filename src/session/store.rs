//! Journal mechanism selection; record semantics stay in the pure reducer.

mod legacy;

use crate::{identity::SessionId, session::RecordEnvelope, storage::ProtectedStore};
use std::path::{Path, PathBuf};

#[cfg(test)]
pub(crate) use legacy::TornTailRepair;
pub(crate) use legacy::{ConversationPage, LoadedSession, SessionError};
pub(crate) use legacy::{MAX_RECORD_BYTES, MAX_SESSION_BYTES, MAX_SESSION_RECORDS};

pub(crate) enum SessionStore {
    Legacy(legacy::LegacyStore),
    Protected {
        home: ProtectedStore,
        id: SessionId,
        path: PathBuf,
        revision: usize,
        _writer: std::fs::File,
        writable: bool,
    },
}

impl SessionStore {
    pub(crate) fn path_for(sessions_dir: &Path, id: SessionId) -> PathBuf {
        legacy::LegacyStore::path_for(sessions_dir, id)
    }

    pub(crate) fn create(
        sessions_dir: &Path,
        created: RecordEnvelope,
    ) -> Result<Self, SessionError> {
        legacy::LegacyStore::create(sessions_dir, created).map(Self::Legacy)
    }

    pub(super) fn create_batch(
        sessions_dir: &Path,
        records: &[RecordEnvelope],
    ) -> Result<Self, SessionError> {
        legacy::LegacyStore::create_batch(sessions_dir, records).map(Self::Legacy)
    }

    pub(crate) fn inspect(path: &Path) -> Result<LoadedSession, SessionError> {
        legacy::LegacyStore::inspect(path)
    }

    pub(crate) fn open_for_resume(
        path: &Path,
        loaded: LoadedSession,
    ) -> Result<Self, SessionError> {
        legacy::LegacyStore::open_for_resume(path, loaded).map(Self::Legacy)
    }

    pub(crate) fn conversation_page(
        path: &Path,
        before: Option<usize>,
        limit: usize,
    ) -> Result<ConversationPage, SessionError> {
        legacy::LegacyStore::conversation_page(path, before, limit)
    }

    pub(crate) fn conversation_page_from(
        path: &Path,
        start: usize,
        limit: usize,
    ) -> Result<ConversationPage, SessionError> {
        legacy::LegacyStore::conversation_page_from(path, start, limit)
    }

    pub(crate) fn create_protected(
        home: ProtectedStore,
        records: &[RecordEnvelope],
    ) -> Result<Self, SessionError> {
        let id = records
            .first()
            .ok_or(SessionError::MissingCreationRecord)?
            .session_id;
        let writer = home.session_writer(id).map_err(protected_error)?;
        home.create_history(records).map_err(protected_error)?;
        Ok(Self::Protected {
            path: home.database_path(),
            home,
            id,
            revision: records.len(),
            _writer: writer,
            writable: true,
        })
    }

    pub(crate) fn inspect_protected(
        home: &ProtectedStore,
        id: SessionId,
    ) -> Result<LoadedSession, SessionError> {
        let bytes = home.load_history(id).map_err(protected_error)?;
        legacy::LegacyStore::inspect_bytes(&bytes)
    }

    pub(crate) fn protected_home(&self) -> Option<&ProtectedStore> {
        match self {
            Self::Protected { home, .. } => Some(home),
            Self::Legacy(_) => None,
        }
    }

    /// Acquire the writer before observing the revision or execution checkpoint.
    pub(super) fn open_protected(
        home: ProtectedStore,
        id: SessionId,
    ) -> Result<Self, SessionError> {
        let writer = home.session_writer(id).map_err(protected_error)?;
        let revision = home.history_metadata(id).map_err(protected_error)?.revision;
        Ok(Self::Protected {
            path: home.database_path(),
            home,
            id,
            revision,
            _writer: writer,
            writable: true,
        })
    }

    pub(super) fn protected_revision(&self) -> Option<usize> {
        match self {
            Self::Protected { revision, .. } => Some(*revision),
            Self::Legacy(_) => None,
        }
    }

    pub(super) fn branch_protected(
        home: ProtectedStore,
        source: SessionId,
        point: crate::identity::ConversationEntryId,
        target: SessionId,
    ) -> Result<(Self, crate::session::RestoredSession), SessionError> {
        let writer = home.session_writer(target).map_err(protected_error)?;
        let state = home
            .branch_history(source, point, target, crate::identity::ThreadId::new())
            .map_err(protected_error)?;
        let revision = home
            .history_metadata(target)
            .map_err(protected_error)?
            .revision;
        Ok((
            Self::Protected {
                path: home.database_path(),
                home,
                id: target,
                revision,
                _writer: writer,
                writable: true,
            },
            state,
        ))
    }

    #[cfg(test)]
    pub(crate) fn append(&mut self, record: &RecordEnvelope) -> Result<(), SessionError> {
        self.append_guarded(record, None)
    }

    pub(crate) fn append_guarded(
        &mut self,
        record: &RecordEnvelope,
        privacy_generation: Option<u64>,
    ) -> Result<(), SessionError> {
        match self {
            Self::Legacy(store) => store.append(record),
            Self::Protected {
                home,
                id,
                revision,
                writable,
                path,
                ..
            } => {
                if !*writable {
                    return Err(SessionError::WriterPoisoned { path: path.clone() });
                }
                let result = match privacy_generation {
                    Some(generation) => home
                        .append_history_if_privacy_generation(*id, *revision, record, generation),
                    None => home.append_history(*id, *revision, record),
                };
                if let Err(error) = result {
                    *writable = false;
                    return Err(protected_error(error));
                }
                *revision += 1;
                Ok(())
            }
        }
    }
    pub(crate) fn session_id(&self) -> SessionId {
        match self {
            Self::Legacy(store) => store.session_id(),
            Self::Protected { id, .. } => *id,
        }
    }
    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::Legacy(store) => store.path(),
            Self::Protected { path, .. } => path,
        }
    }
}

fn protected_error(error: anyhow::Error) -> SessionError {
    SessionError::Protected(format!("{error:#}"))
}
