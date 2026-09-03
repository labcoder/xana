//! Profile, Project, and capability projections for graphical management.

use super::{DesktopControlPlane, DesktopError, bounded, control_error};
use crate::{
    capability::resolve_builtin_capability_snapshot,
    config::{PermissionMode, XanaConfig},
    private_state::ProjectLifecycle,
    profile::ProfileStore,
    project::{ProjectStore, WorkspaceStatus},
};
use std::path::PathBuf;

const MANAGEMENT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProfileSummary {
    pub name: String,
    pub id: String,
    pub archived: bool,
    pub connection: String,
    pub model: String,
    pub permission: String,
    pub ready: bool,
    pub readiness: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProjectSummary {
    pub id: String,
    pub name: String,
    pub workspace: PathBuf,
    pub lifecycle: String,
    pub workspace_status: String,
    pub conversation_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopManagementSnapshot {
    pub version: u16,
    pub default_profile: String,
    pub profiles: Vec<DesktopProfileSummary>,
    pub projects: Vec<DesktopProjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProfileDraft {
    pub name: String,
    pub connection: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProjectDraft {
    pub name: String,
    pub workspace: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntityMutationReceipt {
    pub semantic_code: String,
    pub subject: String,
    pub effect: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopCapabilityFact {
    pub kind: String,
    pub id: String,
    pub installed: Option<bool>,
    pub enabled: Option<bool>,
    pub available: Option<bool>,
    pub permitted: Option<bool>,
    pub selected: bool,
    pub containment: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopCapabilitySnapshot {
    pub version: u16,
    pub configuration: String,
    pub profile: String,
    pub connection: String,
    pub model: String,
    pub permission: String,
    pub facts: Vec<DesktopCapabilityFact>,
    pub warnings: Vec<String>,
}

impl DesktopControlPlane {
    pub fn management_snapshot(&self) -> Result<DesktopManagementSnapshot, DesktopError> {
        let registry =
            XanaConfig::load_registry_from(self.paths.config_file()).map_err(control_error)?;
        let profiles = ProfileStore::open(&self.paths);
        let profile_rows = profiles
            .list_global(true)
            .map_err(control_error)?
            .into_iter()
            .map(|profile| {
                let resolution = (!profile.archived)
                    .then(|| profiles.resolve_global(&profile.id))
                    .transpose();
                let (ready, readiness) = match resolution {
                    Ok(Some(resolved)) => (resolved.is_ready(), resolved.readiness),
                    Ok(None) => (false, vec!["Profile is archived".to_owned()]),
                    Err(error) => (false, vec![bounded(error.to_string())]),
                };
                DesktopProfileSummary {
                    name: bounded(profile.id),
                    id: profile.profile_id.to_string(),
                    archived: profile.archived,
                    connection: bounded(profile.connection),
                    model: bounded(profile.model),
                    permission: profile
                        .permission_mode
                        .map_or("user-policy", PermissionMode::as_str)
                        .to_owned(),
                    ready,
                    readiness: readiness.into_iter().map(bounded).collect(),
                }
            })
            .collect();
        let project_store = ProjectStore::open(&self.paths).map_err(control_error)?;
        let project_rows = project_store
            .list(true)
            .map_err(control_error)?
            .into_iter()
            .map(|project| {
                let inspection = project_store.inspect(project.id);
                let (workspace_status, conversation_count) = match inspection {
                    Ok(inspection) => (
                        match inspection.workspace_status {
                            WorkspaceStatus::Available => "available",
                            WorkspaceStatus::Missing => "missing",
                            WorkspaceStatus::ChangedIdentity => "changed_identity",
                        },
                        inspection.conversation_count,
                    ),
                    Err(_) => ("unknown", 0),
                };
                DesktopProjectSummary {
                    id: project.id.to_string(),
                    name: bounded(project.name),
                    workspace: project.canonical_workspace,
                    lifecycle: match project.lifecycle {
                        ProjectLifecycle::Active => "active",
                        ProjectLifecycle::Archived => "archived",
                    }
                    .to_owned(),
                    workspace_status: workspace_status.to_owned(),
                    conversation_count,
                }
            })
            .collect();
        Ok(DesktopManagementSnapshot {
            version: MANAGEMENT_VERSION,
            default_profile: bounded(registry.default_profile),
            profiles: profile_rows,
            projects: project_rows,
        })
    }

    pub fn create_profile(
        &self,
        draft: DesktopProfileDraft,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        let profile = ProfileStore::open(&self.paths)
            .create_global(draft.name, draft.connection, draft.model)
            .map_err(control_error)?;
        Ok(entity_receipt(
            "profile.create.completed.v1",
            profile.id,
            "created",
            "Available to new Conversations after explicit selection.",
        ))
    }

    pub fn set_profile_archived(
        &self,
        profile: &str,
        archived: bool,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        let profile = ProfileStore::open(&self.paths)
            .set_global_archived(profile, archived)
            .map_err(control_error)?;
        Ok(entity_receipt(
            if archived {
                "profile.archive.completed.v1"
            } else {
                "profile.unarchive.completed.v1"
            },
            profile.id,
            if archived { "archived" } else { "unarchived" },
            "Existing frozen Conversations are unchanged.",
        ))
    }

    pub fn delete_profile(
        &self,
        profile: &str,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        ProfileStore::open(&self.paths)
            .delete_global(profile)
            .map_err(control_error)?;
        Ok(entity_receipt(
            "profile.delete.completed.v1",
            profile.to_owned(),
            "deleted",
            "Existing frozen Conversations retain their snapshots.",
        ))
    }

    pub fn create_project(
        &self,
        draft: DesktopProjectDraft,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        let project = ProjectStore::open(&self.paths)
            .and_then(|store| store.create(&draft.name, &draft.workspace))
            .map_err(control_error)?;
        Ok(entity_receipt(
            "project.create.completed.v1",
            project.id.to_string(),
            "created",
            "Workspace files were not changed.",
        ))
    }

    pub fn set_project_archived(
        &self,
        project: &str,
        archived: bool,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        let id = project.parse().map_err(control_error)?;
        let project = ProjectStore::open(&self.paths)
            .and_then(|store| {
                if archived {
                    store.archive(id)
                } else {
                    store.unarchive(id)
                }
            })
            .map_err(control_error)?;
        Ok(entity_receipt(
            if archived {
                "project.archive.completed.v1"
            } else {
                "project.unarchive.completed.v1"
            },
            project.id.to_string(),
            if archived { "archived" } else { "unarchived" },
            "Workspace files and Conversations were preserved.",
        ))
    }

    pub fn forget_project(
        &self,
        project: &str,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        let id = project.parse().map_err(control_error)?;
        let removed = ProjectStore::open(&self.paths)
            .and_then(|store| store.forget(id))
            .map_err(control_error)?;
        Ok(entity_receipt(
            "project.forget.completed.v1",
            project.to_owned(),
            if removed {
                "forgotten"
            } else {
                "already_absent"
            },
            "Workspace files and Conversation history were preserved; Conversations are Ungrouped.",
        ))
    }

    pub fn capability_snapshot(&self) -> Result<DesktopCapabilitySnapshot, DesktopError> {
        let registry =
            XanaConfig::load_registry_from(self.paths.config_file()).map_err(control_error)?;
        let profile = registry
            .profiles
            .get(&registry.default_profile)
            .ok_or_else(|| control_error("default Profile is unavailable"))?;
        let mut warnings = Vec::new();
        let mut facts = Vec::new();
        match resolve_builtin_capability_snapshot(profile.capabilities.as_deref()) {
            Ok(snapshot) => facts.extend(snapshot.capabilities().iter().map(|capability| {
                DesktopCapabilityFact {
                    kind: "native".to_owned(),
                    id: capability.to_string(),
                    installed: Some(true),
                    enabled: Some(true),
                    available: Some(true),
                    permitted: None,
                    selected: true,
                    containment: "application_owned_permission_gate".to_owned(),
                    detail: "Built-in capability; each effect is still authorized at invocation."
                        .to_owned(),
                }
            })),
            Err(error) => warnings.push(bounded(error.to_string())),
        }
        facts.extend(profile.skills.iter().map(|id| DesktopCapabilityFact {
            kind: "skill".to_owned(),
            id: bounded(id.clone()),
            installed: None,
            enabled: Some(true),
            available: None,
            permitted: None,
            selected: true,
            containment: "untrusted_context_no_authority".to_owned(),
            detail:
                "Availability depends on the active workspace and installed plugins.".to_owned(),
        }));
        facts.extend(registry.plugins.keys().map(|id| DesktopCapabilityFact {
            kind: "agent_plugin".to_owned(),
            id: bounded(id.clone()),
            installed: None,
            enabled: Some(profile.plugins.contains(id)),
            available: None,
            permitted: None,
            selected: profile.plugins.contains(id),
            containment: "out_of_process_declarative_bundle".to_owned(),
            detail:
                "Install and revision health are resolved by the Agent Plugin manager.".to_owned(),
        }));
        facts.extend(registry.mcp_servers.keys().map(|id| {
            DesktopCapabilityFact {
                kind: "mcp".to_owned(),
                id: bounded(id.clone()),
                installed: None,
                enabled: Some(profile.mcp_servers.contains(id)),
                available: None,
                permitted: None,
                selected: profile.mcp_servers.contains(id),
                containment: "supervised_or_remote_tool_boundary".to_owned(),
                detail:
                    "Live readiness is unknown until the configured server is explicitly probed."
                        .to_owned(),
            }
        }));
        facts.extend(
            registry
                .external_agents
                .keys()
                .map(|id| DesktopCapabilityFact {
                    kind: "external_agent".to_owned(),
                    id: bounded(id.clone()),
                    installed: None,
                    enabled: Some(profile.external_agents.contains(id)),
                    available: None,
                    permitted: None,
                    selected: profile.external_agents.contains(id),
                    containment: "remote_trust_and_egress_boundary".to_owned(),
                    detail:
                        "Trust, remote reachability, and task acceptance remain separate facts."
                            .to_owned(),
                }),
        );
        facts.extend(
            registry
                .service_routes
                .keys()
                .map(|id| DesktopCapabilityFact {
                    kind: "focused_route".to_owned(),
                    id: bounded(id.clone()),
                    installed: None,
                    enabled: Some(profile.service_routes.contains(id)),
                    available: None,
                    permitted: None,
                    selected: profile.service_routes.contains(id),
                    containment: "focused_service_route".to_owned(),
                    detail:
                        "Provider capability and policy are resolved when the route is invoked."
                            .to_owned(),
                }),
        );
        facts.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.id.cmp(&right.id)));
        Ok(DesktopCapabilitySnapshot {
            version: MANAGEMENT_VERSION,
            configuration: "valid".to_owned(),
            profile: bounded(registry.default_profile),
            connection: bounded(profile.connection.clone()),
            model: bounded(profile.model.clone()),
            permission: registry.permission_mode.as_str().to_owned(),
            facts,
            warnings,
        })
    }
}

fn entity_receipt(
    semantic_code: &str,
    subject: String,
    effect: &str,
    detail: &str,
) -> DesktopEntityMutationReceipt {
    DesktopEntityMutationReceipt {
        semantic_code: semantic_code.to_owned(),
        subject: bounded(subject),
        effect: effect.to_owned(),
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection},
        shell::ShellConfig,
    };
    use std::fs;
    use tempfile::tempdir;

    fn control() -> (tempfile::TempDir, DesktopControlPlane) {
        let directory = tempdir().unwrap();
        let control =
            DesktopControlPlane::resolve(Some(directory.path().join("home").into_os_string()))
                .unwrap();
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        fs::create_dir_all(control.paths.config_file().parent().unwrap()).unwrap();
        fs::write(control.paths.config_file(), rendered).unwrap();
        crate::private_state::ensure_interoperable_records(&control.paths).unwrap();
        (directory, control)
    }

    #[test]
    fn profile_and_project_mutations_preserve_typed_scope() {
        let (directory, control) = control();
        let profile = control
            .create_profile(DesktopProfileDraft {
                name: "review".to_owned(),
                connection: "local".to_owned(),
                model: "qwen".to_owned(),
            })
            .unwrap();
        assert_eq!(profile.semantic_code, "profile.create.completed.v1");

        let workspace = directory.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let project = control
            .create_project(DesktopProjectDraft {
                name: "Xana".to_owned(),
                workspace: workspace.clone(),
            })
            .unwrap();
        assert_eq!(project.semantic_code, "project.create.completed.v1");
        let snapshot = control.management_snapshot().unwrap();
        assert!(snapshot.profiles.iter().any(|row| row.name == "review"));
        let canonical_workspace = fs::canonicalize(workspace).unwrap();
        assert!(
            snapshot
                .projects
                .iter()
                .any(|row| row.workspace == canonical_workspace)
        );
    }

    #[test]
    fn capability_snapshot_keeps_permission_and_availability_distinct() {
        let (_directory, control) = control();
        let snapshot = control.capability_snapshot().unwrap();
        assert_eq!(snapshot.permission, "ask");
        assert!(snapshot.facts.iter().all(|fact| fact.permitted.is_none()));
        assert!(
            snapshot
                .facts
                .iter()
                .any(|fact| fact.kind == "native" && fact.available == Some(true))
        );
    }
}
