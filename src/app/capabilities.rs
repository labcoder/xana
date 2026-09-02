//! Deterministic, offline capability reporting for every local frontend.

use crate::{
    cli::CapabilitiesArgs,
    command_catalog::{
        self, AuthorityRequirement, CommandContext, CommandSurface, PresentationCapabilities,
    },
    config::{ConnectionRegistry, PermissionMode, XanaConfig},
    paths::XanaPaths,
};
use anyhow::Result;
use serde::Serialize;
use std::{io::Write, path::Path};

const REPORT_VERSION: u16 = 1;

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilityReport {
    version: u16,
    platform: PlatformFacts,
    host: HostFacts,
    workspace: WorkspaceFacts,
    selection: SelectionFacts,
    integrations: IntegrationFacts,
    containment: ContainmentFacts,
    presentation: PresentationFacts,
    commands: Vec<CommandFact>,
    conversation_states: Vec<&'static str>,
    missing_setup: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct PlatformFacts {
    os: &'static str,
    architecture: &'static str,
}

#[derive(Debug, Serialize)]
struct HostFacts {
    location: &'static str,
    execution: &'static str,
    authority: &'static str,
}

#[derive(Debug, Serialize)]
struct WorkspaceFacts {
    path: String,
    permission: Option<&'static str>,
    permission_selected: bool,
}

#[derive(Debug, Default, Serialize)]
struct SelectionFacts {
    configuration: &'static str,
    profile: Option<String>,
    connection: Option<String>,
    model: Option<String>,
    credential_readiness: &'static str,
}

#[derive(Debug, Default, Serialize)]
struct IntegrationFacts {
    configured_connections: usize,
    selected_skills: Vec<String>,
    selected_plugins: Vec<String>,
    selected_mcp_servers: Vec<String>,
    selected_external_agents: Vec<String>,
    selected_focused_routes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ContainmentFacts {
    skills_and_plugins: &'static str,
    supervised_processes: &'static str,
    remote_integrations: &'static str,
    os_or_remote_isolation: &'static str,
}

#[derive(Debug, Serialize)]
struct PresentationFacts {
    plain: PresentationCapabilities,
    tui_baseline: PresentationCapabilities,
    desktop: PresentationCapabilities,
}

#[derive(Debug, Serialize)]
struct CommandFact {
    id: &'static str,
    family: &'static str,
    mode: &'static str,
    available: bool,
    availability: crate::command_catalog::AvailabilityCode,
    reason: Option<&'static str>,
    selected: bool,
    authorized: bool,
}

pub(super) fn run(args: CapabilitiesArgs, paths: &XanaPaths, output: &mut dyn Write) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file()).ok();
    let workspace = std::env::current_dir()
        .ok()
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let report = build_report(registry.as_ref(), &workspace);
    if args.json {
        serde_json::to_writer_pretty(&mut *output, &report)?;
        writeln!(output)?;
    } else {
        write_text(&report, output)?;
    }
    Ok(())
}

fn build_report(registry: Option<&ConnectionRegistry>, workspace: &Path) -> CapabilityReport {
    let configured = registry.is_some();
    let mut selection = SelectionFacts {
        configuration: if configured {
            "valid"
        } else {
            "missing_or_invalid"
        },
        credential_readiness: "not_probed",
        ..SelectionFacts::default()
    };
    let mut integrations = IntegrationFacts::default();
    let permission = registry.map(|registry| permission_label(registry.permission_mode));
    if let Some(registry) = registry {
        integrations.configured_connections = registry.connections.len();
        selection.profile = Some(registry.default_profile.clone());
        if let Some(profile) = registry.profiles.get(&registry.default_profile) {
            selection.connection = Some(profile.connection.clone());
            selection.model = Some(profile.model.clone());
            integrations.selected_skills = profile.skills.clone();
            integrations.selected_plugins = profile.plugins.clone();
            integrations.selected_mcp_servers = profile.mcp_servers.clone();
            integrations.selected_external_agents = profile.external_agents.clone();
            integrations.selected_focused_routes = profile.service_routes.clone();
        }
    }
    let context = CommandContext {
        surface: CommandSurface::Cli,
        authority: AuthorityRequirement::Owner,
        interactive: false,
        configured,
    };
    let commands = command_catalog::commands_for(CommandSurface::Cli)
        .map(|command| {
            let availability = command.availability(context);
            CommandFact {
                id: command.stable_id,
                family: command.name,
                mode: command.mode,
                available: availability.enabled,
                availability: availability.code,
                reason: availability.reason,
                selected: false,
                authorized: context.authority.rank() >= command.authority.rank(),
            }
        })
        .collect();
    CapabilityReport {
        version: command_catalog::COMMAND_CATALOG_VERSION.max(REPORT_VERSION),
        platform: PlatformFacts {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
        },
        host: HostFacts {
            location: "local_machine",
            execution: "application_owned",
            authority: "local_owner",
        },
        workspace: WorkspaceFacts {
            path: workspace.display().to_string(),
            permission,
            permission_selected: permission.is_some(),
        },
        selection,
        integrations,
        containment: ContainmentFacts {
            skills_and_plugins: "cooperative_consent",
            supervised_processes: "integration_specific",
            remote_integrations: "remote_boundary",
            os_or_remote_isolation: "not_inferred",
        },
        presentation: PresentationFacts {
            plain: crate::presentation::ResolvedPresentation::plain().plain_capabilities(),
            tui_baseline: crate::presentation::ResolvedPresentation {
                theme: crate::presentation::ResolvedTheme::Dark,
                color_depth: crate::presentation::ColorDepth::Ansi16,
                unicode: true,
                width: crate::presentation::WidthClass::Wide,
                reduced_motion: false,
            }
            .tui_capabilities(true, true, false),
            desktop: PresentationCapabilities::desktop(),
        },
        commands,
        conversation_states: command_catalog::ConversationDisplayState::all()
            .into_iter()
            .map(command_catalog::ConversationDisplayState::label)
            .collect(),
        missing_setup: if configured {
            Vec::new()
        } else {
            vec!["configuration"]
        },
    }
}

fn permission_label(permission: PermissionMode) -> &'static str {
    match permission {
        PermissionMode::Deny => "deny",
        PermissionMode::Ask => "ask",
        PermissionMode::Allow => "allow",
    }
}

fn write_text(report: &CapabilityReport, output: &mut dyn Write) -> std::io::Result<()> {
    writeln!(output, "Xana capabilities here")?;
    writeln!(
        output,
        "  Platform:     {} / {}",
        report.platform.os, report.platform.architecture
    )?;
    writeln!(
        output,
        "  Host:         {} / {}",
        report.host.location, report.host.execution
    )?;
    writeln!(output, "  Workspace:    {}", report.workspace.path)?;
    writeln!(
        output,
        "  Permission:   {} (selected policy; each action is still checked)",
        report.workspace.permission.unwrap_or("not configured")
    )?;
    writeln!(
        output,
        "  Profile:      {}",
        report
            .selection
            .profile
            .as_deref()
            .unwrap_or("not selected")
    )?;
    writeln!(
        output,
        "  Connection:   {}",
        report
            .selection
            .connection
            .as_deref()
            .unwrap_or("not selected")
    )?;
    writeln!(
        output,
        "  Model:        {}",
        report.selection.model.as_deref().unwrap_or("not selected")
    )?;
    writeln!(
        output,
        "  Extensions:   {} skill(s), {} plugin(s), {} MCP server(s), {} external agent(s), {} focused route(s)",
        report.integrations.selected_skills.len(),
        report.integrations.selected_plugins.len(),
        report.integrations.selected_mcp_servers.len(),
        report.integrations.selected_external_agents.len(),
        report.integrations.selected_focused_routes.len(),
    )?;
    if report.missing_setup.is_empty() {
        writeln!(
            output,
            "  Readiness:    configuration is valid; credentials and network were not probed"
        )?;
    } else {
        writeln!(output, "  Readiness:    run `xana setup`")?;
    }
    writeln!(
        output,
        "\nAvailability is not permission or selection. Use `xana capabilities --json` for stable command and presentation facts."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection},
        shell::{ShellConfig, ShellKind},
    };

    fn configured_registry() -> ConnectionRegistry {
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".to_owned(),
                base_url: "http://127.0.0.1:11434/v1".to_owned(),
            },
            model: "qwen".to_owned(),
            max_tool_rounds: 8,
            reasoning_effort: None,
            shell: ShellConfig {
                kind: ShellKind::Platform,
                program: None,
            },
            permission_mode: PermissionMode::Ask,
        })
        .unwrap();
        XanaConfig::parse_registry(&rendered).unwrap()
    }

    #[test]
    fn missing_setup_is_reported_without_network_or_failure() {
        let report = build_report(None, Path::new("workspace"));
        assert_eq!(report.selection.configuration, "missing_or_invalid");
        assert_eq!(report.missing_setup, ["configuration"]);
        assert!(report.commands.iter().any(|command| {
            command.id == "setup.run.v1" && command.available && command.authorized
        }));
    }

    #[test]
    fn configured_report_keeps_selection_and_availability_separate() {
        let registry = configured_registry();
        let report = build_report(Some(&registry), Path::new("workspace"));
        assert_eq!(report.selection.connection.as_deref(), Some("local"));
        assert_eq!(report.selection.model.as_deref(), Some("qwen"));
        assert_eq!(report.workspace.permission, Some("ask"));
        assert!(report.commands.iter().all(|command| !command.selected));
    }

    #[test]
    fn report_json_is_stable_and_contains_no_credentials() {
        let report = build_report(Some(&configured_registry()), Path::new("workspace"));
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"credential_readiness\":\"not_probed\""));
        assert!(!json.contains("api_key"));
    }
}
