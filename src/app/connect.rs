//! Provider-neutral integration hub and focused-service setup transaction.

use crate::{
    cli::{ConnectArgs, ConnectIntegration, FocusedServiceProviderChoice},
    config::{CredentialReference, NewServiceRoute, XanaConfig},
    paths::XanaPaths,
    plugin::PluginManager,
    private_state::{ExternalAgentStateDocument, read_document},
};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, Write};

const MAX_ATTEMPTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FocusedServiceKind {
    Image,
    Vision,
}

impl FocusedServiceKind {
    fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Vision => "vision",
        }
    }

    fn operation(self) -> &'static str {
        match self {
            Self::Image => "image.generate",
            Self::Vision => "vision.analyze",
        }
    }

    fn default_route(self) -> &'static str {
        match self {
            Self::Image => "illustrate",
            Self::Vision => "describe",
        }
    }

    fn adapter(self, provider: FocusedServiceProviderChoice) -> &'static str {
        match (self, provider) {
            (Self::Image, FocusedServiceProviderChoice::OpenAi) => "openai.images",
            (Self::Image, FocusedServiceProviderChoice::OpenRouter) => "openrouter.images",
            (Self::Vision, FocusedServiceProviderChoice::OpenAi) => "openai.vision",
            (Self::Vision, FocusedServiceProviderChoice::OpenRouter) => "openrouter.vision",
        }
    }
}

pub(super) fn write_hub(
    args: &ConnectArgs,
    integration: Option<ConnectIntegration>,
    paths: &XanaPaths,
    output: &mut impl Write,
) -> Result<()> {
    if has_focused_options(args) {
        bail!(
            "focused-service setup options require `xana connect image` or `xana connect vision`"
        );
    }
    writeln!(
        output,
        "Xana integration hub (read-only until you choose an exact command)"
    )?;
    writeln!(output, "  provider        xana connect provider")?;
    writeln!(output, "  profile         xana connect profile")?;
    writeln!(
        output,
        "  plugin          xana plugin list | review | install | enable"
    )?;
    writeln!(
        output,
        "  MCP             xana mcp list | add-stdio | add-http | remove"
    )?;
    writeln!(
        output,
        "  external agent  xana external-agent list | add | refresh | trust"
    )?;
    writeln!(output, "  image route     xana connect image")?;
    writeln!(output, "  vision route    xana connect vision")?;
    let registry = XanaConfig::load_registry_from(paths.config_file());
    let plugins = if paths.package_state_file().exists() {
        Some(PluginManager::open(paths).list())
    } else {
        Some(Ok(Vec::new()))
    };
    let healthy_plugins = plugins.as_ref().and_then(|result| {
        result.as_ref().ok().map(|plugins| {
            plugins
                .iter()
                .filter(|plugin| !plugin.health.starts_with("degraded:"))
                .count()
        })
    });
    let external_state = paths
        .external_agent_state_file()
        .exists()
        .then(|| read_document::<ExternalAgentStateDocument>(&paths.external_agent_state_file()))
        .transpose();
    let trusted_agents = external_state.as_ref().map(|state| {
        state.as_ref().map_or(0, |state| {
            state
                .agents
                .values()
                .filter(|agent| agent.trusted_identity_digest.is_some())
                .count()
        })
    });
    writeln!(output)?;
    writeln!(output, "Current readiness (redacted, no discovery started)")?;
    match (&registry, &plugins, healthy_plugins) {
        (Ok(registry), Some(Ok(plugins)), Some(healthy)) => writeln!(
            output,
            "  plugins         declared={} installed={} healthy={}",
            registry.plugins.len(),
            plugins.len(),
            healthy
        )?,
        _ => writeln!(
            output,
            "  plugins         unavailable; run `xana doctor` for exact recovery"
        )?,
    }
    match &registry {
        Ok(registry) => writeln!(
            output,
            "  MCP clients     configured={} exposed_by_profiles={}",
            registry.mcp_servers.len(),
            registry
                .profiles
                .values()
                .filter(|profile| !profile.mcp_servers.is_empty())
                .count()
        )?,
        Err(_) => writeln!(
            output,
            "  MCP clients     unavailable; run `xana doctor` for exact recovery"
        )?,
    }
    match (&registry, &external_state, trusted_agents) {
        (Ok(registry), Ok(state), Ok(trusted)) => writeln!(
            output,
            "  external agents configured={} refreshed={} trusted={}",
            registry.external_agents.len(),
            state.as_ref().map_or(0, |state| state.agents.len()),
            trusted
        )?,
        _ => writeln!(
            output,
            "  external agents unavailable; run `xana doctor` for exact recovery"
        )?,
    }
    if let Some(integration) = integration {
        writeln!(output)?;
        writeln!(output, "Next steps")?;
        match integration {
            ConnectIntegration::Plugin => {
                writeln!(output, "  1. xana plugin review <SOURCE>")?;
                writeln!(output, "  2. xana plugin install <SOURCE> --yes")?;
                writeln!(output, "  3. xana plugin enable <NAME> --profile <PROFILE>")?;
                writeln!(output, "  4. xana plugin list")?;
                writeln!(
                    output,
                    "  Review, installation, and profile enablement remain separate authority-changing steps."
                )?;
            }
            ConnectIntegration::Mcp => {
                writeln!(
                    output,
                    "  1. xana mcp add-stdio <SERVER> --command <PROGRAM> --profile <PROFILE> --allow-tool <NAME>"
                )?;
                writeln!(
                    output,
                    "     or xana mcp add-http <SERVER> --url <HTTPS_URL> --profile <PROFILE> --allow-tool <NAME>"
                )?;
                writeln!(output, "  2. xana mcp refresh <SERVER>")?;
                writeln!(output, "  3. xana mcp tools <SERVER>")?;
                writeln!(
                    output,
                    "  Transport, profile exposure, egress, and primitive allowlists are all required."
                )?;
            }
            ConnectIntegration::ExternalAgent => {
                writeln!(
                    output,
                    "  1. xana external-agent add <NAME> --endpoint <HTTPS_URL>"
                )?;
                writeln!(output, "  2. xana external-agent refresh <NAME>")?;
                writeln!(output, "  3. xana external-agent show <NAME>")?;
                writeln!(output, "  4. xana external-agent trust <NAME>")?;
                writeln!(
                    output,
                    "  Endpoint declaration, fetched identity, and trust are reviewed separately."
                )?;
            }
            _ => writeln!(output, "  Use the exact command shown above.")?,
        }
    }
    Ok(())
}

pub(super) fn run_focused_service(
    args: &ConnectArgs,
    kind: FocusedServiceKind,
    paths: &XanaPaths,
    terminal: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("focused-service setup requires a valid Xana configuration")?;
    if args.remove {
        let route = args
            .route
            .as_deref()
            .context("focused-service removal requires --route ROUTE")?;
        if !args.yes
            && !confirm(
                terminal,
                input,
                output,
                &format!("Remove route {route:?} from every profile? [y/N]: "),
            )?
        {
            writeln!(output, "No changes made.")?;
            return Ok(());
        }
        XanaConfig::remove_service_route(paths.config_file(), route)?;
        writeln!(
            output,
            "Removed focused-service route {route:?} atomically."
        )?;
        writeln!(
            output,
            "Backup: {}",
            paths.config_file().with_extension("toml.bak").display()
        )?;
        return Ok(());
    }

    let interactive = terminal && !has_focused_options(args);
    let provider = match args.service_provider {
        Some(provider) => provider,
        None if interactive => choose_provider(input, output)?,
        None => bail!("noninteractive focused-service setup requires --service-provider"),
    };
    let route_default = kind.default_route();
    let route = value_or_prompt(
        args.route.as_deref(),
        interactive,
        input,
        output,
        "Route name",
        Some(route_default),
    )?;
    let connection_default = format!("{}-{}", provider_name(provider), kind.label());
    let connection = value_or_prompt(
        args.service_connection.as_deref(),
        interactive,
        input,
        output,
        "Service connection name",
        Some(&connection_default),
    )?;
    let model = value_or_prompt(
        args.model.as_deref(),
        interactive,
        input,
        output,
        "Exact model id",
        None,
    )?;
    let credential_default = credential_default(provider);
    let credential_env = value_or_prompt(
        args.credential_env.as_deref(),
        interactive,
        input,
        output,
        "API-key environment variable",
        Some(credential_default),
    )?;
    let profile = value_or_prompt(
        args.profile.as_deref(),
        interactive,
        input,
        output,
        "Profile to expose this route",
        Some(&registry.default_profile),
    )?;
    let make_default = interactive || args.make_default;

    writeln!(output)?;
    writeln!(output, "Review focused-service setup")?;
    writeln!(output, "  Operation:   {}", kind.operation())?;
    writeln!(output, "  Route:       {route}")?;
    writeln!(output, "  Connection:  {connection}")?;
    writeln!(output, "  Adapter:     {}", kind.adapter(provider))?;
    writeln!(output, "  Model:       {model}")?;
    writeln!(
        output,
        "  Credential:  environment {credential_env} (value not read now)"
    )?;
    writeln!(output, "  Profile:     {profile}")?;
    writeln!(output, "  Outbound:    prompt_text + selected_artifacts")?;
    writeln!(output, "  Default:     {make_default}")?;
    writeln!(output, "  Applies:     new conversation")?;
    if !args.yes
        && !confirm(
            terminal,
            input,
            output,
            "Install this focused-service route? [y/N]: ",
        )?
    {
        writeln!(output, "No changes made.")?;
        return Ok(());
    }

    XanaConfig::add_service_route(
        paths.config_file(),
        NewServiceRoute {
            route: route.clone(),
            connection,
            adapter: kind.adapter(provider).into(),
            base_url: args.base_url.clone(),
            credential: CredentialReference::Environment {
                variable: credential_env,
            },
            operation: kind.operation().into(),
            model,
            description: Some(format!("Xana {} route", kind.operation())),
            profile,
            make_default,
        },
    )?;
    writeln!(
        output,
        "Focused-service route {route:?} installed atomically."
    )?;
    writeln!(
        output,
        "Backup: {}",
        paths.config_file().with_extension("toml.bak").display()
    )?;
    writeln!(output, "Inspect: xana {} inspect {route}", kind.label())?;
    writeln!(
        output,
        "Start a new conversation for the frozen profile change to apply."
    )?;
    Ok(())
}

fn has_focused_options(args: &ConnectArgs) -> bool {
    args.route.is_some()
        || args.service_connection.is_some()
        || args.service_provider.is_some()
        || args.model.is_some()
        || args.credential_env.is_some()
        || args.base_url.is_some()
        || args.profile.is_some()
        || args.make_default
        || args.remove
        || args.yes
}

fn choose_provider(
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<FocusedServiceProviderChoice> {
    for _ in 0..MAX_ATTEMPTS {
        writeln!(output, "Choose a focused-service provider:")?;
        writeln!(output, "  1. OpenAI API key")?;
        writeln!(output, "  2. OpenRouter API key")?;
        write!(output, "Choice [1-2]: ")?;
        output.flush()?;
        let Some(value) = read_line(input)? else {
            bail!("focused-service setup cancelled at end of input; no changes made");
        };
        match value.trim() {
            "1" => return Ok(FocusedServiceProviderChoice::OpenAi),
            "2" => return Ok(FocusedServiceProviderChoice::OpenRouter),
            _ => writeln!(output, "Choose 1 or 2.")?,
        }
    }
    bail!("too many invalid focused-service provider selections")
}

fn value_or_prompt(
    provided: Option<&str>,
    interactive: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    if let Some(value) = provided {
        return nonblank(value, label);
    }
    if !interactive {
        bail!(
            "noninteractive focused-service setup requires --{}",
            label.to_ascii_lowercase().replace(' ', "-")
        );
    }
    write!(output, "{label}")?;
    if let Some(default) = default {
        write!(output, " [{default}]")?;
    }
    write!(output, ": ")?;
    output.flush()?;
    let value = read_line(input)?
        .context("focused-service setup cancelled at end of input; no changes made")?;
    let selected = if value.trim().is_empty() {
        default.unwrap_or_default()
    } else {
        value.trim()
    };
    nonblank(selected, label)
}

fn nonblank(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        bail!("{label} must be 1..=512 bytes without control characters")
    }
    Ok(value.to_owned())
}

fn confirm(
    terminal: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
) -> Result<bool> {
    if !terminal {
        bail!("noninteractive focused-service changes require --yes")
    }
    write!(output, "{prompt}")?;
    output.flush()?;
    let Some(value) = read_line(input)? else {
        return Ok(false);
    };
    Ok(matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn read_line(input: &mut impl BufRead) -> Result<Option<String>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    if line.len() > 2048 {
        bail!("focused-service setup input exceeds the 2 KiB field limit")
    }
    Ok(Some(line))
}

fn provider_name(provider: FocusedServiceProviderChoice) -> &'static str {
    match provider {
        FocusedServiceProviderChoice::OpenAi => "openai",
        FocusedServiceProviderChoice::OpenRouter => "openrouter",
    }
}

fn credential_default(provider: FocusedServiceProviderChoice) -> &'static str {
    match provider {
        FocusedServiceProviderChoice::OpenAi => "OPENAI_API_KEY",
        FocusedServiceProviderChoice::OpenRouter => "OPENROUTER_API_KEY",
    }
}

#[cfg(test)]
mod tests;
