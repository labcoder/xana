//! Durable acknowledgement that setup intentionally completed without a provider.

use crate::paths::XanaPaths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

const STATE_VERSION: u8 = 1;
const MAX_STATE_BYTES: u64 = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupInstallation {
    Uninitialized,
    Blank,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupStateDocument {
    version: u8,
    mode: SetupMode,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SetupMode {
    Blank,
}

pub(crate) fn inspect(paths: &XanaPaths) -> Result<SetupInstallation> {
    let path = paths.setup_state_file();
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(SetupInstallation::Uninitialized);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not inspect {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("setup state is not a regular file: {}", path.display());
    }
    if metadata.len() > MAX_STATE_BYTES {
        bail!(
            "setup state exceeds the {MAX_STATE_BYTES}-byte limit: {}",
            path.display()
        );
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("could not read setup state from {}", path.display()))?;
    let document: SetupStateDocument = serde_json::from_slice(&bytes)
        .with_context(|| format!("could not decode setup state from {}", path.display()))?;
    if document.version != STATE_VERSION {
        bail!(
            "unsupported setup-state version {} at {}",
            document.version,
            path.display()
        );
    }
    Ok(match document.mode {
        SetupMode::Blank => SetupInstallation::Blank,
    })
}

pub(crate) fn commit_blank(paths: &XanaPaths) -> Result<()> {
    let _lock = crate::config::ConfigTransactionLock::acquire(paths.config_file())
        .context("could not lock configuration while committing blank setup")?;
    match fs::symlink_metadata(paths.config_file()) {
        Ok(_) => bail!(
            "refusing blank setup because configuration already exists at {}",
            paths.config_file().display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "could not inspect configuration at {}",
                    paths.config_file().display()
                )
            });
        }
    }
    let bytes = serde_json::to_vec(&SetupStateDocument {
        version: STATE_VERSION,
        mode: SetupMode::Blank,
    })?;
    super::atomic_write(&paths.setup_state_file(), &bytes)
        .context("could not commit blank setup state")
}

pub(crate) fn clear_blank(paths: &XanaPaths) -> Result<()> {
    remove_if_file(&paths.setup_state_file())
}

fn remove_if_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!(
                "refusing to remove non-file setup state at {}",
                path.display()
            )
        }
        Ok(_) => fs::remove_file(path).with_context(|| {
            format!(
                "could not remove obsolete setup state at {}",
                path.display()
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("could not inspect setup state at {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn paths() -> (tempfile::TempDir, XanaPaths) {
        let directory = tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        (directory, paths)
    }

    #[test]
    fn blank_state_round_trips_without_creating_configuration() {
        let (_directory, paths) = paths();
        assert_eq!(inspect(&paths).unwrap(), SetupInstallation::Uninitialized);
        commit_blank(&paths).unwrap();
        assert_eq!(inspect(&paths).unwrap(), SetupInstallation::Blank);
        assert!(!paths.config_file().exists());
        clear_blank(&paths).unwrap();
        assert_eq!(inspect(&paths).unwrap(), SetupInstallation::Uninitialized);
    }

    #[test]
    fn blank_state_never_replaces_existing_configuration() {
        let (_directory, paths) = paths();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(paths.config_file(), "existing").unwrap();
        assert!(commit_blank(&paths).is_err());
        assert_eq!(fs::read_to_string(paths.config_file()).unwrap(), "existing");
    }
}
