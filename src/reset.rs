//! Scoped, exact removal of Xana-owned state.
//!
//! Every target is derived from typed Xana paths, inspected without following
//! links, and removed in recoverability order. Configuration is always last.

use crate::paths::XanaPaths;
use std::{
    collections::BTreeSet,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ResetScope {
    Setup,
    Sessions,
    Caches,
    Credentials,
}

impl ResetScope {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Sessions => "sessions",
            Self::Caches => "caches",
            Self::Credentials => "credentials",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResetTarget {
    pub(crate) label: &'static str,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResetPlan {
    scopes: BTreeSet<ResetScope>,
    targets: Vec<ResetTarget>,
    credential_ids: Vec<String>,
    workspace_state: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum ResetError {
    Inspect { path: PathBuf, source: io::Error },
    Remove { path: PathBuf, source: io::Error },
    ActiveWorkspace { path: PathBuf },
    TooManyWorkspaceLocks { path: PathBuf, limit: usize },
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
            Self::ActiveWorkspace { path } => write!(
                f,
                "refusing reset while a workspace owner lock is active at {}",
                path.display()
            ),
            Self::TooManyWorkspaceLocks { path, limit } => write!(
                f,
                "refusing to inspect more than {limit} workspace locks under {}",
                path.display()
            ),
        }
    }
}

impl Error for ResetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Inspect { source, .. } | Self::Remove { source, .. } => Some(source),
            Self::ActiveWorkspace { .. } | Self::TooManyWorkspaceLocks { .. } => None,
        }
    }
}

impl ResetPlan {
    pub(crate) fn inspect(
        paths: &XanaPaths,
        scopes: BTreeSet<ResetScope>,
        credential_ids: Vec<String>,
    ) -> Result<Self, ResetError> {
        let mut candidates = Vec::new();
        let setup = scopes.contains(&ResetScope::Setup);
        let sessions = scopes.contains(&ResetScope::Sessions);
        let caches = scopes.contains(&ResetScope::Caches);
        if setup {
            candidates.extend([
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
                ResetTarget {
                    label: "presentation preferences",
                    path: paths.presentation_file(),
                },
                ResetTarget {
                    label: "configuration backup",
                    path: paths.config_file().with_extension("toml.bak"),
                },
            ]);
        }
        if sessions {
            candidates.extend([
                ResetTarget {
                    label: "native sessions",
                    path: paths.data_dir().join("sessions"),
                },
                ResetTarget {
                    label: "artifacts",
                    path: paths.data_dir().join("artifacts"),
                },
                ResetTarget {
                    label: "managed-thread handles",
                    path: paths.data_dir().join("managed-threads"),
                },
                ResetTarget {
                    label: "workspace ownership state",
                    path: paths.data_dir().join("workspace-hosts"),
                },
            ]);
        }
        if caches {
            candidates.extend([
                ResetTarget {
                    label: "model catalogs",
                    path: paths.cache_dir().join("models"),
                },
                ResetTarget {
                    label: "abandoned setup probe cache",
                    path: paths.cache_dir().join("setup-probe"),
                },
                ResetTarget {
                    label: "abandoned setup selection",
                    path: paths.cache_dir().join("setup-selection.toml"),
                },
            ]);
        }
        if setup {
            // Keep this last: derived state remains recoverable while the
            // current configuration is usable if an earlier removal fails.
            candidates.push(ResetTarget {
                label: "configuration",
                path: paths.config_file().to_owned(),
            });
        }

        let mut unique = BTreeSet::new();
        let mut targets = Vec::new();
        for target in candidates {
            if !unique.insert(target.path.clone()) {
                continue;
            }
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
        let mut credential_ids = credential_ids;
        credential_ids.sort();
        credential_ids.dedup();
        Ok(Self {
            scopes,
            targets,
            credential_ids,
            workspace_state: (setup || sessions).then(|| paths.data_dir().join("workspace-hosts")),
        })
    }

    pub(crate) fn scopes(&self) -> &BTreeSet<ResetScope> {
        &self.scopes
    }

    pub(crate) fn targets(&self) -> &[ResetTarget] {
        &self.targets
    }

    pub(crate) fn credential_ids(&self) -> &[String] {
        &self.credential_ids
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.targets.is_empty() && self.credential_ids.is_empty()
    }

    pub(crate) fn validate_execution(&self) -> Result<(), ResetError> {
        if let Some(path) = &self.workspace_state {
            ensure_no_active_workspace_locks(path)?;
        }
        Ok(())
    }

    pub(crate) fn execute_files(self) -> Result<Vec<ResetTarget>, ResetError> {
        self.validate_execution()?;
        let mut removed = Vec::new();
        for target in self.targets {
            if remove_entry(&target.path)? {
                removed.push(target);
            }
        }
        Ok(removed)
    }
}

fn ensure_no_active_workspace_locks(directory: &Path) -> Result<(), ResetError> {
    const MAX_LOCKS: usize = 1_024;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ResetError::Inspect {
                path: directory.to_owned(),
                source,
            });
        }
    };
    let mut inspected = 0usize;
    for entry in entries {
        let entry = entry.map_err(|source| ResetError::Inspect {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("lock") {
            continue;
        }
        inspected += 1;
        if inspected > MAX_LOCKS {
            return Err(ResetError::TooManyWorkspaceLocks {
                path: directory.to_owned(),
                limit: MAX_LOCKS,
            });
        }
        let metadata = fs::symlink_metadata(&path).map_err(|source| ResetError::Inspect {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(ResetError::ActiveWorkspace { path });
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| ResetError::Inspect {
                path: path.clone(),
                source,
            })?;
        match lock.try_lock() {
            Ok(()) => {
                let _ = fs::File::unlock(&lock);
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(ResetError::ActiveWorkspace { path });
            }
            Err(fs::TryLockError::Error(source)) => {
                return Err(ResetError::Inspect { path, source });
            }
        }
    }
    Ok(())
}

fn remove_entry(path: &Path) -> Result<bool, ResetError> {
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
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, XanaPaths) {
        let directory = tempdir().unwrap();
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).unwrap();
        for path in [
            paths.config_file().to_owned(),
            paths.config_file().with_extension("toml.bak"),
            paths.data_dir().join("selection.toml"),
            paths.cache_dir().join("models/codex.json"),
            paths.data_dir().join("managed-threads/route.json"),
            paths.data_dir().join("sessions/session.jsonl"),
            paths.data_dir().join("artifacts/content.bin"),
            paths.data_dir().join("workspace-hosts/workspace.json"),
            paths.presentation_file(),
            paths.cache_dir().join("unrelated.cache"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        (directory, paths)
    }

    #[test]
    fn setup_scope_preserves_owned_history_and_unrelated_cache() {
        let (_directory, paths) = fixture();
        let plan =
            ResetPlan::inspect(&paths, BTreeSet::from([ResetScope::Setup]), Vec::new()).unwrap();
        let removed = plan.execute_files().unwrap();

        assert_eq!(removed.last().unwrap().label, "configuration");
        assert!(!paths.config_file().exists());
        assert!(!paths.cache_dir().join("models").exists());
        assert!(!paths.data_dir().join("managed-threads").exists());
        assert!(paths.data_dir().join("sessions/session.jsonl").is_file());
        assert!(paths.data_dir().join("artifacts/content.bin").is_file());
        assert!(paths.cache_dir().join("unrelated.cache").is_file());
    }

    #[test]
    fn sessions_scope_removes_history_but_preserves_config_and_cache() {
        let (_directory, paths) = fixture();
        ResetPlan::inspect(&paths, BTreeSet::from([ResetScope::Sessions]), Vec::new())
            .unwrap()
            .execute_files()
            .unwrap();

        assert!(paths.config_file().is_file());
        assert!(paths.cache_dir().join("models/codex.json").is_file());
        assert!(!paths.data_dir().join("sessions").exists());
        assert!(!paths.data_dir().join("artifacts").exists());
        assert!(!paths.data_dir().join("managed-threads").exists());
    }

    #[test]
    fn plan_deduplicates_targets_and_credentials() {
        let (_directory, paths) = fixture();
        let plan = ResetPlan::inspect(
            &paths,
            BTreeSet::from([
                ResetScope::Setup,
                ResetScope::Sessions,
                ResetScope::Caches,
                ResetScope::Credentials,
            ]),
            vec!["remote".into(), "remote".into()],
        )
        .unwrap();
        let unique = plan
            .targets()
            .iter()
            .map(|target| &target.path)
            .collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), plan.targets().len());
        assert_eq!(plan.credential_ids(), &["remote"]);
    }

    #[test]
    fn reset_is_idempotent_when_selected_state_is_absent() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).unwrap();
        let plan =
            ResetPlan::inspect(&paths, BTreeSet::from([ResetScope::Setup]), Vec::new()).unwrap();
        assert!(plan.is_empty());
        assert!(plan.execute_files().unwrap().is_empty());
    }

    #[test]
    fn session_reset_refuses_an_active_workspace_lock_before_deleting_history() {
        let (_directory, paths) = fixture();
        let lock_path = paths.data_dir().join("workspace-hosts/active.lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        lock.lock().unwrap();
        let plan =
            ResetPlan::inspect(&paths, BTreeSet::from([ResetScope::Sessions]), Vec::new()).unwrap();

        assert!(matches!(
            plan.execute_files(),
            Err(ResetError::ActiveWorkspace { .. })
        ));
        assert!(paths.data_dir().join("sessions/session.jsonl").is_file());
        fs::File::unlock(&lock).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reset_unlinks_a_directory_symlink_without_following_it() {
        use std::os::unix::fs::symlink;
        let directory = tempdir().unwrap();
        let root = directory.path().join("xana-home");
        let external = directory.path().join("external-models");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("keep.json"), b"keep").unwrap();
        let paths = XanaPaths::resolve(Some(root.into_os_string())).unwrap();
        fs::create_dir_all(paths.cache_dir()).unwrap();
        symlink(&external, paths.cache_dir().join("models")).unwrap();

        ResetPlan::inspect(&paths, BTreeSet::from([ResetScope::Caches]), Vec::new())
            .unwrap()
            .execute_files()
            .unwrap();

        assert!(!paths.cache_dir().join("models").exists());
        assert!(external.join("keep.json").is_file());
    }
}
