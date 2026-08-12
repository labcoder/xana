//! CLI adapter for inert Agent Plugin review and lifecycle operations.

use crate::{
    cli::PluginCommand,
    paths::XanaPaths,
    plugin::{
        AGENT_PLUGIN_STATUS, AGENT_PLUGIN_VERSION, InstalledPlugin, McpServerKind, PackageSource,
        PluginManager, PluginReview,
    },
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
        PluginCommand::Inspect {
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
        if plugin.enabled_scopes.is_empty() {
            "disabled"
        } else {
            "enabled"
        }
    )?;
    Ok(())
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
