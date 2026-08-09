//! Validated create-new configuration filesystem boundary.

use crate::config::{ConfigError, XanaConfig};
use std::{
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    Created { path: PathBuf },
    AlreadyInitialized { path: PathBuf },
}

#[derive(Debug)]
pub(crate) enum InitFileError {
    ExistingConfigurationInvalid { path: PathBuf, source: ConfigError },
    GeneratedConfigurationInvalid { source: ConfigError },
    ParentUnavailable { path: PathBuf },
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for InitFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExistingConfigurationInvalid { path, source } => write!(
                f,
                "existing configuration at {} is not valid and was left unchanged: {source}",
                path.display()
            ),
            Self::GeneratedConfigurationInvalid { source } => {
                write!(f, "generated configuration is not valid: {source}")
            }
            Self::ParentUnavailable { path } => write!(
                f,
                "configuration path {} has no usable parent directory",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(f, "could not create {}: {source}", path.display())
            }
        }
    }
}

impl Error for InitFileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ExistingConfigurationInvalid { source, .. }
            | Self::GeneratedConfigurationInvalid { source } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::ParentUnavailable { .. } => None,
        }
    }
}

pub(crate) fn write_new_config(path: &Path, rendered: &str) -> Result<WriteOutcome, InitFileError> {
    match XanaConfig::load_from(path) {
        Ok(_) => {
            return Ok(WriteOutcome::AlreadyInitialized {
                path: path.to_owned(),
            });
        }
        Err(error) if error.is_missing_config() => {}
        Err(source) => {
            return Err(InitFileError::ExistingConfigurationInvalid {
                path: path.to_owned(),
                source,
            });
        }
    }

    XanaConfig::parse(rendered)
        .map_err(|source| InitFileError::GeneratedConfigurationInvalid { source })?;

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| InitFileError::ParentUnavailable {
            path: path.to_owned(),
        })?;

    fs::create_dir_all(parent).map_err(|source| InitFileError::Io {
        path: parent.to_owned(),
        source,
    })?;

    create_config_file(path, rendered)
}

fn create_config_file(path: &Path, rendered: &str) -> Result<WriteOutcome, InitFileError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|source| InitFileError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(rendered.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|source| InitFileError::Io {
            path: path.to_owned(),
            source,
        })?;

    Ok(WriteOutcome::Created {
        path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InitialConfig, InitialConnection};
    use tempfile::tempdir;

    const OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

    fn rendered_config(model: &str) -> String {
        XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "ollama".to_owned(),
                base_url: OLLAMA_BASE_URL.to_owned(),
            },
            model: model.to_owned(),
            max_tool_rounds: 8,
            shell: crate::shell::ShellConfig::default(),
            permission_mode: crate::config::PermissionMode::Ask,
            reasoning_effort: None,
        })
        .expect("render config")
    }

    #[test]
    fn valid_document_creates_parent_and_loadable_config() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("nested/config.toml");
        let rendered = rendered_config("qwen3:1.7b");

        let outcome = write_new_config(&path, &rendered).expect("create config");
        let loaded = XanaConfig::load_from(&path).expect("load created config");

        assert_eq!(outcome, WriteOutcome::Created { path: path.clone() });
        assert_eq!(loaded.model, "qwen3:1.7b");
    }

    #[test]
    fn existing_valid_config_is_idempotent_and_unchanged() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        let original = rendered_config("original-model");
        fs::write(&path, &original).expect("write existing config");

        let outcome = write_new_config(&path, &rendered_config("replacement-model"))
            .expect("valid existing config is idempotent");

        assert_eq!(
            outcome,
            WriteOutcome::AlreadyInitialized { path: path.clone() }
        );
        assert_eq!(fs::read_to_string(path).expect("read config"), original);
    }

    #[test]
    fn existing_invalid_config_is_never_overwritten() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        let original = "version = [";
        fs::write(&path, original).expect("write invalid config");

        let error = write_new_config(&path, &rendered_config("model"))
            .expect_err("invalid existing config should fail");

        assert!(matches!(
            error,
            InitFileError::ExistingConfigurationInvalid { path: actual, .. }
                if actual == path
        ));
        assert_eq!(fs::read_to_string(path).expect("read config"), original);
    }

    #[test]
    fn existing_legacy_config_is_never_hidden_by_a_new_file() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        let legacy = directory.path().join("config.kv");
        let original = "model=qwen3:1.7b\nbase_url=http://localhost:11434/v1\n";
        fs::write(&legacy, original).expect("write legacy config");

        let error = write_new_config(&path, &rendered_config("model"))
            .expect_err("legacy config should stop initialization");

        assert!(matches!(
            error,
            InitFileError::ExistingConfigurationInvalid {
                source: ConfigError::LegacyConfigFound { .. },
                ..
            }
        ));
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(legacy).expect("read legacy"), original);
    }

    #[test]
    fn invalid_rendered_document_creates_no_directory() {
        let directory = tempdir().expect("temporary directory");
        let parent = directory.path().join("not-created");
        let path = parent.join("config.toml");

        let error = write_new_config(&path, "version = [")
            .expect_err("invalid generated config should fail");

        assert!(matches!(
            error,
            InitFileError::GeneratedConfigurationInvalid { .. }
        ));
        assert!(!parent.exists());
    }

    #[test]
    fn a_create_race_cannot_overwrite_the_winner() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        let winner = "winner bytes";
        fs::write(&path, winner).expect("race winner creates config");

        let error = create_config_file(&path, &rendered_config("loser"))
            .expect_err("create_new should reject the loser");

        assert!(matches!(
            error,
            InitFileError::Io { source, .. }
                if source.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read_to_string(path).expect("read winner"), winner);
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_config_requests_user_only_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");

        write_new_config(&path, &rendered_config("model")).expect("create config");
        let mode = fs::metadata(path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600);
    }
}
