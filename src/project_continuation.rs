//! Shared, source-preserving Project continuation transactions.
//!
//! Presentation adapters may review a [`ProjectContinuationPlan`] and then ask
//! this service to commit that exact plan.  The service owns profile resolution,
//! target creation, provenance, and rollback so CLI and graphical clients do
//! not grow separate continuation rules.

use crate::{
    config::{ProviderKind, XanaConfig},
    identity::ProjectId,
    paths::XanaPaths,
    portable_project::PortableProjectStore,
    profile::{ProfileStore, ResolvedProfile},
    project::{ContinuationOwner, ContinuationPlacement, ProjectContinuation, ProjectStore},
    session::DurableSession,
    workspace_host::ConversationRef,
};
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};

/// A reviewed continuation plus the target profile it will freeze.
#[derive(Debug, Clone)]
pub(crate) struct ProjectContinuationPlan {
    pub(crate) continuation: ProjectContinuation,
    pub(crate) target_profile: ResolvedProfile,
    pub(crate) target_workspace: PathBuf,
}

/// Result of committing one reviewed Project continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectContinuationReceipt {
    Reassigned {
        conversation: ConversationRef,
        workspace: PathBuf,
    },
    StartedFresh {
        conversation: ConversationRef,
        workspace: PathBuf,
    },
}

/// Plans and commits one owner-correct continuation transaction.
pub(crate) struct ProjectContinuationService<'a> {
    paths: &'a XanaPaths,
    projects: ProjectStore,
}

impl<'a> ProjectContinuationService<'a> {
    pub(crate) fn open(paths: &'a XanaPaths) -> Result<Self> {
        Ok(Self {
            paths,
            projects: ProjectStore::open(paths)?,
        })
    }

    pub(crate) fn plan(
        &self,
        source: &ConversationRef,
        source_workspace: &Path,
        project: ProjectId,
        requested_profile: Option<&str>,
    ) -> Result<ProjectContinuationPlan> {
        let owner = owner_for(source)?;
        let source_id = source
            .conversation_id()
            .context("pending native Conversation has no stable source identity")?;
        if owner == ContinuationOwner::Native {
            let ConversationRef::Native { session_id } = source else {
                unreachable!("native owner is only returned for a retained native Conversation")
            };
            let (_, restored) =
                DurableSession::inspect_restored(self.paths.data_dir(), *session_id)
                    .context("could not inspect native continuation source")?;
            if restored.workspace_root.canonicalize()? != source_workspace.canonicalize()? {
                bail!(
                    "source Conversation belongs to {}; requested source workspace resolved to {}",
                    restored.workspace_root.display(),
                    source_workspace.display()
                );
            }
        }
        let continuation = self.projects.plan_continuation(
            &source_id.to_string(),
            source_workspace,
            project,
            owner,
        )?;
        let target_profile = resolve_target_profile(self.paths, project, requested_profile)?;
        validate_owner_profile(self.paths, owner, &target_profile)?;
        if !target_profile.is_ready() {
            bail!(
                "target Profile {} is not ready: {}",
                target_profile.name,
                target_profile.readiness.join("; ")
            );
        }
        let target_workspace = self.projects.get(project)?.canonical_workspace;
        Ok(ProjectContinuationPlan {
            continuation,
            target_profile,
            target_workspace,
        })
    }

    pub(crate) fn commit(
        &self,
        source: &ConversationRef,
        plan: ProjectContinuationPlan,
    ) -> Result<ProjectContinuationReceipt> {
        let ProjectContinuationPlan {
            continuation,
            target_profile,
            target_workspace,
        } = plan;
        match continuation.placement {
            ContinuationPlacement::ReassignExisting => {
                self.projects.place_conversation(
                    &continuation.source_conversation,
                    &target_workspace,
                    Some(continuation.project),
                )?;
                Ok(ProjectContinuationReceipt::Reassigned {
                    conversation: source.clone(),
                    workspace: target_workspace,
                })
            }
            ContinuationPlacement::StartFresh {
                new_conversation,
                owner,
            } => {
                let created_session = if owner == ContinuationOwner::Native {
                    Some(DurableSession::create_with_id(
                        self.paths.data_dir(),
                        target_workspace.clone(),
                        new_conversation,
                    )?)
                } else {
                    None
                };
                if let Err(error) = ProfileStore::open(self.paths).commit_project_continuation(
                    &continuation.source_conversation,
                    &new_conversation.to_string(),
                    continuation.project,
                    &target_profile,
                ) {
                    if let Some(session) = created_session {
                        session.discard_unstarted().with_context(|| {
                            format!(
                                "Project continuation failed ({error}); its empty native Conversation also could not be rolled back"
                            )
                        })?;
                    }
                    return Err(error.into());
                }
                drop(created_session);
                let conversation = match owner {
                    ContinuationOwner::Native => ConversationRef::Native {
                        session_id: new_conversation,
                    },
                    ContinuationOwner::ManagedCodex => ConversationRef::NewManaged {
                        conversation_id: crate::identity::ConversationId::for_native(
                            new_conversation,
                        ),
                        connection: target_profile.connection.value,
                    },
                };
                Ok(ProjectContinuationReceipt::StartedFresh {
                    conversation,
                    workspace: target_workspace,
                })
            }
        }
    }
}

fn owner_for(source: &ConversationRef) -> Result<ContinuationOwner> {
    match source {
        ConversationRef::Native { .. } => Ok(ContinuationOwner::Native),
        ConversationRef::Managed { .. } => Ok(ContinuationOwner::ManagedCodex),
        ConversationRef::NewManaged { .. } | ConversationRef::NewNative => {
            bail!("a pending Conversation cannot be continued into another Project")
        }
    }
}

pub(crate) fn resolve_target_profile(
    paths: &XanaPaths,
    project: ProjectId,
    requested: Option<&str>,
) -> Result<ResolvedProfile> {
    let store = ProfileStore::open(paths);
    if let Ok(portable) = PortableProjectStore::open(paths).resolve(paths, project) {
        let name = requested.or(portable.manifest.default_profile.as_deref());
        if let Some(name) = name
            && portable.manifest.profiles.contains_key(name)
        {
            return Ok(store.resolve_project(paths, project, name)?);
        }
    }
    let name = requested.map(str::to_owned).unwrap_or_else(|| {
        XanaConfig::load_registry_from(paths.config_file())
            .map(|registry| registry.default_profile)
            .unwrap_or_else(|_| "default".to_owned())
    });
    Ok(store.resolve_global(&name)?)
}

pub(crate) fn validate_owner_profile(
    paths: &XanaPaths,
    owner: ContinuationOwner,
    profile: &ResolvedProfile,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())?;
    let kind = registry
        .connections
        .get(&profile.connection.value)
        .context("resolved connection is no longer configured")?
        .kind;
    match (owner, kind == ProviderKind::Codex) {
        (ContinuationOwner::Native, false) | (ContinuationOwner::ManagedCodex, true) => Ok(()),
        (ContinuationOwner::Native, true) => {
            bail!("target Profile selects managed Codex for a native continuation")
        }
        (ContinuationOwner::ManagedCodex, false) => {
            bail!("managed Codex continuation requires a Codex Profile")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode},
        private_state::{ProjectRegistryDocument, ensure_interoperable_records, read_document},
        shell::ShellConfig,
    };
    use std::{ffi::OsString, fs};
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, XanaPaths, PathBuf, PathBuf) {
        let directory = tempdir().unwrap();
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
        ensure_interoperable_records(&paths).unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        (
            directory,
            paths,
            source.canonicalize().unwrap(),
            target.canonicalize().unwrap(),
        )
    }

    #[test]
    fn same_workspace_commit_reassigns_without_creating_history() {
        let (_directory, paths, workspace, _) = fixture();
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Source", &workspace)
            .unwrap();
        let source = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let source_ref = ConversationRef::Native {
            session_id: source.session_id(),
        };
        drop(source);

        let service = ProjectContinuationService::open(&paths).unwrap();
        let plan = service
            .plan(&source_ref, &workspace, project.id, None)
            .unwrap();
        let receipt = service.commit(&source_ref, plan).unwrap();

        assert!(matches!(
            receipt,
            ProjectContinuationReceipt::Reassigned { .. }
        ));
        assert_eq!(
            ProjectStore::open(&paths)
                .unwrap()
                .membership(
                    &source_ref
                        .conversation_id()
                        .expect("stable source")
                        .to_string()
                )
                .unwrap(),
            Some(project.id)
        );
    }

    #[test]
    fn cross_workspace_commit_preserves_source_and_records_target_provenance() {
        let (_directory, paths, source_workspace, target_workspace) = fixture();
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Target", &target_workspace)
            .unwrap();
        let source = DurableSession::create(paths.data_dir(), source_workspace.clone()).unwrap();
        let source_session = source.session_id();
        let source_ref = ConversationRef::Native {
            session_id: source_session,
        };
        drop(source);

        let service = ProjectContinuationService::open(&paths).unwrap();
        let plan = service
            .plan(&source_ref, &source_workspace, project.id, None)
            .unwrap();
        let receipt = service.commit(&source_ref, plan).unwrap();
        let ProjectContinuationReceipt::StartedFresh {
            conversation: ConversationRef::Native { session_id: target },
            workspace,
        } = receipt
        else {
            panic!("expected a fresh native continuation")
        };

        assert_eq!(workspace, target_workspace);
        assert!(DurableSession::inspect(paths.data_dir(), source_session).is_ok());
        assert_eq!(
            DurableSession::inspect(paths.data_dir(), target)
                .unwrap()
                .record_count,
            1
        );
        let document: ProjectRegistryDocument = read_document(&paths.projects_file()).unwrap();
        assert_eq!(
            document
                .conversation_predecessors
                .get(&target.to_string())
                .map(String::as_str),
            Some(source_session.to_string().as_str())
        );
        assert_eq!(
            document.conversation_memberships.get(&target.to_string()),
            Some(&project.id)
        );
    }
}
