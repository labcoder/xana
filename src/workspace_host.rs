//! Process-owned, canonical-workspace conversation and root-turn ownership.

#[cfg(test)]
use crate::message::Message;
use crate::{
    bounded_file,
    identity::{ConversationId, SessionId},
    managed::thread_store::{ManagedConversationHandle, ManagedThreadStore},
    session::{DurableSession, NativeConversationHandle},
    workspace_identity::{WorkspaceIdentity, next_locked_generation},
};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const DESCRIPTOR_VERSION: u16 = 2;
const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;
const MAX_CONVERSATIONS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "owner", rename_all = "snake_case")]
pub(crate) enum ConversationRef {
    Native {
        session_id: SessionId,
    },
    Managed {
        conversation_id: ConversationId,
        connection: String,
        thread_id: String,
    },
    NewNative,
    NewManaged {
        conversation_id: ConversationId,
        connection: String,
    },
}

impl fmt::Display for ConversationRef {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native { session_id } => write!(output, "native/{session_id}"),
            Self::Managed {
                conversation_id,
                connection,
                thread_id,
            } => write!(
                output,
                "managed/{conversation_id} ({connection} thread {thread_id})"
            ),
            Self::NewNative => output.write_str("native/new"),
            Self::NewManaged {
                conversation_id,
                connection,
            } => write!(output, "managed/{conversation_id} ({connection}, pending)"),
        }
    }
}

impl ConversationRef {
    pub(crate) fn conversation_id(&self) -> Option<ConversationId> {
        match self {
            Self::Native { session_id } => Some(ConversationId::for_native(*session_id)),
            Self::Managed {
                conversation_id, ..
            }
            | Self::NewManaged {
                conversation_id, ..
            } => Some(*conversation_id),
            Self::NewNative => None,
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
    /// Optional local organization only; `None` is the first-class Ungrouped view.
    pub(crate) project: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) workspace: PathBuf,
    pub(crate) workspace_id: String,
    pub(crate) conversations: Vec<ConversationProjection>,
    pub(crate) active: Option<ActiveRootDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActiveRootDescriptor {
    version: u16,
    workspace: PathBuf,
    workspace_id: String,
    host_id: Uuid,
    generation: u64,
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
    Busy(Option<Box<ActiveRootDescriptor>>),
    Invalid(String),
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for WorkspaceHostError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(Some(owner)) => write!(
                output,
                "workspace already has an active Xana root ({}, process {}, generation {}). Wait or cancel it in its controlling terminal; use `xana attach` when a foreground server is available, or start and draft a new conversation without submitting work",
                owner.conversation, owner.process_id, owner.generation
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
    protected: Option<crate::storage::ProtectedStore>,
    data_root: PathBuf,
    workspace: PathBuf,
    workspace_id: String,
    host_id: Uuid,
    lock_path: PathBuf,
    descriptor_path: PathBuf,
    controlled: Arc<Mutex<Option<ConversationRef>>>,
}

impl WorkspaceHost {
    pub(crate) fn open(data_root: &Path, workspace: &Path) -> Result<Self, WorkspaceHostError> {
        let identity =
            WorkspaceIdentity::resolve(workspace).map_err(|source| WorkspaceHostError::Io {
                path: workspace.to_owned(),
                source,
            })?;
        let workspace = identity.canonical_path().to_owned();
        let workspace_id = identity.collision_key().to_owned();
        let directory = data_root.join("workspace-hosts");
        fs::create_dir_all(&directory).map_err(|source| WorkspaceHostError::Io {
            path: directory.clone(),
            source,
        })?;
        Ok(Self {
            protected: crate::storage::ProtectedStore::configured(data_root)
                .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?,
            data_root: data_root.to_owned(),
            workspace,
            workspace_id: workspace_id.clone(),
            host_id: Uuid::new_v4(),
            lock_path: directory.join(format!("{workspace_id}.lock")),
            descriptor_path: directory.join(format!("{workspace_id}.json")),
            controlled: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
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
                    self.read_descriptor().ok().flatten().map(Box::new),
                ));
            }
            Err(fs::TryLockError::Error(source)) => {
                return Err(WorkspaceHostError::Io {
                    path: self.lock_path.clone(),
                    source,
                });
            }
        }
        let generation = next_generation(&lock, &self.lock_path)?;
        let descriptor = ActiveRootDescriptor {
            version: DESCRIPTOR_VERSION,
            workspace: self.workspace.clone(),
            workspace_id: self.workspace_id.clone(),
            host_id: self.host_id,
            generation,
            process_id: std::process::id(),
            conversation: conversation.clone(),
        };
        if let Err(error) =
            write_owned_descriptor(self.protected.as_ref(), &self.descriptor_path, &descriptor)
        {
            drop(lock);
            return Err(error);
        }
        *self.controlled.lock().map_err(|_| {
            WorkspaceHostError::Invalid("host ownership lock was poisoned".into())
        })? = Some(conversation);
        Ok(ActiveRootLease {
            protected: self.protected.clone(),
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
        if let Ok(projects) = crate::private_state::read_document::<
            crate::private_state::ProjectRegistryDocument,
        >(&self.data_root.join("interoperable/projects.json"))
        {
            for (conversation_id, branch) in &projects.conversation_branches {
                let crate::private_state::ConversationBranchContinuation::ManagedFreshContinuation {
                    connection,
                    ..
                } = &branch.continuation
                else {
                    continue;
                };
                if !same_file::is_same_file(&branch.workspace_root, &self.workspace)
                    .unwrap_or(false)
                    || conversations.iter().any(|projection| {
                        projection.conversation.conversation_id() == Some(*conversation_id)
                    })
                {
                    continue;
                }
                let conversation = ConversationRef::NewManaged {
                    conversation_id: *conversation_id,
                    connection: connection.clone(),
                };
                conversations.push(ConversationProjection {
                    state: state_for(&conversation, active.as_ref(), controlled.as_ref()),
                    conversation,
                    record_count: None,
                    modified: None,
                    selected: false,
                    project: None,
                });
            }
            for projection in &mut conversations {
                let keys = conversation_membership_keys(&projection.conversation);
                projection.project = keys
                    .iter()
                    .find_map(|key| projects.conversation_memberships.get(key))
                    .and_then(|project| projects.projects.get(project))
                    .map(|project| project.name.clone());
            }
        }
        conversations.truncate(MAX_CONVERSATIONS);
        Ok(WorkspaceSnapshot {
            workspace: self.workspace.clone(),
            workspace_id: self.workspace_id.clone(),
            conversations,
            active,
        })
    }

    #[cfg(test)]
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

    pub(crate) fn conversation_history_page(
        &self,
        conversation: &ConversationRef,
        before: Option<usize>,
        limit: usize,
    ) -> Result<Option<crate::session::ConversationPage>, WorkspaceHostError> {
        match conversation {
            ConversationRef::Native { session_id } => {
                DurableSession::conversation_page(&self.data_root, *session_id, before, limit)
                    .map(Some)
                    .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))
            }
            ConversationRef::Managed { .. } => Ok(None),
            ConversationRef::NewNative | ConversationRef::NewManaged { .. } => {
                Ok(Some(crate::session::ConversationPage {
                    messages: Vec::new(),
                    start: 0,
                    total: 0,
                    has_older: false,
                }))
            }
        }
    }

    pub(crate) fn conversation_history_from(
        &self,
        conversation: &ConversationRef,
        start: usize,
        limit: usize,
    ) -> Result<Option<crate::session::ConversationPage>, WorkspaceHostError> {
        if let ConversationRef::Native { session_id } = conversation {
            DurableSession::conversation_page_from(&self.data_root, *session_id, start, limit)
                .map(Some)
                .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))
        } else {
            self.conversation_history_page(conversation, None, limit)
        }
    }

    pub(crate) fn archive_managed_conversation(
        &self,
        conversation: &ConversationRef,
    ) -> Result<bool, WorkspaceHostError> {
        let ConversationRef::Managed {
            connection,
            thread_id,
            ..
        } = conversation
        else {
            return Err(WorkspaceHostError::Invalid(
                "only managed conversation handles can be archived".to_owned(),
            ));
        };
        if self
            .active_descriptor()?
            .is_some_and(|active| active.conversation == *conversation)
        {
            return Err(WorkspaceHostError::Invalid(
                "an active managed conversation cannot be archived".to_owned(),
            ));
        }
        let mut store = ManagedThreadStore::open(&self.data_root, connection, &self.workspace)
            .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?;
        store
            .archive_thread(thread_id)
            .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))
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
        let bytes = match read_owned_descriptor(self.protected.as_ref(), &self.descriptor_path) {
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
        let matches_workspace = WorkspaceIdentity::resolve(&self.workspace)
            .and_then(|identity| identity.matches(&descriptor.workspace))
            .unwrap_or(false);
        if descriptor.version != DESCRIPTOR_VERSION
            || descriptor.workspace_id != self.workspace_id
            || !matches_workspace
            || descriptor.generation == 0
        {
            return Err(WorkspaceHostError::Invalid(
                "descriptor identity does not match its workspace".to_owned(),
            ));
        }
        Ok(Some(descriptor))
    }
}

pub(crate) struct ActiveRootLease {
    protected: Option<crate::storage::ProtectedStore>,
    lock: Option<fs::File>,
    descriptor_path: PathBuf,
    host_id: Uuid,
    controlled: Arc<Mutex<Option<ConversationRef>>>,
}

impl Drop for ActiveRootLease {
    fn drop(&mut self) {
        if let Ok(bytes) = read_owned_descriptor(self.protected.as_ref(), &self.descriptor_path)
            && let Ok(descriptor) = serde_json::from_slice::<ActiveRootDescriptor>(&bytes)
            && descriptor.host_id == self.host_id
        {
            if let Some(store) = &self.protected {
                if let Some(name) = descriptor_name(&self.descriptor_path) {
                    let _ = store.remove_document(&name);
                }
            } else {
                let _ = fs::remove_file(&self.descriptor_path);
            }
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

fn next_generation(lock: &fs::File, path: &Path) -> Result<u64, WorkspaceHostError> {
    next_locked_generation(lock).map_err(|source| {
        if source.kind() == io::ErrorKind::InvalidData {
            WorkspaceHostError::Invalid(format!(
                "{} contains an invalid owner generation; run `xana doctor` before retrying",
                path.display()
            ))
        } else {
            WorkspaceHostError::Io {
                path: path.to_owned(),
                source,
            }
        }
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

fn descriptor_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("workspace-hosts/{name}"))
}

fn read_owned_descriptor(
    store: Option<&crate::storage::ProtectedStore>,
    path: &Path,
) -> Result<Vec<u8>, bounded_file::BoundedReadError> {
    if let Some(store) = store {
        let result = descriptor_name(path)
            .ok_or_else(|| anyhow::anyhow!("invalid descriptor name"))
            .and_then(|name| store.document(&name, MAX_DESCRIPTOR_BYTES));
        return result
            .map_err(|error| bounded_file::BoundedReadError::Io {
                path: path.to_owned(),
                source: io::Error::other(error.to_string()),
            })?
            .ok_or_else(|| bounded_file::BoundedReadError::Io {
                path: path.to_owned(),
                source: io::Error::from(io::ErrorKind::NotFound),
            });
    }
    bounded_file::read(path, MAX_DESCRIPTOR_BYTES)
}

fn write_owned_descriptor(
    store: Option<&crate::storage::ProtectedStore>,
    path: &Path,
    descriptor: &ActiveRootDescriptor,
) -> Result<(), WorkspaceHostError> {
    if let Some(store) = store {
        let name = descriptor_name(path)
            .ok_or_else(|| WorkspaceHostError::Invalid("invalid descriptor name".into()))?;
        let bytes = serde_json::to_vec(descriptor)
            .map_err(|error| WorkspaceHostError::Invalid(error.to_string()))?;
        return store
            .set_document(&name, &bytes, MAX_DESCRIPTOR_BYTES)
            .map_err(|error| WorkspaceHostError::Invalid(error.to_string()));
    }
    write_descriptor(path, descriptor)
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
        project: None,
    }
}

fn managed_projection(
    entry: ManagedConversationHandle,
    active: Option<&ActiveRootDescriptor>,
    controlled: Option<&ConversationRef>,
) -> ConversationProjection {
    let conversation = ConversationRef::Managed {
        conversation_id: entry.conversation_id,
        connection: entry.connection,
        thread_id: entry.thread_id,
    };
    ConversationProjection {
        state: state_for(&conversation, active, controlled),
        conversation,
        record_count: None,
        modified: None,
        selected: entry.current,
        project: None,
    }
}

fn conversation_membership_keys(conversation: &ConversationRef) -> Vec<String> {
    match conversation {
        ConversationRef::Native { session_id } => {
            vec![session_id.to_string(), conversation.to_string()]
        }
        ConversationRef::Managed {
            conversation_id,
            connection,
            thread_id,
        } => vec![
            conversation_id.to_string(),
            thread_id.clone(),
            format!("{connection}/{thread_id}"),
            conversation.to_string(),
        ],
        ConversationRef::NewManaged {
            conversation_id, ..
        } => vec![conversation_id.to_string(), conversation.to_string()],
        ConversationRef::NewNative => vec![conversation.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_projects_optional_local_membership_and_keeps_ungrouped_first_class() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let session =
            DurableSession::create(directory.path(), workspace.canonicalize().unwrap()).unwrap();
        let session_id = session.session_id();
        drop(session);
        let project_id = crate::identity::ProjectId::new();
        let mut projects = crate::private_state::ProjectRegistryDocument::default();
        projects.projects.insert(
            project_id,
            crate::private_state::ProjectRecord {
                id: project_id,
                name: "Xana".into(),
                canonical_workspace: workspace.canonicalize().unwrap(),
                lifecycle: crate::private_state::ProjectLifecycle::Active,
                created_unix_ms: 1,
                updated_unix_ms: 1,
            },
        );
        projects
            .conversation_memberships
            .insert(session_id.to_string(), project_id);
        fs::create_dir_all(directory.path().join("interoperable")).unwrap();
        fs::write(
            directory.path().join("interoperable/projects.json"),
            serde_json::to_vec_pretty(&projects).unwrap(),
        )
        .unwrap();

        let snapshot = WorkspaceHost::open(directory.path(), &workspace)
            .unwrap()
            .snapshot()
            .unwrap();
        assert_eq!(snapshot.conversations[0].project.as_deref(), Some("Xana"));
    }

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
    fn root_owner_generations_increase_across_leases() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let first = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let first_lease = first.acquire_root(ConversationRef::NewNative).unwrap();
        let first_generation = first.snapshot().unwrap().active.unwrap().generation;
        drop(first_lease);

        let second = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let _second_lease = second.acquire_root(ConversationRef::NewNative).unwrap();
        let second_generation = second.snapshot().unwrap().active.unwrap().generation;

        assert!(second_generation > first_generation);
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
                .set_thread(
                    Some(ConversationId::new()),
                    Some("thread-a".into()),
                    Some("identity-v1"),
                )
                .unwrap();
            managed
                .set_thread(
                    Some(ConversationId::new()),
                    Some("thread-b".into()),
                    Some("identity-v1"),
                )
                .unwrap();
        }
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let snapshot = host.snapshot().unwrap();

        assert_eq!(snapshot.workspace, workspace.canonicalize().unwrap());
        assert_eq!(snapshot.conversations.len(), 4);
        assert!(snapshot.active.is_none());
    }

    #[test]
    fn managed_archive_refuses_the_active_root_then_removes_only_its_local_handle() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let conversation_id = ConversationId::new();
        let conversation = ConversationRef::Managed {
            conversation_id,
            connection: "codex".to_owned(),
            thread_id: "thread-active".to_owned(),
        };
        let mut store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        store
            .set_thread(
                Some(conversation_id),
                Some("thread-active".into()),
                Some("identity-v1"),
            )
            .unwrap();
        drop(store);
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let lease = host.acquire_root(conversation.clone()).unwrap();

        assert!(matches!(
            host.archive_managed_conversation(&conversation),
            Err(WorkspaceHostError::Invalid(reason)) if reason.contains("active")
        ));

        drop(lease);
        assert!(host.archive_managed_conversation(&conversation).unwrap());
        assert!(
            host.snapshot()
                .unwrap()
                .conversations
                .iter()
                .all(|entry| entry.conversation != conversation)
        );
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
                conversation_id: ConversationId::new(),
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
                workspace_id: host.workspace_id.clone(),
                host_id: Uuid::new_v4(),
                generation: 1,
                process_id: u32::MAX,
                conversation: ConversationRef::NewNative,
            },
        )
        .unwrap();

        assert!(host.snapshot().unwrap().active.is_none());
        assert!(host.acquire_root(ConversationRef::NewNative).is_ok());
    }
}
