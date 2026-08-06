use reqwest::Url;
use serde::Deserialize;
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigDocument {
    version: u32,
    default_profile: String,
    permission_mode: PermissionMode,
    providers: BTreeMap<String, ProviderConnection>,
    profiles: BTreeMap<String, AgentProfile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum ProviderKind {
    #[serde(rename = "openai_compat")]
    OpenAiCompat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionMode {
    Allow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConnection {
    kind: ProviderKind,
    base_url: String,
}

#[derive(Debug, Deserialize)]
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
    pub(crate) max_tool_rounds: usize,
}

#[derive(Debug)]
pub(crate) enum ConfigError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Decode(toml::de::Error),
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
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            Self::Decode(source) => write!(f, "could not decode config.toml: {source}"),
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
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Decode(source) => Some(source),
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

    fn parse(input: &str) -> Result<Self, ConfigError> {
        let header: VersionHeader = toml::from_str(input).map_err(ConfigError::Decode)?;

        if header.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: header.version,
            });
        }

        let document: ConfigDocument = toml::from_str(input).map_err(ConfigError::Decode)?;

        validate_and_resolve(document)
    }
}

fn validate_and_resolve(mut document: ConfigDocument) -> Result<XanaConfig, ConfigError> {
    debug_assert_eq!(document.version, CONFIG_VERSION);

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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    const MINIMAL: &str = r#"
version = 1
default_profile = "default"
permission_mode = "allow"

[providers.local]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[profiles.default]
provider = "local"
model = "qwen3:1.7b"
"#;

    fn parse_ok(input: &str) -> XanaConfig {
        XanaConfig::parse(input).unwrap_or_else(|error| panic!("valid fixture failed: {error}"))
    }

    fn parse_error(input: &str) -> ConfigError {
        XanaConfig::parse(input).expect_err("fixture should fail")
    }

    #[test]
    fn minimal_v1_resolves_default_profile_and_default_round_limit() {
        let config = parse_ok(MINIMAL);

        assert_eq!(config.provider_name, "local");
        assert_eq!(config.provider_kind, ProviderKind::OpenAiCompat);
        assert_eq!(config.base_url, "http://localhost:11434/v1");
        assert_eq!(config.model, "qwen3:1.7b");
        assert_eq!(config.permission_mode, PermissionMode::Allow);
        assert_eq!(config.max_tool_rounds, 8);
    }

    #[test]
    fn explicit_round_limit_overrides_the_default() {
        let input = MINIMAL.replace(
            "model = \"qwen3:1.7b\"",
            "model = \"qwen3:1.7b\"\nmax_tool_rounds = 12",
        );

        let config = parse_ok(&input);

        assert_eq!(config.max_tool_rounds, 12);
    }

    #[test]
    fn missing_required_field_is_a_decode_error() {
        let input = MINIMAL.replace("permission_mode = \"allow\"\n", "");

        let error = parse_error(&input);

        assert!(matches!(error, ConfigError::Decode(_)));
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let input = MINIMAL.replace(
            "permission_mode = \"allow\"",
            "permission_mode = \"allow\"\nfuture_toggle = true",
        );

        let error = parse_error(&input);

        assert!(matches!(error, ConfigError::Decode(_)));
    }

    #[test]
    fn unknown_nested_field_and_plaintext_api_key_are_rejected() {
        let input = MINIMAL.replace(
            "base_url = \"http://localhost:11434/v1\"",
            "base_url = \"http://localhost:11434/v1\"\napi_key = \"secret\"",
        );

        let error = parse_error(&input);

        assert!(matches!(error, ConfigError::Decode(_)));
    }

    #[test]
    fn future_version_is_reported_before_future_fields() {
        let input = MINIMAL.replace("version = 1", "version = 9\nfuture_schema_field = true");

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::UnsupportedVersion { found: 9 }
        ));
    }

    #[test]
    fn permission_modes_other_than_allow_are_rejected() {
        let input = MINIMAL.replace("permission_mode = \"allow\"", "permission_mode = \"ask\"");

        let error = parse_error(&input);

        assert!(matches!(error, ConfigError::Decode(_)));
    }

    #[test]
    fn invalid_provider_name_is_rejected() {
        let input = MINIMAL.replace("[providers.local]", "[providers.Bad]");

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::InvalidName {
                section: "provider",
                name,
            } if name == "Bad"
        ));
    }

    #[test]
    fn invalid_profile_name_is_rejected() {
        let input = MINIMAL.replace("[profiles.default]", "[profiles.Bad]");

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::InvalidName {
                section: "profile",
                name,
            } if name == "Bad"
        ));
    }

    #[test]
    fn missing_default_profile_carries_its_name() {
        let input = MINIMAL.replace(
            "default_profile = \"default\"",
            "default_profile = \"missing\"",
        );

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::MissingDefaultProfile { name } if name == "missing"
        ));
    }

    #[test]
    fn every_profile_must_reference_a_known_provider() {
        let input =
            format!("{MINIMAL}\n[profiles.broken]\nprovider = \"missing\"\nmodel = \"other\"\n");

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::UnknownProvider { profile, provider }
                if profile == "broken" && provider == "missing"
        ));
    }

    #[test]
    fn blank_model_is_rejected() {
        let input = MINIMAL.replace("model = \"qwen3:1.7b\"", "model = \"   \"");

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::EmptyModel { profile } if profile == "default"
        ));
    }

    #[test]
    fn zero_tool_rounds_is_rejected() {
        let input = MINIMAL.replace(
            "model = \"qwen3:1.7b\"",
            "model = \"qwen3:1.7b\"\nmax_tool_rounds = 0",
        );

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::InvalidToolRoundLimit {
                profile,
                value: 0,
            } if profile == "default"
        ));
    }

    #[test]
    fn tool_rounds_above_the_limit_are_rejected() {
        let input = MINIMAL.replace(
            "model = \"qwen3:1.7b\"",
            "model = \"qwen3:1.7b\"\nmax_tool_rounds = 65",
        );

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::InvalidToolRoundLimit {
                profile,
                value: 65,
            } if profile == "default"
        ));
    }

    #[test]
    fn invalid_url_forms_are_rejected() {
        let invalid_urls = [
            "/v1",
            "ftp://localhost/v1",
            "http://user:secret@localhost/v1",
            "http://localhost/v1?token=secret",
            "http://localhost/v1#fragment",
        ];

        for invalid_url in invalid_urls {
            let input = MINIMAL.replace("http://localhost:11434/v1", invalid_url);

            let error = parse_error(&input);

            assert!(matches!(
                error,
                ConfigError::InvalidBaseUrl { provider, .. } if provider == "local"
            ));
        }
    }

    #[test]
    fn valid_non_default_entries_are_checked_but_only_default_is_resolved() {
        let input = r#"
version = 1
default_profile = "default"
permission_mode = "allow"

[providers.local]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[providers.remote]
kind = "openai_compat"
base_url = "https://example.test/v1"

[profiles.default]
provider = "local"
model = "main-model"

[profiles.worker]
provider = "remote"
model = "worker-model"
max_tool_rounds = 4
"#;

        let config = parse_ok(input);

        assert_eq!(config.provider_name, "local");
        assert_eq!(config.model, "main-model");
        assert_eq!(config.max_tool_rounds, 8);
    }

    #[test]
    fn load_from_reads_config_toml() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, MINIMAL).expect("write config");

        let config = XanaConfig::load_from(&path).expect("load config");

        assert_eq!(config.provider_name, "local");
    }

    #[test]
    fn legacy_file_is_detected_only_when_toml_is_missing() {
        let directory = tempdir().expect("temporary config directory");
        let config_path = directory.path().join("config.toml");
        let legacy_path = directory.path().join("config.kv");
        fs::write(
            &legacy_path,
            "model=qwen3:1.7b\nbase_url=http://localhost:11434/v1\n",
        )
        .expect("write legacy config");

        let error = XanaConfig::load_from(&config_path).expect_err("legacy file should fail");

        assert!(matches!(
            error,
            ConfigError::LegacyConfigFound {
                legacy_path: actual_legacy,
                config_path: actual_config,
            } if actual_legacy == legacy_path && actual_config == config_path
        ));
    }

    #[test]
    fn config_toml_wins_when_both_files_exist() {
        let directory = tempdir().expect("temporary config directory");
        let config_path = directory.path().join("config.toml");
        let legacy_path = directory.path().join("config.kv");
        fs::write(&config_path, MINIMAL).expect("write TOML config");
        fs::write(&legacy_path, "not even valid legacy syntax").expect("write legacy config");

        let config = XanaConfig::load_from(&config_path).expect("TOML wins");

        assert_eq!(config.model, "qwen3:1.7b");
    }

    #[test]
    fn malformed_toml_preserves_the_decode_source() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::write(&path, "version = [").expect("write malformed config");

        let error = XanaConfig::load_from(&path).expect_err("malformed TOML should fail");

        assert!(matches!(&error, ConfigError::Decode(_)));
        assert!(error.source().is_some());
    }

    #[test]
    fn missing_configuration_reports_the_config_toml_path() {
        let directory = tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");

        let error = XanaConfig::load_from(&path).expect_err("missing config should fail");

        assert!(matches!(
            error,
            ConfigError::Io {
                path: actual_path,
                source,
            } if actual_path == path && source.kind() == io::ErrorKind::NotFound
        ));
    }
}
