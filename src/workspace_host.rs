//! Process-owned, canonical-workspace conversation and root-turn ownership.

use crate::{
    bounded_file,
    identity::SessionId,
    managed::thread_store::{ManagedConversationHandle, ManagedThreadStore},
    message::Message,
    session::{DurableSession, NativeConversationHandle},
};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const DESCRIPTOR_VERSION: u16 = 1;
const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;
const MAX_CONVERSATIONS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "owner", rename_all = "snake_case")]
pub(crate) enum ConversationRef {
    Native {
        session_id: SessionId,
    },
    Managed {
        connection: String,
        thread_id: String,
    },
    NewNative,
    NewManaged {
        connection: String,
    },
}

impl fmt::Display for ConversationRef {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native { session_id } => write!(output, "native/{session_id}"),
            Self::Managed {
                connection,
                thread_id,
            } => write!(output, "managed/{connection}/{thread_id}"),
            Self::NewNative => output.write_str("native/new"),
            Self::NewManaged { connection } => write!(output, "managed/{connection}/new"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConversationState {
    Inactive,
    Active,
    Controlled,
    Observable,
    Unavailable,
}

impl ConversationState {
    pub(crate) const fn all() -> [Self; 5] {
        [
            Self::Inactive,
            Self::Active,
            Self::Controlled,
            Self::Observable,
            Self::Unavailable,
        ]
    }
}

impl fmt::Display for ConversationState {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Inactive => "inactive",
            Self::Active => "active",
            Self::Controlled => "controlled",
            Self::Observable => "observable",
            Self::Unavailable => "unavailable",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationProjection {
    pub(crate) conversation: ConversationRef,
    pub(crate) state: ConversationState,
    pub(crate) record_count: Option<usize>,
    pub(crate) modified: Option<std::time::SystemTime>,
    pub(crate) selected: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) workspace: PathBuf,
    pub(crate) conversations: Vec<ConversationProjection>,
    pub(crate) active: Option<ActiveRootDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActiveRootDescriptor {
    version: u16,
    workspace: PathBuf,
    host_id: Uuid,
    process_id: u32,
    pub(crate) conversation: ConversationRef,
}

impl ActiveRootDescriptor {
    pub(crate) fn process_id(&self) -> u32 {
        self.process_id
    }
}

#[derive(Debug)]
pub(crate) enum WorkspaceHostError {
    Busy(Option<ActiveRootDescriptor>),
    Invalid(String),
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for WorkspaceHostError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(Some(owner)) => write!(
                output,
                "workspace already has an active Xana root ({}, process {}). Wait or cancel it in its controlling terminal; use `xana attach` when a foreground server is available, or start and draft a new conversation without submitting work",
                owner.conversation, owner.process_id
            ),
            Self::Busy(None) => output.write_str(
                "workspace already has an active Xana root. Wait or cancel it in its controlling terminal; use `xana attach` when a foreground server is available, or start and draft a new conversation without submitting work",
            ),
            Self::Invalid(reason) => write!(output, "invalid workspace host state: {reason}"),
            Self::Io { path, source } => {
                write!(output, "could not access workspace host state at {}: {source}", path.display())
            }
        }
    }
}

impl Error for WorkspaceHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) struct WorkspaceHost {
    data_root: PathBuf,
    workspace: PathBuf,
    host_id: Uuid,
    lock_path: PathBuf,
    descriptor_path: PathBuf,
    controlled: Arc<Mutex<Option<ConversationRef>>>,
}

impl WorkspaceHost {
    pub(crate) fn open(data_root: &Path, workspace: &Path) -> Result<Self, WorkspaceHostError> {
        let workspace = workspace
            .canonicalize()
            .map_err(|source| WorkspaceHostError::Io {
                path: workspace.to_owned(),
                source,
            })?;
        let directory = data_root.join("workspace-hosts");
        fs::create_dir_all(&directory).map_err(|source| WorkspaceHostError::Io {
            path: directory.clone(),
            source,
        })?;
        let key = blake3::hash(workspace.as_os_str().as_encoded_bytes()).to_hex();
        Ok(Self {
            data_root: data_root.to_owned(),
            workspace,
            host_id: Uuid::new_v4(),
            lock_path: directory.join(format!("{key}.lock")),
            descriptor_path: directory.join(format!("{key}.json")),
            controlled: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn acquire_root(
        &self,
        conversation: ConversationRef,
    ) -> Result<ActiveRootLease, WorkspaceHostError> {
        if self
            .controlled
            .lock()
            .map_err(|_| WorkspaceHostError::Invalid("host ownership lock was poisoned".into()))?
            .is_some()
        {
            return Err(WorkspaceHostError::Invalid(
                "this host already controls an active root".to_owned(),
            ));
        }
        let lock = open_lock(&self.lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(WorkspaceHostError::Busy(
                    self.read_descriptor().ok().flatten(),
                ));
            }
            Err(fs::TryLockError::Error(source)) => {
                return Err(WorkspaceHostError::Io {
                    path: self.lock_path.clone(),
                    source,
                });
            }
        }
        let descriptor = ActiveRootDescriptor {
            version: DESCRIPTOR_VERSION,
            workspace: self.workspace.clone(),
            host_id: self.host_id,
            process_id: std::process::id(),
            conversation: conversation.clone(),
        };
        if let Err(error) = write_descriptor(&self.descriptor_path, &descriptor) {
            drop(lock);
            return Err(error);
        }
        *self.controlled.lock().map_err(|_| {
            WorkspaceHostError::Invalid("host ownership lock was poisoned".into())
        })? = Some(conversation);
        Ok(ActiveRootLease {
            lock: Some(lock),
            descriptor_path: self.descriptor_path.clone(),
            host_id: self.host_id,
            controlled: Arc::clone(&self.controlled),
        })
    }

    pub(crate) fn snapshot(&self) -> Result<WorkspaceSnapshot, WorkspaceHostError> {
        let active = self.active_descriptor()?;
        let controlled = self
            .controlled
            .lock()
            .map_err(|_| WorkspaceHostError::Invalid("host ownership lock was poisoned".into()))?
            .clone();
        let mut conversations =
            DurableSession::list_for_workspace(&self.data_root, &self.workspace)
                .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?
                .into_iter()
                .map(|entry| native_projection(entry, active.as_ref(), controlled.as_ref()))
                .collect::<Vec<_>>();
        let managed = ManagedThreadStore::list_for_workspace(&self.data_root, &self.workspace)
            .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?;
        conversations.extend(
            managed
                .into_iter()
                .map(|entry| managed_projection(entry, active.as_ref(), controlled.as_ref())),
        );
        conversations.truncate(MAX_CONVERSATIONS);
        Ok(WorkspaceSnapshot {
            workspace: self.workspace.clone(),
            conversations,
            active,
        })
    }

    pub(crate) fn conversation_history(
        &self,
        conversation: &ConversationRef,
    ) -> Result<Option<Vec<Message>>, WorkspaceHostError> {
        match conversation {
            ConversationRef::Native { session_id } => {
                let (_, restored) = DurableSession::inspect_restored(&self.data_root, *session_id)
                    .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?;
                restored
                    .conversation_path()
                    .map(Some)
                    .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))
            }
            ConversationRef::Managed { .. } => Ok(None),
            ConversationRef::NewNative | ConversationRef::NewManaged { .. } => Ok(Some(Vec::new())),
        }
    }

    fn active_descriptor(&self) -> Result<Option<ActiveRootDescriptor>, WorkspaceHostError> {
        let lock = match open_lock(&self.lock_path) {
            Ok(lock) => lock,
            Err(WorkspaceHostError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        match lock.try_lock() {
            Ok(()) => {
                fs::File::unlock(&lock).map_err(|source| WorkspaceHostError::Io {
                    path: self.lock_path.clone(),
                    source,
                })?;
                Ok(None)
            }
            Err(fs::TryLockError::WouldBlock) => self.read_descriptor(),
            Err(fs::TryLockError::Error(source)) => Err(WorkspaceHostError::Io {
                path: self.lock_path.clone(),
                source,
            }),
        }
    }

    fn read_descriptor(&self) -> Result<Option<ActiveRootDescriptor>, WorkspaceHostError> {
        let bytes = match bounded_file::read(&self.descriptor_path, MAX_DESCRIPTOR_BYTES) {
            Ok(bytes) => bytes,
            Err(bounded_file::BoundedReadError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(WorkspaceHostError::Invalid(error.to_string())),
        };
        let descriptor: ActiveRootDescriptor = serde_json::from_slice(&bytes)
            .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?;
        if descriptor.version != DESCRIPTOR_VERSION || descriptor.workspace != self.workspace {
            return Err(WorkspaceHostError::Invalid(
                "descriptor identity does not match its workspace".to_owned(),
            ));
        }
        Ok(Some(descriptor))
    }
}

pub(crate) struct ActiveRootLease {
    lock: Option<fs::File>,
    descriptor_path: PathBuf,
    host_id: Uuid,
    controlled: Arc<Mutex<Option<ConversationRef>>>,
}

impl Drop for ActiveRootLease {
    fn drop(&mut self) {
        if let Ok(bytes) = bounded_file::read(&self.descriptor_path, MAX_DESCRIPTOR_BYTES)
            && let Ok(descriptor) = serde_json::from_slice::<ActiveRootDescriptor>(&bytes)
            && descriptor.host_id == self.host_id
        {
            let _ = fs::remove_file(&self.descriptor_path);
        }
        if let Ok(mut controlled) = self.controlled.lock() {
            *controlled = None;
        }
        if let Some(lock) = self.lock.take() {
            let _ = fs::File::unlock(&lock);
        }
    }
}

fn open_lock(path: &Path) -> Result<fs::File, WorkspaceHostError> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| WorkspaceHostError::Io {
            path: path.to_owned(),
            source,
        })
}

fn write_descriptor(
    path: &Path,
    descriptor: &ActiveRootDescriptor,
) -> Result<(), WorkspaceHostError> {
    use io::Write as _;
    let bytes = serde_json::to_vec(descriptor)
        .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?;
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(WorkspaceHostError::Invalid(
            "active-root descriptor exceeds its byte bound".to_owned(),
        ));
    }
    let mut file = atomic_write_file::AtomicWriteFile::open(path).map_err(|source| {
        WorkspaceHostError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    file.write_all(&bytes)
        .map_err(|source| WorkspaceHostError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.commit().map_err(|source| WorkspaceHostError::Io {
        path: path.to_owned(),
        source,
    })
}

fn state_for(
    conversation: &ConversationRef,
    active: Option<&ActiveRootDescriptor>,
    controlled: Option<&ConversationRef>,
) -> ConversationState {
    if controlled == Some(conversation) {
        ConversationState::Controlled
    } else if active.is_some_and(|descriptor| descriptor.conversation == *conversation) {
        ConversationState::Active
    } else {
        ConversationState::Inactive
    }
}

fn native_projection(
    entry: NativeConversationHandle,
    active: Option<&ActiveRootDescriptor>,
    controlled: Option<&ConversationRef>,
) -> ConversationProjection {
    let conversation = ConversationRef::Native {
        session_id: entry.session_id,
    };
    ConversationProjection {
        state: state_for(&conversation, active, controlled),
        conversation,
        record_count: Some(entry.record_count),
        modified: Some(entry.modified),
        selected: false,
    }
}

fn managed_projection(
    entry: ManagedConversationHandle,
    active: Option<&ActiveRootDescriptor>,
    controlled: Option<&ConversationRef>,
) -> ConversationProjection {
    let conversation = ConversationRef::Managed {
        connection: entry.connection,
        thread_id: entry.thread_id,
    };
    ConversationProjection {
        state: state_for(&conversation, active, controlled),
        conversation,
        record_count: None,
        modified: None,
        selected: entry.current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn canonical_aliases_share_one_root_gate_and_drop_releases_it() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let aliased = workspace.join(".");
        let first = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let second = WorkspaceHost::open(directory.path(), &aliased).unwrap();
        let lease = first.acquire_root(ConversationRef::NewNative).unwrap();

        assert!(matches!(
            second.acquire_root(ConversationRef::NewNative),
            Err(WorkspaceHostError::Busy(Some(_)))
        ));
        drop(lease);
        assert!(second.acquire_root(ConversationRef::NewNative).is_ok());
    }

    #[test]
    fn snapshot_lists_multiple_native_and_managed_conversations() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        drop(DurableSession::create(directory.path(), workspace.clone()).unwrap());
        drop(DurableSession::create(directory.path(), workspace.clone()).unwrap());
        {
            let mut managed =
                ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
            managed
                .set_thread(Some("thread-a".into()), Some("identity-v1"))
                .unwrap();
            managed
                .set_thread(Some("thread-b".into()), Some("identity-v1"))
                .unwrap();
        }
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let snapshot = host.snapshot().unwrap();

        assert_eq!(snapshot.workspace, workspace.canonicalize().unwrap());
        assert_eq!(snapshot.conversations.len(), 4);
        assert!(snapshot.active.is_none());
    }

    #[test]
    fn explicit_history_reads_native_content_but_never_invents_managed_content() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let native = DurableSession::create(directory.path(), workspace.clone()).unwrap();
        let session_id = native.session_id();
        drop(native);
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();

        assert_eq!(
            host.conversation_history(&ConversationRef::Native { session_id })
                .unwrap(),
            Some(Vec::new())
        );
        assert_eq!(
            host.conversation_history(&ConversationRef::Managed {
                connection: "codex".to_owned(),
                thread_id: "opaque".to_owned(),
            })
            .unwrap(),
            None
        );
    }

    #[test]
    fn stale_descriptor_without_a_lock_is_not_treated_as_active() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        write_descriptor(
            &host.descriptor_path,
            &ActiveRootDescriptor {
                version: DESCRIPTOR_VERSION,
                workspace: host.workspace.clone(),
                host_id: Uuid::new_v4(),
                process_id: u32::MAX,
                conversation: ConversationRef::NewNative,
            },
        )
        .unwrap();

        assert!(host.snapshot().unwrap().active.is_none());
        assert!(host.acquire_root(ConversationRef::NewNative).is_ok());
    }

    #[test]
    fn state_vocabulary_keeps_observation_and_unavailability_distinct() {
        assert_ne!(ConversationState::Observable, ConversationState::Controlled);
        assert_ne!(ConversationState::Unavailable, ConversationState::Inactive);
    }
}
