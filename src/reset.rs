//! Narrow, explicit removal of first-run setup state.
//!
//! Reset deliberately preserves sessions, artifacts, credential-manager
//! entries, and externally owned Codex state. The configuration is removed
//! last so a partial failure leaves Xana initialized whenever possible.

use crate::paths::XanaPaths;
use std::{error::Error, fmt, fs, io, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResetTarget {
    pub(crate) label: &'static str,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResetPlan {
    targets: Vec<ResetTarget>,
}

#[derive(Debug)]
pub(crate) enum ResetError {
    Inspect { path: PathBuf, source: io::Error },
    Remove { path: PathBuf, source: io::Error },
}

impl fmt::Display for ResetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspect { path, source } => write!(
                f,
                "could not inspect reset target {}: {source}",
                path.display()
            ),
            Self::Remove { path, source } => write!(
                f,
                "could not remove reset target {}: {source}",
                path.display()
            ),
        }
    }
}

impl Error for ResetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Inspect { source, .. } | Self::Remove { source, .. } => Some(source),
        }
    }
}

impl ResetPlan {
    pub(crate) fn inspect(paths: &XanaPaths) -> Result<Self, ResetError> {
        let candidates = [
            ResetTarget {
                label: "model selection",
                path: paths.data_dir().join("selection.toml"),
            },
            ResetTarget {
                label: "model catalogs",
                path: paths.cache_dir().join("models"),
            },
            ResetTarget {
                label: "managed-thread handles",
                path: paths.data_dir().join("managed-threads"),
            },
            // Keep this last: derived state is recoverable while the current
            // configuration remains usable if an earlier removal fails.
            ResetTarget {
                label: "configuration",
                path: paths.config_file().to_owned(),
            },
        ];
        let mut targets = Vec::new();
        for target in candidates {
            match fs::symlink_metadata(&target.path) {
                Ok(_) => targets.push(target),
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ResetError::Inspect {
                        path: target.path,
                        source,
                    });
                }
            }
        }
        Ok(Self { targets })
    }

    pub(crate) fn targets(&self) -> &[ResetTarget] {
        &self.targets
    }

    pub(crate) fn execute(self) -> Result<Vec<ResetTarget>, ResetError> {
        let mut removed = Vec::new();
        for target in self.targets {
            if remove_entry(&target.path)? {
                removed.push(target);
            }
        }
        Ok(removed)
    }
}

fn remove_entry(path: &std::path::Path) -> Result<bool, ResetError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(ResetError::Inspect {
                path: path.to_owned(),
                source,
            });
        }
    };
    let result = if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        // Symlinks are unlinked rather than followed, including a symlink
        // occupying one of the expected directory locations.
        fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ResetError::Remove {
            path: path.to_owned(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn reset_removes_only_setup_state_and_preserves_owned_history() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
        for path in [
            paths.config_file().to_owned(),
            paths.data_dir().join("selection.toml"),
            paths.cache_dir().join("models/codex.json"),
            paths.data_dir().join("managed-threads/route.json"),
            paths.data_dir().join("sessions/session.jsonl"),
            paths.data_dir().join("artifacts/content.bin"),
            paths.cache_dir().join("unrelated.cache"),
        ] {
            fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
            fs::write(path, b"fixture").expect("fixture file");
        }

        let plan = ResetPlan::inspect(&paths).expect("reset plan");
        assert_eq!(plan.targets().len(), 4);
        let removed = plan.execute().expect("execute reset");

        assert_eq!(removed.len(), 4);
        assert!(!paths.config_file().exists());
        assert!(!paths.data_dir().join("selection.toml").exists());
        assert!(!paths.cache_dir().join("models").exists());
        assert!(!paths.data_dir().join("managed-threads").exists());
        assert!(paths.data_dir().join("sessions/session.jsonl").is_file());
        assert!(paths.data_dir().join("artifacts/content.bin").is_file());
        assert!(paths.cache_dir().join("unrelated.cache").is_file());
    }

    #[test]
    fn reset_is_idempotent_when_setup_state_is_absent() {
        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");

        let plan = ResetPlan::inspect(&paths).expect("empty reset plan");

        assert!(plan.targets().is_empty());
        assert!(plan.execute().expect("empty reset").is_empty());
        assert!(!paths.data_dir().exists());
    }

    #[cfg(unix)]
    #[test]
    fn reset_unlinks_a_directory_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary Xana home");
        let root = directory.path().join("xana-home");
        let external = directory.path().join("external-models");
        fs::create_dir_all(&external).expect("external directory");
        fs::write(external.join("keep.json"), b"keep").expect("external file");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
        fs::create_dir_all(paths.cache_dir()).expect("cache directory");
        symlink(&external, paths.cache_dir().join("models")).expect("models symlink");

        ResetPlan::inspect(&paths)
            .expect("reset plan")
            .execute()
            .expect("execute reset");

        assert!(!paths.cache_dir().join("models").exists());
        assert!(external.join("keep.json").is_file());
    }
}
