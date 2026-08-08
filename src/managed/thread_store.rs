//! Durable, non-secret handles for externally owned managed threads.

use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

const DOCUMENT_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: u64 = 64 * 1024;
const MAX_THREAD_ID_BYTES: usize = 4096;

#[derive(Debug)]
pub(crate) enum ManagedThreadStoreError {
    Busy(PathBuf),
    Invalid(String),
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for ManagedThreadStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(path) => write!(
                f,
                "managed thread is already open by another Xana process ({})",
                path.display()
            ),
            Self::Invalid(reason) => write!(f, "invalid managed thread state: {reason}"),
            Self::Io { path, source } => {
                write!(f, "could not access {}: {source}", path.display())
            }
        }
    }
}

impl Error for ManagedThreadStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedThreadDocument {
    version: u32,
    connection: String,
    workspace: PathBuf,
    thread_id: Option<String>,
}

pub(crate) struct ManagedThreadStore {
    state_path: PathBuf,
    connection: String,
    workspace: PathBuf,
    thread_id: Option<String>,
    _writer_lock: fs::File,
}

impl ManagedThreadStore {
    pub(crate) fn open(
        data_root: &Path,
        connection: &str,
        workspace: &Path,
    ) -> Result<Self, ManagedThreadStoreError> {
        let directory = data_root.join("managed-threads");
        fs::create_dir_all(&directory).map_err(|source| ManagedThreadStoreError::Io {
            path: directory.clone(),
            source,
        })?;

        let mut hasher = blake3::Hasher::new();
        hasher.update(connection.as_bytes());
        hasher.update(&[0]);
        hasher.update(workspace.as_os_str().as_encoded_bytes());
        let key = hasher.finalize().to_hex();
        let state_path = directory.join(format!("{key}.json"));
        let lock_path = directory.join(format!("{key}.lock"));
        let writer_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| ManagedThreadStoreError::Io {
                path: lock_path.clone(),
                source,
            })?;
        match writer_lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(ManagedThreadStoreError::Busy(state_path));
            }
            Err(fs::TryLockError::Error(source)) => {
                return Err(ManagedThreadStoreError::Io {
                    path: lock_path,
                    source,
                });
            }
        }

        let thread_id = match fs::metadata(&state_path) {
            Ok(metadata) => {
                if metadata.len() > MAX_DOCUMENT_BYTES {
                    return Err(ManagedThreadStoreError::Invalid(format!(
                        "{} exceeds the {MAX_DOCUMENT_BYTES}-byte limit",
                        state_path.display()
                    )));
                }
                let bytes =
                    fs::read(&state_path).map_err(|source| ManagedThreadStoreError::Io {
                        path: state_path.clone(),
                        source,
                    })?;
                let document: ManagedThreadDocument = serde_json::from_slice(&bytes)
                    .map_err(|error| ManagedThreadStoreError::Invalid(error.to_string()))?;
                if document.version != DOCUMENT_VERSION
                    || document.connection != connection
                    || document.workspace != workspace
                {
                    return Err(ManagedThreadStoreError::Invalid(
                        "route identity does not match its state file".into(),
                    ));
                }
                document.thread_id
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(ManagedThreadStoreError::Io {
                    path: state_path.clone(),
                    source,
                });
            }
        };

        Ok(Self {
            state_path,
            connection: connection.to_owned(),
            workspace: workspace.to_owned(),
            thread_id,
            _writer_lock: writer_lock,
        })
    }

    pub(crate) fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub(crate) fn set_thread_id(
        &mut self,
        thread_id: Option<String>,
    ) -> Result<(), ManagedThreadStoreError> {
        if thread_id
            .as_deref()
            .is_some_and(|id| id.is_empty() || id.len() > MAX_THREAD_ID_BYTES)
        {
            return Err(ManagedThreadStoreError::Invalid(format!(
                "thread id must contain 1 to {MAX_THREAD_ID_BYTES} bytes"
            )));
        }
        let document = ManagedThreadDocument {
            version: DOCUMENT_VERSION,
            connection: self.connection.clone(),
            workspace: self.workspace.clone(),
            thread_id: thread_id.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| ManagedThreadStoreError::Invalid(error.to_string()))?;
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(ManagedThreadStoreError::Invalid(format!(
                "managed thread state exceeds the {MAX_DOCUMENT_BYTES}-byte limit"
            )));
        }
        let mut file =
            atomic_write_file::AtomicWriteFile::open(&self.state_path).map_err(|source| {
                ManagedThreadStoreError::Io {
                    path: self.state_path.clone(),
                    source,
                }
            })?;
        file.write_all(&bytes)
            .map_err(|source| ManagedThreadStoreError::Io {
                path: self.state_path.clone(),
                source,
            })?;
        file.commit()
            .map_err(|source| ManagedThreadStoreError::Io {
                path: self.state_path.clone(),
                source,
            })?;
        self.thread_id = thread_id;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn managed_handle_round_trips_and_clear_keeps_route_identity() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        {
            let mut store =
                ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            assert_eq!(store.thread_id(), None);
            store.set_thread_id(Some("thr_123".into())).unwrap();
        }
        {
            let mut store =
                ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            assert_eq!(store.thread_id(), Some("thr_123"));
            store.set_thread_id(None).unwrap();
        }
        let store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        assert_eq!(store.thread_id(), None);
    }

    #[test]
    fn same_route_has_one_writer_but_other_workspaces_are_independent() {
        let directory = tempdir().unwrap();
        let first_workspace = directory.path().join("first");
        let second_workspace = directory.path().join("second");
        fs::create_dir(&first_workspace).unwrap();
        fs::create_dir(&second_workspace).unwrap();
        let _first = ManagedThreadStore::open(directory.path(), "codex", &first_workspace).unwrap();
        assert!(matches!(
            ManagedThreadStore::open(directory.path(), "codex", &first_workspace),
            Err(ManagedThreadStoreError::Busy(_))
        ));
        assert!(ManagedThreadStore::open(directory.path(), "codex", &second_workspace).is_ok());
    }

    #[test]
    fn oversized_thread_ids_are_rejected_before_a_state_write() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        assert!(matches!(
            store.set_thread_id(Some("x".repeat(MAX_THREAD_ID_BYTES + 1))),
            Err(ManagedThreadStoreError::Invalid(_))
        ));
        assert!(!store.state_path.is_file());
    }
}
