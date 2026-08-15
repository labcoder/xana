//! Versioned shared configuration loading, validation, and initial rendering.
//!
//! This module owns the human-authored TOML schema and resolves it into an
//! immutable agent snapshot. It does not read process environment variables or
//! invent initializer-specific validation.

use crate::{
    bounded_file,
    permission::{PermissionPolicy, PermissionRule, PolicyDecision, PolicyError},
    shell::{Shell, ShellConfig, ShellError},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

mod interoperable;

pub(crate) use interoperable::{
    EgressPolicyDeclaration, ExternalAgentDeclaration, McpPrimitiveSelection, McpServerDeclaration,
    OutboundDataClass, PluginDeclaration, ProfileUse, ServiceConnectionDeclaration,
    ServiceRouteDeclaration,
};

pub(crate) const CONFIG_VERSION: u32 = 4;
const MIN_CONFIG_VERSION: u32 = 1;
const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;
const MAX_MAX_TOOL_ROUNDS: usize = 64;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_ROUTE_FAN_OUT: usize = 64;
const MAX_ROUTE_DESCENDANTS: usize = 256;
const MAX_ROUTE_CONCURRENCY: usize = 32;
const MAX_ROUTE_DEADLINE_SECONDS: u64 = 24 * 60 * 60;
const MAX_ROUTE_CONTEXT_TOKENS: usize = 1_000_000;
const MAX_ROUTE_REPORT_BYTES: usize = 1024 * 1024;
const MAX_ROUTE_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct VersionHeader {
    version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigDocument {
    version: u32,
    default_profile: String,
    #[serde(default)]
    default_child_route: Option<String>,
    permission_mode: PermissionMode,
    #[serde(default)]
    permission_rules: Vec<PermissionRule>,
    #[serde(default)]
    shell: ShellConfig,
    providers: BTreeMap<String, ProviderConnection>,
    profiles: BTreeMap<String, AgentProfile>,
    #[serde(default)]
    routes: BTreeMap<String, TaskRoute>,
    #[serde(default)]
    plugins: BTreeMap<String, PluginDeclaration>,
    #[serde(default)]
    mcp_servers: BTreeMap<String, McpServerDeclaration>,
    #[serde(default)]
    external_agents: BTreeMap<String, ExternalAgentDeclaration>,
    #[serde(default)]
    service_connections: BTreeMap<String, ServiceConnectionDeclaration>,
    #[serde(default)]
    service_routes: BTreeMap<String, ServiceRouteDeclaration>,
    #[serde(default)]
    egress_policies: BTreeMap<String, EgressPolicyDeclaration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum ProviderKind {
    #[serde(rename = "openai_compat")]
    OpenAiCompat,
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "codex")]
    Codex,
}

impl ProviderKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompat => "openai_compat",
            Self::Ollama => "ollama",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionMode {
    Deny,
    Ask,
    Allow,
}

impl From<PermissionMode> for PolicyDecision {
    fn from(value: PermissionMode) -> Self {
        match value {
            PermissionMode::Deny => Self::Deny,
            PermissionMode::Ask => Self::Ask,
            PermissionMode::Allow => Self::Allow,
        }
    }
}

impl PermissionMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Allow => "allow",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CredentialReference {
    Environment { variable: String },
    Stored { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelOverride {
    #[serde(default)]
    pub(crate) input_modalities: Vec<String>,
    pub(crate) tools: Option<bool>,
    pub(crate) reasoning: Option<bool>,
    pub(crate) context_tokens: Option<usize>,
    pub(crate) max_output_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConnection {
    kind: ProviderKind,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    credential: Option<CredentialReference>,
    #[serde(default)]
    models: BTreeMap<String, ModelOverride>,
    #[serde(default)]
    codex_program: Option<String>,
    #[serde(default)]
    codex_home: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_id: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "is_false")]
    archived: bool,
    #[serde(alias = "provider")]
    connection: String,
    model: String,
    #[serde(default = "default_max_tool_rounds")]
    max_tool_rounds: usize,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    reasoning_summary: Option<String>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    permission_mode: Option<PermissionMode>,
    #[serde(default)]
    orchestration: OrchestrationLimits,
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    plugins: Vec<String>,
    #[serde(default)]
    mcp_servers: Vec<String>,
    #[serde(default)]
    mcp_allowlists: BTreeMap<String, McpPrimitiveSelection>,
    #[serde(default)]
    external_agents: Vec<String>,
    #[serde(default)]
    service_routes: Vec<String>,
    #[serde(default)]
    egress_policy: Option<String>,
    #[serde(default = "interoperable::default_profile_uses")]
    applies_to: Vec<ProfileUse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskRoute {
    profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrchestrationLimits {
    #[serde(default = "default_max_fan_out")]
    pub(crate) max_fan_out: usize,
    #[serde(default = "default_max_descendants")]
    pub(crate) max_descendants: usize,
    #[serde(default = "default_max_concurrency")]
    pub(crate) max_concurrency: usize,
    #[serde(default = "default_deadline_seconds")]
    pub(crate) deadline_seconds: u64,
    #[serde(default = "default_max_context_tokens")]
    pub(crate) max_context_tokens: usize,
    #[serde(default = "default_max_report_bytes")]
    pub(crate) max_report_bytes: usize,
    #[serde(default = "default_max_artifact_bytes")]
    pub(crate) max_artifact_bytes: usize,
}

impl Default for OrchestrationLimits {
    fn default() -> Self {
        Self {
            max_fan_out: default_max_fan_out(),
            max_descendants: default_max_descendants(),
            max_concurrency: default_max_concurrency(),
            deadline_seconds: default_deadline_seconds(),
            max_context_tokens: default_max_context_tokens(),
            max_report_bytes: default_max_report_bytes(),
            max_artifact_bytes: default_max_artifact_bytes(),
        }
    }
}

fn default_max_fan_out() -> usize {
    4
}

fn default_max_descendants() -> usize {
    8
}

fn default_max_concurrency() -> usize {
    2
}

fn default_deadline_seconds() -> u64 {
    300
}

fn default_max_context_tokens() -> usize {
    8_192
}

fn default_max_report_bytes() -> usize {
    32 * 1024
}

fn default_max_artifact_bytes() -> usize {
    8 * 1024 * 1024
}

fn default_max_tool_rounds() -> usize {
    DEFAULT_MAX_TOOL_ROUNDS
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct XanaConfig {
    pub(crate) provider_name: String,
    pub(crate) provider_kind: ProviderKind,
    pub(crate) base_url: String,
    pub(crate) credential: Option<CredentialReference>,
    pub(crate) codex_program: Option<String>,
    pub(crate) codex_home: Option<PathBuf>,
    pub(crate) model: String,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) permission_rules: Vec<PermissionRule>,
    pub(crate) shell: ShellConfig,
    pub(crate) max_tool_rounds: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionConfig {
    pub(crate) id: String,
    pub(crate) kind: ProviderKind,
    pub(crate) base_url: Option<String>,
    pub(crate) credential: Option<CredentialReference>,
    pub(crate) models: BTreeMap<String, ModelOverride>,
    pub(crate) codex_program: Option<String>,
    pub(crate) codex_home: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileConfig {
    pub(crate) id: String,
    pub(crate) profile_id: uuid::Uuid,
    pub(crate) archived: bool,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) max_tool_rounds: usize,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) reasoning_summary: Option<String>,
    pub(crate) capabilities: Option<Vec<String>>,
    pub(crate) permission_mode: Option<PermissionMode>,
    pub(crate) orchestration: OrchestrationLimits,
    pub(crate) identity: Option<String>,
    pub(crate) skills: Vec<String>,
    pub(crate) plugins: Vec<String>,
    pub(crate) mcp_servers: Vec<String>,
    pub(crate) mcp_allowlists: BTreeMap<String, McpPrimitiveSelection>,
    pub(crate) external_agents: Vec<String>,
    pub(crate) service_routes: Vec<String>,
    pub(crate) egress_policy: Option<String>,
    pub(crate) applies_to: Vec<ProfileUse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteConfig {
    pub(crate) id: String,
    pub(crate) profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionRegistry {
    pub(crate) default_profile: String,
    pub(crate) default_child_route: Option<String>,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) permission_rules: Vec<PermissionRule>,
    pub(crate) connections: BTreeMap<String, ConnectionConfig>,
    pub(crate) profiles: BTreeMap<String, ProfileConfig>,
    pub(crate) routes: BTreeMap<String, RouteConfig>,
    pub(crate) plugins: BTreeMap<String, PluginDeclaration>,
    pub(crate) mcp_servers: BTreeMap<String, McpServerDeclaration>,
    pub(crate) external_agents: BTreeMap<String, ExternalAgentDeclaration>,
    pub(crate) service_connections: BTreeMap<String, ServiceConnectionDeclaration>,
    pub(crate) service_routes: BTreeMap<String, ServiceRouteDeclaration>,
    pub(crate) egress_policies: BTreeMap<String, EgressPolicyDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewConnection {
    pub(crate) id: String,
    pub(crate) kind: ProviderKind,
    pub(crate) base_url: Option<String>,
    pub(crate) credential: Option<CredentialReference>,
    pub(crate) model: String,
    pub(crate) codex_program: Option<String>,
    pub(crate) codex_home: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewProfile {
    pub(crate) id: String,
    pub(crate) connection: String,
    pub(crate) model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewExternalAgent {
    pub(crate) id: String,
    pub(crate) endpoint: String,
    pub(crate) credential: Option<CredentialReference>,
    pub(crate) egress_policy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewServiceRoute {
    pub(crate) route: String,
    pub(crate) connection: String,
    pub(crate) adapter: String,
    pub(crate) base_url: Option<String>,
    pub(crate) credential: CredentialReference,
    pub(crate) operation: String,
    pub(crate) model: String,
    pub(crate) description: Option<String>,
    pub(crate) profile: String,
    pub(crate) make_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewMcpServer {
    pub(crate) id: String,
    pub(crate) declaration: McpServerDeclaration,
    pub(crate) profile: String,
    pub(crate) selection: McpPrimitiveSelection,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProfileUpdate {
    pub(crate) connection: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<Option<String>>,
    pub(crate) reasoning_summary: Option<Option<String>>,
    pub(crate) identity: Option<Option<String>>,
    pub(crate) permission_mode: Option<Option<PermissionMode>>,
    pub(crate) max_tool_rounds: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InitialConnection {
    Ollama {
        name: String,
        base_url: String,
    },
    OpenAiCompatible {
        name: String,
        base_url: String,
    },
    Native {
        name: String,
        kind: ProviderKind,
        base_url: Option<String>,
        credential: Option<CredentialReference>,
    },
    Codex {
        name: String,
        program: String,
        home: Option<PathBuf>,
    },
}

impl InitialConnection {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Ollama { name, .. }
            | Self::OpenAiCompatible { name, .. }
            | Self::Native { name, .. }
            | Self::Codex { name, .. } => name,
        }
    }

    pub(crate) fn is_codex(&self) -> bool {
        matches!(self, Self::Codex { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitialConfig {
    pub(crate) connection: InitialConnection,
    pub(crate) model: String,
    pub(crate) max_tool_rounds: usize,
    pub(crate) shell: ShellConfig,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) reasoning_effort: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ConfigError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Decode(toml::de::Error),
    Encode(toml::ser::Error),
    TooLarge {
        actual: u64,
        limit: usize,
    },
    LegacyConfigFound {
        legacy_path: PathBuf,
        config_path: PathBuf,
    },
    UnsupportedVersion {
        found: u32,
    },
    InvalidName {
        section: &'static str,
        name: String,
    },
    MissingDefaultProfile {
        name: String,
    },
    UnknownProvider {
        profile: String,
        provider: String,
    },
    UnknownProfile {
        route: String,
        profile: String,
    },
    MissingDefaultChildRoute {
        route: String,
    },
    EmptyModel {
        profile: String,
    },
    InvalidBaseUrl {
        provider: String,
        reason: &'static str,
    },
    InvalidCredential {
        provider: String,
        reason: &'static str,
    },
    InvalidCodexHome {
        provider: String,
    },
    InvalidCodexConfiguration {
        provider: String,
        reason: &'static str,
    },
    InvalidModelModality {
        provider: String,
        model: String,
        modality: String,
    },
    Edit(String),
    ConnectionAlreadyExists {
        name: String,
    },
    ConnectionReferenced {
        name: String,
        profiles: Vec<String>,
    },
    InvalidToolRoundLimit {
        profile: String,
        value: usize,
    },
    InvalidProfileOption {
        profile: String,
        option: &'static str,
        reason: &'static str,
    },
    InvalidCapability {
        profile: String,
        capability: String,
    },
    DuplicateCapability {
        profile: String,
        capability: String,
    },
    InvalidOrchestrationLimit {
        profile: String,
        field: &'static str,
        value: u64,
        maximum: u64,
    },
    InvalidInteroperableConfig {
        section: &'static str,
        name: String,
        reason: String,
    },
    InvalidShell(ShellError),
    InvalidPermissionPolicy(PolicyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigReadiness {
    Healthy,
    Missing,
    Invalid,
    Incompatible,
    Indeterminate,
}

impl ConfigReadiness {
    pub(crate) fn inspect(path: &Path) -> Self {
        match XanaConfig::load_from(path) {
            Ok(_) => Self::Healthy,
            Err(ConfigError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                Self::Missing
            }
            Err(ConfigError::UnsupportedVersion { .. } | ConfigError::LegacyConfigFound { .. }) => {
                Self::Incompatible
            }
            Err(ConfigError::Io { .. } | ConfigError::Encode(_)) => Self::Indeterminate,
            Err(_) => Self::Invalid,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::Incompatible => "incompatible",
            Self::Indeterminate => "indeterminate",
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            Self::Decode(source) => write!(f, "could not decode config.toml: {source}"),
            Self::Encode(source) => write!(f, "could not encode config.toml: {source}"),
            Self::TooLarge { actual, limit } => write!(
                f,
                "config.toml contains {actual} bytes, exceeding the {limit}-byte limit"
            ),
            Self::LegacyConfigFound {
                legacy_path,
                config_path,
            } => write!(
                f,
                "found legacy configuration at {}; migrate it manually to {} using docs/configuration.md",
                legacy_path.display(),
                config_path.display()
            ),
            Self::UnsupportedVersion { found } => write!(
                f,
                "unsupported configuration version {found}; this Xana build supports version {CONFIG_VERSION}"
            ),
            Self::InvalidName { section, name } => write!(
                f,
                "invalid {section} name {name:?}; use lowercase ASCII letters, digits, '_' or '-', beginning with a letter or digit"
            ),
            Self::MissingDefaultProfile { name } => {
                write!(f, "default profile {name:?} does not exist")
            }
            Self::UnknownProvider { profile, provider } => write!(
                f,
                "profile {profile:?} references unknown provider {provider:?}"
            ),
            Self::UnknownProfile { route, profile } => {
                write!(f, "route {route:?} references unknown profile {profile:?}")
            }
            Self::MissingDefaultChildRoute { route } => {
                write!(f, "default child route {route:?} does not exist")
            }
            Self::EmptyModel { profile } => {
                write!(f, "profile {profile:?} must name a non-blank model")
            }
            Self::InvalidBaseUrl { provider, reason } => {
                write!(f, "provider {provider:?} has an invalid base URL: {reason}")
            }
            Self::InvalidCredential { provider, reason } => {
                write!(
                    f,
                    "provider {provider:?} has an invalid credential reference: {reason}"
                )
            }
            Self::InvalidCodexHome { provider } => write!(
                f,
                "provider {provider:?} must use an absolute codex_home and only Codex connections may set it"
            ),
            Self::InvalidCodexConfiguration { provider, reason } => {
                write!(
                    f,
                    "provider {provider:?} has invalid Codex configuration: {reason}"
                )
            }
            Self::InvalidModelModality {
                provider,
                model,
                modality,
            } => write!(
                f,
                "provider {provider:?} model {model:?} declares unknown input modality {modality:?}"
            ),
            Self::Edit(reason) => write!(f, "could not edit config.toml: {reason}"),
            Self::ConnectionAlreadyExists { name } => {
                write!(f, "provider connection {name:?} already exists")
            }
            Self::ConnectionReferenced { name, profiles } => write!(
                f,
                "provider connection {name:?} is still referenced by profile(s): {}",
                profiles.join(", ")
            ),
            Self::InvalidToolRoundLimit { profile, value } => write!(
                f,
                "profile {profile:?} has max_tool_rounds = {value}; expected 1..={MAX_MAX_TOOL_ROUNDS}"
            ),
            Self::InvalidProfileOption {
                profile,
                option,
                reason,
            } => write!(f, "profile {profile:?} has invalid {option}: {reason}"),
            Self::InvalidCapability {
                profile,
                capability,
            } => write!(
                f,
                "profile {profile:?} contains invalid capability id {capability:?}"
            ),
            Self::DuplicateCapability {
                profile,
                capability,
            } => write!(
                f,
                "profile {profile:?} selects capability {capability:?} more than once"
            ),
            Self::InvalidOrchestrationLimit {
                profile,
                field,
                value,
                maximum,
            } => write!(
                f,
                "profile {profile:?} has {field} = {value}; expected 1..={maximum}"
            ),
            Self::InvalidInteroperableConfig {
                section,
                name,
                reason,
            } => write!(f, "invalid {section} {name:?}: {reason}"),
            Self::InvalidShell(source) => write!(f, "invalid shell configuration: {source}"),
            Self::InvalidPermissionPolicy(source) => {
                write!(f, "invalid permission policy: {source}")
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Decode(source) => Some(source),
            Self::Encode(source) => Some(source),
            Self::InvalidShell(source) => Some(source),
            Self::InvalidPermissionPolicy(source) => Some(source),
            Self::LegacyConfigFound { .. }
            | Self::TooLarge { .. }
            | Self::UnsupportedVersion { .. }
            | Self::InvalidName { .. }
            | Self::MissingDefaultProfile { .. }
            | Self::UnknownProvider { .. }
            | Self::UnknownProfile { .. }
            | Self::MissingDefaultChildRoute { .. }
            | Self::EmptyModel { .. }
            | Self::InvalidBaseUrl { .. }
            | Self::InvalidCredential { .. }
            | Self::InvalidCodexHome { .. }
            | Self::InvalidCodexConfiguration { .. }
            | Self::InvalidModelModality { .. }
            | Self::Edit(_)
            | Self::ConnectionAlreadyExists { .. }
            | Self::ConnectionReferenced { .. }
            | Self::InvalidToolRoundLimit { .. }
            | Self::InvalidProfileOption { .. }
            | Self::InvalidCapability { .. }
            | Self::DuplicateCapability { .. }
            | Self::InvalidOrchestrationLimit { .. }
            | Self::InvalidInteroperableConfig { .. } => None,
        }
    }
}

impl XanaConfig {
    pub(crate) fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match read_config(path) {
            Ok(input) => Self::parse(&input),
            Err(ConfigError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                let not_found = source;
                let legacy_path = path.with_file_name("config.kv");

                match fs::metadata(&legacy_path) {
                    Ok(metadata) if metadata.is_file() => Err(ConfigError::LegacyConfigFound {
                        legacy_path,
                        config_path: path.to_owned(),
                    }),
                    Ok(_) => Err(ConfigError::Io {
                        path: path.to_owned(),
                        source: not_found,
                    }),
                    Err(source) if source.kind() == io::ErrorKind::NotFound => {
                        Err(ConfigError::Io {
                            path: path.to_owned(),
                            source: not_found,
                        })
                    }
                    Err(source) => Err(ConfigError::Io {
                        path: legacy_path,
                        source,
                    }),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn parse(input: &str) -> Result<Self, ConfigError> {
        let header: VersionHeader = toml::from_str(input).map_err(ConfigError::Decode)?;

        if !(MIN_CONFIG_VERSION..=CONFIG_VERSION).contains(&header.version) {
            return Err(ConfigError::UnsupportedVersion {
                found: header.version,
            });
        }

        let document: ConfigDocument = toml::from_str(input).map_err(ConfigError::Decode)?;

        validate_and_resolve(document)
    }

    pub(crate) fn document_version(input: &str) -> Result<u32, ConfigError> {
        let header: VersionHeader = toml::from_str(input).map_err(ConfigError::Decode)?;
        Ok(header.version)
    }

    pub(crate) fn migrate_to_current(input: &str) -> Result<String, ConfigError> {
        let version = Self::document_version(input)?;
        if !(MIN_CONFIG_VERSION..=CONFIG_VERSION).contains(&version) {
            return Err(ConfigError::UnsupportedVersion { found: version });
        }
        let before = Self::parse_registry(input)?;
        let mut document = input
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        migrate_profile_connection_keys(&mut document)?;
        document["version"] = toml_edit::value(CONFIG_VERSION as i64);
        let rendered = document.to_string();
        let after = Self::parse_registry(&rendered)?;
        if before != after {
            return Err(ConfigError::Edit(
                "migration changed resolved configuration semantics".into(),
            ));
        }
        Ok(rendered)
    }

    pub(crate) fn render_initial(input: InitialConfig) -> Result<String, ConfigError> {
        let InitialConfig {
            connection,
            model,
            max_tool_rounds,
            shell,
            permission_mode,
            reasoning_effort,
        } = input;

        let (connection_name, connection) = match connection {
            InitialConnection::Ollama { name, base_url } => (
                name,
                ProviderConnection {
                    kind: ProviderKind::Ollama,
                    base_url: Some(base_url),
                    credential: None,
                    models: BTreeMap::new(),
                    codex_program: None,
                    codex_home: None,
                },
            ),
            InitialConnection::OpenAiCompatible { name, base_url } => (
                name,
                ProviderConnection {
                    kind: ProviderKind::OpenAiCompat,
                    base_url: Some(base_url),
                    credential: None,
                    models: BTreeMap::new(),
                    codex_program: None,
                    codex_home: None,
                },
            ),
            InitialConnection::Native {
                name,
                kind,
                base_url,
                credential,
            } => (
                name,
                ProviderConnection {
                    kind,
                    base_url,
                    credential,
                    models: BTreeMap::new(),
                    codex_program: None,
                    codex_home: None,
                },
            ),
            InitialConnection::Codex {
                name,
                program,
                home,
            } => (
                name,
                ProviderConnection {
                    kind: ProviderKind::Codex,
                    base_url: None,
                    credential: None,
                    models: BTreeMap::new(),
                    codex_program: Some(program),
                    codex_home: home,
                },
            ),
        };

        let mut providers = BTreeMap::new();
        providers.insert(connection_name.clone(), connection);

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "default".to_owned(),
            AgentProfile {
                profile_id: Some(stable_profile_id("global", "default")),
                archived: false,
                connection: connection_name,
                model,
                max_tool_rounds,
                reasoning_effort,
                reasoning_summary: None,
                capabilities: None,
                permission_mode: None,
                orchestration: OrchestrationLimits::default(),
                identity: None,
                skills: Vec::new(),
                plugins: Vec::new(),
                mcp_servers: Vec::new(),
                mcp_allowlists: BTreeMap::new(),
                external_agents: Vec::new(),
                service_routes: Vec::new(),
                egress_policy: None,
                applies_to: interoperable::default_profile_uses(),
            },
        );

        let mut routes = BTreeMap::new();
        routes.insert(
            "default".to_owned(),
            TaskRoute {
                profile: "default".to_owned(),
            },
        );

        let document = ConfigDocument {
            version: CONFIG_VERSION,
            default_profile: "default".to_owned(),
            default_child_route: Some("default".to_owned()),
            permission_mode,
            permission_rules: Vec::new(),
            shell,
            providers,
            profiles,
            routes,
            plugins: BTreeMap::new(),
            mcp_servers: BTreeMap::new(),
            external_agents: BTreeMap::new(),
            service_connections: BTreeMap::new(),
            service_routes: BTreeMap::new(),
            egress_policies: BTreeMap::new(),
        };

        let rendered = toml::to_string_pretty(&document).map_err(ConfigError::Encode)?;
        Self::parse(&rendered)?;

        Ok(rendered)
    }

    pub(crate) fn load_registry_from(path: &Path) -> Result<ConnectionRegistry, ConfigError> {
        let input = read_config(path)?;
        Self::parse_registry(&input)
    }

    pub(crate) fn parse_registry(input: &str) -> Result<ConnectionRegistry, ConfigError> {
        let header: VersionHeader = toml::from_str(input).map_err(ConfigError::Decode)?;
        if !(MIN_CONFIG_VERSION..=CONFIG_VERSION).contains(&header.version) {
            return Err(ConfigError::UnsupportedVersion {
                found: header.version,
            });
        }
        let document: ConfigDocument = toml::from_str(input).map_err(ConfigError::Decode)?;
        validate_document(&document)?;
        Ok(registry_from_document(document))
    }

    pub(crate) fn add_connection(path: &Path, input: NewConnection) -> Result<(), ConfigError> {
        validate_name("provider", &input.id)?;
        if input.model.trim().is_empty() {
            return Err(ConfigError::EmptyModel {
                profile: format!("provider {} model", input.id),
            });
        }
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        let providers = document
            .get_mut("providers")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| ConfigError::Edit("providers must be a table".into()))?;
        if providers.contains_key(&input.id) {
            return Err(ConfigError::ConnectionAlreadyExists { name: input.id });
        }
        let mut connection = toml_edit::Table::new();
        connection["kind"] = toml_edit::value(input.kind.as_str());
        if let Some(base_url) = input.base_url {
            connection["base_url"] = toml_edit::value(base_url);
        }
        if let Some(reference) = input.credential {
            let mut credential = toml_edit::InlineTable::new();
            match reference {
                CredentialReference::Environment { variable } => {
                    credential.insert("source", "environment".into());
                    credential.insert("variable", variable.into());
                }
                CredentialReference::Stored { id } => {
                    credential.insert("source", "stored".into());
                    credential.insert("id", id.into());
                }
            }
            connection["credential"] =
                toml_edit::Item::Value(toml_edit::Value::InlineTable(credential));
        }
        if let Some(program) = input.codex_program {
            connection["codex_program"] = toml_edit::value(program);
        }
        if let Some(home) = input.codex_home {
            connection["codex_home"] = toml_edit::value(home.to_string_lossy().into_owned());
        }
        let mut models = toml_edit::Table::new();
        models[&input.model] = toml_edit::Item::Table(toml_edit::Table::new());
        connection["models"] = toml_edit::Item::Table(models);
        providers[&input.id] = toml_edit::Item::Table(connection);
        migrate_profile_connection_keys(&mut document)?;
        document["version"] = toml_edit::value(CONFIG_VERSION as i64);
        let rendered = document.to_string();
        Self::parse(&rendered)?;
        atomic_config_write(path, rendered.as_bytes())
    }

    pub(crate) fn remove_connection(path: &Path, id: &str) -> Result<(), ConfigError> {
        let registry = Self::load_registry_from(path)?;
        if !registry.connections.contains_key(id) {
            return Err(ConfigError::UnknownProvider {
                profile: "connection remove".into(),
                provider: id.to_owned(),
            });
        }
        let profiles = registry
            .profiles
            .values()
            .filter(|profile| profile.connection == id)
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        if !profiles.is_empty() {
            return Err(ConfigError::ConnectionReferenced {
                name: id.to_owned(),
                profiles,
            });
        }
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        document
            .get_mut("providers")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| ConfigError::Edit("providers must be a table".into()))?
            .remove(id);
        migrate_profile_connection_keys(&mut document)?;
        document["version"] = toml_edit::value(CONFIG_VERSION as i64);
        let rendered = document.to_string();
        Self::parse(&rendered)?;
        atomic_config_write(path, rendered.as_bytes())
    }

    pub(crate) fn add_external_agent(
        path: &Path,
        input: NewExternalAgent,
    ) -> Result<(), ConfigError> {
        validate_name("external agent", &input.id)?;
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        if document.get("external_agents").is_none() {
            document["external_agents"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let agents = document
            .get_mut("external_agents")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| ConfigError::Edit("external_agents must be a table".into()))?;
        if agents.contains_key(&input.id) {
            return Err(ConfigError::Edit(format!(
                "external agent {:?} already exists",
                input.id
            )));
        }
        let mut agent = toml_edit::Table::new();
        agent["endpoint"] = toml_edit::value(input.endpoint);
        agent["enabled"] = toml_edit::value(true);
        if let Some(reference) = input.credential {
            let mut credential = toml_edit::InlineTable::new();
            match reference {
                CredentialReference::Environment { variable } => {
                    credential.insert("source", "environment".into());
                    credential.insert("variable", variable.into());
                }
                CredentialReference::Stored { id } => {
                    credential.insert("source", "stored".into());
                    credential.insert("id", id.into());
                }
            }
            agent["credential"] = toml_edit::Item::Value(toml_edit::Value::InlineTable(credential));
        }
        if let Some(policy) = input.egress_policy {
            agent["egress_policy"] = toml_edit::value(policy);
        }
        agents[&input.id] = toml_edit::Item::Table(agent);
        document["version"] = toml_edit::value(CONFIG_VERSION as i64);
        let rendered = document.to_string();
        Self::parse_registry(&rendered)?;
        atomic_config_write(path, rendered.as_bytes())
    }

    pub(crate) fn remove_external_agent(path: &Path, id: &str) -> Result<(), ConfigError> {
        let registry = Self::load_registry_from(path)?;
        if !registry.external_agents.contains_key(id) {
            return Err(ConfigError::Edit(format!("unknown external agent {id:?}")));
        }
        let profiles = registry
            .profiles
            .values()
            .filter(|profile| profile.external_agents.iter().any(|value| value == id))
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        if !profiles.is_empty() {
            return Err(ConfigError::Edit(format!(
                "external agent {id:?} is referenced by profiles {}",
                profiles.join(", ")
            )));
        }
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        document
            .get_mut("external_agents")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| ConfigError::Edit("external_agents must be a table".into()))?
            .remove(id);
        let rendered = document.to_string();
        Self::parse_registry(&rendered)?;
        atomic_config_write(path, rendered.as_bytes())
    }

    pub(crate) fn add_service_route(
        path: &Path,
        input: NewServiceRoute,
    ) -> Result<(), ConfigError> {
        validate_name("service route", &input.route)?;
        validate_name("service connection", &input.connection)?;
        validate_name("profile", &input.profile)?;
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        migrate_profile_connection_keys(&mut document)?;

        if !document
            .get("profiles")
            .and_then(toml_edit::Item::as_table)
            .is_some_and(|profiles| profiles.contains_key(&input.profile))
        {
            return Err(ConfigError::Edit(format!(
                "unknown profile {:?}",
                input.profile
            )));
        }
        ensure_table(&mut document, "service_connections")?;
        let service_connections = document["service_connections"]
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit("service_connections must be a table".into()))?;
        if service_connections.contains_key(&input.connection) {
            return Err(ConfigError::Edit(format!(
                "service connection {:?} already exists",
                input.connection
            )));
        }
        let mut connection = toml_edit::Table::new();
        connection["adapter"] = toml_edit::value(input.adapter);
        if let Some(base_url) = input.base_url {
            connection["base_url"] = toml_edit::value(base_url);
        }
        connection["credential"] = credential_item(input.credential);
        service_connections[&input.connection] = toml_edit::Item::Table(connection);

        ensure_table(&mut document, "egress_policies")?;
        let required = ["prompt_text", "selected_artifacts"];
        let route_policy = insert_exact_egress_policy(
            &mut document,
            &format!("service-route-{}", input.route),
            &required,
        )?;
        let profile_policy = profile_egress_policy_with(
            &mut document,
            &input.profile,
            &format!("profile-{}-service-{}", input.profile, input.route),
            &required,
        )?;
        let profile = document["profiles"][&input.profile]
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit("profile must be a table".into()))?;
        profile["egress_policy"] = toml_edit::value(profile_policy);
        merge_string_array(profile, "service_routes", &[&input.route])?;

        ensure_table(&mut document, "service_routes")?;
        let service_routes = document["service_routes"]
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit("service_routes must be a table".into()))?;
        if service_routes.contains_key(&input.route) {
            return Err(ConfigError::Edit(format!(
                "service route {:?} already exists",
                input.route
            )));
        }
        if input.make_default {
            for (_, item) in service_routes.iter_mut() {
                if let Some(route) = item.as_table_mut()
                    && route.get("operation").and_then(toml_edit::Item::as_str)
                        == Some(input.operation.as_str())
                {
                    route["default"] = toml_edit::value(false);
                }
            }
        }
        let mut route = toml_edit::Table::new();
        route["operation"] = toml_edit::value(input.operation);
        route["connection"] = toml_edit::value(input.connection);
        route["model"] = toml_edit::value(input.model);
        if let Some(description) = input.description {
            route["description"] = toml_edit::value(description);
        }
        route["default"] = toml_edit::value(input.make_default);
        route["egress_policy"] = toml_edit::value(route_policy);
        service_routes[&input.route] = toml_edit::Item::Table(route);

        document["version"] = toml_edit::value(CONFIG_VERSION as i64);
        let rendered = document.to_string();
        Self::parse_registry(&rendered)?;
        atomic_config_write_with_backup(path, rendered.as_bytes())
    }

    pub(crate) fn add_mcp_server(path: &Path, input: NewMcpServer) -> Result<(), ConfigError> {
        validate_name("MCP server", &input.id)?;
        validate_name("profile", &input.profile)?;
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        migrate_profile_connection_keys(&mut document)?;
        if !document
            .get("profiles")
            .and_then(toml_edit::Item::as_table)
            .is_some_and(|profiles| profiles.contains_key(&input.profile))
        {
            return Err(ConfigError::Edit(format!(
                "unknown profile {:?}",
                input.profile
            )));
        }
        ensure_table(&mut document, "mcp_servers")?;
        if document["mcp_servers"]
            .as_table()
            .is_some_and(|servers| servers.contains_key(&input.id))
        {
            return Err(ConfigError::Edit(format!(
                "MCP server {:?} already exists",
                input.id
            )));
        }
        let mut server = toml_edit::Table::new();
        match input.declaration {
            McpServerDeclaration::Stdio {
                command,
                args,
                environment,
                cwd,
                enabled,
                ..
            } => {
                server["transport"] = toml_edit::value("stdio");
                server["command"] = toml_edit::value(command);
                server["args"] = toml_edit::value(toml_owned_string_array(args));
                if !environment.is_empty() {
                    let mut values = toml_edit::InlineTable::new();
                    for (name, value) in environment {
                        values.insert(&name, value.into());
                    }
                    server["environment"] =
                        toml_edit::Item::Value(toml_edit::Value::InlineTable(values));
                }
                if let Some(cwd) = cwd {
                    server["cwd"] = toml_edit::value(cwd.to_string_lossy().into_owned());
                }
                server["enabled"] = toml_edit::value(enabled);
            }
            McpServerDeclaration::StreamableHttp {
                url,
                credential,
                enabled,
                ..
            } => {
                server["transport"] = toml_edit::value("streamable_http");
                server["url"] = toml_edit::value(url);
                if let Some(credential) = credential {
                    server["credential"] = credential_item(credential);
                }
                server["enabled"] = toml_edit::value(enabled);
            }
        }

        ensure_table(&mut document, "egress_policies")?;
        let required = ["prompt_text", "workspace_metadata"];
        let server_policy = insert_exact_egress_policy(
            &mut document,
            &format!("mcp-server-{}", input.id),
            &required,
        )?;
        let profile_policy = profile_egress_policy_with(
            &mut document,
            &input.profile,
            &format!("profile-{}-mcp-{}", input.profile, input.id),
            &required,
        )?;
        server["egress_policy"] = toml_edit::value(server_policy);
        document["mcp_servers"]
            .as_table_mut()
            .expect("table was ensured")
            .insert(&input.id, toml_edit::Item::Table(server));

        let profile = document["profiles"][&input.profile]
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit("profile must be a table".into()))?;
        profile["egress_policy"] = toml_edit::value(profile_policy);
        merge_string_array(profile, "mcp_servers", &[&input.id])?;
        if profile.get("mcp_allowlists").is_none() {
            profile["mcp_allowlists"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let allowlists = profile["mcp_allowlists"]
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit("profile mcp_allowlists must be a table".into()))?;
        let mut allowlist = toml_edit::Table::new();
        allowlist["tools"] = toml_edit::value(toml_owned_string_array(input.selection.tools));
        allowlist["resources"] =
            toml_edit::value(toml_owned_string_array(input.selection.resources));
        allowlist["resource_templates"] =
            toml_edit::value(toml_owned_string_array(input.selection.resource_templates));
        allowlist["prompts"] = toml_edit::value(toml_owned_string_array(input.selection.prompts));
        allowlists[&input.id] = toml_edit::Item::Table(allowlist);

        document["version"] = toml_edit::value(CONFIG_VERSION as i64);
        let rendered = document.to_string();
        Self::parse_registry(&rendered)?;
        atomic_config_write_with_backup(path, rendered.as_bytes())
    }

    pub(crate) fn remove_mcp_server(path: &Path, id: &str) -> Result<(), ConfigError> {
        let registry = Self::load_registry_from(path)?;
        if !registry.mcp_servers.contains_key(id) {
            return Err(ConfigError::Edit(format!("unknown MCP server {id:?}")));
        }
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        document
            .get_mut("mcp_servers")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| ConfigError::Edit("mcp_servers must be a table".into()))?
            .remove(id);
        if let Some(profiles) = document
            .get_mut("profiles")
            .and_then(toml_edit::Item::as_table_mut)
        {
            for (_, item) in profiles.iter_mut() {
                if let Some(profile) = item.as_table_mut() {
                    remove_string_array_value(profile, "mcp_servers", id)?;
                    if let Some(allowlists) = profile
                        .get_mut("mcp_allowlists")
                        .and_then(toml_edit::Item::as_table_mut)
                    {
                        allowlists.remove(id);
                    }
                }
            }
        }
        let rendered = document.to_string();
        Self::parse_registry(&rendered)?;
        atomic_config_write_with_backup(path, rendered.as_bytes())
    }

    pub(crate) fn remove_service_route(path: &Path, route: &str) -> Result<(), ConfigError> {
        let registry = Self::load_registry_from(path)?;
        let declaration = registry
            .service_routes
            .get(route)
            .ok_or_else(|| ConfigError::Edit(format!("unknown service route {route:?}")))?;
        let connection = declaration.connection.clone();
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        document
            .get_mut("service_routes")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| ConfigError::Edit("service_routes must be a table".into()))?
            .remove(route);
        if let Some(profiles) = document
            .get_mut("profiles")
            .and_then(toml_edit::Item::as_table_mut)
        {
            for (_, item) in profiles.iter_mut() {
                if let Some(profile) = item.as_table_mut() {
                    remove_string_array_value(profile, "service_routes", route)?;
                }
            }
        }
        let connection_still_used = document
            .get("service_routes")
            .and_then(toml_edit::Item::as_table)
            .is_some_and(|routes| {
                routes.iter().any(|(_, item)| {
                    item.as_table()
                        .and_then(|route| route.get("connection"))
                        .and_then(toml_edit::Item::as_str)
                        == Some(connection.as_str())
                })
            });
        if !connection_still_used
            && let Some(connections) = document
                .get_mut("service_connections")
                .and_then(toml_edit::Item::as_table_mut)
        {
            connections.remove(&connection);
        }
        let rendered = document.to_string();
        Self::parse_registry(&rendered)?;
        atomic_config_write_with_backup(path, rendered.as_bytes())
    }

    pub(crate) fn add_profile(path: &Path, input: NewProfile) -> Result<(), ConfigError> {
        validate_name("profile", &input.id)?;
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        let profiles = profiles_table_mut(&mut document)?;
        if profiles.contains_key(&input.id) {
            return Err(ConfigError::Edit(format!(
                "profile {:?} already exists",
                input.id
            )));
        }
        let mut profile = toml_edit::Table::new();
        profile["profile_id"] = toml_edit::value(uuid::Uuid::new_v4().to_string());
        profile["connection"] = toml_edit::value(input.connection);
        profile["model"] = toml_edit::value(input.model);
        profiles[&input.id] = toml_edit::Item::Table(profile);
        validate_and_write_profile_edit(path, document)
    }

    pub(crate) fn update_profile(
        path: &Path,
        id: &str,
        update: ProfileUpdate,
    ) -> Result<(), ConfigError> {
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        let profile = profile_table_mut(&mut document, id)?;
        if let Some(value) = update.connection {
            profile["connection"] = toml_edit::value(value);
        }
        if let Some(value) = update.model {
            profile["model"] = toml_edit::value(value);
        }
        set_optional_string(profile, "reasoning_effort", update.reasoning_effort);
        set_optional_string(profile, "reasoning_summary", update.reasoning_summary);
        set_optional_string(profile, "identity", update.identity);
        if let Some(value) = update.permission_mode {
            match value {
                Some(value) => profile["permission_mode"] = toml_edit::value(value.as_str()),
                None => {
                    profile.remove("permission_mode");
                }
            }
        }
        if let Some(value) = update.max_tool_rounds {
            profile["max_tool_rounds"] = toml_edit::value(value as i64);
        }
        validate_and_write_profile_edit(path, document)
    }

    pub(crate) fn set_profile_skill(
        path: &Path,
        id: &str,
        skill: &str,
        enabled: bool,
    ) -> Result<(), ConfigError> {
        Self::set_profile_reference(path, id, "skills", skill, enabled)
    }

    pub(crate) fn set_profile_plugin(
        path: &Path,
        id: &str,
        plugin: &str,
        enabled: bool,
    ) -> Result<(), ConfigError> {
        Self::set_profile_reference(path, id, "plugins", plugin, enabled)
    }

    fn set_profile_reference(
        path: &Path,
        id: &str,
        key: &str,
        value: &str,
        enabled: bool,
    ) -> Result<(), ConfigError> {
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        let profile = profile_table_mut(&mut document, id)?;
        let mut values = profile
            .get(key)
            .and_then(toml_edit::Item::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(toml_edit::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if enabled {
            if !values.iter().any(|item| item == value) {
                values.push(value.to_owned());
            }
        } else {
            values.retain(|item| item != value);
        }
        values.sort();
        let mut array = toml_edit::Array::new();
        for value in values {
            array.push(value);
        }
        profile[key] = toml_edit::value(array);
        validate_and_write_profile_edit(path, document)
    }

    pub(crate) fn duplicate_profile(
        path: &Path,
        source_id: &str,
        id: &str,
    ) -> Result<(), ConfigError> {
        validate_name("profile", id)?;
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        let profiles = profiles_table_mut(&mut document)?;
        if profiles.contains_key(id) {
            return Err(ConfigError::Edit(format!("profile {id:?} already exists")));
        }
        let mut duplicate = profiles
            .get(source_id)
            .cloned()
            .ok_or_else(|| ConfigError::Edit(format!("unknown profile {source_id:?}")))?;
        let table = duplicate
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit(format!("profile {source_id:?} must be a table")))?;
        table["profile_id"] = toml_edit::value(uuid::Uuid::new_v4().to_string());
        table.remove("archived");
        profiles.insert(id, duplicate);
        validate_and_write_profile_edit(path, document)
    }

    pub(crate) fn rename_profile(path: &Path, old: &str, new: &str) -> Result<(), ConfigError> {
        validate_name("profile", new)?;
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        {
            let profiles = profiles_table_mut(&mut document)?;
            if profiles.contains_key(new) {
                return Err(ConfigError::Edit(format!("profile {new:?} already exists")));
            }
            let profile = profiles
                .remove(old)
                .ok_or_else(|| ConfigError::Edit(format!("unknown profile {old:?}")))?;
            profiles.insert(new, profile);
        }
        if document
            .get("default_profile")
            .and_then(toml_edit::Item::as_str)
            == Some(old)
        {
            document["default_profile"] = toml_edit::value(new);
        }
        if let Some(routes) = document
            .get_mut("routes")
            .and_then(toml_edit::Item::as_table_mut)
        {
            for (_, route) in routes.iter_mut() {
                if let Some(table) = route.as_table_mut()
                    && table.get("profile").and_then(toml_edit::Item::as_str) == Some(old)
                {
                    table["profile"] = toml_edit::value(new);
                }
            }
        }
        validate_and_write_profile_edit(path, document)
    }

    pub(crate) fn set_profile_archived(
        path: &Path,
        id: &str,
        archived: bool,
    ) -> Result<(), ConfigError> {
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        if archived
            && document
                .get("default_profile")
                .and_then(toml_edit::Item::as_str)
                == Some(id)
        {
            return Err(ConfigError::Edit(
                "the default profile cannot be archived".into(),
            ));
        }
        let profile = profile_table_mut(&mut document, id)?;
        if archived {
            profile["archived"] = toml_edit::value(true);
        } else {
            profile.remove("archived");
        }
        validate_and_write_profile_edit(path, document)
    }

    pub(crate) fn delete_profile(path: &Path, id: &str) -> Result<(), ConfigError> {
        let registry = Self::load_registry_from(path)?;
        if registry.default_profile == id {
            return Err(ConfigError::Edit(
                "the default profile cannot be deleted".into(),
            ));
        }
        let routes = registry
            .routes
            .values()
            .filter(|route| route.profile == id)
            .map(|route| route.id.clone())
            .collect::<Vec<_>>();
        if !routes.is_empty() {
            return Err(ConfigError::Edit(format!(
                "profile {id:?} is referenced by route(s): {}",
                routes.join(", ")
            )));
        }
        let source = read_config(path)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        if profiles_table_mut(&mut document)?.remove(id).is_none() {
            return Err(ConfigError::Edit(format!("unknown profile {id:?}")));
        }
        validate_and_write_profile_edit(path, document)
    }
}

fn profiles_table_mut(
    document: &mut toml_edit::DocumentMut,
) -> Result<&mut toml_edit::Table, ConfigError> {
    document
        .get_mut("profiles")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| ConfigError::Edit("profiles must be a table".into()))
}

fn profile_table_mut<'a>(
    document: &'a mut toml_edit::DocumentMut,
    id: &str,
) -> Result<&'a mut toml_edit::Table, ConfigError> {
    profiles_table_mut(document)?
        .get_mut(id)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| ConfigError::Edit(format!("unknown profile {id:?}")))
}

fn set_optional_string(table: &mut toml_edit::Table, key: &str, value: Option<Option<String>>) {
    if let Some(value) = value {
        match value {
            Some(value) => table[key] = toml_edit::value(value),
            None => {
                table.remove(key);
            }
        }
    }
}

fn ensure_table(document: &mut toml_edit::DocumentMut, key: &str) -> Result<(), ConfigError> {
    if document.get(key).is_none() {
        document[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    document[key]
        .as_table()
        .map(|_| ())
        .ok_or_else(|| ConfigError::Edit(format!("{key} must be a table")))
}

fn insert_exact_egress_policy(
    document: &mut toml_edit::DocumentMut,
    base: &str,
    allowed: &[&str],
) -> Result<String, ConfigError> {
    ensure_table(document, "egress_policies")?;
    let policies = document["egress_policies"]
        .as_table_mut()
        .ok_or_else(|| ConfigError::Edit("egress_policies must be a table".into()))?;
    let name = unique_table_name(policies, base)?;
    let mut policy = toml_edit::Table::new();
    policy["allowed"] = toml_edit::value(toml_string_array(allowed));
    policies.insert(&name, toml_edit::Item::Table(policy));
    Ok(name)
}

fn profile_egress_policy_with(
    document: &mut toml_edit::DocumentMut,
    profile: &str,
    derived_name: &str,
    required: &[&str],
) -> Result<String, ConfigError> {
    let existing = document["profiles"][profile]
        .get("egress_policy")
        .and_then(toml_edit::Item::as_str)
        .map(str::to_owned);
    let mut allowed = match existing.as_deref() {
        Some(name) => document["egress_policies"][name]
            .get("allowed")
            .and_then(toml_edit::Item::as_array)
            .ok_or_else(|| ConfigError::Edit(format!("egress policy {name:?} is invalid")))?
            .iter()
            .filter_map(toml_edit::Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    if required
        .iter()
        .all(|required| allowed.iter().any(|value| value == required))
    {
        return existing.ok_or_else(|| {
            ConfigError::Edit("profile egress policy resolution was inconsistent".into())
        });
    }
    for required in required {
        if !allowed.iter().any(|value| value == required) {
            allowed.push((*required).to_owned());
        }
    }
    allowed.sort();
    allowed.dedup();
    let borrowed = allowed.iter().map(String::as_str).collect::<Vec<_>>();
    insert_exact_egress_policy(document, derived_name, &borrowed)
}

fn unique_table_name(table: &toml_edit::Table, base: &str) -> Result<String, ConfigError> {
    if !table.contains_key(base) {
        return Ok(base.to_owned());
    }
    for suffix in 2..=1_000 {
        let candidate = format!("{base}-{suffix}");
        if !table.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    Err(ConfigError::Edit(format!(
        "could not allocate a unique egress policy derived from {base:?}"
    )))
}

fn credential_item(reference: CredentialReference) -> toml_edit::Item {
    let mut credential = toml_edit::InlineTable::new();
    match reference {
        CredentialReference::Environment { variable } => {
            credential.insert("source", "environment".into());
            credential.insert("variable", variable.into());
        }
        CredentialReference::Stored { id } => {
            credential.insert("source", "stored".into());
            credential.insert("id", id.into());
        }
    }
    toml_edit::Item::Value(toml_edit::Value::InlineTable(credential))
}

fn toml_string_array(values: &[&str]) -> toml_edit::Array {
    let mut array = toml_edit::Array::new();
    for value in values {
        array.push(*value);
    }
    array
}

fn toml_owned_string_array(values: Vec<String>) -> toml_edit::Array {
    let mut array = toml_edit::Array::new();
    for value in values {
        array.push(value);
    }
    array
}

fn merge_string_array(
    table: &mut toml_edit::Table,
    key: &str,
    added: &[&str],
) -> Result<(), ConfigError> {
    let mut values = table
        .get(key)
        .and_then(toml_edit::Item::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for value in added {
        if !values.iter().any(|existing| existing == value) {
            values.push((*value).to_owned());
        }
    }
    values.sort();
    values.dedup();
    let mut array = toml_edit::Array::new();
    for value in values {
        array.push(value);
    }
    table[key] = toml_edit::value(array);
    Ok(())
}

fn remove_string_array_value(
    table: &mut toml_edit::Table,
    key: &str,
    removed: &str,
) -> Result<(), ConfigError> {
    let Some(current) = table.get(key) else {
        return Ok(());
    };
    let array = current
        .as_array()
        .ok_or_else(|| ConfigError::Edit(format!("{key} must be an array")))?;
    let values = array
        .iter()
        .filter_map(toml_edit::Value::as_str)
        .filter(|value| *value != removed)
        .collect::<Vec<_>>();
    table[key] = toml_edit::value(toml_string_array(&values));
    Ok(())
}

fn validate_and_write_profile_edit(
    path: &Path,
    mut document: toml_edit::DocumentMut,
) -> Result<(), ConfigError> {
    migrate_profile_connection_keys(&mut document)?;
    document["version"] = toml_edit::value(CONFIG_VERSION as i64);
    let rendered = document.to_string();
    XanaConfig::parse_registry(&rendered)?;
    atomic_config_write(path, rendered.as_bytes())
}

fn migrate_profile_connection_keys(
    document: &mut toml_edit::DocumentMut,
) -> Result<(), ConfigError> {
    let profiles = document
        .get_mut("profiles")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| ConfigError::Edit("profiles must be a table".into()))?;
    for (name, item) in profiles.iter_mut() {
        let profile = item
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit(format!("profile {name:?} must be a table")))?;
        if profile.contains_key("provider") && profile.contains_key("connection") {
            return Err(ConfigError::Edit(format!(
                "profile {name:?} cannot contain both provider and connection"
            )));
        }
        if let Some(connection) = profile.remove("provider") {
            profile.insert("connection", connection);
        }
    }
    Ok(())
}

fn read_config(path: &Path) -> Result<String, ConfigError> {
    bounded_file::read_to_string(path, MAX_CONFIG_BYTES).map_err(|error| match error {
        bounded_file::BoundedReadError::TooLarge { actual, limit, .. } => {
            ConfigError::TooLarge { actual, limit }
        }
        bounded_file::BoundedReadError::Io { path, source } => ConfigError::Io { path, source },
    })
}

fn atomic_config_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let mut file =
        atomic_write_file::AtomicWriteFile::open(path).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.commit().map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })
}

fn atomic_config_write_with_backup(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let previous = read_config(path)?.into_bytes();
    let backup = path.with_extension("toml.bak");
    let prior_backup = match bounded_file::read(&backup, MAX_CONFIG_BYTES) {
        Ok(bytes) => Some(bytes),
        Err(bounded_file::BoundedReadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => {
            return Err(match error {
                bounded_file::BoundedReadError::TooLarge { actual, limit, .. } => {
                    ConfigError::TooLarge { actual, limit }
                }
                bounded_file::BoundedReadError::Io { path, source } => {
                    ConfigError::Io { path, source }
                }
            });
        }
    };
    atomic_config_write(&backup, &previous)?;
    if let Err(error) = atomic_config_write(path, bytes) {
        match prior_backup {
            Some(bytes) => atomic_config_write(&backup, &bytes)?,
            None => match fs::remove_file(&backup) {
                Ok(()) => {}
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ConfigError::Io {
                        path: backup,
                        source,
                    });
                }
            },
        }
        return Err(error);
    }
    Ok(())
}

impl ConfigError {
    pub(crate) fn is_missing_config(&self) -> bool {
        matches!(
            self,
            Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound
        )
    }
}

fn validate_document(document: &ConfigDocument) -> Result<(), ConfigError> {
    Shell::resolve(document.shell.clone()).map_err(ConfigError::InvalidShell)?;
    PermissionPolicy::validate_rules(&document.permission_rules)
        .map_err(ConfigError::InvalidPermissionPolicy)?;

    for (name, provider) in &document.providers {
        validate_name("provider", name)?;
        if provider.kind == ProviderKind::Codex && provider.base_url.is_some() {
            return Err(ConfigError::InvalidCodexConfiguration {
                provider: name.clone(),
                reason: "Codex app-server uses stdio and does not accept base_url",
            });
        }
        if provider.kind != ProviderKind::Codex && provider.codex_program.is_some() {
            return Err(ConfigError::InvalidCodexConfiguration {
                provider: name.clone(),
                reason: "only Codex connections may set codex_program",
            });
        }
        if provider
            .codex_program
            .as_deref()
            .is_some_and(|program| program.trim().is_empty())
        {
            return Err(ConfigError::InvalidCodexConfiguration {
                provider: name.clone(),
                reason: "codex_program cannot be blank",
            });
        }
        if let Some(base_url) = provider.base_url.as_deref() {
            validate_base_url(name, base_url)?;
        } else if default_base_url(provider.kind).is_none() {
            return Err(ConfigError::InvalidBaseUrl {
                provider: name.clone(),
                reason: "this provider kind requires base_url",
            });
        }
        if provider.kind == ProviderKind::Codex && provider.credential.is_some() {
            return Err(ConfigError::InvalidCredential {
                provider: name.clone(),
                reason: "Codex app-server owns its account credentials",
            });
        }
        if matches!(
            provider.kind,
            ProviderKind::OpenAi | ProviderKind::OpenRouter | ProviderKind::Anthropic
        ) && provider.credential.is_none()
        {
            return Err(ConfigError::InvalidCredential {
                provider: name.clone(),
                reason: "an API connection requires a credential reference",
            });
        }
        if let Some(credential) = &provider.credential {
            validate_credential(name, credential)?;
        }
        if let Some(home) = &provider.codex_home
            && (!home.is_absolute() || provider.kind != ProviderKind::Codex)
        {
            return Err(ConfigError::InvalidCodexHome {
                provider: name.clone(),
            });
        }
        for (model, descriptor) in &provider.models {
            if model.trim().is_empty() {
                return Err(ConfigError::EmptyModel {
                    profile: format!("provider {name} model override"),
                });
            }
            for modality in &descriptor.input_modalities {
                if !matches!(modality.as_str(), "text" | "image") {
                    return Err(ConfigError::InvalidModelModality {
                        provider: name.clone(),
                        model: model.clone(),
                        modality: modality.clone(),
                    });
                }
            }
        }
    }
    if document
        .profiles
        .get(&document.default_profile)
        .is_some_and(|profile| profile.archived)
    {
        return Err(ConfigError::Edit(
            "the default profile cannot be archived".into(),
        ));
    }

    for (name, profile) in &document.profiles {
        validate_name("profile", name)?;

        if !document.providers.contains_key(&profile.connection) {
            return Err(ConfigError::UnknownProvider {
                profile: name.clone(),
                provider: profile.connection.clone(),
            });
        }

        if profile.model.trim().is_empty() {
            return Err(ConfigError::EmptyModel {
                profile: name.clone(),
            });
        }

        if !(1..=MAX_MAX_TOOL_ROUNDS).contains(&profile.max_tool_rounds) {
            return Err(ConfigError::InvalidToolRoundLimit {
                profile: name.clone(),
                value: profile.max_tool_rounds,
            });
        }

        if profile
            .reasoning_effort
            .as_deref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 64)
        {
            return Err(ConfigError::InvalidProfileOption {
                profile: name.clone(),
                option: "reasoning_effort",
                reason: "expected 1 to 64 non-blank bytes",
            });
        }
        if profile
            .reasoning_summary
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "concise" | "detailed" | "off" | "none"))
        {
            return Err(ConfigError::InvalidProfileOption {
                profile: name.clone(),
                option: "reasoning_summary",
                reason: "expected auto, concise, detailed, or off",
            });
        }
        if let Some(capabilities) = &profile.capabilities {
            let mut unique = std::collections::BTreeSet::new();
            for capability in capabilities {
                if !valid_stable_id(capability) {
                    return Err(ConfigError::InvalidCapability {
                        profile: name.clone(),
                        capability: capability.clone(),
                    });
                }
                if !unique.insert(capability) {
                    return Err(ConfigError::DuplicateCapability {
                        profile: name.clone(),
                        capability: capability.clone(),
                    });
                }
            }
        }
        validate_orchestration_limits(name, &profile.orchestration)?;
    }

    for (name, route) in &document.routes {
        validate_name("route", name)?;
        if !document.profiles.contains_key(&route.profile) {
            return Err(ConfigError::UnknownProfile {
                route: name.clone(),
                profile: route.profile.clone(),
            });
        }
        if document
            .profiles
            .get(&route.profile)
            .is_some_and(|profile| profile.archived)
        {
            return Err(ConfigError::InvalidInteroperableConfig {
                section: "route",
                name: name.clone(),
                reason: format!("references archived profile {:?}", route.profile),
            });
        }
    }

    interoperable::validate(document)?;

    validate_name("default profile", &document.default_profile)?;

    if !document.profiles.contains_key(&document.default_profile) {
        return Err(ConfigError::MissingDefaultProfile {
            name: document.default_profile.clone(),
        });
    }

    if let Some(route) = &document.default_child_route {
        validate_name("default child route", route)?;
        if !document.routes.contains_key(route) {
            return Err(ConfigError::MissingDefaultChildRoute {
                route: route.clone(),
            });
        }
    }

    Ok(())
}

fn validate_and_resolve(mut document: ConfigDocument) -> Result<XanaConfig, ConfigError> {
    validate_document(&document)?;

    let default_profile_name = document.default_profile;
    let profile = document.profiles.remove(&default_profile_name).ok_or(
        ConfigError::MissingDefaultProfile {
            name: default_profile_name,
        },
    )?;

    let provider_name = profile.connection;
    let provider = document
        .providers
        .remove(&provider_name)
        .expect("all profile provider references were validated");

    let base_url = provider
        .base_url
        .clone()
        .or_else(|| default_base_url(provider.kind).map(str::to_owned))
        .expect("provider base URL requirements were validated");

    Ok(XanaConfig {
        provider_name,
        provider_kind: provider.kind,
        base_url,
        credential: provider.credential,
        codex_program: provider.codex_program,
        codex_home: provider.codex_home,
        model: profile.model,
        permission_mode: document.permission_mode,
        permission_rules: document.permission_rules,
        shell: document.shell,
        max_tool_rounds: profile.max_tool_rounds,
    })
}

fn registry_from_document(document: ConfigDocument) -> ConnectionRegistry {
    let mut connections = document
        .providers
        .into_iter()
        .map(|(id, provider)| {
            let connection = ConnectionConfig {
                id: id.clone(),
                kind: provider.kind,
                base_url: provider
                    .base_url
                    .or_else(|| default_base_url(provider.kind).map(str::to_owned)),
                credential: provider.credential,
                models: provider.models,
                codex_program: provider.codex_program,
                codex_home: provider.codex_home,
            };
            (id, connection)
        })
        .collect::<BTreeMap<_, _>>();
    let profiles = document
        .profiles
        .into_iter()
        .map(|(id, profile)| {
            connections
                .get_mut(&profile.connection)
                .expect("profile references were validated")
                .models
                .entry(profile.model.clone())
                .or_default();
            (
                id.clone(),
                ProfileConfig {
                    profile_id: profile
                        .profile_id
                        .unwrap_or_else(|| stable_profile_id("global", &id)),
                    id,
                    archived: profile.archived,
                    connection: profile.connection,
                    model: profile.model,
                    max_tool_rounds: profile.max_tool_rounds,
                    reasoning_effort: profile.reasoning_effort,
                    reasoning_summary: profile.reasoning_summary,
                    capabilities: profile.capabilities,
                    permission_mode: profile.permission_mode,
                    orchestration: profile.orchestration,
                    identity: profile.identity,
                    skills: profile.skills,
                    plugins: profile.plugins,
                    mcp_servers: profile.mcp_servers,
                    mcp_allowlists: profile.mcp_allowlists,
                    external_agents: profile.external_agents,
                    service_routes: profile.service_routes,
                    egress_policy: profile.egress_policy,
                    applies_to: profile.applies_to,
                },
            )
        })
        .collect();
    let routes = document
        .routes
        .into_iter()
        .map(|(id, route)| {
            (
                id.clone(),
                RouteConfig {
                    id,
                    profile: route.profile,
                },
            )
        })
        .collect();
    ConnectionRegistry {
        default_profile: document.default_profile,
        default_child_route: document.default_child_route,
        permission_mode: document.permission_mode,
        permission_rules: document.permission_rules,
        connections,
        profiles,
        routes,
        plugins: document.plugins,
        mcp_servers: document.mcp_servers,
        external_agents: document.external_agents,
        service_connections: document.service_connections,
        service_routes: document.service_routes,
        egress_policies: document.egress_policies,
    }
}

pub(crate) fn stable_profile_id(scope: &str, name: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("https://github.com/labcoder/xana/profile/{scope}/{name}").as_bytes(),
    )
}

fn default_base_url(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::OpenAiCompat => None,
        ProviderKind::Ollama => Some("http://localhost:11434/v1"),
        ProviderKind::OpenAi => Some("https://api.openai.com/v1"),
        ProviderKind::OpenRouter => Some("https://openrouter.ai/api/v1"),
        ProviderKind::Anthropic => Some("https://api.anthropic.com"),
        ProviderKind::Codex => Some("codex-app-server://stdio"),
    }
}

fn validate_credential(provider: &str, reference: &CredentialReference) -> Result<(), ConfigError> {
    match reference {
        CredentialReference::Environment { variable } => {
            let mut bytes = variable.bytes();
            let valid = bytes
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
            if !valid {
                return Err(ConfigError::InvalidCredential {
                    provider: provider.to_owned(),
                    reason: "environment variable names must use ASCII letters, digits, and underscores",
                });
            }
        }
        CredentialReference::Stored { id } if !valid_name(id) => {
            return Err(ConfigError::InvalidCredential {
                provider: provider.to_owned(),
                reason: "stored credential ids use the same syntax as provider names",
            });
        }
        CredentialReference::Stored { .. } => {}
    }
    Ok(())
}

fn validate_name(section: &'static str, name: &str) -> Result<(), ConfigError> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(ConfigError::InvalidName {
            section,
            name: name.to_owned(),
        })
    }
}

fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();

    let Some(first) = bytes.next() else {
        return false;
    };

    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

fn valid_stable_id(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'
                        || byte == b'-'
                })
        })
}

fn validate_orchestration_limits(
    profile: &str,
    limits: &OrchestrationLimits,
) -> Result<(), ConfigError> {
    let check = |field, value: u64, maximum: u64| {
        if value == 0 || value > maximum {
            Err(ConfigError::InvalidOrchestrationLimit {
                profile: profile.to_owned(),
                field,
                value,
                maximum,
            })
        } else {
            Ok(())
        }
    };
    check(
        "max_fan_out",
        limits.max_fan_out as u64,
        MAX_ROUTE_FAN_OUT as u64,
    )?;
    check(
        "max_descendants",
        limits.max_descendants as u64,
        MAX_ROUTE_DESCENDANTS as u64,
    )?;
    check(
        "max_concurrency",
        limits.max_concurrency as u64,
        MAX_ROUTE_CONCURRENCY as u64,
    )?;
    if limits.max_concurrency > limits.max_descendants {
        return Err(ConfigError::InvalidProfileOption {
            profile: profile.to_owned(),
            option: "orchestration.max_concurrency",
            reason: "cannot exceed max_descendants",
        });
    }
    if limits.max_fan_out > limits.max_descendants {
        return Err(ConfigError::InvalidProfileOption {
            profile: profile.to_owned(),
            option: "orchestration.max_fan_out",
            reason: "cannot exceed max_descendants",
        });
    }
    check(
        "deadline_seconds",
        limits.deadline_seconds,
        MAX_ROUTE_DEADLINE_SECONDS,
    )?;
    check(
        "max_context_tokens",
        limits.max_context_tokens as u64,
        MAX_ROUTE_CONTEXT_TOKENS as u64,
    )?;
    check(
        "max_report_bytes",
        limits.max_report_bytes as u64,
        MAX_ROUTE_REPORT_BYTES as u64,
    )?;
    check(
        "max_artifact_bytes",
        limits.max_artifact_bytes as u64,
        MAX_ROUTE_ARTIFACT_BYTES as u64,
    )?;
    Ok(())
}

fn validate_base_url(provider: &str, base_url: &str) -> Result<(), ConfigError> {
    let invalid = |reason| ConfigError::InvalidBaseUrl {
        provider: provider.to_owned(),
        reason,
    };

    let url = Url::parse(base_url).map_err(|_| invalid("expected an absolute URL"))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("scheme must be http or https"));
    }

    if !url.has_host() {
        return Err(invalid("URL must include a host"));
    }

    if url.cannot_be_a_base() {
        return Err(invalid("URL cannot be used as a base URL"));
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("credentials do not belong in config.toml"));
    }

    if url.query().is_some() {
        return Err(invalid("query strings are not allowed"));
    }

    if url.fragment().is_some() {
        return Err(invalid("fragments are not allowed"));
    }

    Ok(())
}

#[cfg(test)]
mod tests;
