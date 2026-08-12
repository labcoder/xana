//! Presentation adapter for optional project lifecycle commands.

use crate::{
    cli::{ProjectCommand, ProjectOwnerChoice},
    paths::XanaPaths,
    portable_project::PortableProjectStore,
    private_state::ProjectLifecycle,
    profile::{ProfileStore, ResolvedProfile},
    project::{ContinuationOwner, ContinuationPlacement, Project, ProjectStore, WorkspaceStatus},
    session::DurableSession,
};
use anyhow::{Context, Result, bail};
use std::{env, io::Write, path::PathBuf};

pub(super) fn run_command(
    command: ProjectCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let store = ProjectStore::open(paths)
        .context("could not open project state; run `xana config migrate --apply` and retry")?;
    match command {
        ProjectCommand::List { all } => {
            let projects = store.list(all)?;
            if projects.is_empty() {
                writeln!(output, "No {}projects.", if all { "" } else { "active " })?;
            }
            for project in projects {
                render_project(output, &project)?;
            }
        }
        ProjectCommand::Create { name, workspace } => {
            let workspace = workspace_or_current(workspace)?;
            let project = store.create(&name, &workspace)?;
            writeln!(output, "Project created.")?;
            render_project(output, &project)?;
            writeln!(output, "Workspace files were not changed.")?;
        }
        ProjectCommand::Inspect { project_id } => {
            let inspection = store.inspect(project_id)?;
            render_project(output, &inspection.project)?;
            writeln!(
                output,
                "Workspace status: {}",
                match inspection.workspace_status {
                    WorkspaceStatus::Available => "available",
                    WorkspaceStatus::Missing => "missing (use `xana project relink`)",
                    WorkspaceStatus::ChangedIdentity => {
                        "changed identity (review, then use `xana project relink`)"
                    }
                }
            )?;
            writeln!(output, "Conversations: {}", inspection.conversation_count)?;
        }
        ProjectCommand::Rename { project_id, name } => {
            render_project(output, &store.rename(project_id, &name)?)?;
        }
        ProjectCommand::Archive { project_id } => {
            render_project(output, &store.archive(project_id)?)?;
            writeln!(output, "Workspace and conversations were preserved.")?;
        }
        ProjectCommand::Unarchive { project_id } => {
            render_project(output, &store.unarchive(project_id)?)?;
        }
        ProjectCommand::Relink {
            project_id,
            workspace,
        } => {
            render_project(output, &store.relink(project_id, &workspace)?)?;
        }
        ProjectCommand::Forget { project_id, yes } => {
            if !yes {
                bail!(
                    "forget removes only local project organization; review it, then repeat with --yes"
                );
            }
            if store.forget(project_id)? {
                writeln!(
                    output,
                    "Project forgotten. Conversations are Ungrouped; workspace and history were preserved."
                )?;
            } else {
                writeln!(output, "Project was already absent.")?;
            }
        }
        ProjectCommand::Assign {
            project_id,
            conversation,
            workspace,
        } => {
            let workspace = workspace_or_current(workspace)?;
            store.place_conversation(&conversation, &workspace, Some(project_id))?;
            writeln!(
                output,
                "Conversation {conversation} assigned to project {project_id}."
            )?;
        }
        ProjectCommand::Ungroup { conversation } => {
            store.ungroup_conversation(&conversation)?;
            writeln!(output, "Conversation {conversation} is Ungrouped.")?;
        }
        ProjectCommand::Membership { conversation } => match store.membership(&conversation)? {
            Some(project) => writeln!(output, "Conversation {conversation}: project {project}")?,
            None => writeln!(output, "Conversation {conversation}: Ungrouped")?,
        },
        ProjectCommand::Continue {
            project_id,
            conversation,
            source_workspace,
            owner,
            profile,
            apply,
        } => {
            let source_workspace = workspace_or_current(source_workspace)?;
            let owner = match owner {
                ProjectOwnerChoice::Native => ContinuationOwner::Native,
                ProjectOwnerChoice::ManagedCodex => ContinuationOwner::ManagedCodex,
            };
            if apply && owner == ContinuationOwner::Native {
                let source_id: crate::identity::SessionId =
                    conversation.parse().map_err(|error| {
                        anyhow::anyhow!(
                            "native continuation source must be a session UUID: {error}"
                        )
                    })?;
                let (_, restored) = DurableSession::inspect_restored(paths.data_dir(), source_id)?;
                if restored.workspace_root.canonicalize()? != source_workspace.canonicalize()? {
                    bail!(
                        "source session belongs to {}; --source-workspace resolved to {}",
                        restored.workspace_root.display(),
                        source_workspace.display()
                    );
                }
            }
            let continuation =
                store.plan_continuation(&conversation, &source_workspace, project_id, owner)?;
            writeln!(
                output,
                "Source: {}  Target project: {}",
                continuation.source_conversation, continuation.project
            )?;
            writeln!(output, "Continuation review: {}", continuation.handoff)?;
            let target_profile = resolve_target_profile(paths, project_id, profile.as_deref())?;
            validate_owner_profile(paths, owner, &target_profile)?;
            writeln!(
                output,
                "Profile: {} ({}, ready={})",
                target_profile.name,
                target_profile.scope,
                target_profile.is_ready()
            )?;
            for reason in &target_profile.readiness {
                writeln!(output, "  not ready: {reason}")?;
            }
            if !target_profile.is_ready() {
                bail!("target profile is not ready; no continuation state was changed");
            }
            let placement = if profile.is_some()
                && matches!(
                    continuation.placement,
                    ContinuationPlacement::ReassignExisting
                ) {
                ContinuationPlacement::StartFresh {
                    new_conversation: crate::identity::SessionId::new(),
                    owner,
                }
            } else {
                continuation.placement
            };
            match placement {
                ContinuationPlacement::ReassignExisting => writeln!(
                    output,
                    "Action: {}",
                    if apply {
                        store.place_conversation(
                            &conversation,
                            &source_workspace,
                            Some(project_id),
                        )?;
                        "existing conversation assigned; history and profile snapshot preserved"
                    } else {
                        "review only; repeat with --apply to assign the existing conversation"
                    }
                )?,
                ContinuationPlacement::StartFresh {
                    new_conversation,
                    owner,
                } => {
                    writeln!(
                        output,
                        "New conversation: {new_conversation} ({})",
                        match owner {
                            ContinuationOwner::Native => "native owner",
                            ContinuationOwner::ManagedCodex => {
                                "managed Codex owner starts fresh on its first turn"
                            }
                        }
                    )?;
                    if apply {
                        let project = store.get(project_id)?;
                        let created_session = if owner == ContinuationOwner::Native {
                            Some(DurableSession::create_with_id(
                                paths.data_dir(),
                                project.canonical_workspace.clone(),
                                new_conversation,
                            )?)
                        } else {
                            None
                        };
                        if let Err(error) = ProfileStore::open(paths).commit_project_continuation(
                            &conversation,
                            &new_conversation.to_string(),
                            project_id,
                            &target_profile,
                        ) {
                            if let Some(session) = created_session {
                                session.discard_unstarted().with_context(|| {
                                    format!(
                                        "project continuation failed ({error}); its empty native session also could not be rolled back"
                                    )
                                })?;
                            }
                            return Err(error.into());
                        }
                        drop(created_session);
                        writeln!(
                            output,
                            "Committed: source preserved, predecessor/profile provenance recorded, target workspace {}.",
                            project.canonical_workspace.display()
                        )?;
                        match owner {
                            ContinuationOwner::Native => writeln!(
                                output,
                                "Resume: `xana --resume {new_conversation}` from the target workspace"
                            )?,
                            ContinuationOwner::ManagedCodex => writeln!(
                                output,
                                "Start `xana --resume {new_conversation}` in the target workspace; the frozen Codex profile is selected now and the vendor thread is created only when the first turn is sent."
                            )?,
                        }
                    } else {
                        writeln!(output, "Review only; repeat with --apply to commit.")?;
                    }
                }
            }
        }
        ProjectCommand::Share { project_id } => {
            let project = store.get(project_id)?;
            let shared = PortableProjectStore::share(&project)?;
            writeln!(output, "Portable Xana project: {}", shared.path.display())?;
            writeln!(output, "Manifest digest: {}", shared.digest)?;
            writeln!(
                output,
                "No credential, authority, history, or private path was shared."
            )?;
        }
        ProjectCommand::InspectPortable { workspace } => {
            let workspace = workspace_or_current(workspace)?;
            let inspection =
                PortableProjectStore::inspect_with_user_authority(&workspace, paths.config_file())?;
            writeln!(output, "Portable project: {}", inspection.manifest.name)?;
            writeln!(output, "Portable id: {}", inspection.manifest.portable_id)?;
            writeln!(output, "Manifest: {}", inspection.path.display())?;
            writeln!(
                output,
                "Review is read-only; nothing was registered or enabled."
            )?;
        }
        ProjectCommand::Register { workspace } => {
            let workspace = workspace_or_current(workspace)?;
            let resolution = PortableProjectStore::open(paths).register(paths, &workspace)?;
            writeln!(output, "Registered project {}.", resolution.project.id)?;
            render_resolution(output, &resolution)?;
        }
        ProjectCommand::Refresh { project_id } => {
            let resolution = PortableProjectStore::open(paths).refresh(paths, project_id)?;
            writeln!(output, "Portable manifest review accepted.")?;
            render_resolution(output, &resolution)?;
        }
        ProjectCommand::Diff { project_id } | ProjectCommand::Setup { project_id } => {
            let resolution = PortableProjectStore::open(paths).resolve(paths, project_id)?;
            render_resolution(output, &resolution)?;
        }
        ProjectCommand::Bind {
            project_id,
            logical,
            local,
        } => {
            PortableProjectStore::open(paths).bind(project_id, &logical, &local)?;
            writeln!(
                output,
                "Bound {logical} privately for project {project_id}; the local value is not printed or written to the workspace."
            )?;
        }
        ProjectCommand::StopSharing { project_id, yes } => {
            if !yes {
                bail!(
                    "stop-sharing removes `.agents/xana/project.toml`; review it, then repeat with --yes"
                );
            }
            if PortableProjectStore::open(paths).stop_sharing(paths, project_id)? {
                writeln!(
                    output,
                    "Portable sharing stopped; local project state is preserved."
                )?;
            } else {
                writeln!(output, "Portable sharing was already absent.")?;
            }
        }
    }
    Ok(())
}

fn resolve_target_profile(
    paths: &XanaPaths,
    project: crate::identity::ProjectId,
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
        crate::config::XanaConfig::load_registry_from(paths.config_file())
            .map(|registry| registry.default_profile)
            .unwrap_or_else(|_| "default".to_owned())
    });
    Ok(store.resolve_global(&name)?)
}

fn validate_owner_profile(
    paths: &XanaPaths,
    owner: ContinuationOwner,
    profile: &ResolvedProfile,
) -> Result<()> {
    let registry = crate::config::XanaConfig::load_registry_from(paths.config_file())?;
    let kind = registry
        .connections
        .get(&profile.connection.value)
        .ok_or_else(|| anyhow::anyhow!("resolved connection is no longer configured"))?
        .kind;
    match (owner, kind == crate::config::ProviderKind::Codex) {
        (ContinuationOwner::Native, false) | (ContinuationOwner::ManagedCodex, true) => Ok(()),
        (ContinuationOwner::Native, true) => {
            bail!("profile selects managed Codex; use --owner codex")
        }
        (ContinuationOwner::ManagedCodex, false) => {
            bail!("--owner codex requires a profile whose connection kind is Codex")
        }
    }
}

fn workspace_or_current(workspace: Option<PathBuf>) -> Result<PathBuf> {
    workspace.map_or_else(
        || env::current_dir().context("could not read current directory"),
        Ok,
    )
}

fn render_project(output: &mut dyn Write, project: &Project) -> Result<()> {
    writeln!(
        output,
        "{}  {}  {}  {}",
        project.id,
        match project.lifecycle {
            ProjectLifecycle::Active => "active",
            ProjectLifecycle::Archived => "archived",
        },
        project.name,
        project.canonical_workspace.display()
    )?;
    writeln!(
        output,
        "created={} updated={}",
        project.created_unix_ms, project.updated_unix_ms
    )?;
    Ok(())
}

fn render_resolution(
    output: &mut dyn Write,
    resolution: &crate::portable_project::PortableResolution,
) -> Result<()> {
    writeln!(
        output,
        "Project: {} ({})",
        resolution.project.name, resolution.project.id
    )?;
    writeln!(output, "Portable id: {}", resolution.manifest.portable_id)?;
    writeln!(
        output,
        "Manifest: {}",
        if resolution.stale {
            "changed; run `xana project refresh` after review"
        } else {
            "registered digest matches"
        }
    )?;
    if resolution.missing.is_empty() {
        writeln!(
            output,
            "Bindings: ready ({} private name(s); values redacted)",
            resolution.bindings.len()
        )?;
    } else {
        writeln!(output, "Bindings: not ready")?;
        for logical in &resolution.missing {
            writeln!(
                output,
                "  missing {logical}: `xana project bind {} {logical} LOCAL_NAME`",
                resolution.project.id
            )?;
        }
    }
    writeln!(output, "Ready: {}", resolution.is_ready())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cli::ProjectOwnerChoice,
        config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
        private_state::{ProjectRegistryDocument, read_document},
        session::DurableSession,
        shell::ShellConfig,
    };
    use std::{ffi::OsString, fs};
    use tempfile::tempdir;

    #[test]
    fn applied_native_cross_workspace_continuation_is_resumable_and_preserves_source() {
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
        crate::private_state::ensure_interoperable_records(&paths).unwrap();
        let source_workspace = directory.path().join("source");
        let target_workspace = directory.path().join("target");
        fs::create_dir_all(&source_workspace).unwrap();
        fs::create_dir_all(&target_workspace).unwrap();
        let source = DurableSession::create(paths.data_dir(), source_workspace.clone()).unwrap();
        let source_id = source.session_id();
        drop(source);
        let project = ProjectStore::open(&paths)
            .unwrap()
            .create("Target", &target_workspace)
            .unwrap();

        let mut output = Vec::new();
        run_command(
            ProjectCommand::Continue {
                project_id: project.id,
                conversation: source_id.to_string(),
                source_workspace: Some(source_workspace),
                owner: ProjectOwnerChoice::Native,
                profile: Some("default".into()),
                apply: true,
            },
            &paths,
            &mut output,
        )
        .unwrap();

        let document: ProjectRegistryDocument = read_document(&paths.projects_file()).unwrap();
        let (target, _) = document
            .conversation_predecessors
            .iter()
            .find(|(_, predecessor)| *predecessor == &source_id.to_string())
            .unwrap();
        let target_id: crate::identity::SessionId = target.parse().unwrap();
        assert_eq!(
            DurableSession::inspect(paths.data_dir(), target_id)
                .unwrap()
                .record_count,
            1
        );
        assert!(DurableSession::inspect(paths.data_dir(), source_id).is_ok());
        assert_eq!(document.conversation_memberships[target], project.id);
        assert_eq!(
            document.conversation_profiles[target].profile_name,
            "default"
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("source preserved")
        );
    }
}
