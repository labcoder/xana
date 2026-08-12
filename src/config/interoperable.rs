//! Declarative Milestone 3 configuration that is not conversational-provider wire state.
//!
//! These values are human-authored references and policy inputs. They never
//! contain resolved credentials, endpoint trust, package install state, or
//! running-process state.

use super::{ConfigDocument, ConfigError, CredentialReference, valid_name, valid_stable_id};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

const MAX_GUIDANCE_BYTES: usize = 16 * 1024;
const MAX_DECLARATIONS: usize = 256;
const MAX_REFERENCES_PER_PROFILE: usize = 128;
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_OPTIONS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProfileUse {
    Primary,
    Child,
}

pub(super) fn default_profile_uses() -> Vec<ProfileUse> {
    vec![ProfileUse::Primary, ProfileUse::Child]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PluginSource {
    Directory {
        path: PathBuf,
        #[serde(default)]
        linked: bool,
    },
    Git {
        url: String,
        revision: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginDeclaration {
    pub(crate) source: PluginSource,
    #[serde(default)]
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum McpServerDeclaration {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        egress_policy: Option<String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        credential: Option<CredentialReference>,
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        egress_policy: Option<String>,
    },
}

impl McpServerDeclaration {
    pub(crate) fn enabled(&self) -> bool {
        match self {
            Self::Stdio { enabled, .. } | Self::StreamableHttp { enabled, .. } => *enabled,
        }
    }

    fn egress_policy(&self) -> Option<&str> {
        match self {
            Self::Stdio { egress_policy, .. } | Self::StreamableHttp { egress_policy, .. } => {
                egress_policy.as_deref()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalAgentDeclaration {
    pub(crate) endpoint: String,
    #[serde(default)]
    pub(crate) credential: Option<CredentialReference>,
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) egress_policy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceConnectionDeclaration {
    pub(crate) adapter: String,
    #[serde(default)]
    pub(crate) base_url: Option<String>,
    #[serde(default)]
    pub(crate) credential: Option<CredentialReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceRouteDeclaration {
    pub(crate) operation: String,
    pub(crate) connection: String,
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) options: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) egress_policy: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutboundDataClass {
    PromptText,
    XanaSummary,
    SelectedMessages,
    SelectedFileContents,
    SelectedArtifacts,
    WorkspaceMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EgressPolicyDeclaration {
    #[serde(default)]
    pub(crate) allowed: Vec<OutboundDataClass>,
}

pub(super) fn validate(document: &ConfigDocument) -> Result<(), ConfigError> {
    validate_count("plugins", document.plugins.len())?;
    validate_count("MCP servers", document.mcp_servers.len())?;
    validate_count("external agents", document.external_agents.len())?;
    validate_count("service connections", document.service_connections.len())?;
    validate_count("service routes", document.service_routes.len())?;
    validate_count("egress policies", document.egress_policies.len())?;

    for (name, plugin) in &document.plugins {
        validate_declaration_name("plugin", name)?;
        match &plugin.source {
            PluginSource::Directory { path, linked } => {
                if !path.is_absolute() {
                    return invalid("plugin", name, "directory paths must be absolute");
                }
                if !linked {
                    return invalid(
                        "plugin",
                        name,
                        "directory sources must opt into visibly mutable linked mode",
                    );
                }
            }
            PluginSource::Git { url, revision } => {
                validate_https_url("plugin", name, url)?;
                if revision.trim().is_empty() || revision.len() > 128 {
                    return invalid(
                        "plugin",
                        name,
                        "Git sources require an exact non-blank revision of at most 128 bytes",
                    );
                }
            }
        }
    }

    for (name, server) in &document.mcp_servers {
        validate_declaration_name("MCP server", name)?;
        validate_policy_reference(document, "MCP server", name, server.egress_policy())?;
        match server {
            McpServerDeclaration::Stdio {
                command,
                args,
                environment,
                cwd,
                ..
            } => {
                if command.trim().is_empty() || command.len() > 4096 {
                    return invalid("MCP server", name, "stdio command must be 1..=4096 bytes");
                }
                validate_arguments("MCP server", name, args)?;
                if environment.len() > MAX_OPTIONS {
                    return invalid("MCP server", name, "too many environment entries");
                }
                for (key, value) in environment {
                    if !valid_environment_name(key) || value.len() > 4096 {
                        return invalid(
                            "MCP server",
                            name,
                            "environment entries require valid names and values at most 4096 bytes",
                        );
                    }
                }
                if cwd.as_ref().is_some_and(|path| !path.is_absolute()) {
                    return invalid("MCP server", name, "stdio cwd must be absolute when set");
                }
            }
            McpServerDeclaration::StreamableHttp {
                url, credential, ..
            } => {
                validate_https_url("MCP server", name, url)?;
                if let Some(reference) = credential {
                    super::validate_credential(name, reference)?;
                }
            }
        }
    }

    for (name, agent) in &document.external_agents {
        validate_declaration_name("external agent", name)?;
        validate_https_url("external agent", name, &agent.endpoint)?;
        validate_policy_reference(
            document,
            "external agent",
            name,
            agent.egress_policy.as_deref(),
        )?;
        if let Some(reference) = &agent.credential {
            super::validate_credential(name, reference)?;
        }
    }

    for (name, connection) in &document.service_connections {
        validate_declaration_name("service connection", name)?;
        if !valid_stable_id(&connection.adapter) {
            return invalid(
                "service connection",
                name,
                "adapter must be a stable capability id",
            );
        }
        if let Some(url) = &connection.base_url {
            validate_https_url("service connection", name, url)?;
        }
        if let Some(reference) = &connection.credential {
            super::validate_credential(name, reference)?;
        }
    }

    for (name, route) in &document.service_routes {
        validate_declaration_name("service route", name)?;
        if !valid_stable_id(&route.operation) {
            return invalid(
                "service route",
                name,
                "operation must be a stable capability id",
            );
        }
        if !document.service_connections.contains_key(&route.connection) {
            return invalid("service route", name, "connection does not exist");
        }
        if route.model.trim().is_empty() || route.model.len() > 512 {
            return invalid("service route", name, "model must be 1..=512 bytes");
        }
        if route.options.len() > MAX_OPTIONS
            || route
                .options
                .iter()
                .any(|(key, value)| !valid_name(key) || value.len() > 4096)
        {
            return invalid(
                "service route",
                name,
                "options are invalid or exceed bounds",
            );
        }
        validate_policy_reference(
            document,
            "service route",
            name,
            route.egress_policy.as_deref(),
        )?;
    }

    for (name, policy) in &document.egress_policies {
        validate_declaration_name("egress policy", name)?;
        let mut unique = std::collections::BTreeSet::new();
        if policy.allowed.iter().any(|class| !unique.insert(*class)) {
            return invalid(
                "egress policy",
                name,
                "outbound data classes must be unique",
            );
        }
    }

    for (name, profile) in &document.profiles {
        if profile
            .identity
            .as_deref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_GUIDANCE_BYTES)
        {
            return invalid(
                "profile",
                name,
                "identity must be 1..=16384 non-blank bytes",
            );
        }
        validate_references("profile skills", name, &profile.skills, |_| true)?;
        validate_references("profile plugins", name, &profile.plugins, |value| {
            document.plugins.contains_key(value)
        })?;
        validate_references("profile MCP servers", name, &profile.mcp_servers, |value| {
            document.mcp_servers.contains_key(value)
        })?;
        validate_references(
            "profile external agents",
            name,
            &profile.external_agents,
            |value| document.external_agents.contains_key(value),
        )?;
        validate_references(
            "profile service routes",
            name,
            &profile.service_routes,
            |value| document.service_routes.contains_key(value),
        )?;
        validate_policy_reference(document, "profile", name, profile.egress_policy.as_deref())?;
        if profile.applies_to.is_empty() {
            return invalid("profile", name, "applies_to cannot be empty");
        }
        let mut uses = std::collections::BTreeSet::new();
        if profile.applies_to.iter().any(|value| !uses.insert(*value)) {
            return invalid("profile", name, "applies_to entries must be unique");
        }
    }

    Ok(())
}

fn validate_count(section: &'static str, value: usize) -> Result<(), ConfigError> {
    if value > MAX_DECLARATIONS {
        return Err(ConfigError::InvalidInteroperableConfig {
            section,
            name: "configuration".into(),
            reason: format!("contains {value} entries; maximum is {MAX_DECLARATIONS}"),
        });
    }
    Ok(())
}

fn validate_declaration_name(section: &'static str, name: &str) -> Result<(), ConfigError> {
    if valid_name(name) {
        Ok(())
    } else {
        invalid(section, name, "name must use Xana stable-name syntax")
    }
}

fn validate_policy_reference(
    document: &ConfigDocument,
    section: &'static str,
    name: &str,
    policy: Option<&str>,
) -> Result<(), ConfigError> {
    if policy.is_some_and(|value| !document.egress_policies.contains_key(value)) {
        return invalid(section, name, "egress policy does not exist");
    }
    Ok(())
}

fn validate_references(
    section: &'static str,
    name: &str,
    values: &[String],
    exists: impl Fn(&str) -> bool,
) -> Result<(), ConfigError> {
    if values.len() > MAX_REFERENCES_PER_PROFILE {
        return invalid(section, name, "too many references");
    }
    let mut unique = std::collections::BTreeSet::new();
    for value in values {
        if !valid_stable_id(value) || !unique.insert(value) || !exists(value) {
            return invalid(
                section,
                name,
                "contains an invalid, duplicate, or unknown reference",
            );
        }
    }
    Ok(())
}

fn validate_arguments(
    section: &'static str,
    name: &str,
    args: &[String],
) -> Result<(), ConfigError> {
    if args.len() > MAX_ARGUMENTS
        || args.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
        || args.iter().any(|argument| argument.as_bytes().contains(&0))
    {
        return invalid(
            section,
            name,
            "stdio arguments exceed their count or byte bounds",
        );
    }
    Ok(())
}

fn validate_https_url(section: &'static str, name: &str, value: &str) -> Result<(), ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::InvalidInteroperableConfig {
        section,
        name: name.to_owned(),
        reason: "expected an absolute HTTPS URL".into(),
    })?;
    if url.scheme() != "https"
        || !url.has_host()
        || url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return invalid(
            section,
            name,
            "URL must be absolute HTTPS without credentials or fragments",
        );
    }
    Ok(())
}

fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn invalid<T>(section: &'static str, name: &str, reason: &'static str) -> Result<T, ConfigError> {
    Err(ConfigError::InvalidInteroperableConfig {
        section,
        name: name.to_owned(),
        reason: reason.into(),
    })
}
