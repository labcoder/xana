//! Presentation adapter for optional project lifecycle commands.

use crate::{
    cli::{ProjectCommand, ProjectOwnerChoice},
    paths::XanaPaths,
    portable_project::PortableProjectStore,
    private_state::ProjectLifecycle,
    project::{ContinuationOwner, ContinuationPlacement, Project, ProjectStore, WorkspaceStatus},
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
        } => {
            let source_workspace = workspace_or_current(source_workspace)?;
            let owner = match owner {
                ProjectOwnerChoice::Native => ContinuationOwner::Native,
                ProjectOwnerChoice::ManagedCodex => ContinuationOwner::ManagedCodex,
            };
            let continuation =
                store.plan_continuation(&conversation, &source_workspace, project_id, owner)?;
            writeln!(
                output,
                "Source: {}  Target project: {}",
                continuation.source_conversation, continuation.project
            )?;
            writeln!(output, "Continuation review: {}", continuation.handoff)?;
            match continuation.placement {
                ContinuationPlacement::ReassignExisting => writeln!(
                    output,
                    "Action: `xana project assign {project_id} {conversation}`"
                )?,
                ContinuationPlacement::StartFresh {
                    new_conversation,
                    owner,
                } => writeln!(
                    output,
                    "New conversation: {new_conversation} ({})",
                    match owner {
                        ContinuationOwner::Native => "native owner",
                        ContinuationOwner::ManagedCodex => "managed Codex owner starts fresh",
                    }
                )?,
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
