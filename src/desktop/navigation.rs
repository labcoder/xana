//! Bounded, presentation-safe Project and Conversation navigation for Desktop.

use super::{DesktopError, DesktopErrorCode};
use crate::{
    bounded_file,
    config::ConfigReadiness,
    conversation_branch::ConversationBranchService,
    identity::ProjectId,
    managed::thread_store::ManagedThreadStore,
    message::{ContentBlock, Role},
    paths::XanaPaths,
    private_state::ProjectLifecycle,
    project::{Project, ProjectStore, WorkspaceStatus},
    project_continuation::{ProjectContinuationReceipt, ProjectContinuationService},
    session::DurableSession,
    workspace_host::{ConversationRef, ConversationState, WorkspaceHost},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const SNAPSHOT_VERSION: u16 = 1;
const PREFERENCE_VERSION: u16 = 2;
const MAX_PREFERENCE_BYTES: usize = 16 * 1024;
const MAX_RECENT_LAUNCHES: usize = 12;
const MAX_PROJECTS: usize = 10_000;
const MAX_CONVERSATIONS: usize = 100_000;
const MAX_TITLE_BYTES: usize = 160;
const TITLE_PAGE_SIZE: usize = 32;

/// The two supported Desktop sidebar presentations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSidebarMode {
    #[default]
    Full,
    Mini,
}

/// Availability of one Project's workspace without exposing filesystem authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopWorkspaceStatus {
    Available,
    Missing,
    ChangedIdentity,
}

impl DesktopWorkspaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::ChangedIdentity => "changed_identity",
        }
    }
}

/// Runtime state of one retained Conversation in the navigation projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopNavigationConversationState {
    Idle,
    Active,
    Controlled,
    Observable,
    Unavailable,
}

impl DesktopNavigationConversationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
            Self::Controlled => "controlled",
            Self::Observable => "observable",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One bounded Conversation row keyed by Xana-owned stable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConversationNode {
    pub id: String,
    pub title: String,
    pub owner: String,
    pub connection: String,
    pub workspace_id: String,
    pub workspace_label: String,
    pub state: DesktopNavigationConversationState,
    pub selected: bool,
    pub needs_attention: bool,
    pub record_count: Option<usize>,
    pub modified_unix_ms: Option<u64>,
    /// Exact bounded source point accepted by the shared branch service.
    pub branch_point: Option<String>,
}

/// One optional local Project and the Conversations assigned to its workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProjectNode {
    pub id: String,
    pub name: String,
    pub workspace_label: String,
    /// Opaque canonical-workspace identity used only for continuation review.
    pub workspace_id: Option<String>,
    pub workspace_status: DesktopWorkspaceStatus,
    pub archived: bool,
    pub conversations: Vec<DesktopConversationNode>,
}

/// Atomic navigation state consumed by graphical clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopNavigationSnapshot {
    pub version: u16,
    pub sidebar_mode: DesktopSidebarMode,
    pub projects: Vec<DesktopProjectNode>,
    pub ungrouped: Vec<DesktopConversationNode>,
    pub selected_conversation: Option<String>,
    pub project_count: usize,
    pub conversation_count: usize,
    pub truncated: bool,
}

/// Kind of one explicit choice on the Desktop cold-launch surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopLaunchChoiceKind {
    Project,
    Conversation,
    Workspace,
}

/// One bounded, presentation-safe launch choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLaunchChoice {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub kind: DesktopLaunchChoiceKind,
    pub(super) target: DesktopLaunchTarget,
}

/// Read-only state shown before a workspace runtime is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLaunchCatalog {
    pub configuration_state: String,
    pub projects: Vec<DesktopLaunchChoice>,
    pub recent: Vec<DesktopLaunchChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DesktopLaunchTarget {
    pub(super) workspace: PathBuf,
    pub(super) conversation: Option<ConversationRef>,
    pub(super) force_new: bool,
}

impl DesktopNavigationSnapshot {
    pub fn empty(sidebar_mode: DesktopSidebarMode) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            sidebar_mode,
            projects: Vec::new(),
            ungrouped: Vec::new(),
            selected_conversation: None,
            project_count: 0,
            conversation_count: 0,
            truncated: false,
        }
    }

    pub fn conversation(&self, id: &str) -> Option<&DesktopConversationNode> {
        self.ungrouped
            .iter()
            .find(|entry| entry.id == id)
            .or_else(|| {
                self.projects
                    .iter()
                    .flat_map(|project| &project.conversations)
                    .find(|entry| entry.id == id)
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopNavigationPreferences {
    version: u16,
    sidebar_mode: DesktopSidebarMode,
    #[serde(default)]
    recent_launches: Vec<DesktopRecentLaunch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopRecentLaunch {
    workspace: PathBuf,
    conversation: Option<String>,
    touched_unix_ms: u64,
}

impl Default for DesktopNavigationPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCE_VERSION,
            sidebar_mode: DesktopSidebarMode::Full,
            recent_launches: Vec::new(),
        }
    }
}

pub(super) struct DesktopNavigationStore {
    paths: XanaPaths,
    launch_workspace: PathBuf,
    preference_file: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct DesktopConversationDestination {
    pub(super) workspace: PathBuf,
    pub(super) conversation: ConversationRef,
}

struct ProjectConversationProjection {
    workspace_status: DesktopWorkspaceStatus,
    workspace_id: Option<String>,
    conversations: Vec<(DesktopConversationNode, bool)>,
}

impl DesktopNavigationStore {
    pub(super) fn launch_catalog(paths: &XanaPaths) -> Result<DesktopLaunchCatalog, DesktopError> {
        let preference_file = paths
            .data_dir()
            .join("frontend")
            .join("desktop-navigation.toml");
        let preferences = load_preferences(&preference_file);
        let projects = ProjectStore::list_existing(paths, false).map_err(navigation_error)?;
        let mut project_choices = Vec::with_capacity(projects.len().min(MAX_PROJECTS));
        for project in projects.into_iter().take(MAX_PROJECTS) {
            let Ok(workspace) = project.canonical_workspace.canonicalize() else {
                continue;
            };
            project_choices.push(DesktopLaunchChoice {
                id: format!("project:{}", project.id),
                label: bounded(project.name),
                detail: workspace.display().to_string(),
                kind: DesktopLaunchChoiceKind::Project,
                target: DesktopLaunchTarget {
                    workspace,
                    conversation: None,
                    force_new: false,
                },
            });
        }

        let mut recent = Vec::new();
        for (index, entry) in preferences
            .recent_launches
            .into_iter()
            .take(MAX_RECENT_LAUNCHES)
            .enumerate()
        {
            let Ok(workspace) = entry.workspace.canonicalize() else {
                continue;
            };
            if workspace != entry.workspace {
                continue;
            }
            let conversation = match entry.conversation.as_deref() {
                Some(id) => match resolve_read_only_conversation(paths, &workspace, id)? {
                    Some(conversation) => Some(conversation),
                    None => continue,
                },
                None => None,
            };
            let (kind, label) = conversation.as_ref().map_or_else(
                || {
                    (
                        DesktopLaunchChoiceKind::Workspace,
                        format!("{} workspace", workspace_label(&workspace)),
                    )
                },
                |conversation| {
                    (
                        DesktopLaunchChoiceKind::Conversation,
                        read_only_conversation_title(paths, conversation),
                    )
                },
            );
            recent.push(DesktopLaunchChoice {
                id: format!("recent:{index}"),
                label,
                detail: workspace.display().to_string(),
                kind,
                target: DesktopLaunchTarget {
                    workspace,
                    conversation,
                    force_new: false,
                },
            });
        }

        Ok(DesktopLaunchCatalog {
            configuration_state: ConfigReadiness::inspect(paths.config_file())
                .as_str()
                .to_owned(),
            projects: project_choices,
            recent,
        })
    }

    pub(super) fn open(paths: &XanaPaths, launch_workspace: &Path) -> Result<Self, DesktopError> {
        let launch_workspace = launch_workspace.canonicalize().map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::WorkspaceUnavailable,
                format!(
                    "could not resolve Desktop launch workspace {}: {error}",
                    launch_workspace.display()
                ),
            )
        })?;
        Ok(Self {
            paths: paths.clone(),
            launch_workspace,
            preference_file: paths
                .data_dir()
                .join("frontend")
                .join("desktop-navigation.toml"),
        })
    }

    pub(super) fn snapshot(
        &self,
        selected: Option<&str>,
    ) -> Result<DesktopNavigationSnapshot, DesktopError> {
        let sidebar_mode = self.load_preferences().sidebar_mode;
        let project_store = ProjectStore::open(&self.paths).map_err(navigation_error)?;
        let projects = project_store.list(true).map_err(navigation_error)?;
        let project_count = projects.len();
        let mut output_projects = Vec::with_capacity(project_count.min(MAX_PROJECTS));
        let mut ungrouped = Vec::new();
        let mut seen_workspaces = HashSet::new();
        let mut conversation_count = 0usize;
        let mut truncated = project_count > MAX_PROJECTS;

        for project in projects.into_iter().take(MAX_PROJECTS) {
            let canonical = project.canonical_workspace.clone();
            seen_workspaces.insert(canonical.clone());
            let projected = self.project_conversations(&project_store, &project, selected)?;
            let ProjectConversationProjection {
                workspace_status,
                workspace_id,
                conversations,
            } = projected;
            conversation_count = conversation_count.saturating_add(conversations.len());
            let (assigned, loose): (Vec<_>, Vec<_>) = conversations
                .into_iter()
                .partition(|(_, assigned)| *assigned);
            ungrouped.extend(loose.into_iter().map(|(conversation, _)| conversation));
            output_projects.push(DesktopProjectNode {
                id: project.id.to_string(),
                name: bounded(project.name),
                workspace_label: workspace_label(&canonical),
                workspace_id,
                workspace_status,
                archived: project.lifecycle == ProjectLifecycle::Archived,
                conversations: assigned
                    .into_iter()
                    .map(|(conversation, _)| conversation)
                    .collect(),
            });
            if conversation_count >= MAX_CONVERSATIONS {
                truncated = true;
                break;
            }
        }

        if conversation_count < MAX_CONVERSATIONS
            && !seen_workspaces.contains(&self.launch_workspace)
        {
            let host = WorkspaceHost::open(self.paths.data_dir(), &self.launch_workspace)
                .map_err(navigation_error)?;
            let snapshot = host.snapshot().map_err(navigation_error)?;
            let mut projected =
                project_workspace_conversations(&self.paths, &host, snapshot, selected)?;
            conversation_count = conversation_count.saturating_add(projected.len());
            ungrouped.extend(projected.drain(..).map(|(conversation, _)| conversation));
        }

        if conversation_count > MAX_CONVERSATIONS {
            truncated = true;
        }
        trim_conversations(&mut output_projects, &mut ungrouped, MAX_CONVERSATIONS);

        Ok(DesktopNavigationSnapshot {
            version: SNAPSHOT_VERSION,
            sidebar_mode,
            projects: output_projects,
            ungrouped,
            selected_conversation: selected.map(ToOwned::to_owned),
            project_count,
            conversation_count: conversation_count.min(MAX_CONVERSATIONS),
            truncated,
        })
    }

    pub(super) fn set_sidebar_mode(&self, mode: DesktopSidebarMode) -> Result<(), DesktopError> {
        let preferences = DesktopNavigationPreferences {
            sidebar_mode: mode,
            ..self.load_preferences()
        };
        self.save_preferences(&preferences)
    }

    pub(super) fn record_recent(&self, selected: Option<&str>) -> Result<(), DesktopError> {
        let mut preferences = self.load_preferences();
        preferences.recent_launches.retain(|entry| {
            entry.workspace != self.launch_workspace || entry.conversation.as_deref() != selected
        });
        preferences.recent_launches.insert(
            0,
            DesktopRecentLaunch {
                workspace: self.launch_workspace.clone(),
                conversation: selected.map(ToOwned::to_owned),
                touched_unix_ms: now_unix_ms(),
            },
        );
        preferences.recent_launches.truncate(MAX_RECENT_LAUNCHES);
        self.save_preferences(&preferences)
    }

    fn save_preferences(
        &self,
        preferences: &DesktopNavigationPreferences,
    ) -> Result<(), DesktopError> {
        if let Some(parent) = self.preference_file.parent() {
            std::fs::create_dir_all(parent).map_err(preference_error)?;
        }
        let rendered = toml::to_string_pretty(&preferences).map_err(preference_error)?;
        let mut file = atomic_write_file::AtomicWriteFile::open(&self.preference_file)
            .map_err(preference_error)?;
        file.write_all(rendered.as_bytes())
            .and_then(|()| file.commit())
            .map_err(preference_error)
    }

    pub(super) fn resolve_conversation(
        &self,
        id: &str,
    ) -> Result<Option<DesktopConversationDestination>, DesktopError> {
        for workspace in self.available_workspaces()? {
            let host =
                WorkspaceHost::open(self.paths.data_dir(), &workspace).map_err(navigation_error)?;
            let snapshot = host.snapshot().map_err(navigation_error)?;
            if let Some(conversation) = snapshot
                .conversations
                .into_iter()
                .map(|entry| entry.conversation)
                .find(|conversation| conversation.to_string() == id)
            {
                return Ok(Some(DesktopConversationDestination {
                    workspace,
                    conversation,
                }));
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_new_workspace(
        &self,
        project_id: Option<&str>,
    ) -> Result<PathBuf, DesktopError> {
        let Some(project_id) = project_id else {
            return Ok(self.launch_workspace.clone());
        };
        let projects = ProjectStore::open(&self.paths)
            .and_then(|store| store.list(true))
            .map_err(navigation_error)?;
        let project = projects
            .into_iter()
            .find(|project| project.id.to_string() == project_id)
            .ok_or_else(|| {
                DesktopError::new(
                    DesktopErrorCode::StateInvalid,
                    format!("Desktop Project {project_id} is no longer available"),
                )
            })?;
        project.canonical_workspace.canonicalize().map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::WorkspaceUnavailable,
                format!(
                    "Desktop Project {} workspace is unavailable: {error}",
                    project.name
                ),
            )
        })
    }

    pub(super) fn rename_project(&self, id: &str, name: &str) -> Result<(), DesktopError> {
        let id = parse_project_id(id)?;
        ProjectStore::open(&self.paths)
            .and_then(|store| store.rename(id, name))
            .map(|_| ())
            .map_err(navigation_error)
    }

    pub(super) fn set_project_archived(
        &self,
        id: &str,
        archived: bool,
    ) -> Result<(), DesktopError> {
        let id = parse_project_id(id)?;
        ProjectStore::open(&self.paths)
            .and_then(|store| {
                if archived {
                    store.archive(id)
                } else {
                    store.unarchive(id)
                }
            })
            .map(|_| ())
            .map_err(navigation_error)
    }

    pub(super) fn ungroup_conversation(&self, id: &str) -> Result<(), DesktopError> {
        let destination = self.require_conversation(id)?;
        let store = ProjectStore::open(&self.paths).map_err(navigation_error)?;
        for key in conversation_membership_keys(&destination.conversation) {
            store.ungroup_conversation(&key).map_err(navigation_error)?;
        }
        Ok(())
    }

    pub(super) fn archive_managed_conversation(&self, id: &str) -> Result<bool, DesktopError> {
        let destination = self.require_conversation(id)?;
        WorkspaceHost::open(self.paths.data_dir(), &destination.workspace)
            .and_then(|host| host.archive_managed_conversation(&destination.conversation))
            .map_err(navigation_error)
    }

    pub(super) fn move_conversation(
        &self,
        id: &str,
        project_id: &str,
        allow_fresh_continuation: bool,
    ) -> Result<Option<DesktopConversationDestination>, DesktopError> {
        let destination = self.require_conversation(id)?;
        let project_id = parse_project_id(project_id)?;
        let service = ProjectContinuationService::open(&self.paths).map_err(navigation_error)?;
        let plan = service
            .plan(
                &destination.conversation,
                &destination.workspace,
                project_id,
                None,
            )
            .map_err(navigation_error)?;
        let starts_fresh = matches!(
            plan.continuation.placement,
            crate::project::ContinuationPlacement::StartFresh { .. }
        );
        if starts_fresh && !allow_fresh_continuation {
            return Err(DesktopError::new(
                DesktopErrorCode::CommandRejected,
                format!(
                    "moving this Conversation to Project {project_id} crosses workspaces; confirm a source-preserving fresh continuation"
                ),
            ));
        }
        let receipt = service
            .commit(&destination.conversation, plan)
            .map_err(navigation_error)?;
        match receipt {
            ProjectContinuationReceipt::Reassigned { .. } => Ok(None),
            ProjectContinuationReceipt::StartedFresh {
                workspace,
                conversation,
            } => Ok(Some(DesktopConversationDestination {
                workspace,
                conversation,
            })),
        }
    }

    pub(super) fn branch_conversation(
        &self,
        id: &str,
        source_point: &str,
    ) -> Result<DesktopConversationDestination, DesktopError> {
        let destination = self.require_conversation(id)?;
        let source = destination.conversation.conversation_id().ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                "pending Conversation cannot be branched",
            )
        })?;
        let receipt = ConversationBranchService::open(&self.paths, &destination.workspace)
            .and_then(|service| service.branch(source, source_point))
            .map_err(navigation_error)?;
        Ok(DesktopConversationDestination {
            workspace: destination.workspace,
            conversation: receipt.target_ref,
        })
    }

    fn require_conversation(
        &self,
        id: &str,
    ) -> Result<DesktopConversationDestination, DesktopError> {
        self.resolve_conversation(id)?.ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("Conversation {id} is no longer available"),
            )
        })
    }

    fn load_preferences(&self) -> DesktopNavigationPreferences {
        load_preferences(&self.preference_file)
    }

    fn project_conversations(
        &self,
        store: &ProjectStore,
        project: &Project,
        selected: Option<&str>,
    ) -> Result<ProjectConversationProjection, DesktopError> {
        let inspection = store.inspect(project.id).map_err(navigation_error)?;
        let status = match inspection.workspace_status {
            WorkspaceStatus::Available => DesktopWorkspaceStatus::Available,
            WorkspaceStatus::Missing => DesktopWorkspaceStatus::Missing,
            WorkspaceStatus::ChangedIdentity => DesktopWorkspaceStatus::ChangedIdentity,
        };
        if status != DesktopWorkspaceStatus::Available {
            return Ok(ProjectConversationProjection {
                workspace_status: status,
                workspace_id: None,
                conversations: Vec::new(),
            });
        }
        let host = WorkspaceHost::open(self.paths.data_dir(), &project.canonical_workspace)
            .map_err(navigation_error)?;
        let snapshot = host.snapshot().map_err(navigation_error)?;
        Ok(ProjectConversationProjection {
            workspace_status: status,
            workspace_id: Some(snapshot.workspace_id.clone()),
            conversations: project_workspace_conversations(&self.paths, &host, snapshot, selected)?,
        })
    }

    fn available_workspaces(&self) -> Result<Vec<PathBuf>, DesktopError> {
        let projects = ProjectStore::open(&self.paths)
            .and_then(|store| store.list(true))
            .map_err(navigation_error)?;
        let mut seen = HashSet::new();
        let mut workspaces = Vec::new();
        for project in projects {
            let Ok(workspace) = project.canonical_workspace.canonicalize() else {
                continue;
            };
            if seen.insert(workspace.clone()) {
                workspaces.push(workspace);
            }
        }
        if seen.insert(self.launch_workspace.clone()) {
            workspaces.push(self.launch_workspace.clone());
        }
        Ok(workspaces)
    }
}

fn project_workspace_conversations(
    paths: &XanaPaths,
    host: &WorkspaceHost,
    snapshot: crate::workspace_host::WorkspaceSnapshot,
    selected: Option<&str>,
) -> Result<Vec<(DesktopConversationNode, bool)>, DesktopError> {
    let workspace_id = snapshot.workspace_id.clone();
    let workspace_label = workspace_label(&snapshot.workspace);
    snapshot
        .conversations
        .into_iter()
        .map(|projection| {
            let id = projection.conversation.to_string();
            let assigned = projection.project.is_some();
            let title = conversation_title(host, &projection.conversation);
            let (owner, connection) = conversation_owner(&projection.conversation);
            let state = match projection.state {
                ConversationState::Inactive => DesktopNavigationConversationState::Idle,
                ConversationState::Active => DesktopNavigationConversationState::Active,
                ConversationState::Controlled => DesktopNavigationConversationState::Controlled,
                ConversationState::Observable => DesktopNavigationConversationState::Observable,
                ConversationState::Unavailable => DesktopNavigationConversationState::Unavailable,
            };
            let modified_unix_ms = projection
                .modified
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .and_then(|value| u64::try_from(value.as_millis()).ok());
            let branch_point = conversation_branch_point(paths, &projection.conversation);
            Ok((
                DesktopConversationNode {
                    id: id.clone(),
                    title,
                    owner: owner.to_owned(),
                    connection,
                    workspace_id: workspace_id.clone(),
                    workspace_label: workspace_label.clone(),
                    state,
                    selected: selected == Some(id.as_str()),
                    needs_attention: matches!(
                        state,
                        DesktopNavigationConversationState::Unavailable
                    ),
                    record_count: projection.record_count,
                    modified_unix_ms,
                    branch_point,
                },
                assigned,
            ))
        })
        .collect()
}

fn load_preferences(path: &Path) -> DesktopNavigationPreferences {
    bounded_file::read(path, MAX_PREFERENCE_BYTES)
        .ok()
        .and_then(|bytes| toml::from_slice::<DesktopNavigationPreferences>(&bytes).ok())
        .filter(|preferences| matches!(preferences.version, 1 | PREFERENCE_VERSION))
        .map(|mut preferences| {
            preferences.version = PREFERENCE_VERSION;
            preferences.recent_launches.truncate(MAX_RECENT_LAUNCHES);
            preferences
        })
        .unwrap_or_default()
}

fn resolve_read_only_conversation(
    paths: &XanaPaths,
    workspace: &Path,
    id: &str,
) -> Result<Option<ConversationRef>, DesktopError> {
    let native = DurableSession::list_for_workspace(paths.data_dir(), workspace)
        .map_err(navigation_error)?
        .into_iter()
        .map(|entry| ConversationRef::Native {
            session_id: entry.session_id,
        })
        .find(|conversation| conversation.to_string() == id);
    if native.is_some() {
        return Ok(native);
    }
    Ok(
        ManagedThreadStore::list_for_workspace(paths.data_dir(), workspace)
            .map_err(navigation_error)?
            .into_iter()
            .map(|entry| ConversationRef::Managed {
                conversation_id: entry.conversation_id,
                connection: entry.connection,
                thread_id: entry.thread_id,
            })
            .find(|conversation| conversation.to_string() == id),
    )
}

fn read_only_conversation_title(paths: &XanaPaths, conversation: &ConversationRef) -> String {
    let fallback = match conversation {
        ConversationRef::Native { session_id } => {
            format!("Native {}", short(&session_id.to_string()))
        }
        ConversationRef::Managed {
            connection,
            thread_id,
            ..
        } => format!("{connection} {}", short(thread_id)),
        ConversationRef::NewNative => "New native Conversation".to_owned(),
        ConversationRef::NewManaged { connection, .. } => {
            format!("New {connection} Conversation")
        }
    };
    let ConversationRef::Native { session_id } = conversation else {
        return bounded(fallback);
    };
    DurableSession::conversation_page(paths.data_dir(), *session_id, None, TITLE_PAGE_SIZE)
        .ok()
        .and_then(|page| {
            page.messages.into_iter().find_map(|message| {
                (message.role == Role::User)
                    .then(|| {
                        message
                            .content
                            .into_iter()
                            .find_map(|content| match content {
                                ContentBlock::Text(text) => Some(text),
                                _ => None,
                            })
                    })
                    .flatten()
            })
        })
        .map(|title| bounded(title.split_whitespace().collect::<Vec<_>>().join(" ")))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| bounded(fallback))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn conversation_branch_point(paths: &XanaPaths, conversation: &ConversationRef) -> Option<String> {
    match conversation {
        ConversationRef::Native { session_id } => {
            DurableSession::inspect(paths.data_dir(), *session_id)
                .ok()?
                .recent_active_entry_ids
                .last()
                .map(ToString::to_string)
        }
        ConversationRef::Managed { thread_id, .. } => Some(thread_id.clone()),
        ConversationRef::NewNative | ConversationRef::NewManaged { .. } => None,
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

fn parse_project_id(value: &str) -> Result<ProjectId, DesktopError> {
    value.parse().map_err(|_| {
        DesktopError::new(
            DesktopErrorCode::StateInvalid,
            format!("Project identity {value:?} is invalid"),
        )
    })
}

fn conversation_title(host: &WorkspaceHost, conversation: &ConversationRef) -> String {
    let fallback = match conversation {
        ConversationRef::Native { session_id } => {
            format!("Native {}", short(&session_id.to_string()))
        }
        ConversationRef::Managed {
            connection,
            thread_id,
            ..
        } => format!("{connection} {}", short(thread_id)),
        ConversationRef::NewNative => "New native Conversation".to_owned(),
        ConversationRef::NewManaged { connection, .. } => {
            format!("New {connection} Conversation")
        }
    };
    host.conversation_history_page(conversation, None, TITLE_PAGE_SIZE)
        .ok()
        .flatten()
        .and_then(|page| {
            page.messages.into_iter().find_map(|message| {
                (message.role == Role::User)
                    .then(|| {
                        message
                            .content
                            .into_iter()
                            .find_map(|content| match content {
                                ContentBlock::Text(text) => Some(text),
                                _ => None,
                            })
                    })
                    .flatten()
            })
        })
        .map(|title| bounded(title.split_whitespace().collect::<Vec<_>>().join(" ")))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| bounded(fallback))
}

fn conversation_owner(conversation: &ConversationRef) -> (&'static str, String) {
    match conversation {
        ConversationRef::Native { .. } | ConversationRef::NewNative => {
            ("native", "native".to_owned())
        }
        ConversationRef::Managed { connection, .. }
        | ConversationRef::NewManaged { connection, .. } => {
            ("managed", bounded(connection.clone()))
        }
    }
}

fn sort_conversations(conversations: &mut [DesktopConversationNode]) {
    conversations.sort_by(|left, right| {
        right
            .modified_unix_ms
            .cmp(&left.modified_unix_ms)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn trim_conversations(
    projects: &mut [DesktopProjectNode],
    ungrouped: &mut Vec<DesktopConversationNode>,
    mut remaining: usize,
) {
    for project in projects {
        sort_conversations(&mut project.conversations);
        let keep = project.conversations.len().min(remaining);
        project.conversations.truncate(keep);
        remaining = remaining.saturating_sub(keep);
    }
    sort_conversations(ungrouped);
    ungrouped.truncate(remaining);
}

fn workspace_label(workspace: &Path) -> String {
    workspace
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| workspace.display().to_string())
}

fn short(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn bounded(mut value: String) -> String {
    if value.len() <= MAX_TITLE_BYTES {
        return value;
    }
    let mut end = MAX_TITLE_BYTES.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value.push_str("...");
    value
}

fn navigation_error(error: impl std::fmt::Display) -> DesktopError {
    DesktopError::new(
        DesktopErrorCode::StateInvalid,
        format!("could not build Desktop navigation: {error}"),
    )
}

fn preference_error(error: impl std::fmt::Display) -> DesktopError {
    DesktopError::new(
        DesktopErrorCode::StateInvalid,
        format!("could not persist Desktop navigation preference: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
        identity::{ConversationId, SessionId},
        message::Message,
        profile::ProfileStore,
        session::DurableSession,
        shell::ShellConfig,
    };
    use std::{ffi::OsString, fs};

    fn fixture() -> (tempfile::TempDir, XanaPaths, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(
            paths.config_file(),
            XanaConfig::render_initial(InitialConfig {
                connection: InitialConnection::Ollama {
                    name: "local".into(),
                    base_url: "http://localhost:11434/v1".into(),
                },
                model: "qwen".into(),
                max_tool_rounds: 8,
                shell: ShellConfig::default(),
                permission_mode: PermissionMode::Ask,
                reasoning_effort: None,
            })
            .unwrap(),
        )
        .unwrap();
        crate::private_state::ensure_interoperable_records(&paths).unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        (directory, paths, workspace.canonicalize().unwrap())
    }

    #[test]
    fn projects_and_ungrouped_conversations_keep_stable_identity() {
        let (_directory, paths, workspace) = fixture();
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Xana", &workspace)
            .unwrap();
        let grouped = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let grouped_id = grouped.session_id();
        drop(grouped);
        let loose = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let loose_id = loose.session_id();
        drop(loose);
        ProjectStore::open(&paths)
            .unwrap()
            .place_conversation(&grouped_id.to_string(), &workspace, Some(project.id))
            .unwrap();

        let snapshot = DesktopNavigationStore::open(&paths, &workspace)
            .unwrap()
            .snapshot(Some(
                &ConversationRef::Native {
                    session_id: grouped_id,
                }
                .to_string(),
            ))
            .unwrap();

        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(snapshot.projects[0].id, project.id.to_string());
        assert_eq!(snapshot.projects[0].conversations.len(), 1);
        assert!(snapshot.projects[0].conversations[0].selected);
        assert_eq!(snapshot.ungrouped.len(), 1);
        assert!(snapshot.ungrouped[0].id.contains(&loose_id.to_string()));
    }

    #[test]
    fn sidebar_preference_round_trips_without_runtime_or_conversation_state() {
        let (_directory, paths, workspace) = fixture();
        let store = DesktopNavigationStore::open(&paths, &workspace).unwrap();
        assert_eq!(
            store.snapshot(None).unwrap().sidebar_mode,
            DesktopSidebarMode::Full
        );
        store.set_sidebar_mode(DesktopSidebarMode::Mini).unwrap();
        assert_eq!(
            DesktopNavigationStore::open(&paths, &workspace)
                .unwrap()
                .snapshot(None)
                .unwrap()
                .sidebar_mode,
            DesktopSidebarMode::Mini
        );
        let stored = std::fs::read_to_string(store.preference_file).unwrap();
        assert!(!stored.contains(&SessionId::new().to_string()));
        assert!(!stored.contains("conversation"));
    }

    #[test]
    fn cold_catalog_is_read_only_when_xana_has_no_state() {
        let directory = tempfile::tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();

        let catalog = DesktopNavigationStore::launch_catalog(&paths).unwrap();

        assert_eq!(catalog.configuration_state, "missing");
        assert!(catalog.projects.is_empty());
        assert!(catalog.recent.is_empty());
        assert!(!paths.data_dir().exists());
    }

    #[test]
    fn cold_catalog_offers_active_projects_and_exact_recent_conversations() {
        let (_directory, paths, workspace) = fixture();
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Xana", &workspace)
            .unwrap();
        let mut session = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        session
            .append_message(Message::text(Role::User, "A durable recent task"))
            .unwrap();
        let conversation = ConversationRef::Native {
            session_id: session.session_id(),
        };
        drop(session);
        let store = DesktopNavigationStore::open(&paths, &workspace).unwrap();
        store
            .record_recent(Some(&conversation.to_string()))
            .unwrap();

        let catalog = DesktopNavigationStore::launch_catalog(&paths).unwrap();

        assert_eq!(catalog.configuration_state, "healthy");
        assert_eq!(catalog.projects.len(), 1);
        assert_eq!(catalog.projects[0].id, format!("project:{}", project.id));
        assert_eq!(catalog.projects[0].kind, DesktopLaunchChoiceKind::Project);
        assert_eq!(catalog.recent.len(), 1);
        assert_eq!(catalog.recent[0].label, "A durable recent task");
        assert_eq!(
            catalog.recent[0].kind,
            DesktopLaunchChoiceKind::Conversation
        );
        assert_eq!(catalog.recent[0].target.conversation, Some(conversation));
    }

    #[test]
    fn legacy_sidebar_preference_is_migrated_without_losing_its_mode() {
        let (_directory, paths, workspace) = fixture();
        let preference_file = paths
            .data_dir()
            .join("frontend")
            .join("desktop-navigation.toml");
        fs::create_dir_all(preference_file.parent().unwrap()).unwrap();
        fs::write(&preference_file, "version = 1\nsidebar_mode = \"mini\"\n").unwrap();

        let store = DesktopNavigationStore::open(&paths, &workspace).unwrap();
        assert_eq!(
            store.snapshot(None).unwrap().sidebar_mode,
            DesktopSidebarMode::Mini
        );
        store.record_recent(None).unwrap();
        let persisted = fs::read_to_string(preference_file).unwrap();
        assert!(persisted.contains("version = 2"));
        assert!(persisted.contains("sidebar_mode = \"mini\""));
    }

    #[test]
    fn opaque_conversation_selection_resolves_to_workspace_owned_identity() {
        let (_directory, paths, workspace) = fixture();
        let session = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let expected = ConversationRef::Native {
            session_id: session.session_id(),
        };
        drop(session);

        let destination = DesktopNavigationStore::open(&paths, &workspace)
            .unwrap()
            .resolve_conversation(&expected.to_string())
            .unwrap()
            .unwrap();

        assert_eq!(destination.workspace, workspace);
        assert_eq!(destination.conversation, expected);
    }

    #[test]
    fn managed_archive_removes_only_the_inactive_local_handle() {
        let (_directory, paths, workspace) = fixture();
        let conversation_id = ConversationId::new();
        let mut managed = ManagedThreadStore::open(paths.data_dir(), "codex", &workspace).unwrap();
        managed
            .set_thread(
                Some(conversation_id),
                Some("thread-for-desktop".to_owned()),
                Some("identity-v1"),
            )
            .unwrap();
        drop(managed);
        let conversation = ConversationRef::Managed {
            conversation_id,
            connection: "codex".to_owned(),
            thread_id: "thread-for-desktop".to_owned(),
        };
        let store = DesktopNavigationStore::open(&paths, &workspace).unwrap();

        assert!(
            store
                .archive_managed_conversation(&conversation.to_string())
                .unwrap()
        );
        assert!(
            store
                .resolve_conversation(&conversation.to_string())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn missing_project_workspace_remains_visible_and_actionable() {
        let (directory, paths, workspace) = fixture();
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Movable", &workspace)
            .unwrap();
        std::fs::remove_dir(&workspace).unwrap();
        let fallback = directory.path().join("fallback");
        std::fs::create_dir(&fallback).unwrap();

        let snapshot = DesktopNavigationStore::open(&paths, &fallback)
            .unwrap()
            .snapshot(None)
            .unwrap();
        assert_eq!(snapshot.projects[0].id, project.id.to_string());
        assert_eq!(
            snapshot.projects[0].workspace_status,
            DesktopWorkspaceStatus::Missing
        );
        assert!(snapshot.projects[0].conversations.is_empty());
    }

    #[test]
    fn project_lifecycle_and_ungroup_mutations_refresh_the_shared_store() {
        let (_directory, paths, workspace) = fixture();
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Before", &workspace)
            .unwrap();
        let session = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let conversation = ConversationRef::Native {
            session_id: session.session_id(),
        };
        drop(session);
        let store = DesktopNavigationStore::open(&paths, &workspace).unwrap();
        ProjectStore::open(&paths)
            .unwrap()
            .place_conversation(
                &conversation
                    .conversation_id()
                    .expect("stable conversation")
                    .to_string(),
                &workspace,
                Some(project.id),
            )
            .unwrap();

        store
            .rename_project(&project.id.to_string(), "After")
            .unwrap();
        store
            .set_project_archived(&project.id.to_string(), true)
            .unwrap();
        store
            .ungroup_conversation(&conversation.to_string())
            .unwrap();

        let snapshot = store.snapshot(Some(&conversation.to_string())).unwrap();
        assert_eq!(snapshot.projects[0].name, "After");
        assert!(snapshot.projects[0].archived);
        assert!(snapshot.projects[0].conversations.is_empty());
        assert_eq!(snapshot.ungrouped[0].id, conversation.to_string());
    }

    #[test]
    fn cross_workspace_move_requires_confirmation_then_preserves_the_source() {
        let (directory, paths, source_workspace) = fixture();
        let target_workspace = directory.path().join("target");
        fs::create_dir(&target_workspace).unwrap();
        let target_workspace = target_workspace.canonicalize().unwrap();
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Target", &target_workspace)
            .unwrap();
        let session = DurableSession::create(paths.data_dir(), source_workspace.clone()).unwrap();
        let source_session = session.session_id();
        let source = ConversationRef::Native {
            session_id: source_session,
        };
        drop(session);
        let store = DesktopNavigationStore::open(&paths, &source_workspace).unwrap();

        assert!(
            store
                .move_conversation(&source.to_string(), &project.id.to_string(), false,)
                .is_err()
        );
        let target = store
            .move_conversation(&source.to_string(), &project.id.to_string(), true)
            .unwrap()
            .expect("cross-workspace continuation");

        assert_eq!(target.workspace, target_workspace);
        assert_ne!(target.conversation, source);
        assert!(DurableSession::inspect(paths.data_dir(), source_session).is_ok());
    }

    #[test]
    fn projected_native_branch_point_creates_a_source_preserving_branch() {
        let (_directory, paths, workspace) = fixture();
        let mut source = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let source_session = source.session_id();
        let source_id = ConversationId::for_native(source_session);
        let profile = ProfileStore::open(&paths)
            .resolve_global("default")
            .unwrap();
        ProfileStore::open(&paths)
            .freeze(&source_id.to_string(), &profile)
            .unwrap();
        let source_point = source
            .append_message(Message::text(Role::User, "branch here"))
            .unwrap();
        drop(source);
        let source_ref = ConversationRef::Native {
            session_id: source_session,
        };
        let store = DesktopNavigationStore::open(&paths, &workspace).unwrap();
        let snapshot = store.snapshot(Some(&source_ref.to_string())).unwrap();
        assert_eq!(
            snapshot
                .conversation(&source_ref.to_string())
                .unwrap()
                .branch_point
                .as_deref(),
            Some(source_point.to_string().as_str())
        );

        let target = store
            .branch_conversation(&source_ref.to_string(), &source_point.to_string())
            .unwrap();

        assert_ne!(target.conversation, source_ref);
        assert!(DurableSession::inspect(paths.data_dir(), source_session).is_ok());
    }
}
