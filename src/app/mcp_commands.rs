//! Typed application commands for MCP configuration and explicit primitive use.

use crate::{
    cli::McpCommand,
    config::{
        ConnectionRegistry, CredentialReference, McpPrimitiveSelection, McpServerDeclaration,
        NewMcpServer, OutboundDataClass, XanaConfig,
    },
    credential::CredentialResolver,
    identity::OperationId,
    mcp::{
        McpApplication, McpApplicationError, McpArgument, McpEnvironmentValue, McpGuardedTransport,
        McpHttpClient, McpHttpConnection, McpHttpEndpoint, McpHttpSecurity, McpLocalServer,
        McpPrimitiveAllowlist, McpPrimitiveTransport, McpProcessConfig, McpServerExposure,
        McpStdioClient, McpTransportResponse, mcp_http_recipient,
    },
    outbound::{OutboundGuard, OutboundPolicyLayers, RecipientIdentity, RecipientKind},
    paths::XanaPaths,
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const DISPLAY_LIMIT: usize = 64;

#[derive(Clone)]
enum DeferredMcpTransportSpec {
    Stdio(McpProcessConfig),
    Http {
        endpoint: McpHttpEndpoint,
        credential: Option<CredentialReference>,
    },
}

struct DeferredMcpTransport {
    spec: DeferredMcpTransportSpec,
    active: Mutex<Option<Arc<dyn McpPrimitiveTransport>>>,
}

impl DeferredMcpTransport {
    fn new(spec: DeferredMcpTransportSpec) -> Self {
        Self {
            spec,
            active: Mutex::new(None),
        }
    }

    async fn active(&self) -> Result<Arc<dyn McpPrimitiveTransport>, McpApplicationError> {
        let mut active = self.active.lock().await;
        if let Some(transport) = active.as_ref() {
            return Ok(Arc::clone(transport));
        }
        let transport: Arc<dyn McpPrimitiveTransport> = match &self.spec {
            DeferredMcpTransportSpec::Stdio(config) => Arc::new(
                McpStdioClient::spawn(config.clone())
                    .await
                    .map_err(|error| McpApplicationError::Transport(error.to_string()))?,
            ),
            DeferredMcpTransportSpec::Http {
                endpoint,
                credential,
            } => {
                let bearer = credential
                    .as_ref()
                    .map(|reference| CredentialResolver::default().resolve(reference))
                    .transpose()
                    .map_err(|error| McpApplicationError::Transport(error.to_string()))?;
                let client = McpHttpClient::connect(endpoint.clone())
                    .await
                    .map_err(|error| McpApplicationError::Transport(error.to_string()))?;
                Arc::new(McpHttpConnection::new(client, bearer))
            }
        };
        *active = Some(Arc::clone(&transport));
        Ok(transport)
    }
}

impl McpPrimitiveTransport for DeferredMcpTransport {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: serde_json::Value,
        tool_schema: Option<serde_json::Value>,
        operation_id: OperationId,
        cancellation: &'a CancellationToken,
    ) -> futures::future::BoxFuture<
        'a,
        std::result::Result<McpTransportResponse, McpApplicationError>,
    > {
        Box::pin(async move {
            self.active()
                .await?
                .request(method, params, tool_schema, operation_id, cancellation)
                .await
        })
    }
}

pub(super) async fn run(
    command: McpCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load MCP configuration")?;
    match command {
        McpCommand::List => return list_configured(&registry, output),
        McpCommand::AddStdio {
            server,
            command,
            arguments,
            cwd,
            profile,
            tools,
            resources,
            prompts,
            yes,
        } => {
            if !yes {
                anyhow::bail!(
                    "MCP configuration requires --yes after reviewing the exact command, arguments, profile, and allowlist"
                )
            }
            let profile = profile.unwrap_or_else(|| registry.default_profile.clone());
            XanaConfig::add_mcp_server(
                paths.config_file(),
                NewMcpServer {
                    id: server.clone(),
                    declaration: McpServerDeclaration::Stdio {
                        command,
                        args: arguments,
                        environment: BTreeMap::new(),
                        cwd,
                        enabled: true,
                        egress_policy: None,
                    },
                    profile: profile.clone(),
                    selection: McpPrimitiveSelection {
                        tools,
                        resources,
                        resource_templates: Vec::new(),
                        prompts,
                    },
                },
            )?;
            writeln!(
                output,
                "MCP stdio server {server:?} added and enabled for profile {profile:?}."
            )?;
            writeln!(
                output,
                "Backup: {}",
                paths.config_file().with_extension("toml.bak").display()
            )?;
            writeln!(output, "Next: xana mcp refresh {server}")?;
            return Ok(());
        }
        McpCommand::AddHttp {
            server,
            url,
            credential_env,
            profile,
            tools,
            resources,
            prompts,
            yes,
        } => {
            if !yes {
                anyhow::bail!(
                    "MCP configuration requires --yes after reviewing the exact endpoint, profile, credential reference, and allowlist"
                )
            }
            let profile = profile.unwrap_or_else(|| registry.default_profile.clone());
            XanaConfig::add_mcp_server(
                paths.config_file(),
                NewMcpServer {
                    id: server.clone(),
                    declaration: McpServerDeclaration::StreamableHttp {
                        url,
                        credential: credential_env
                            .map(|variable| CredentialReference::Environment { variable }),
                        enabled: true,
                        egress_policy: None,
                    },
                    profile: profile.clone(),
                    selection: McpPrimitiveSelection {
                        tools,
                        resources,
                        resource_templates: Vec::new(),
                        prompts,
                    },
                },
            )?;
            writeln!(
                output,
                "MCP HTTP server {server:?} added and enabled for profile {profile:?}."
            )?;
            writeln!(
                output,
                "Backup: {}",
                paths.config_file().with_extension("toml.bak").display()
            )?;
            writeln!(output, "Next: xana mcp refresh {server}")?;
            return Ok(());
        }
        McpCommand::Remove { server, yes } => {
            if !yes {
                anyhow::bail!(
                    "MCP removal requires --yes and removes only the declaration and profile references"
                )
            }
            XanaConfig::remove_mcp_server(paths.config_file(), &server)?;
            writeln!(
                output,
                "MCP server {server:?} and its profile references were removed."
            )?;
            writeln!(
                output,
                "Backup: {}",
                paths.config_file().with_extension("toml.bak").display()
            )?;
            return Ok(());
        }
        _ => {}
    }
    let server = command_server(&command).expect("non-list MCP command has a server");
    let saves_metadata_approval = matches!(command, McpCommand::Refresh { .. });
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
        Some(if saves_metadata_approval {
            crate::outbound::OutboundApprovalDecision::SaveAllow
        } else {
            crate::outbound::OutboundApprovalDecision::AllowOnce
        }),
    )
    .await?;
    let cancellation = CancellationToken::new();
    if saves_metadata_approval {
        let review = application
            .outbound_review(
                server,
                "server/discover",
                &serde_json::json!({}),
                OperationId::new(),
            )?
            .context("MCP server has no outbound review contract")?;
        writeln!(output, "{}", review.render())?;
        writeln!(
            output,
            "This explicit refresh saves the exact recipient/workspace_metadata grant; revoke it with `xana outbound revoke`."
        )?;
    }
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
        McpCommand::Serve { .. } => {
            anyhow::bail!("`mcp serve` must be started from Xana's top-level command")
        }
        McpCommand::List
        | McpCommand::AddStdio { .. }
        | McpCommand::AddHttp { .. }
        | McpCommand::Remove { .. } => unreachable!("handled before refresh"),
    }
    Ok(())
}

pub(super) async fn serve(
    paths: &XanaPaths,
    workspace: PathBuf,
    profile_name: String,
    allow: Vec<String>,
) -> Result<()> {
    let workspace = workspace.canonicalize().with_context(|| {
        format!(
            "could not canonicalize MCP workspace {}",
            workspace.display()
        )
    })?;
    if !workspace.is_dir() {
        anyhow::bail!("MCP workspace must be an existing directory");
    }
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load MCP server configuration")?;
    let profile = registry
        .profiles
        .get(&profile_name)
        .with_context(|| format!("unknown MCP server profile {profile_name:?}"))?;
    if profile.archived {
        anyhow::bail!("MCP server profile {profile_name:?} is archived");
    }
    let effective_permission = profile.permission_mode.unwrap_or(registry.permission_mode);
    if effective_permission != crate::config::PermissionMode::Allow {
        anyhow::bail!(
            "MCP server profile {profile_name:?} must use permission_mode = \"allow\"; noninteractive ask/deny policy cannot authorize calls"
        );
    }

    let allowed = allow.into_iter().collect::<std::collections::BTreeSet<_>>();
    if allowed.is_empty() || allowed.iter().any(|name| name != "xana_docs") {
        anyhow::bail!("the bounded local server currently supports only exact `--allow xana_docs`");
    }
    if profile
        .capabilities
        .as_ref()
        .is_some_and(|capabilities| !capabilities.iter().any(|value| value == "xana.docs.read"))
    {
        anyhow::bail!(
            "profile {profile_name:?} does not select the xana.docs.read capability required by xana_docs"
        );
    }
    let shell = crate::shell::Shell::resolve(crate::shell::ShellConfig::default())
        .context("could not resolve the platform shell")?;
    let tools = crate::tool::ToolRegistry::builtins_from_names(shell, &allowed)
        .context("could not compose the local MCP tool allowlist")?;
    let policy = crate::permission::PermissionPolicy::new(
        effective_permission.into(),
        registry.permission_rules.clone(),
        &workspace,
    )
    .context("could not resolve the local MCP permission policy")?;
    McpLocalServer::new(workspace, profile_name, tools, policy)
        .run_stdio()
        .await
        .context("local MCP server stopped with an error")
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
        None,
    )
    .await?;
    let cancellation = CancellationToken::new();
    for server in servers {
        if !matches!(
            application.outbound_disposition(server, "server/discover", &serde_json::json!({}),)?,
            Some(crate::outbound::OutboundDisposition::SavedAllow)
        ) {
            continue;
        }
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
    default_decision: Option<crate::outbound::OutboundApprovalDecision>,
) -> Result<McpApplication> {
    let mut application = McpApplication::new(
        crate::tool::BUILTIN_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned()),
    );
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
                let transport = Arc::new(DeferredMcpTransport::new(
                    DeferredMcpTransportSpec::Stdio(config),
                )) as Arc<_>;
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
                let recipient = mcp_http_recipient(server, &endpoint, None, None)?;
                (
                    Arc::new(DeferredMcpTransport::new(DeferredMcpTransportSpec::Http {
                        endpoint,
                        credential: credential.clone(),
                    })) as Arc<_>,
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
        let transport = Arc::new(
            McpGuardedTransport::new(
                transport,
                OutboundGuard::open(paths)?,
                recipient,
                policy,
                default_decision,
            )
            .with_outbound_audit(crate::diagnostics::outbound_audit(paths)?),
        );
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
        McpCommand::List
        | McpCommand::Serve { .. }
        | McpCommand::AddStdio { .. }
        | McpCommand::AddHttp { .. }
        | McpCommand::Remove { .. } => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn application_construction_does_not_spawn_stdio_before_guarded_request() {
        let registry = XanaConfig::parse_registry(
            r#"
version = 4
default_profile = "default"
permission_mode = "ask"

[providers.local]
kind = "ollama"

[profiles.default]
connection = "local"
model = "qwen"
mcp_servers = ["lazy"]
egress_policy = "mcp"

[profiles.default.mcp_allowlists.lazy]
tools = ["review"]

[mcp_servers.lazy]
transport = "stdio"
command = "xana-command-that-must-not-exist"
enabled = true
egress_policy = "mcp"

[egress_policies.mcp]
allowed = ["prompt_text", "workspace_metadata"]
"#,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        let profile = &registry.profiles["default"];

        let application = build_application(
            &registry,
            &paths,
            directory.path(),
            &["lazy".to_owned()],
            &profile.mcp_allowlists,
            &registry.egress_policies["mcp"].allowed,
            None,
        )
        .await;

        assert!(application.is_ok(), "construction must remain effect free");
    }

    #[tokio::test]
    async fn profile_activation_skips_unapproved_mcp_recipient_without_spawning() {
        let registry = XanaConfig::parse_registry(
            r#"
version = 4
default_profile = "default"
permission_mode = "ask"

[providers.local]
kind = "ollama"

[profiles.default]
connection = "local"
model = "qwen"
mcp_servers = ["lazy"]
egress_policy = "mcp"

[profiles.default.mcp_allowlists.lazy]
tools = ["review"]

[mcp_servers.lazy]
transport = "stdio"
command = "xana-command-that-must-not-exist"
enabled = true
egress_policy = "mcp"

[egress_policies.mcp]
allowed = ["prompt_text", "workspace_metadata"]
"#,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        let profile = &registry.profiles["default"];
        let mut tools = crate::tool::ToolRegistry::new();

        let activated = activate_profile_tools(
            &registry,
            &paths,
            directory.path(),
            &["lazy".to_owned()],
            &profile.mcp_allowlists,
            &registry.egress_policies["mcp"].allowed,
            &mut tools,
        )
        .await
        .unwrap();

        assert_eq!(activated, 0);
        assert!(tools.definitions().is_empty());
    }
}
