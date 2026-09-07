//! Durable, non-secret handles for externally owned managed threads.

use crate::{bounded_file, identity::ConversationId};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

const DOCUMENT_VERSION: u32 = 3;
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_THREAD_ID_BYTES: usize = 4096;
const MAX_IDENTITY_VERSION_BYTES: usize = 128;
const MAX_THREADS: usize = 128;

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
struct ManagedThreadDocumentV1 {
    version: u32,
    connection: String,
    workspace: PathBuf,
    thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedThreadEntry {
    conversation_id: ConversationId,
    thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_version: Option<String>,
    /// Host receipt: 0 = explicitly no memory tools, 1 = exact v1 tools.
    /// Absence is a legacy/unknown registration, never an inferred capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memory_tools_version: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedThreadEntryV2 {
    thread_id: String,
    #[serde(default)]
    identity_version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedThreadDocumentV2 {
    #[serde(rename = "version")]
    _version: u32,
    connection: String,
    workspace: PathBuf,
    current_thread_id: Option<String>,
    threads: Vec<ManagedThreadEntryV2>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedThreadDocument {
    version: u32,
    connection: String,
    workspace: PathBuf,
    current_thread_id: Option<String>,
    threads: Vec<ManagedThreadEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedConversationHandle {
    pub(crate) conversation_id: ConversationId,
    pub(crate) connection: String,
    pub(crate) thread_id: String,
    pub(crate) current: bool,
}

struct DecodedManagedDocument {
    current_conversation_id: Option<ConversationId>,
    current_thread_id: Option<String>,
    identity_version: Option<String>,
    threads: Vec<ManagedThreadEntry>,
    connection: String,
}

pub(crate) struct ManagedThreadStore {
    protected: Option<crate::storage::ProtectedStore>,
    state_path: PathBuf,
    connection: String,
    workspace: PathBuf,
    conversation_id: Option<ConversationId>,
    thread_id: Option<String>,
    identity_version: Option<String>,
    threads: Vec<ManagedThreadEntry>,
    writer_lock: fs::File,
}

impl ManagedThreadStore {
    pub(crate) fn open(
        data_root: &Path,
        connection: &str,
        workspace: &Path,
    ) -> Result<Self, ManagedThreadStoreError> {
        let protected =
            crate::storage::ProtectedStore::configured(data_root).map_err(protected_error)?;
        let workspace = workspace
            .canonicalize()
            .map_err(|source| ManagedThreadStoreError::Io {
                path: workspace.to_owned(),
                source,
            })?;
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

        let (conversation_id, thread_id, identity_version, threads) =
            match read_managed_document(protected.as_ref(), &state_path) {
                Ok(bytes) => {
                    let decoded = decode_document(&bytes, connection, &workspace)?;
                    (
                        decoded.current_conversation_id,
                        decoded.current_thread_id,
                        decoded.identity_version,
                        decoded.threads,
                    )
                }
                Err(bounded_file::BoundedReadError::Io { source, .. })
                    if source.kind() == io::ErrorKind::NotFound =>
                {
                    (None, None, None, Vec::new())
                }
                Err(bounded_file::BoundedReadError::Io { path, source }) => {
                    return Err(ManagedThreadStoreError::Io { path, source });
                }
                Err(bounded_file::BoundedReadError::TooLarge { actual, limit, .. }) => {
                    return Err(ManagedThreadStoreError::Invalid(format!(
                        "{} contains {actual} bytes, exceeding the {limit}-byte limit",
                        state_path.display()
                    )));
                }
            };

        Ok(Self {
            protected,
            state_path,
            connection: connection.to_owned(),
            workspace,
            conversation_id,
            thread_id,
            identity_version,
            threads,
            writer_lock,
        })
    }

    pub(crate) fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub(crate) fn conversation_id(&self) -> Option<ConversationId> {
        self.conversation_id
    }

    pub(crate) fn identity_version(&self) -> Option<&str> {
        self.identity_version.as_deref()
    }

    pub(crate) fn set_thread(
        &mut self,
        conversation_id: Option<ConversationId>,
        thread_id: Option<String>,
        identity_version: Option<&str>,
    ) -> Result<(), ManagedThreadStoreError> {
        validate_thread_state(conversation_id, thread_id.as_deref(), identity_version)?;
        let identity_version = identity_version.map(str::to_owned);
        let mut threads = self.threads.clone();
        if let Some(id) = thread_id.as_deref() {
            let conversation_id = conversation_id.expect("validated managed Conversation id");
            let memory_tools_version = threads
                .iter()
                .find(|entry| entry.thread_id == id && entry.conversation_id == conversation_id)
                .and_then(|entry| entry.memory_tools_version);
            threads
                .retain(|entry| entry.thread_id != id && entry.conversation_id != conversation_id);
            threads.push(ManagedThreadEntry {
                conversation_id,
                thread_id: id.to_owned(),
                identity_version: identity_version.clone(),
                memory_tools_version,
            });
            if threads.len() > MAX_THREADS {
                threads.remove(0);
            }
        }
        self.commit(conversation_id, thread_id, identity_version, threads)
    }

    pub(crate) fn retain_thread(
        &mut self,
        conversation_id: ConversationId,
        thread_id: String,
        identity_version: Option<&str>,
    ) -> Result<(), ManagedThreadStoreError> {
        validate_thread_state(Some(conversation_id), Some(&thread_id), identity_version)?;
        let mut threads = self.threads.clone();
        let memory_tools_version = threads
            .iter()
            .find(|entry| entry.thread_id == thread_id && entry.conversation_id == conversation_id)
            .and_then(|entry| entry.memory_tools_version);
        threads.retain(|entry| {
            entry.thread_id != thread_id && entry.conversation_id != conversation_id
        });
        threads.push(ManagedThreadEntry {
            conversation_id,
            thread_id,
            identity_version: identity_version.map(str::to_owned),
            memory_tools_version,
        });
        if threads.len() > MAX_THREADS {
            threads.remove(0);
        }
        self.commit(
            self.conversation_id,
            self.thread_id.clone(),
            self.identity_version.clone(),
            threads,
        )
    }

    pub(crate) fn identity_version_for(&self, thread_id: &str) -> Option<&str> {
        self.threads
            .iter()
            .find(|entry| entry.thread_id == thread_id)
            .and_then(|entry| entry.identity_version.as_deref())
    }

    pub(crate) fn memory_tools_current(&self, thread_id: &str, available: bool) -> bool {
        self.threads.iter().any(|entry| {
            entry.thread_id == thread_id && entry.memory_tools_version == Some(u32::from(available))
        })
    }

    pub(crate) fn mark_memory_tools_current(
        &mut self,
        thread_id: &str,
        available: bool,
    ) -> Result<(), ManagedThreadStoreError> {
        let mut threads = self.threads.clone();
        let entry = threads
            .iter_mut()
            .find(|entry| entry.thread_id == thread_id)
            .ok_or_else(|| {
                ManagedThreadStoreError::Invalid(
                    "cannot mark memory tools for an unknown thread".into(),
                )
            })?;
        entry.memory_tools_version = Some(u32::from(available));
        self.commit(
            self.conversation_id,
            self.thread_id.clone(),
            self.identity_version.clone(),
            threads,
        )
    }

    pub(crate) fn archive_thread(
        &mut self,
        thread_id: &str,
    ) -> Result<bool, ManagedThreadStoreError> {
        validate_thread_id(thread_id)?;
        let mut threads = self.threads.clone();
        let original_len = threads.len();
        threads.retain(|entry| entry.thread_id != thread_id);
        if threads.len() == original_len {
            return Ok(false);
        }
        let current = (self.thread_id.as_deref() != Some(thread_id))
            .then(|| self.thread_id.clone())
            .flatten();
        let current_conversation = current.as_deref().and_then(|current| {
            threads
                .iter()
                .find(|entry| entry.thread_id == current)
                .map(|entry| entry.conversation_id)
        });
        let identity = current.as_deref().and_then(|current| {
            threads
                .iter()
                .find(|entry| entry.thread_id == current)
                .and_then(|entry| entry.identity_version.clone())
        });
        self.commit(current_conversation, current, identity, threads)?;
        Ok(true)
    }

    fn commit(
        &mut self,
        conversation_id: Option<ConversationId>,
        thread_id: Option<String>,
        identity_version: Option<String>,
        threads: Vec<ManagedThreadEntry>,
    ) -> Result<(), ManagedThreadStoreError> {
        let document = ManagedThreadDocument {
            version: DOCUMENT_VERSION,
            connection: self.connection.clone(),
            workspace: self.workspace.clone(),
            current_thread_id: thread_id.clone(),
            threads: threads.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| ManagedThreadStoreError::Invalid(error.to_string()))?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(ManagedThreadStoreError::Invalid(format!(
                "managed thread state exceeds the {MAX_DOCUMENT_BYTES}-byte limit"
            )));
        }
        if let Some(store) = &self.protected {
            store
                .set_document(
                    &document_name(&self.state_path)?,
                    &bytes,
                    MAX_DOCUMENT_BYTES,
                )
                .map_err(protected_error)?;
        } else {
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
        }
        self.thread_id = thread_id;
        self.conversation_id = conversation_id;
        self.identity_version = identity_version;
        self.threads = threads;
        Ok(())
    }

    pub(crate) fn select_thread(&mut self, thread_id: &str) -> Result<(), ManagedThreadStoreError> {
        let entry = self
            .threads
            .iter()
            .find(|entry| entry.thread_id == thread_id)
            .cloned()
            .ok_or_else(|| {
                ManagedThreadStoreError::Invalid("unknown managed conversation".to_owned())
            })?;
        self.set_thread(
            Some(entry.conversation_id),
            Some(entry.thread_id),
            entry.identity_version.as_deref(),
        )
    }

    pub(crate) fn list_for_workspace(
        data_root: &Path,
        workspace: &Path,
    ) -> Result<Vec<ManagedConversationHandle>, ManagedThreadStoreError> {
        const MAX_STATE_FILES: usize = 10_000;
        let workspace = workspace
            .canonicalize()
            .map_err(|source| ManagedThreadStoreError::Io {
                path: workspace.to_owned(),
                source,
            })?;
        let directory = data_root.join("managed-threads");
        if let Some(store) =
            crate::storage::ProtectedStore::configured(data_root).map_err(protected_error)?
        {
            let mut handles = Vec::new();
            for name in store
                .document_names("managed-threads/", MAX_STATE_FILES)
                .map_err(protected_error)?
            {
                let Some(bytes) = store
                    .document(&name, MAX_DOCUMENT_BYTES)
                    .map_err(protected_error)?
                else {
                    continue;
                };
                let Ok(decoded) = decode_catalog_document(&bytes, &workspace) else {
                    continue;
                };
                handles.extend(decoded.threads.into_iter().map(|thread| {
                    ManagedConversationHandle {
                        conversation_id: thread.conversation_id,
                        current: decoded.current_thread_id.as_deref()
                            == Some(thread.thread_id.as_str()),
                        connection: decoded.connection.clone(),
                        thread_id: thread.thread_id,
                    }
                }));
            }
            handles.sort_by(|a, b| {
                a.connection
                    .cmp(&b.connection)
                    .then_with(|| a.thread_id.cmp(&b.thread_id))
            });
            handles.truncate(MAX_THREADS);
            return Ok(handles);
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ManagedThreadStoreError::Io {
                    path: directory,
                    source,
                });
            }
        };
        let mut handles = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= MAX_STATE_FILES {
                return Err(ManagedThreadStoreError::Invalid(format!(
                    "managed conversation catalog exceeds {MAX_STATE_FILES} files"
                )));
            }
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = match bounded_file::read(&path, MAX_DOCUMENT_BYTES) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let Ok(decoded) = decode_catalog_document(&bytes, &workspace) else {
                continue;
            };
            let DecodedManagedDocument {
                current_thread_id,
                threads,
                connection,
                ..
            } = decoded;
            handles.extend(threads.into_iter().map(|thread| ManagedConversationHandle {
                conversation_id: thread.conversation_id,
                current: current_thread_id.as_deref() == Some(thread.thread_id.as_str()),
                connection: connection.clone(),
                thread_id: thread.thread_id,
            }));
        }
        handles.sort_by(|left, right| {
            left.connection
                .cmp(&right.connection)
                .then_with(|| left.thread_id.cmp(&right.thread_id))
        });
        handles.truncate(MAX_THREADS);
        Ok(handles)
    }
}

fn protected_error(error: anyhow::Error) -> ManagedThreadStoreError {
    ManagedThreadStoreError::Invalid(format!("protected managed handles: {error:#}"))
}

fn document_name(path: &Path) -> Result<String, ManagedThreadStoreError> {
    let file = path
        .file_name()
        .and_then(|part| part.to_str())
        .ok_or_else(|| {
            ManagedThreadStoreError::Invalid("invalid managed handle filename".into())
        })?;
    Ok(format!("managed-threads/{file}"))
}

fn read_managed_document(
    store: Option<&crate::storage::ProtectedStore>,
    path: &Path,
) -> Result<Vec<u8>, bounded_file::BoundedReadError> {
    if let Some(store) = store {
        let read = document_name(path)
            .map_err(|e| anyhow::anyhow!(e))
            .and_then(|name| store.document(&name, MAX_DOCUMENT_BYTES));
        return read
            .map_err(|error| bounded_file::BoundedReadError::Io {
                path: path.to_owned(),
                source: io::Error::other(error.to_string()),
            })?
            .ok_or_else(|| bounded_file::BoundedReadError::Io {
                path: path.to_owned(),
                source: io::Error::from(io::ErrorKind::NotFound),
            });
    }
    bounded_file::read(path, MAX_DOCUMENT_BYTES)
}

impl Drop for ManagedThreadStore {
    fn drop(&mut self) {
        // Make deliberate close-and-reopen behavior identical across platform
        // lock APIs instead of relying on implicit descriptor cleanup.
        let _ = self.writer_lock.unlock();
    }
}

fn decode_document(
    bytes: &[u8],
    connection: &str,
    workspace: &Path,
) -> Result<DecodedManagedDocument, ManagedThreadStoreError> {
    let decoded = decode_catalog_document(bytes, workspace)?;
    if decoded.connection != connection {
        return Err(ManagedThreadStoreError::Invalid(
            "route identity does not match its state file".into(),
        ));
    }
    Ok(decoded)
}

fn decode_catalog_document(
    bytes: &[u8],
    workspace: &Path,
) -> Result<DecodedManagedDocument, ManagedThreadStoreError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| ManagedThreadStoreError::Invalid(error.to_string()))?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(1) => {
            let document: ManagedThreadDocumentV1 = serde_json::from_value(value)
                .map_err(|error| ManagedThreadStoreError::Invalid(error.to_string()))?;
            if !same_workspace(&document.workspace, workspace) {
                return Err(ManagedThreadStoreError::Invalid(
                    "route identity does not match its state file".into(),
                ));
            }
            let conversation_id = document.thread_id.as_deref().map(|thread_id| {
                legacy_conversation_id(&document.connection, workspace, thread_id)
            });
            validate_thread_state(
                conversation_id,
                document.thread_id.as_deref(),
                document.identity_version.as_deref(),
            )?;
            let threads = document
                .thread_id
                .as_ref()
                .map(|thread_id| ManagedThreadEntry {
                    conversation_id: conversation_id.expect("legacy thread has derived identity"),
                    thread_id: thread_id.clone(),
                    identity_version: document.identity_version.clone(),
                    memory_tools_version: None,
                })
                .into_iter()
                .collect();
            Ok(DecodedManagedDocument {
                current_conversation_id: conversation_id,
                current_thread_id: document.thread_id,
                identity_version: document.identity_version,
                threads,
                connection: document.connection,
            })
        }
        Some(2) => {
            let document: ManagedThreadDocumentV2 = serde_json::from_value(value)
                .map_err(|error| ManagedThreadStoreError::Invalid(error.to_string()))?;
            if !same_workspace(&document.workspace, workspace)
                || document.threads.len() > MAX_THREADS
            {
                return Err(ManagedThreadStoreError::Invalid(
                    "route identity or thread bound is invalid".into(),
                ));
            }
            let threads = document
                .threads
                .into_iter()
                .map(|thread| ManagedThreadEntry {
                    conversation_id: legacy_conversation_id(
                        &document.connection,
                        workspace,
                        &thread.thread_id,
                    ),
                    thread_id: thread.thread_id,
                    identity_version: thread.identity_version,
                    memory_tools_version: None,
                })
                .collect::<Vec<_>>();
            decoded_document(document.connection, document.current_thread_id, threads)
        }
        Some(version) if version == u64::from(DOCUMENT_VERSION) => {
            let document: ManagedThreadDocument = serde_json::from_value(value)
                .map_err(|error| ManagedThreadStoreError::Invalid(error.to_string()))?;
            if !same_workspace(&document.workspace, workspace)
                || document.threads.len() > MAX_THREADS
            {
                return Err(ManagedThreadStoreError::Invalid(
                    "route identity or thread bound is invalid".into(),
                ));
            }
            decoded_document(
                document.connection,
                document.current_thread_id,
                document.threads,
            )
        }
        _ => Err(ManagedThreadStoreError::Invalid(
            "unsupported managed thread document version".to_owned(),
        )),
    }
}

fn decoded_document(
    connection: String,
    current_thread_id: Option<String>,
    threads: Vec<ManagedThreadEntry>,
) -> Result<DecodedManagedDocument, ManagedThreadStoreError> {
    let mut conversation_ids = std::collections::HashSet::new();
    let mut thread_ids = std::collections::HashSet::new();
    for thread in &threads {
        validate_thread_state(
            Some(thread.conversation_id),
            Some(&thread.thread_id),
            thread.identity_version.as_deref(),
        )?;
        if !conversation_ids.insert(thread.conversation_id)
            || !thread_ids.insert(thread.thread_id.as_str())
        {
            return Err(ManagedThreadStoreError::Invalid(
                "managed Conversation and thread identities must be unique".to_owned(),
            ));
        }
    }
    let current_entry = current_thread_id
        .as_deref()
        .map(|current| {
            threads
                .iter()
                .find(|thread| thread.thread_id == current)
                .ok_or_else(|| {
                    ManagedThreadStoreError::Invalid(
                        "current managed thread is not in the conversation catalog".to_owned(),
                    )
                })
        })
        .transpose()?;
    Ok(DecodedManagedDocument {
        current_conversation_id: current_entry.map(|entry| entry.conversation_id),
        current_thread_id,
        identity_version: current_entry.and_then(|entry| entry.identity_version.clone()),
        threads,
        connection,
    })
}

fn legacy_conversation_id(connection: &str, workspace: &Path, thread_id: &str) -> ConversationId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"xana-managed-conversation-legacy-v1");
    hasher.update(connection.as_bytes());
    hasher.update(&[0]);
    hasher.update(workspace.as_os_str().as_encoded_bytes());
    hasher.update(&[0]);
    hasher.update(thread_id.as_bytes());
    ConversationId::for_legacy_managed_route(hasher.finalize().as_bytes())
}

fn same_workspace(stored: &Path, expected: &Path) -> bool {
    same_file::is_same_file(stored, expected).unwrap_or(false)
}

fn validate_thread_state(
    conversation_id: Option<ConversationId>,
    thread_id: Option<&str>,
    identity_version: Option<&str>,
) -> Result<(), ManagedThreadStoreError> {
    if conversation_id.is_some() != thread_id.is_some() {
        return Err(ManagedThreadStoreError::Invalid(
            "managed Conversation identity and thread id must be present together".into(),
        ));
    }
    if let Some(thread_id) = thread_id {
        validate_thread_id(thread_id)?;
    }
    if identity_version.is_some() && thread_id.is_none() {
        return Err(ManagedThreadStoreError::Invalid(
            "identity version requires a managed thread id".into(),
        ));
    }
    if identity_version
        .is_some_and(|version| version.is_empty() || version.len() > MAX_IDENTITY_VERSION_BYTES)
    {
        return Err(ManagedThreadStoreError::Invalid(format!(
            "identity version must contain 1 to {MAX_IDENTITY_VERSION_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_thread_id(thread_id: &str) -> Result<(), ManagedThreadStoreError> {
    if thread_id.is_empty() || thread_id.len() > MAX_THREAD_ID_BYTES {
        return Err(ManagedThreadStoreError::Invalid(format!(
            "thread id must contain 1 to {MAX_THREAD_ID_BYTES} bytes"
        )));
    }
    Ok(())
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
            assert_eq!(store.identity_version(), None);
            let conversation_id = ConversationId::new();
            store
                .set_thread(
                    Some(conversation_id),
                    Some("thr_123".into()),
                    Some("xana-identity-v1"),
                )
                .unwrap();
            assert_eq!(store.conversation_id(), Some(conversation_id));
        }
        {
            let mut store =
                ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            assert_eq!(store.thread_id(), Some("thr_123"));
            assert_eq!(store.identity_version(), Some("xana-identity-v1"));
            store.set_thread(None, None, None).unwrap();
        }
        let store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        assert_eq!(store.thread_id(), None);
        assert_eq!(store.identity_version(), None);
        assert_eq!(store.threads.len(), 1);
        assert_eq!(store.threads[0].thread_id, "thr_123");
    }

    #[test]
    fn multiple_managed_conversations_are_retained_and_selectable() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        {
            let mut store =
                ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            store
                .set_thread(
                    Some(ConversationId::new()),
                    Some("thr_first".into()),
                    Some("identity-v1"),
                )
                .unwrap();
            store
                .set_thread(
                    Some(ConversationId::new()),
                    Some("thr_second".into()),
                    Some("identity-v2"),
                )
                .unwrap();
            assert_eq!(store.threads[0].thread_id, "thr_first");
            assert_eq!(store.threads[1].thread_id, "thr_second");
            store.select_thread("thr_first").unwrap();
            assert_eq!(store.thread_id(), Some("thr_first"));
            assert_eq!(store.identity_version(), Some("identity-v1"));
        }

        let catalog = ManagedThreadStore::list_for_workspace(directory.path(), &workspace)
            .expect("managed catalog");
        assert_eq!(catalog.len(), 2);
        assert!(
            catalog
                .iter()
                .any(|entry| entry.thread_id == "thr_first" && entry.current)
        );
        assert!(
            catalog
                .iter()
                .any(|entry| entry.thread_id == "thr_second" && !entry.current)
        );
    }

    #[test]
    fn changing_a_provider_thread_for_one_conversation_does_not_duplicate_identity() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let conversation_id = ConversationId::new();
        let mut store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        store
            .set_thread(
                Some(conversation_id),
                Some("thr_first".into()),
                Some("identity-v1"),
            )
            .unwrap();
        store
            .set_thread(
                Some(conversation_id),
                Some("thr_replaced".into()),
                Some("identity-v1"),
            )
            .unwrap();

        assert_eq!(store.threads.len(), 1);
        assert_eq!(store.thread_id(), Some("thr_replaced"));
        assert_eq!(store.conversation_id(), Some(conversation_id));
    }

    #[test]
    fn archiving_removes_only_the_named_local_handle() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        store
            .set_thread(
                Some(ConversationId::new()),
                Some("thr_first".into()),
                Some("identity-v1"),
            )
            .unwrap();
        store
            .set_thread(
                Some(ConversationId::new()),
                Some("thr_second".into()),
                Some("identity-v1"),
            )
            .unwrap();

        assert!(store.archive_thread("thr_first").unwrap());
        assert!(!store.archive_thread("thr_missing").unwrap());
        assert_eq!(store.thread_id(), Some("thr_second"));
        assert_eq!(store.threads.len(), 1);
        assert_eq!(store.threads[0].thread_id, "thr_second");
    }

    #[test]
    fn legacy_handle_without_identity_version_remains_detectable() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let state_path = {
            let store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            store.state_path.clone()
        };
        fs::write(
            state_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 1,
                "connection": "codex",
                "workspace": workspace,
                "thread_id": "thr_legacy"
            }))
            .unwrap(),
        )
        .unwrap();

        let store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        assert_eq!(store.thread_id(), Some("thr_legacy"));
        assert_eq!(store.identity_version(), None);
    }

    #[test]
    fn version_two_handles_receive_stable_xana_conversation_identities() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let state_path = {
            let store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            store.state_path.clone()
        };
        fs::write(
            &state_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 2,
                "connection": "codex",
                "workspace": workspace,
                "current_thread_id": "thr_legacy",
                "threads": [{
                    "thread_id": "thr_legacy",
                    "identity_version": "xana-identity-v1"
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let first = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        let first_id = first.conversation_id().expect("derived Conversation id");
        drop(first);
        let second = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();

        assert_eq!(second.conversation_id(), Some(first_id));
        assert_eq!(second.thread_id(), Some("thr_legacy"));
    }

    #[test]
    fn version_three_round_trip_preserves_xana_and_provider_identities() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let conversation_id = ConversationId::new();
        {
            let mut store =
                ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            store
                .set_thread(
                    Some(conversation_id),
                    Some("thr_provider_owned".into()),
                    Some("xana-identity-v1"),
                )
                .unwrap();
        }

        let store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        assert_eq!(store.conversation_id(), Some(conversation_id));
        assert_eq!(store.thread_id(), Some("thr_provider_owned"));
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&store.state_path).unwrap()).unwrap();
        assert_eq!(persisted["version"], 3);
        assert_eq!(
            persisted["threads"][0]["conversation_id"],
            conversation_id.to_string()
        );
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
            store.set_thread(
                Some(ConversationId::new()),
                Some("x".repeat(MAX_THREAD_ID_BYTES + 1)),
                Some("xana-identity-v1"),
            ),
            Err(ManagedThreadStoreError::Invalid(_))
        ));
        assert!(!store.state_path.is_file());
    }

    #[test]
    fn invalid_identity_versions_are_rejected_before_a_state_write() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();

        assert!(matches!(
            store.set_thread(None, None, Some("xana-identity-v1")),
            Err(ManagedThreadStoreError::Invalid(reason))
                if reason.contains("requires a managed thread id")
        ));
        assert!(matches!(
            store.set_thread(
                Some(ConversationId::new()),
                Some("thr_123".into()),
                Some(&"x".repeat(MAX_IDENTITY_VERSION_BYTES + 1)),
            ),
            Err(ManagedThreadStoreError::Invalid(reason))
                if reason.contains("identity version must contain")
        ));
        assert!(!store.state_path.is_file());
    }

    #[test]
    fn oversized_state_file_is_rejected_before_json_decoding() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let state_path = {
            let mut store =
                ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            store
                .set_thread(
                    Some(ConversationId::new()),
                    Some("thr_123".into()),
                    Some("xana-identity-v1"),
                )
                .unwrap();
            store.state_path.clone()
        };
        fs::write(&state_path, vec![b'x'; MAX_DOCUMENT_BYTES + 1]).unwrap();

        assert!(matches!(
            ManagedThreadStore::open(directory.path(), "codex", &workspace),
            Err(ManagedThreadStoreError::Invalid(reason)) if reason.contains("exceeding")
        ));
    }
}
