//! Typed application commands for MCP configuration and explicit primitive use.

use crate::{
    cli::McpCommand,
    config::{
        ConnectionRegistry, McpPrimitiveSelection, McpServerDeclaration, OutboundDataClass,
        XanaConfig,
    },
    credential::CredentialResolver,
    mcp::{
        McpApplication, McpArgument, McpEnvironmentValue, McpGuardedTransport, McpHttpClient,
        McpHttpConnection, McpHttpEndpoint, McpHttpSecurity, McpPrimitiveAllowlist,
        McpProcessConfig, McpServerExposure, McpStdioClient, mcp_http_recipient,
    },
    outbound::{OutboundGuard, OutboundPolicyLayers, RecipientIdentity, RecipientKind},
    paths::XanaPaths,
};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, io::Write, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

const DISPLAY_LIMIT: usize = 64;

pub(super) async fn run(
    command: McpCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load MCP configuration")?;
    if matches!(command, McpCommand::List) {
        return list_configured(&registry, output);
    }
    let server = command_server(&command).expect("non-list MCP command has a server");
    let workspace = std::env::current_dir()
        .context("could not resolve the MCP workspace")?
        .canonicalize()
        .context("could not canonicalize the MCP workspace")?;
    let profile = registry
        .profiles
        .get(&registry.default_profile)
        .context("default profile is missing")?;
    let profile_egress = resolve_profile_egress(&registry, profile.egress_policy.as_deref());
    let application = build_application(
        &registry,
        paths,
        &workspace,
        &profile.mcp_servers,
        &profile.mcp_allowlists,
        &profile_egress,
    )
    .await?;
    let cancellation = CancellationToken::new();
    application
        .refresh(server, &cancellation)
        .await
        .with_context(|| format!("could not refresh MCP server {server:?}"))?;

    match command {
        McpCommand::Refresh { server } => {
            writeln!(
                output,
                "MCP server {server:?} is ready; bounded catalog refreshed."
            )?;
        }
        McpCommand::Tools { query, .. } => {
            for tool in application.tools(&query, DISPLAY_LIMIT).await {
                writeln!(
                    output,
                    "{}\t{}\t{}",
                    tool.qualified_name, tool.server, tool.description_preview
                )?;
            }
        }
        McpCommand::Resources { query, .. } => {
            for resource in application.resources(&query, DISPLAY_LIMIT).await {
                writeln!(
                    output,
                    "{}\t{}\t{}",
                    resource.server, resource.uri, resource.name
                )?;
            }
        }
        McpCommand::Read { server, uri } => {
            let document = application
                .read_resource(&server, &uri, &cancellation)
                .await?;
            serde_json::to_writer_pretty(&mut *output, &document)?;
            writeln!(output)?;
        }
        McpCommand::Prompts { query, .. } => {
            for prompt in application.prompts(&query, DISPLAY_LIMIT).await {
                writeln!(
                    output,
                    "{}\t{}\t{}",
                    prompt.server,
                    prompt.source_name,
                    prompt.description_preview.as_deref().unwrap_or("")
                )?;
            }
        }
        McpCommand::Prompt {
            server,
            name,
            arguments,
        } => {
            let arguments = parse_arguments(arguments)?;
            let preview = application
                .invoke_prompt(&server, &name, arguments, &cancellation)
                .await?;
            serde_json::to_writer_pretty(&mut *output, &preview)?;
            writeln!(output)?;
        }
        McpCommand::List => unreachable!("handled before refresh"),
    }
    Ok(())
}

pub(super) async fn activate_profile_tools(
    registry: &ConnectionRegistry,
    paths: &XanaPaths,
    workspace: &Path,
    servers: &[String],
    allowlists: &BTreeMap<String, McpPrimitiveSelection>,
    profile_egress: &[OutboundDataClass],
    tools: &mut crate::tool::ToolRegistry,
) -> Result<usize> {
    if servers.is_empty() {
        return Ok(0);
    }
    let application = build_application(
        registry,
        paths,
        workspace,
        servers,
        allowlists,
        profile_egress,
    )
    .await?;
    let cancellation = CancellationToken::new();
    for server in servers {
        application
            .refresh(server, &cancellation)
            .await
            .with_context(|| format!("profile MCP server {server:?} is not ready"))?;
    }
    application
        .activate_tools(tools, &cancellation)
        .await
        .context("could not activate profile MCP tools")
}

fn list_configured(registry: &ConnectionRegistry, output: &mut dyn Write) -> Result<()> {
    let profile = registry
        .profiles
        .get(&registry.default_profile)
        .context("default profile is missing")?;
    if registry.mcp_servers.is_empty() {
        writeln!(output, "No MCP servers are configured.")?;
        return Ok(());
    }
    for (name, declaration) in &registry.mcp_servers {
        let selected = profile.mcp_servers.contains(name);
        let allowlist = profile.mcp_allowlists.get(name);
        let primitive_count = allowlist.map_or(0, |selection| {
            selection.tools.len()
                + selection.resources.len()
                + selection.resource_templates.len()
                + selection.prompts.len()
        });
        let (transport, enabled) = match declaration {
            McpServerDeclaration::Stdio { enabled, .. } => ("stdio", *enabled),
            McpServerDeclaration::StreamableHttp { enabled, .. } => ("streamable_http", *enabled),
        };
        writeln!(
            output,
            "{name}\t{transport}\t{}\t{}\t{primitive_count} primitive(s)",
            if enabled { "enabled" } else { "disabled" },
            if selected { "selected" } else { "not selected" }
        )?;
    }
    Ok(())
}

async fn build_application(
    registry: &ConnectionRegistry,
    paths: &XanaPaths,
    workspace: &Path,
    selected_servers: &[String],
    allowlists: &BTreeMap<String, McpPrimitiveSelection>,
    profile_egress: &[OutboundDataClass],
) -> Result<McpApplication> {
    let mut application = McpApplication::new(
        crate::tool::BUILTIN_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned()),
    );
    let credentials = CredentialResolver::default();
    for server in selected_servers {
        let declaration = registry
            .mcp_servers
            .get(server)
            .with_context(|| format!("profile references missing MCP server {server:?}"))?;
        let selection = allowlists.get(server).cloned().unwrap_or_default();
        let digest = configured_identity_digest(declaration)?;
        let exposure = McpServerExposure {
            server: server.clone(),
            configured_identity_digest: digest,
            enabled: declaration_enabled(declaration),
            profile_selected: true,
            allowlist: allowlist(selection),
        };
        let (transport, recipient): (
            Arc<dyn crate::mcp::McpPrimitiveTransport>,
            RecipientIdentity,
        ) = match declaration {
            McpServerDeclaration::Stdio {
                command,
                args,
                environment,
                cwd,
                ..
            } => {
                let mut config =
                    McpProcessConfig::new(command, cwd.as_deref().unwrap_or(workspace));
                config.arguments = args.iter().cloned().map(McpArgument::visible).collect();
                config.environment = environment
                    .iter()
                    .map(|(name, value)| (name.clone(), McpEnvironmentValue::new(value.clone())))
                    .collect();
                let transport = Arc::new(McpStdioClient::spawn(config).await?) as Arc<_>;
                let recipient = RecipientIdentity::new(
                    RecipientKind::McpStdio,
                    server,
                    command,
                    serde_json::to_vec(declaration)?.as_slice(),
                )?;
                (transport, recipient)
            }
            McpServerDeclaration::StreamableHttp {
                url, credential, ..
            } => {
                let endpoint = McpHttpEndpoint::parse(
                    url,
                    McpHttpSecurity {
                        allow_loopback_http: true,
                    },
                )?;
                let client = McpHttpClient::connect(endpoint).await?;
                let bearer = credential
                    .as_ref()
                    .map(|reference| credentials.resolve(reference))
                    .transpose()?;
                let recipient = mcp_http_recipient(server, client.endpoint(), None, None)?;
                (
                    Arc::new(McpHttpConnection::new(client, bearer)) as Arc<_>,
                    recipient,
                )
            }
        };
        let connection_allowed = resolve_server_egress(registry, declaration);
        let all = OutboundDataClass::ALL.into_iter().collect();
        let policy = OutboundPolicyLayers {
            connection_allowed: connection_allowed.into_iter().collect(),
            user_ceiling: all,
            profile_allowed: profile_egress.iter().copied().collect(),
            conversation_allowed: None,
        };
        let transport = Arc::new(McpGuardedTransport::new(
            transport,
            OutboundGuard::open(paths)?,
            recipient,
            policy,
        ));
        application.add_server(exposure, transport)?;
    }
    Ok(application)
}

fn resolve_profile_egress(
    registry: &ConnectionRegistry,
    policy: Option<&str>,
) -> Vec<OutboundDataClass> {
    policy
        .and_then(|policy| registry.egress_policies.get(policy))
        .map(|policy| policy.allowed.clone())
        .unwrap_or_default()
}

fn resolve_server_egress(
    registry: &ConnectionRegistry,
    declaration: &McpServerDeclaration,
) -> Vec<OutboundDataClass> {
    declaration
        .egress_policy()
        .and_then(|policy| registry.egress_policies.get(policy))
        .map(|policy| policy.allowed.clone())
        .unwrap_or_default()
}

fn declaration_enabled(declaration: &McpServerDeclaration) -> bool {
    match declaration {
        McpServerDeclaration::Stdio { enabled, .. }
        | McpServerDeclaration::StreamableHttp { enabled, .. } => *enabled,
    }
}

fn allowlist(selection: McpPrimitiveSelection) -> McpPrimitiveAllowlist {
    McpPrimitiveAllowlist {
        tools: selection.tools.into_iter().collect(),
        resources: selection.resources.into_iter().collect(),
        resource_templates: selection.resource_templates.into_iter().collect(),
        prompts: selection.prompts.into_iter().collect(),
    }
}

fn configured_identity_digest(declaration: &McpServerDeclaration) -> Result<String> {
    let encoded = serde_json::to_vec(declaration).context("could not encode MCP identity")?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn command_server(command: &McpCommand) -> Option<&str> {
    match command {
        McpCommand::Refresh { server }
        | McpCommand::Tools { server, .. }
        | McpCommand::Resources { server, .. }
        | McpCommand::Read { server, .. }
        | McpCommand::Prompts { server, .. }
        | McpCommand::Prompt { server, .. } => Some(server),
        McpCommand::List => None,
    }
}

fn parse_arguments(arguments: Vec<String>) -> Result<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();
    for argument in arguments {
        let (name, value) = argument
            .split_once('=')
            .context("MCP prompt arguments must use KEY=VALUE")?;
        if name.is_empty() || parsed.insert(name.to_owned(), value.to_owned()).is_some() {
            anyhow::bail!("MCP prompt argument names must be non-empty and unique");
        }
    }
    Ok(parsed)
}
