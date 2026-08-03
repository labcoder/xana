use directories::ProjectDirs;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct XanaConfig {
    pub model: String,
    pub base_url: String,
}

#[derive(Debug)]
pub enum ConfigError {
    ConfigDirectoryUnavailable,
    Io(io::Error),
    MalformedLine { line: usize, contents: String },
    UnknownKey { line: usize, key: String },
    DuplicateKey { line: usize, key: String },
    MissingField(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigDirectoryUnavailable => {
                write!(f, "could not determine the platform config directory")
            }
            Self::Io(error) => write!(f, "could not read config.kv: {error}"),
            Self::MalformedLine { line, contents } => {
                write!(f, "malformed config line {line}: {contents:?}")
            }
            Self::UnknownKey { line, key } => {
                write!(f, "unknown config key {key:?} on line {line}")
            }
            Self::DuplicateKey { line, key } => {
                write!(f, "duplicate config key {key:?} on line {line}")
            }
            Self::MissingField(field) => {
                write!(f, "missing required config key {field:?}")
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::ConfigDirectoryUnavailable
            | Self::MalformedLine { .. }
            | Self::UnknownKey { .. }
            | Self::DuplicateKey { .. }
            | Self::MissingField(_) => None,
        }
    }
}

impl From<io::Error> for ConfigError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    let project_dirs = ProjectDirs::from("io.github", "labcoder", "xana")
        .ok_or(ConfigError::ConfigDirectoryUnavailable)?;

    Ok(project_dirs.config_dir().join("config.kv"))
}

impl XanaConfig {
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let input = fs::read_to_string(path)?;
        Self::parse(&input)
    }

    fn parse(input: &str) -> Result<Self, ConfigError> {
        let mut model = None;
        let mut base_url = None;

        for (index, raw_line) in input.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw_line.trim();

            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let (raw_key, raw_value) =
                trimmed
                    .split_once('=')
                    .ok_or_else(|| ConfigError::MalformedLine {
                        line,
                        contents: raw_line.to_owned(),
                    })?;

            let key = raw_key.trim();
            let value = raw_value.trim();

            if key.is_empty() || value.is_empty() {
                return Err(ConfigError::MalformedLine {
                    line,
                    contents: raw_line.to_owned(),
                });
            }

            match key {
                "model" => {
                    if model.is_some() {
                        return Err(ConfigError::DuplicateKey {
                            line,
                            key: key.to_owned(),
                        });
                    }
                    model = Some(value.to_owned());
                }
                "base_url" => {
                    if base_url.is_some() {
                        return Err(ConfigError::DuplicateKey {
                            line,
                            key: key.to_owned(),
                        });
                    }
                    base_url = Some(value.to_owned());
                }
                _ => {
                    return Err(ConfigError::UnknownKey {
                        line,
                        key: key.to_owned(),
                    });
                }
            }
        }

        Ok(Self {
            model: model.ok_or(ConfigError::MissingField("model"))?,
            base_url: base_url.ok_or(ConfigError::MissingField("base_url"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(input: &str) -> XanaConfig {
        match XanaConfig::parse(input) {
            Ok(config) => config,
            Err(error) => panic!("valid fixture failed: {error}"),
        }
    }

    #[test]
    fn valid_config_parses() {
        let config = parse_ok("model = qwen3:1.7b\nbase_url = http://localhost:11434/v1\n");

        assert_eq!(config.model, "qwen3:1.7b");
        assert_eq!(config.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let config =
            parse_ok("\n# comment\n model = qwen3 \n\n base_url = http://localhost:11434/v1 \n");

        assert_eq!(config.model, "qwen3");
        assert_eq!(config.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn missing_equals_is_malformed_with_line_number() {
        let result = XanaConfig::parse("model = qwen3\nbad line\nbase_url = http://x");

        assert!(matches!(
            result,
            Err(ConfigError::MalformedLine { line: 2, .. })
        ));
    }

    #[test]
    fn unknown_key_names_the_key() {
        let result = XanaConfig::parse("model = qwen3\nmodle = typo\nbase_url = http://x");

        assert!(matches!(
            result,
            Err(ConfigError::UnknownKey { line: 2, key }) if key == "modle"
        ));
    }

    #[test]
    fn duplicate_key_is_rejected() {
        let result = XanaConfig::parse("model = first\nmodel = second\nbase_url = http://x");

        assert!(matches!(
            result,
            Err(ConfigError::DuplicateKey { line: 2, key }) if key == "model"
        ));
    }

    #[test]
    fn missing_field_is_reported_after_the_scan() {
        let result = XanaConfig::parse("model = qwen3\n# base_url is absent");

        assert!(matches!(result, Err(ConfigError::MissingField("base_url"))));
    }
}
