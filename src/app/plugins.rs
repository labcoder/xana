//! CLI adapter for inert Agent Plugin review and lifecycle operations.

use crate::{
    cli::PluginCommand,
    config::XanaConfig,
    paths::XanaPaths,
    plugin::{
        AGENT_PLUGIN_STATUS, AGENT_PLUGIN_VERSION, InstalledPlugin, McpServerKind, PackageSource,
        PluginManager, PluginReview, PluginScope, PluginUpdateReview,
    },
    portable_project::PortableProjectStore,
    private_state::{ProjectRegistryDocument, read_document},
    profile::ProfileStore,
    project::ProjectStore,
};
use anyhow::{Result, bail};
use std::{io::Write, path::PathBuf};

pub(super) fn run_command(
    command: PluginCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let manager = PluginManager::open(paths);
    match command {
        PluginCommand::List { json } => {
            let installed = manager.list()?;
            if json {
                writeln!(output, "{}", serde_json::to_string_pretty(&installed)?)?;
            } else if installed.is_empty() {
                writeln!(output, "No Agent Plugins are installed.")?;
            } else {
                for plugin in installed {
                    render_installed(output, &plugin)?;
                }
            }
        }
        PluginCommand::Inspect { name, json } => {
            let installed = manager.inspect_installed(&name)?;
            if json {
                writeln!(output, "{}", serde_json::to_string_pretty(&installed)?)?;
            } else {
                render_installed(output, &installed)?;
            }
        }
        PluginCommand::Review {
            source,
            git,
            revision,
            linked,
            json,
        } => {
            let source = package_source(source, git, revision, linked)?;
            let review = manager.inspect_source(&source)?;
            render_review(output, &review, json)?;
            writeln!(
                output,
                "No package was installed, enabled, activated, or executed."
            )?;
        }
        PluginCommand::Install {
            source,
            git,
            revision,
            linked,
            yes,
            json,
        } => {
            let source = package_source(source, git, revision, linked)?;
            let review = manager.inspect_source(&source)?;
            render_review(output, &review, json)?;
            if !yes {
                writeln!(
                    output,
                    "Review only. Re-run with --yes to install this exact source; installation will not enable skills or MCP servers."
                )?;
                return Ok(());
            }
            let installed = manager.install(&source, &review.digest)?;
            if json {
                writeln!(output, "{}", serde_json::to_string_pretty(&installed)?)?;
            } else {
                writeln!(
                    output,
                    "Installed {} at revision {} (disabled).",
                    label(&installed.name),
                    installed.active_revision
                )?;
                if installed.mutable {
                    writeln!(
                        output,
                        "WARNING: linked development mode is visibly mutable; source drift invalidates this review."
                    )?;
                }
                writeln!(
                    output,
                    "No skill was activated and no MCP process or network connection was started."
                )?;
            }
        }
        PluginCommand::Enable {
            name,
            project,
            profile,
            json,
        } => {
            let scope = plugin_scope(project, profile.as_deref())?;
            let scope_key = scope.key()?;
            let already_enabled = manager
                .inspect_installed(&name)?
                .enabled_scopes
                .contains(&scope_key);
            let installed = manager.enable(&name, &scope)?;
            if let Some(profile) = profile.as_deref()
                && let Err(error) = set_profile_plugin(paths, project, profile, &name, true)
            {
                if !already_enabled {
                    let _ = manager.disable(&name, &scope);
                }
                return Err(error);
            }
            render_lifecycle(output, &installed, json, "Enabled")?;
        }
        PluginCommand::Disable {
            name,
            project,
            profile,
            json,
        } => {
            let scope = plugin_scope(project, profile.as_deref())?;
            let scope_key = scope.key()?;
            let was_enabled = manager
                .inspect_installed(&name)?
                .enabled_scopes
                .contains(&scope_key);
            let installed = manager.disable(&name, &scope)?;
            if let Some(profile) = profile.as_deref()
                && let Err(error) = set_profile_plugin(paths, project, profile, &name, false)
            {
                if was_enabled {
                    let _ = manager.enable(&name, &scope);
                }
                return Err(error);
            }
            render_lifecycle(output, &installed, json, "Disabled")?;
        }
        PluginCommand::UpdateCheck {
            name,
            revision,
            json,
        } => render_update_review(output, &manager.check_update(&name, revision)?, json)?,
        PluginCommand::Update {
            name,
            revision,
            digest,
            yes,
            json,
        } => {
            if !yes {
                bail!("plugin update requires --yes after reviewing `xana plugin update-check`");
            }
            let installed = manager.update(&name, revision, &digest)?;
            render_lifecycle(output, &installed, json, "Updated")?;
            if installed.enabled_scopes.is_empty() {
                writeln!(
                    output,
                    "The update changed declared capabilities; review and enable the new revision explicitly."
                )?;
            }
        }
        PluginCommand::Rollback { name, yes, json } => {
            if !yes {
                bail!("plugin rollback requires --yes");
            }
            render_lifecycle(output, &manager.rollback(&name)?, json, "Rolled back")?;
        }
        PluginCommand::Remove { name, yes } => {
            if !yes {
                bail!("plugin removal requires --yes");
            }
            let references = referencing_profiles(paths, &name)?;
            if !references.is_empty() {
                bail!(
                    "plugin {name:?} is still referenced by {}; disable those profile scopes before removal",
                    references.join(", ")
                );
            }
            if manager.remove(&name)? {
                writeln!(
                    output,
                    "Removed {}. Workspace content was not changed.",
                    label(&name)
                )?;
            } else {
                writeln!(output, "Plugin {} was not installed.", label(&name))?;
            }
        }
        PluginCommand::Gc { yes } => {
            if !yes {
                bail!("plugin garbage collection requires --yes");
            }
            writeln!(
                output,
                "Removed {} unreferenced plugin version tree(s).",
                manager.garbage_collect()?
            )?;
        }
    }
    Ok(())
}

fn package_source(
    source: String,
    git: bool,
    revision: Option<String>,
    linked: bool,
) -> Result<PackageSource> {
    match (git, revision, linked) {
        (true, Some(revision), false) => Ok(PackageSource::Git {
            url: source,
            revision,
        }),
        (false, None, true) => Ok(PackageSource::Linked(PathBuf::from(source))),
        (false, None, false) => Ok(PackageSource::Directory(PathBuf::from(source))),
        _ => bail!(
            "Git plugin sources require both --git and --revision; linked sources use only --linked"
        ),
    }
}

fn render_review(output: &mut dyn Write, review: &PluginReview, json: bool) -> Result<()> {
    if json {
        writeln!(output, "{}", serde_json::to_string_pretty(review)?)?;
        return Ok(());
    }
    writeln!(
        output,
        "Agent Plugin review (spec {AGENT_PLUGIN_VERSION}, {AGENT_PLUGIN_STATUS})"
    )?;
    writeln!(output, "  Name:       {}", label(&review.manifest.name))?;
    writeln!(
        output,
        "  Version:    {}",
        review
            .manifest
            .version
            .as_deref()
            .map(label)
            .unwrap_or_else(|| "not declared".to_owned())
    )?;
    writeln!(output, "  Digest:     {}", review.digest)?;
    writeln!(
        output,
        "  Source:     {}",
        if review.mutable {
            "linked and mutable"
        } else {
            "immutable on install"
        }
    )?;
    writeln!(
        output,
        "  Skills:     {}",
        if review.skills.is_empty() {
            "none".to_owned()
        } else {
            review
                .skills
                .iter()
                .map(|item| label(item))
                .collect::<Vec<_>>()
                .join(", ")
        }
    )?;
    if review.mcp_servers.is_empty() {
        writeln!(output, "  MCP:        none compatible")?;
    } else {
        writeln!(output, "  MCP requests:")?;
        for server in &review.mcp_servers {
            writeln!(
                output,
                "    {} [{}] {}",
                label(&server.name),
                match server.kind {
                    McpServerKind::Stdio => "local process",
                    McpServerKind::StreamableHttp => "network",
                },
                label(&server.destination)
            )?;
        }
    }
    for diagnostic in &review.diagnostics {
        writeln!(output, "  Note: {}", label(diagnostic))?;
    }
    Ok(())
}

fn render_installed(output: &mut dyn Write, plugin: &InstalledPlugin) -> Result<()> {
    writeln!(
        output,
        "{} [{}; {}; {}]",
        label(&plugin.name),
        plugin.active_revision,
        if plugin.mutable {
            "linked mutable"
        } else {
            "immutable"
        },
        plugin.health
    )?;
    writeln!(
        output,
        "  Source: {:?} {}",
        plugin.source.kind,
        label(&plugin.source.location)
    )?;
    writeln!(
        output,
        "  Enabled scopes: {}",
        if plugin.enabled_scopes.is_empty() {
            "none".to_owned()
        } else {
            plugin.enabled_scopes.join(", ")
        }
    )?;
    writeln!(
        output,
        "  Rollback: {}",
        if plugin.rollback_available {
            "available"
        } else {
            "unavailable"
        }
    )?;
    if let Some(revision) = &plugin.pending_update {
        writeln!(output, "  Pending reviewed update: {revision}")?;
    }
    Ok(())
}

fn render_lifecycle(
    output: &mut dyn Write,
    plugin: &InstalledPlugin,
    json: bool,
    action: &str,
) -> Result<()> {
    if json {
        writeln!(output, "{}", serde_json::to_string_pretty(plugin)?)?;
    } else {
        writeln!(
            output,
            "{action} {} at {}.",
            label(&plugin.name),
            plugin.active_revision
        )?;
        render_installed(output, plugin)?;
    }
    Ok(())
}

fn render_update_review(
    output: &mut dyn Write,
    review: &PluginUpdateReview,
    json: bool,
) -> Result<()> {
    if json {
        writeln!(output, "{}", serde_json::to_string_pretty(review)?)?;
        return Ok(());
    }
    writeln!(output, "Plugin update review: {}", label(&review.name))?;
    writeln!(output, "  Current:   {}", review.current_revision)?;
    writeln!(output, "  Candidate: {}", review.candidate_revision)?;
    writeln!(output, "  Changed:   {}", review.changed)?;
    writeln!(
        output,
        "  Added skills: {}",
        list_or_none(&review.added_skills)
    )?;
    writeln!(
        output,
        "  Removed skills: {}",
        list_or_none(&review.removed_skills)
    )?;
    writeln!(
        output,
        "  Added MCP: {}",
        list_or_none(&review.added_mcp_servers)
    )?;
    writeln!(
        output,
        "  Removed MCP: {}",
        list_or_none(&review.removed_mcp_servers)
    )?;
    writeln!(
        output,
        "  Approval:  {}",
        if review.requires_reapproval {
            "required because capabilities changed"
        } else {
            "existing exact scopes may carry forward"
        }
    )?;
    Ok(())
}

fn plugin_scope(
    project: Option<crate::identity::ProjectId>,
    profile: Option<&str>,
) -> Result<PluginScope> {
    Ok(match (project, profile) {
        (None, None) => PluginScope::User,
        (Some(project), None) => PluginScope::Project(project),
        (project, Some(profile)) => PluginScope::Profile {
            project,
            profile: profile.to_owned(),
        },
    })
}

fn set_profile_plugin(
    paths: &XanaPaths,
    project: Option<crate::identity::ProjectId>,
    profile: &str,
    plugin: &str,
    enabled: bool,
) -> Result<()> {
    match project {
        Some(project) => {
            PortableProjectStore::open(paths)
                .set_profile_plugin(paths, project, profile, plugin, enabled)?;
        }
        None => XanaConfig::set_profile_plugin(paths.config_file(), profile, plugin, enabled)?,
    }
    Ok(())
}

fn referencing_profiles(paths: &XanaPaths, plugin: &str) -> Result<Vec<String>> {
    let mut references = ProfileStore::open(paths)
        .list_global(true)?
        .into_iter()
        .filter(|profile| profile.plugins.iter().any(|item| item == plugin))
        .map(|profile| format!("global profile {:?}", profile.id))
        .collect::<Vec<_>>();
    let portable = PortableProjectStore::open(paths);
    for project in ProjectStore::open(paths)?.list(true)? {
        if PortableProjectStore::detect(&project.canonical_workspace)?.is_none() {
            continue;
        }
        references.extend(
            portable
                .list_profiles(paths, project.id, true)?
                .into_iter()
                .filter(|(_, profile)| profile.plugins.iter().any(|item| item == plugin))
                .map(|(name, _)| format!("project {} profile {name:?}", project.id)),
        );
    }
    let document = read_document::<ProjectRegistryDocument>(&paths.projects_file())?;
    references.extend(document.conversation_profiles.into_iter().filter_map(
        |(conversation, snapshot)| {
            snapshot
                .resolved
                .get("plugin_revisions")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|revisions| revisions.contains_key(plugin))
                .then(|| format!("frozen conversation {conversation:?}"))
        },
    ));
    references.sort();
    Ok(references)
}

fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values
            .iter()
            .map(|value| label(value))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn label(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}
