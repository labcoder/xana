//! Versioned shared configuration loading, validation, and initial rendering.
//!
//! This module owns the human-authored TOML schema and resolves it into an
//! immutable agent snapshot. It does not read process environment variables or
//! invent initializer-specific validation.

use crate::shell::{Shell, ShellConfig, ShellError};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

const CONFIG_VERSION: u32 = 1;
const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;
const MAX_MAX_TOOL_ROUNDS: usize = 64;

#[derive(Debug, Deserialize)]
struct VersionHeader {
    version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigDocument {
    version: u32,
    default_profile: String,
    permission_mode: PermissionMode,
    #[serde(default)]
    shell: ShellConfig,
    providers: BTreeMap<String, ProviderConnection>,
    profiles: BTreeMap<String, AgentProfile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ProviderKind {
    #[serde(rename = "openai_compat")]
    OpenAiCompat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionMode {
    Allow,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConnection {
    kind: ProviderKind,
    base_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
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
    pub(crate) model: String,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) shell: ShellConfig,
    pub(crate) max_tool_rounds: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitialConfig {
    pub(crate) provider_name: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) max_tool_rounds: usize,
    pub(crate) shell: ShellConfig,
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
    InvalidToolRoundLimit {
        profile: String,
        value: usize,
    },
    InvalidShell(ShellError),
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
            Self::InvalidToolRoundLimit { profile, value } => write!(
                f,
                "profile {profile:?} has max_tool_rounds = {value}; expected 1..={MAX_MAX_TOOL_ROUNDS}"
            ),
            Self::InvalidShell(source) => write!(f, "invalid shell configuration: {source}"),
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
            Self::LegacyConfigFound { .. }
            | Self::UnsupportedVersion { .. }
            | Self::InvalidName { .. }
            | Self::MissingDefaultProfile { .. }
            | Self::UnknownProvider { .. }
            | Self::EmptyModel { .. }
            | Self::InvalidBaseUrl { .. }
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

        if header.version != CONFIG_VERSION {
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
        } = input;

        let mut providers = BTreeMap::new();
        providers.insert(
            provider_name.clone(),
            ProviderConnection {
                kind: ProviderKind::OpenAiCompat,
                base_url,
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
            permission_mode: PermissionMode::Allow,
            shell,
            providers,
            profiles,
        };

        let rendered = toml::to_string_pretty(&document).map_err(ConfigError::Encode)?;
        Self::parse(&rendered)?;

        Ok(rendered)
    }
}

impl ConfigError {
    pub(crate) fn is_missing_config(&self) -> bool {
        matches!(
            self,
            Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound
        )
    }
}

fn validate_and_resolve(mut document: ConfigDocument) -> Result<XanaConfig, ConfigError> {
    debug_assert_eq!(document.version, CONFIG_VERSION);

    Shell::resolve(document.shell.clone()).map_err(ConfigError::InvalidShell)?;

    for (name, provider) in &document.providers {
        validate_name("provider", name)?;
        validate_base_url(name, &provider.base_url)?;
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

    Ok(XanaConfig {
        provider_name,
        provider_kind: provider.kind,
        base_url: provider.base_url,
        model: profile.model,
        permission_mode: document.permission_mode,
        shell: document.shell,
        max_tool_rounds: profile.max_tool_rounds,
    })
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
