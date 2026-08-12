//! Thin CLI presentation adapter for the profile domain.

use crate::{
    cli::ProfileCommand,
    config::{ProfileConfig, ProfileUpdate, ProfileUse},
    paths::XanaPaths,
    portable_project::{PortableProfile, PortableProjectStore},
    profile::{ProfileScope, ProfileStore, ResolvedProfile},
};
use anyhow::{Result, bail};
use std::io::Write;

pub(super) fn run_command(
    command: ProfileCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let profiles = ProfileStore::open(paths);
    let portable = PortableProjectStore::open(paths);
    match command {
        ProfileCommand::List { project, all } => match project {
            Some(project) => {
                for (name, profile) in portable.list_profiles(paths, project, all)? {
                    render_portable(output, project, &name, &profile)?;
                }
            }
            None => {
                for profile in profiles.list_global(all)? {
                    render_global(output, &profile)?;
                }
            }
        },
        ProfileCommand::Create {
            name,
            connection,
            model,
            project,
            authority_profile,
        } => match project {
            Some(project) => {
                let authority_profile = authority_profile.ok_or_else(|| {
                    anyhow::anyhow!("project profile creation requires --authority-profile")
                })?;
                let profile = portable.create_profile(
                    paths,
                    project,
                    &name,
                    PortableProfile {
                        profile_id: None,
                        archived: false,
                        authority_profile,
                        connection,
                        model,
                        reasoning_effort: None,
                        reasoning_summary: None,
                        identity: None,
                        capabilities: Vec::new(),
                        permission_mode: None,
                        max_tool_rounds: None,
                        orchestration: None,
                        skills: Vec::new(),
                        plugins: Vec::new(),
                        mcp_servers: Vec::new(),
                        external_agents: Vec::new(),
                        service_routes: Vec::new(),
                        egress: Vec::new(),
                        applies_to: vec![ProfileUse::Primary, ProfileUse::Child],
                    },
                )?;
                render_portable(output, project, &name, &profile)?;
            }
            None => {
                if authority_profile.is_some() {
                    bail!("--authority-profile is valid only with --project");
                }
                render_global(output, &profiles.create_global(name, connection, model)?)?;
            }
        },
        ProfileCommand::Inspect { name, project } => match project {
            Some(project) => render_portable(
                output,
                project,
                &name,
                &portable.inspect_profile(paths, project, &name)?,
            )?,
            None => render_global(output, &profiles.inspect_global(&name)?)?,
        },
        ProfileCommand::Edit {
            name,
            project,
            connection,
            model,
            reasoning_effort,
            reasoning_summary,
            identity,
            permission_mode,
            max_tool_rounds,
        } => match project {
            Some(project) => {
                let mut profile = portable.inspect_profile(paths, project, &name)?;
                if let Some(value) = connection {
                    profile.connection = value;
                }
                if let Some(value) = model {
                    profile.model = value;
                }
                if let Some(value) = reasoning_effort {
                    profile.reasoning_effort = Some(value);
                }
                if let Some(value) = reasoning_summary {
                    profile.reasoning_summary = Some(value);
                }
                if let Some(value) = identity {
                    profile.identity = Some(value);
                }
                if let Some(value) = permission_mode {
                    profile.permission_mode = Some(value.into());
                }
                if let Some(value) = max_tool_rounds {
                    profile.max_tool_rounds = Some(value);
                }
                let profile = portable.replace_profile(paths, project, &name, profile)?;
                render_portable(output, project, &name, &profile)?;
            }
            None => {
                let profile = profiles.edit_global(
                    &name,
                    ProfileUpdate {
                        connection,
                        model,
                        reasoning_effort: reasoning_effort.map(Some),
                        reasoning_summary: reasoning_summary.map(Some),
                        identity: identity.map(Some),
                        permission_mode: permission_mode.map(|value| Some(value.into())),
                        max_tool_rounds,
                    },
                )?;
                render_global(output, &profile)?;
            }
        },
        ProfileCommand::Duplicate {
            source,
            name,
            project,
        } => match project {
            Some(project) => render_portable(
                output,
                project,
                &name,
                &portable.duplicate_profile(paths, project, &source, &name)?,
            )?,
            None => render_global(output, &profiles.duplicate_global(&source, &name)?)?,
        },
        ProfileCommand::Rename { old, new, project } => match project {
            Some(project) => render_portable(
                output,
                project,
                &new,
                &portable.rename_profile(paths, project, &old, &new)?,
            )?,
            None => render_global(output, &profiles.rename_global(&old, &new)?)?,
        },
        ProfileCommand::Archive { name, project } => match project {
            Some(project) => render_portable(
                output,
                project,
                &name,
                &portable.set_profile_archived(paths, project, &name, true)?,
            )?,
            None => render_global(output, &profiles.set_global_archived(&name, true)?)?,
        },
        ProfileCommand::Unarchive { name, project } => match project {
            Some(project) => render_portable(
                output,
                project,
                &name,
                &portable.set_profile_archived(paths, project, &name, false)?,
            )?,
            None => render_global(output, &profiles.set_global_archived(&name, false)?)?,
        },
        ProfileCommand::Delete { name, project, yes } => {
            if !yes {
                bail!("profile deletion requires review and --yes");
            }
            match project {
                Some(project) => portable.delete_profile(paths, project, &name)?,
                None => profiles.delete_global(&name)?,
            }
            writeln!(
                output,
                "Deleted profile {name:?}; conversations retain frozen snapshots."
            )?;
        }
        ProfileCommand::Resolve {
            name,
            project,
            json,
        } => {
            let resolved = resolve(&profiles, paths, project, &name)?;
            render_resolved(output, &resolved, json)?;
        }
        ProfileCommand::Freeze {
            name,
            conversation,
            project,
        } => {
            let resolved = resolve(&profiles, paths, project, &name)?;
            let snapshot = profiles.freeze(&conversation, &resolved)?;
            writeln!(
                output,
                "Frozen profile {} ({}) for conversation {conversation}; digest {}.",
                snapshot.profile_name, snapshot.profile_id, snapshot.digest
            )?;
        }
        ProfileCommand::Snapshot { conversation } => {
            let snapshot = profiles.snapshot(&conversation)?.ok_or_else(|| {
                anyhow::anyhow!("conversation {conversation} has no frozen profile snapshot")
            })?;
            writeln!(output, "{}", serde_json::to_string_pretty(&snapshot)?)?;
            if let Some(predecessor) = profiles.predecessor(&conversation)? {
                writeln!(output, "Predecessor: {predecessor}")?;
            }
        }
        ProfileCommand::Continue {
            name,
            conversation,
            project,
        } => {
            let resolved = resolve(&profiles, paths, project, &name)?;
            let continuation = profiles.continue_with(&conversation, &resolved)?;
            writeln!(
                output,
                "Created linked continuation {continuation} from {conversation} with profile {name:?}."
            )?;
        }
    }
    Ok(())
}

fn resolve(
    store: &ProfileStore,
    paths: &XanaPaths,
    project: Option<crate::identity::ProjectId>,
    name: &str,
) -> Result<ResolvedProfile> {
    match project {
        Some(project) => Ok(store.resolve_project(paths, project, name)?),
        None => Ok(store.resolve_global(name)?),
    }
}

fn render_global(output: &mut dyn Write, profile: &ProfileConfig) -> Result<()> {
    writeln!(
        output,
        "{}  {}  global/{}  {}/{}  permissions={}  applies={}",
        if profile.archived {
            "archived"
        } else {
            "active"
        },
        profile.profile_id,
        profile.id,
        profile.connection,
        profile.model,
        profile
            .permission_mode
            .map_or("user-policy", crate::config::PermissionMode::as_str),
        profile
            .applies_to
            .iter()
            .map(|value| match value {
                ProfileUse::Primary => "primary",
                ProfileUse::Child => "child",
            })
            .collect::<Vec<_>>()
            .join(",")
    )?;
    Ok(())
}

fn render_portable(
    output: &mut dyn Write,
    project: crate::identity::ProjectId,
    name: &str,
    profile: &PortableProfile,
) -> Result<()> {
    writeln!(
        output,
        "{}  {}  project:{project}/{name}  {}/{}  ceiling={}",
        if profile.archived {
            "archived"
        } else {
            "active"
        },
        profile
            .profile_id
            .map_or_else(|| "derived".into(), |id| id.to_string()),
        profile.connection,
        profile.model,
        profile.authority_profile
    )?;
    Ok(())
}

fn render_resolved(output: &mut dyn Write, profile: &ResolvedProfile, json: bool) -> Result<()> {
    if json {
        writeln!(output, "{}", serde_json::to_string_pretty(profile)?)?;
        return Ok(());
    }
    writeln!(
        output,
        "Profile: {} ({})\nScope: {}\nConnection: {}\nModel: {}\nPermissions: {}\nReady: {}",
        profile.name,
        profile.profile_id,
        match profile.scope {
            ProfileScope::Global => "global".to_owned(),
            ProfileScope::Project(project) => format!("project:{project}"),
        },
        profile.connection.value,
        profile.model.value,
        profile.permission_mode.value.as_str(),
        profile.is_ready()
    )?;
    for reason in &profile.readiness {
        writeln!(output, "  not ready: {reason}")?;
    }
    writeln!(
        output,
        "Provenance: {}",
        serde_json::to_string(&profile.redacted_json()?)?
    )?;
    Ok(())
}
