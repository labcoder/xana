//! CLI adapter for bounded Agent Skills discovery and activation.

use crate::{
    cli::SkillCommand,
    config::XanaConfig,
    paths::XanaPaths,
    portable_project::PortableProjectStore,
    project::ProjectStore,
    skill::{ActivatedSkill, SkillCatalog, SkillEntry, SkillSource, standard_sources},
};
use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::Serialize;
use std::{env, io::Write, path::Path};

pub(super) fn run_command(
    command: SkillCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    match command {
        SkillCommand::List { workspace, json } => {
            let catalog = catalog(&workspace_path(workspace)?, std::iter::empty())?;
            if json {
                #[derive(Serialize)]
                struct Listing<'a> {
                    skills: Vec<&'a SkillEntry>,
                    diagnostics: &'a [crate::skill::SkillDiagnostic],
                }
                writeln!(
                    output,
                    "{}",
                    serde_json::to_string_pretty(&Listing {
                        skills: catalog.entries().collect(),
                        diagnostics: catalog.diagnostics(),
                    })?
                )?;
            } else {
                for entry in catalog.entries() {
                    render_entry(output, entry)?;
                }
                for diagnostic in catalog.diagnostics() {
                    writeln!(
                        output,
                        "{} [{}; invalid]",
                        terminal_label(&diagnostic.qualified_name),
                        terminal_label(&diagnostic.source)
                    )?;
                    writeln!(output, "  {}", terminal_label(&diagnostic.reason))?;
                }
            }
        }
        SkillCommand::Inspect {
            skill,
            workspace,
            json,
        } => {
            let catalog = catalog(&workspace_path(workspace)?, std::iter::empty())?;
            let entry = catalog.resolve(&skill)?;
            if json {
                writeln!(output, "{}", serde_json::to_string_pretty(entry)?)?;
            } else {
                render_entry(output, entry)?;
                writeln!(output, "Allowed tools: ignored as authority by Xana")?;
            }
        }
        SkillCommand::Validate { path } => {
            let skill_path = if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md")
            {
                path.parent()
                    .context("SKILL.md path has no skill directory")?
                    .to_owned()
            } else {
                path
            };
            let root = skill_path
                .parent()
                .context("skill directory has no source root")?;
            let source = SkillSource::user(root.to_owned());
            let catalog = SkillCatalog::discover(vec![source])?;
            let directory_name = skill_path
                .file_name()
                .and_then(|name| name.to_str())
                .context("skill directory name is not UTF-8")?;
            let activated = catalog.activate(directory_name)?;
            writeln!(
                output,
                "Valid Agent Skill: {} (digest {}, {} referenced resource(s)); no code was executed.",
                activated.entry.qualified_name,
                activated.digest,
                activated.references.len()
            )?;
        }
        SkillCommand::Activate {
            skill,
            workspace,
            json,
        } => {
            let catalog = catalog(&workspace_path(workspace)?, std::iter::empty())?;
            let activated = catalog.activate(&skill)?;
            if json {
                writeln!(
                    output,
                    "{}",
                    serde_json::to_string_pretty(&activation_view(&activated))?
                )?;
            } else {
                writeln!(
                    output,
                    "Activated {} (digest {}, {} instruction bytes, {} referenced resource(s)).",
                    activated.entry.qualified_name,
                    activated.digest,
                    activated.body.len(),
                    activated.references.len()
                )?;
                writeln!(
                    output,
                    "No tool, permission, egress, or execution authority was granted."
                )?;
            }
        }
        SkillCommand::Read {
            skill,
            resource,
            workspace,
            json,
        } => {
            let catalog = catalog(&workspace_path(workspace)?, std::iter::empty())?;
            let content = catalog.read_resource(&skill, &resource)?;
            if json {
                #[derive(Serialize)]
                struct Resource<'a> {
                    skill: &'a str,
                    resource: &'a Path,
                    content: &'a str,
                }
                writeln!(
                    output,
                    "{}",
                    serde_json::to_string_pretty(&Resource {
                        skill: &skill,
                        resource: &resource,
                        content: &content,
                    })?
                )?;
            } else {
                writeln!(output, "{}", terminal_safe(&content))?;
            }
        }
        SkillCommand::Enable {
            skill,
            profile,
            project,
        } => {
            let workspace = project_workspace(paths, project)?;
            let catalog = catalog(&workspace, std::iter::empty())?;
            let qualified = catalog.activate(&skill)?.entry.qualified_name;
            set_profile_skill(paths, project, &profile, &qualified, true)?;
            writeln!(
                output,
                "Enabled {qualified} for profile {profile:?}; it activates in new conversations and grants no authority."
            )?;
        }
        SkillCommand::Disable {
            skill,
            profile,
            project,
        } => {
            let workspace = project_workspace(paths, project)?;
            let catalog = catalog(&workspace, std::iter::empty())?;
            let qualified = catalog.resolve(&skill)?.qualified_name.clone();
            set_profile_skill(paths, project, &profile, &qualified, false)?;
            writeln!(
                output,
                "Disabled {qualified} for profile {profile:?}; existing frozen conversations are unchanged."
            )?;
        }
    }
    Ok(())
}

pub(super) fn catalog(
    workspace: &Path,
    plugins: impl IntoIterator<Item = SkillSource>,
) -> Result<SkillCatalog> {
    let home = BaseDirs::new().map(|directories| directories.home_dir().to_owned());
    SkillCatalog::discover(standard_sources(home.as_deref(), workspace, plugins))
        .context("could not discover Agent Skills")
}

fn workspace_path(workspace: Option<std::path::PathBuf>) -> Result<std::path::PathBuf> {
    workspace
        .map(Ok)
        .unwrap_or_else(env::current_dir)?
        .canonicalize()
        .context("could not canonicalize skill workspace")
}

fn project_workspace(
    paths: &XanaPaths,
    project: Option<crate::identity::ProjectId>,
) -> Result<std::path::PathBuf> {
    match project {
        Some(project) => Ok(ProjectStore::open(paths)?.get(project)?.canonical_workspace),
        None => workspace_path(None),
    }
}

fn set_profile_skill(
    paths: &XanaPaths,
    project: Option<crate::identity::ProjectId>,
    profile: &str,
    skill: &str,
    enabled: bool,
) -> Result<()> {
    match project {
        Some(project) => {
            PortableProjectStore::open(paths)
                .set_profile_skill(paths, project, profile, skill, enabled)?;
        }
        None => XanaConfig::set_profile_skill(paths.config_file(), profile, skill, enabled)?,
    }
    Ok(())
}

fn render_entry(output: &mut dyn Write, entry: &SkillEntry) -> Result<()> {
    writeln!(
        output,
        "{} [{}; {}; compatible]",
        terminal_label(&entry.qualified_name),
        terminal_label(&entry.source),
        if entry.mutable {
            "mutable"
        } else {
            "immutable"
        }
    )?;
    writeln!(output, "  {}", terminal_label(&entry.metadata.description))?;
    Ok(())
}

#[derive(Serialize)]
struct ActivationView<'a> {
    qualified_name: &'a str,
    digest: &'a str,
    instruction_bytes: usize,
    references: Vec<&'a Path>,
    authority_granted: bool,
}

fn activation_view(activated: &ActivatedSkill) -> ActivationView<'_> {
    ActivationView {
        qualified_name: &activated.entry.qualified_name,
        digest: &activated.digest,
        instruction_bytes: activated.body.len(),
        references: activated
            .references
            .keys()
            .map(|path| path.as_path())
            .collect(),
        authority_granted: false,
    }
}

fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character == '\n' || character == '\t' || !character.is_control() {
                character
            } else {
                '\u{fffd}'
            }
        })
        .collect()
}

fn terminal_label(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}
