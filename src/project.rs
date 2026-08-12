//! Optional project identity and lifecycle, independent of presentation.
//!
//! Projects organize conversations but never own or mutate workspaces. The
//! registry and membership relation are one atomically replaced private record.

use crate::{
    identity::{ProjectId, SessionId},
    paths::XanaPaths,
    private_state::{
        PrivateStateError, ProjectLifecycle, ProjectRecord, ProjectRegistryDocument,
        UpdateDocumentError, ensure_interoperable_records, read_document, update_document,
    },
};
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_PROJECT_NAME_BYTES: usize = 128;
const MAX_CONVERSATION_KEY_BYTES: usize = 256;
const MAX_PROJECTS: usize = 10_000;
const MAX_MEMBERSHIPS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Project {
    pub(crate) id: ProjectId,
    pub(crate) name: String,
    pub(crate) canonical_workspace: PathBuf,
    pub(crate) lifecycle: ProjectLifecycle,
    pub(crate) created_unix_ms: u64,
    pub(crate) updated_unix_ms: u64,
}

impl From<&ProjectRecord> for Project {
    fn from(record: &ProjectRecord) -> Self {
        Self {
            id: record.id,
            name: record.name.clone(),
            canonical_workspace: record.canonical_workspace.clone(),
            lifecycle: record.lifecycle,
            created_unix_ms: record.created_unix_ms,
            updated_unix_ms: record.updated_unix_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceStatus {
    Available,
    Missing,
    ChangedIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectInspection {
    pub(crate) project: Project,
    pub(crate) workspace_status: WorkspaceStatus,
    pub(crate) conversation_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationOwner {
    Native,
    ManagedCodex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContinuationPlacement {
    ReassignExisting,
    StartFresh {
        new_conversation: SessionId,
        owner: ContinuationOwner,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectContinuation {
    pub(crate) source_conversation: String,
    pub(crate) project: ProjectId,
    pub(crate) placement: ContinuationPlacement,
    /// Product-authored and bounded; it never includes transcript text.
    pub(crate) handoff: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectStore {
    projects_file: PathBuf,
}

impl ProjectStore {
    pub(crate) fn open(paths: &XanaPaths) -> Result<Self, ProjectError> {
        ensure_interoperable_records(paths).map_err(ProjectError::State)?;
        Ok(Self {
            projects_file: paths.projects_file(),
        })
    }

    pub(crate) fn list(&self, include_archived: bool) -> Result<Vec<Project>, ProjectError> {
        let document = self.read()?;
        let mut projects = document
            .projects
            .values()
            .filter(|record| include_archived || record.lifecycle == ProjectLifecycle::Active)
            .map(Project::from)
            .collect::<Vec<_>>();
        projects.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(projects)
    }

    pub(crate) fn get(&self, id: ProjectId) -> Result<Project, ProjectError> {
        let document = self.read()?;
        document
            .projects
            .get(&id)
            .map(Project::from)
            .ok_or(ProjectError::Unknown(id))
    }

    pub(crate) fn inspect(&self, id: ProjectId) -> Result<ProjectInspection, ProjectError> {
        let document = self.read()?;
        let record = document
            .projects
            .get(&id)
            .ok_or(ProjectError::Unknown(id))?;
        let workspace_status = match record.canonical_workspace.canonicalize() {
            Ok(current) if current == record.canonical_workspace => WorkspaceStatus::Available,
            Ok(_) => WorkspaceStatus::ChangedIdentity,
            Err(error) if error.kind() == io::ErrorKind::NotFound => WorkspaceStatus::Missing,
            Err(error) => {
                return Err(ProjectError::Io {
                    path: record.canonical_workspace.clone(),
                    source: error,
                });
            }
        };
        let conversation_count = document
            .conversation_memberships
            .values()
            .filter(|project| **project == id)
            .count();
        Ok(ProjectInspection {
            project: Project::from(record),
            workspace_status,
            conversation_count,
        })
    }

    pub(crate) fn create(&self, name: &str, workspace: &Path) -> Result<Project, ProjectError> {
        let name = validate_name(name)?;
        let canonical = canonical_workspace(workspace)?;
        let id = ProjectId::new();
        let now = now_unix_ms()?;
        let record = ProjectRecord {
            id,
            name,
            canonical_workspace: canonical.clone(),
            lifecycle: ProjectLifecycle::Active,
            created_unix_ms: now,
            updated_unix_ms: now,
        };
        self.update(|document| {
            if document.projects.len() >= MAX_PROJECTS {
                return Err(ProjectError::Limit("project registry", MAX_PROJECTS));
            }
            ensure_workspace_available(document, &canonical, None)?;
            document.projects.insert(id, record.clone());
            Ok(())
        })?;
        Ok(Project::from(&record))
    }

    pub(crate) fn rename(&self, id: ProjectId, name: &str) -> Result<Project, ProjectError> {
        let name = validate_name(name)?;
        self.update(|document| {
            let record = document
                .projects
                .get_mut(&id)
                .ok_or(ProjectError::Unknown(id))?;
            if record.name != name {
                record.name = name;
                record.updated_unix_ms = now_unix_ms()?;
            }
            Ok(Project::from(&*record))
        })
    }

    pub(crate) fn archive(&self, id: ProjectId) -> Result<Project, ProjectError> {
        self.set_lifecycle(id, ProjectLifecycle::Archived)
    }

    pub(crate) fn unarchive(&self, id: ProjectId) -> Result<Project, ProjectError> {
        self.set_lifecycle(id, ProjectLifecycle::Active)
    }

    pub(crate) fn relink(&self, id: ProjectId, workspace: &Path) -> Result<Project, ProjectError> {
        let canonical = canonical_workspace(workspace)?;
        self.update(|document| {
            ensure_workspace_available(document, &canonical, Some(id))?;
            let record = document
                .projects
                .get_mut(&id)
                .ok_or(ProjectError::Unknown(id))?;
            if record.canonical_workspace != canonical {
                record.canonical_workspace = canonical;
                record.updated_unix_ms = now_unix_ms()?;
            }
            Ok(Project::from(&*record))
        })
    }

    pub(crate) fn forget(&self, id: ProjectId) -> Result<bool, ProjectError> {
        self.update(|document| {
            let removed = document.projects.remove(&id).is_some();
            if removed {
                document
                    .conversation_memberships
                    .retain(|_, project| *project != id);
            }
            Ok(removed)
        })
    }

    pub(crate) fn membership(&self, conversation: &str) -> Result<Option<ProjectId>, ProjectError> {
        let conversation = validate_conversation_key(conversation)?;
        Ok(self
            .read()?
            .conversation_memberships
            .get(conversation)
            .copied())
    }

    pub(crate) fn ungroup_conversation(&self, conversation: &str) -> Result<(), ProjectError> {
        let conversation = validate_conversation_key(conversation)?.to_owned();
        self.update(|document| {
            document.conversation_memberships.remove(&conversation);
            Ok(())
        })
    }

    pub(crate) fn place_conversation(
        &self,
        conversation: &str,
        workspace: &Path,
        project: Option<ProjectId>,
    ) -> Result<(), ProjectError> {
        let conversation = validate_conversation_key(conversation)?.to_owned();
        let canonical = canonical_workspace(workspace)?;
        self.update(|document| {
            match project {
                Some(project) => {
                    let record = document
                        .projects
                        .get(&project)
                        .ok_or(ProjectError::Unknown(project))?;
                    if record.lifecycle != ProjectLifecycle::Active {
                        return Err(ProjectError::Archived(project));
                    }
                    if record.canonical_workspace != canonical {
                        return Err(ProjectError::WorkspaceMismatch {
                            project,
                            expected: record.canonical_workspace.clone(),
                            actual: canonical,
                        });
                    }
                    if document.conversation_memberships.len() >= MAX_MEMBERSHIPS
                        && !document
                            .conversation_memberships
                            .contains_key(&conversation)
                    {
                        return Err(ProjectError::Limit(
                            "conversation membership registry",
                            MAX_MEMBERSHIPS,
                        ));
                    }
                    document
                        .conversation_memberships
                        .insert(conversation, project);
                }
                None => {
                    document.conversation_memberships.remove(&conversation);
                }
            }
            Ok(())
        })
    }

    pub(crate) fn plan_continuation(
        &self,
        conversation: &str,
        source_workspace: &Path,
        project: ProjectId,
        owner: ContinuationOwner,
    ) -> Result<ProjectContinuation, ProjectError> {
        let conversation = validate_conversation_key(conversation)?.to_owned();
        let source = canonical_workspace(source_workspace)?;
        let target = self.get(project)?;
        if target.lifecycle != ProjectLifecycle::Active {
            return Err(ProjectError::Archived(project));
        }
        let (placement, handoff) = if source == target.canonical_workspace {
            (
                ContinuationPlacement::ReassignExisting,
                "same-workspace continuation preserves the conversation and complete history"
                    .to_owned(),
            )
        } else {
            (
                ContinuationPlacement::StartFresh {
                    new_conversation: SessionId::new(),
                    owner,
                },
                "cross-workspace continuation starts a new conversation; the source remains unchanged and no transcript text is copied automatically".to_owned(),
            )
        };
        Ok(ProjectContinuation {
            source_conversation: conversation,
            project,
            placement,
            handoff,
        })
    }

    fn set_lifecycle(
        &self,
        id: ProjectId,
        lifecycle: ProjectLifecycle,
    ) -> Result<Project, ProjectError> {
        self.update(|document| {
            let record = document
                .projects
                .get_mut(&id)
                .ok_or(ProjectError::Unknown(id))?;
            if record.lifecycle != lifecycle {
                record.lifecycle = lifecycle;
                record.updated_unix_ms = now_unix_ms()?;
            }
            Ok(Project::from(&*record))
        })
    }

    fn read(&self) -> Result<ProjectRegistryDocument, ProjectError> {
        read_document(&self.projects_file).map_err(ProjectError::State)
    }

    fn update<T>(
        &self,
        update: impl FnOnce(&mut ProjectRegistryDocument) -> Result<T, ProjectError>,
    ) -> Result<T, ProjectError> {
        match update_document(&self.projects_file, update) {
            Ok(output) => Ok(output),
            Err(UpdateDocumentError::State(error)) => Err(ProjectError::State(error)),
            Err(UpdateDocumentError::Update(error)) => Err(error),
        }
    }
}

fn ensure_workspace_available(
    document: &ProjectRegistryDocument,
    canonical: &Path,
    except: Option<ProjectId>,
) -> Result<(), ProjectError> {
    if let Some(existing) = document.projects.values().find(|record| {
        Some(record.id) != except && record.canonical_workspace.as_path() == canonical
    }) {
        return Err(ProjectError::WorkspaceRegistered {
            existing: existing.id,
            workspace: canonical.to_owned(),
        });
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<String, ProjectError> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_PROJECT_NAME_BYTES || name.chars().any(char::is_control)
    {
        return Err(ProjectError::Invalid(format!(
            "project name must contain 1..={MAX_PROJECT_NAME_BYTES} bytes without control characters"
        )));
    }
    Ok(name.to_owned())
}

fn validate_conversation_key(value: &str) -> Result<&str, ProjectError> {
    if value.is_empty()
        || value.len() > MAX_CONVERSATION_KEY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProjectError::Invalid(format!(
            "conversation identity must contain 1..={MAX_CONVERSATION_KEY_BYTES} bytes without control characters"
        )));
    }
    Ok(value)
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, ProjectError> {
    let canonical = path.canonicalize().map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })?;
    let metadata = fs::metadata(&canonical).map_err(|source| ProjectError::Io {
        path: canonical.clone(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(ProjectError::Invalid(format!(
            "project workspace {} is not a directory",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn now_unix_ms() -> Result<u64, ProjectError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            ProjectError::Invalid(format!("system clock precedes Unix epoch: {error}"))
        })?
        .as_millis();
    u64::try_from(millis)
        .map_err(|_| ProjectError::Invalid("system clock exceeds project timestamp range".into()))
}

#[derive(Debug)]
pub(crate) enum ProjectError {
    Unknown(ProjectId),
    Archived(ProjectId),
    WorkspaceRegistered {
        existing: ProjectId,
        workspace: PathBuf,
    },
    WorkspaceMismatch {
        project: ProjectId,
        expected: PathBuf,
        actual: PathBuf,
    },
    Limit(&'static str, usize),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    State(PrivateStateError),
    Invalid(String),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(id) => write!(formatter, "unknown project {id}"),
            Self::Archived(id) => write!(formatter, "project {id} is archived; unarchive it first"),
            Self::WorkspaceRegistered {
                existing,
                workspace,
            } => write!(
                formatter,
                "{} is already registered as project {existing}; inspect or relink that project",
                workspace.display()
            ),
            Self::WorkspaceMismatch {
                project,
                expected,
                actual,
            } => write!(
                formatter,
                "conversation workspace {} does not match project {project} workspace {}; use continue-in-project for a new conversation",
                actual.display(),
                expected.display()
            ),
            Self::Limit(name, limit) => write!(formatter, "{name} exceeds its {limit}-entry limit"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::State(error) => error.fmt(formatter),
            Self::Invalid(reason) => formatter.write_str(reason),
        }
    }
}

impl Error for ProjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::State(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;

    fn store() -> (tempfile::TempDir, XanaPaths, ProjectStore) {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        let store = ProjectStore::open(&paths).unwrap();
        (directory, paths, store)
    }

    #[test]
    fn lifecycle_is_durable_idempotent_and_never_mutates_workspace() {
        let (directory, paths, store) = store();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("owned.txt"), b"unchanged").unwrap();

        let created = store.create("  Xana  ", &workspace).unwrap();
        assert_eq!(store.list(false).unwrap(), vec![created.clone()]);
        let renamed = store.rename(created.id, "Research").unwrap();
        assert_eq!(renamed.name, "Research");
        let archived = store.archive(created.id).unwrap();
        assert_eq!(store.archive(created.id).unwrap(), archived);
        let active = store.unarchive(created.id).unwrap();
        assert_eq!(active.lifecycle, ProjectLifecycle::Active);

        let reopened = ProjectStore::open(&paths).unwrap();
        assert_eq!(reopened.get(created.id).unwrap(), active);
        assert_eq!(fs::read(workspace.join("owned.txt")).unwrap(), b"unchanged");
        assert!(reopened.forget(created.id).unwrap());
        assert!(!reopened.forget(created.id).unwrap());
        assert!(matches!(
            reopened.get(created.id),
            Err(ProjectError::Unknown(_))
        ));
    }

    #[test]
    fn canonical_workspace_is_unique_and_relink_detects_collisions() {
        let (directory, _, store) = store();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let project = store.create("First", &first).unwrap();

        assert!(matches!(
            store.create("Duplicate", &first),
            Err(ProjectError::WorkspaceRegistered { existing, .. }) if existing == project.id
        ));
        let other = store.create("Second", &second).unwrap();
        assert!(matches!(
            store.relink(other.id, &first),
            Err(ProjectError::WorkspaceRegistered { existing, .. }) if existing == project.id
        ));
    }

    #[test]
    fn membership_is_separate_and_forget_returns_conversations_to_ungrouped() {
        let (directory, _, store) = store();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let project = store.create("Project", &workspace).unwrap();
        let conversation = SessionId::new().to_string();

        store
            .place_conversation(&conversation, &workspace, Some(project.id))
            .unwrap();
        assert_eq!(store.membership(&conversation).unwrap(), Some(project.id));
        store.archive(project.id).unwrap();
        assert!(matches!(
            store.place_conversation(&conversation, &workspace, Some(project.id)),
            Err(ProjectError::Archived(id)) if id == project.id
        ));
        store.forget(project.id).unwrap();
        assert_eq!(store.membership(&conversation).unwrap(), None);
    }

    #[test]
    fn continuation_preserves_same_workspace_and_starts_fresh_across_workspaces() {
        let (directory, _, store) = store();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        let source_project = store.create("Source", &source).unwrap();
        let target_project = store.create("Target", &target).unwrap();
        let conversation = SessionId::new().to_string();

        let same = store
            .plan_continuation(
                &conversation,
                &source,
                source_project.id,
                ContinuationOwner::Native,
            )
            .unwrap();
        assert_eq!(same.placement, ContinuationPlacement::ReassignExisting);

        let cross = store
            .plan_continuation(
                &conversation,
                &source,
                target_project.id,
                ContinuationOwner::ManagedCodex,
            )
            .unwrap();
        assert!(matches!(
            cross.placement,
            ContinuationPlacement::StartFresh {
                owner: ContinuationOwner::ManagedCodex,
                ..
            }
        ));
        assert!(cross.handoff.contains("no transcript text"));
        assert_eq!(store.membership(&conversation).unwrap(), None);
    }

    #[test]
    fn missing_workspace_is_diagnosed_without_rewriting_registry() {
        let (directory, paths, store) = store();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let project = store.create("Project", &workspace).unwrap();
        let before = fs::read(paths.projects_file()).unwrap();
        fs::remove_dir(&workspace).unwrap();

        assert_eq!(
            store.inspect(project.id).unwrap().workspace_status,
            WorkspaceStatus::Missing
        );
        assert_eq!(fs::read(paths.projects_file()).unwrap(), before);
    }

    #[test]
    fn concurrent_creation_cannot_commit_conflicting_workspace_identity() {
        let (directory, _, store) = store();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let attempts = ["First", "Second"].map(|name| {
            let store = store.clone();
            let workspace = workspace.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store.create(name, &workspace)
            })
        });
        barrier.wait();
        let results = attempts.map(|attempt| attempt.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(store.list(true).unwrap().len(), 1);
        assert!(matches!(
            store.create("Retry", &workspace),
            Err(ProjectError::WorkspaceRegistered { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_resolve_to_one_workspace_identity() {
        use std::os::unix::fs::symlink;

        let (directory, _, store) = store();
        let workspace = directory.path().join("workspace");
        let alias = directory.path().join("alias");
        fs::create_dir(&workspace).unwrap();
        symlink(&workspace, &alias).unwrap();
        let project = store.create("Canonical", &workspace).unwrap();

        assert!(matches!(
            store.create("Alias", &alias),
            Err(ProjectError::WorkspaceRegistered { existing, .. }) if existing == project.id
        ));
    }
}
