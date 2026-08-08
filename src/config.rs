//! Versioned shared configuration loading, validation, and initial rendering.
//!
//! This module owns the human-authored TOML schema and resolves it into an
//! immutable agent snapshot. It does not read process environment variables or
//! invent initializer-specific validation.

use crate::{
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

const CONFIG_VERSION: u32 = 2;
const MIN_CONFIG_VERSION: u32 = 1;
const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;
const MAX_MAX_TOOL_ROUNDS: usize = 64;

#[derive(Debug, Deserialize)]
struct VersionHeader {
    version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigDocument {
    version: u32,
    default_profile: String,
    permission_mode: PermissionMode,
    #[serde(default)]
    permission_rules: Vec<PermissionRule>,
    #[serde(default)]
    shell: ShellConfig,
    providers: BTreeMap<String, ProviderConnection>,
    profiles: BTreeMap<String, AgentProfile>,
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
    provider: String,
    model: String,
    #[serde(default = "default_max_tool_rounds")]
    max_tool_rounds: usize,
}

fn default_max_tool_rounds() -> usize {
    DEFAULT_MAX_TOOL_ROUNDS
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
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) max_tool_rounds: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionRegistry {
    pub(crate) default_profile: String,
    pub(crate) connections: BTreeMap<String, ConnectionConfig>,
    pub(crate) profiles: BTreeMap<String, ProfileConfig>,
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
pub(crate) struct InitialConfig {
    pub(crate) provider_name: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) max_tool_rounds: usize,
    pub(crate) shell: ShellConfig,
    pub(crate) permission_mode: PermissionMode,
}

#[derive(Debug)]
pub(crate) enum ConfigError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Decode(toml::de::Error),
    Encode(toml::ser::Error),
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
    InvalidShell(ShellError),
    InvalidPermissionPolicy(PolicyError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            Self::Decode(source) => write!(f, "could not decode config.toml: {source}"),
            Self::Encode(source) => write!(f, "could not encode config.toml: {source}"),
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
            | Self::UnsupportedVersion { .. }
            | Self::InvalidName { .. }
            | Self::MissingDefaultProfile { .. }
            | Self::UnknownProvider { .. }
            | Self::EmptyModel { .. }
            | Self::InvalidBaseUrl { .. }
            | Self::InvalidCredential { .. }
            | Self::InvalidCodexHome { .. }
            | Self::InvalidCodexConfiguration { .. }
            | Self::InvalidModelModality { .. }
            | Self::Edit(_)
            | Self::ConnectionAlreadyExists { .. }
            | Self::ConnectionReferenced { .. }
            | Self::InvalidToolRoundLimit { .. } => None,
        }
    }
}

impl XanaConfig {
    pub(crate) fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(input) => Self::parse(&input),
            Err(source) if source.kind() != io::ErrorKind::NotFound => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
            Err(not_found) => {
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

    pub(crate) fn render_initial(input: InitialConfig) -> Result<String, ConfigError> {
        let InitialConfig {
            provider_name,
            base_url,
            model,
            max_tool_rounds,
            shell,
            permission_mode,
        } = input;

        let mut providers = BTreeMap::new();
        providers.insert(
            provider_name.clone(),
            ProviderConnection {
                kind: ProviderKind::OpenAiCompat,
                base_url: Some(base_url),
                credential: None,
                models: BTreeMap::new(),
                codex_program: None,
                codex_home: None,
            },
        );

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "default".to_owned(),
            AgentProfile {
                provider: provider_name,
                model,
                max_tool_rounds,
            },
        );

        let document = ConfigDocument {
            version: CONFIG_VERSION,
            default_profile: "default".to_owned(),
            permission_mode,
            permission_rules: Vec::new(),
            shell,
            providers,
            profiles,
        };

        let rendered = toml::to_string_pretty(&document).map_err(ConfigError::Encode)?;
        Self::parse(&rendered)?;

        Ok(rendered)
    }

    pub(crate) fn load_registry_from(path: &Path) -> Result<ConnectionRegistry, ConfigError> {
        let input = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })?;
        let header: VersionHeader = toml::from_str(&input).map_err(ConfigError::Decode)?;
        if !(MIN_CONFIG_VERSION..=CONFIG_VERSION).contains(&header.version) {
            return Err(ConfigError::UnsupportedVersion {
                found: header.version,
            });
        }
        let document: ConfigDocument = toml::from_str(&input).map_err(ConfigError::Decode)?;
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
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })?;
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
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Edit(error.to_string()))?;
        document
            .get_mut("providers")
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| ConfigError::Edit("providers must be a table".into()))?
            .remove(id);
        document["version"] = toml_edit::value(CONFIG_VERSION as i64);
        let rendered = document.to_string();
        Self::parse(&rendered)?;
        atomic_config_write(path, rendered.as_bytes())
    }
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

    for (name, profile) in &document.profiles {
        validate_name("profile", name)?;

        if !document.providers.contains_key(&profile.provider) {
            return Err(ConfigError::UnknownProvider {
                profile: name.clone(),
                provider: profile.provider.clone(),
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
    }

    validate_name("default profile", &document.default_profile)?;

    if !document.profiles.contains_key(&document.default_profile) {
        return Err(ConfigError::MissingDefaultProfile {
            name: document.default_profile.clone(),
        });
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

    let provider_name = profile.provider;
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
                .get_mut(&profile.provider)
                .expect("profile references were validated")
                .models
                .entry(profile.model.clone())
                .or_default();
            (
                id.clone(),
                ProfileConfig {
                    id,
                    connection: profile.provider,
                    model: profile.model,
                    max_tool_rounds: profile.max_tool_rounds,
                },
            )
        })
        .collect();
    ConnectionRegistry {
        default_profile: document.default_profile,
        connections,
        profiles,
    }
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
