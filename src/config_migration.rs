//! Reviewable, failure-safe migration for shared configuration and private records.
//!
//! Private records are made durable first; the atomic config version update is
//! the final authoritative marker. A retry after interruption is idempotent.

use crate::{
    bounded_file,
    config::{CONFIG_VERSION, XanaConfig},
    paths::XanaPaths,
    private_state::{
        PrivateRecordInspection, PrivateRecordStatus, ensure_interoperable_records,
        inspect_interoperable_records,
    },
};
use std::{
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct ConfigMigrationPlan {
    config_path: PathBuf,
    original: Vec<u8>,
    migrated: Vec<u8>,
    pub(crate) source_version: u32,
    pub(crate) target_version: u32,
    pub(crate) private_records: Vec<PrivateRecordInspection>,
}

impl ConfigMigrationPlan {
    pub(crate) fn build(paths: &XanaPaths) -> Result<Self, ConfigMigrationError> {
        let config_path = paths.config_file().to_owned();
        let original =
            bounded_file::read(&config_path, MAX_CONFIG_BYTES).map_err(map_read_error)?;
        let text = std::str::from_utf8(&original).map_err(|error| {
            ConfigMigrationError::Invalid(format!("config.toml is not UTF-8: {error}"))
        })?;
        let source_version = XanaConfig::document_version(text)
            .map_err(|error| ConfigMigrationError::Invalid(error.to_string()))?;
        let migrated = XanaConfig::migrate_to_current(text)
            .map_err(|error| ConfigMigrationError::Invalid(error.to_string()))?
            .into_bytes();
        Ok(Self {
            config_path,
            original,
            migrated,
            source_version,
            target_version: CONFIG_VERSION,
            private_records: inspect_interoperable_records(paths),
        })
    }

    pub(crate) fn requires_apply(&self) -> bool {
        self.original != self.migrated
            || self
                .private_records
                .iter()
                .any(|record| record.status == PrivateRecordStatus::Missing)
    }

    pub(crate) fn apply(
        self,
        paths: &XanaPaths,
    ) -> Result<ConfigMigrationOutcome, ConfigMigrationError> {
        let _lock = MigrationLock::acquire(&paths.config_migration_lock_file())?;
        let current =
            bounded_file::read(&self.config_path, MAX_CONFIG_BYTES).map_err(map_read_error)?;
        if current != self.original {
            return Err(ConfigMigrationError::Changed(self.config_path));
        }
        for record in &self.private_records {
            if !matches!(
                record.status,
                PrivateRecordStatus::Healthy | PrivateRecordStatus::Missing
            ) {
                return Err(ConfigMigrationError::Invalid(format!(
                    "{} private state is {}; repair or restore it before migration",
                    record.name,
                    record.status.as_str()
                )));
            }
        }

        let created = ensure_interoperable_records(paths)
            .map_err(|error| ConfigMigrationError::Private(error.to_string()))?;
        let backup_path = self.config_path.with_extension("toml.bak");
        let previous_backup = read_optional(&backup_path)?;
        if let Err(error) = atomic_write(&backup_path, &self.original) {
            remove_created(&created);
            return Err(error);
        }
        if let Err(error) = atomic_write(&self.config_path, &self.migrated) {
            let restore = restore_optional(&backup_path, previous_backup.as_deref());
            remove_created(&created);
            restore?;
            return Err(error);
        }

        Ok(ConfigMigrationOutcome {
            changed_config: self.original != self.migrated,
            initialized_private_records: created.len(),
            backup_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigMigrationOutcome {
    pub(crate) changed_config: bool,
    pub(crate) initialized_private_records: usize,
    pub(crate) backup_path: PathBuf,
}

#[derive(Debug)]
pub(crate) enum ConfigMigrationError {
    Busy(PathBuf),
    Changed(PathBuf),
    Io { path: PathBuf, source: io::Error },
    Invalid(String),
    Private(String),
}

impl fmt::Display for ConfigMigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(path) => write!(
                formatter,
                "another Xana process owns configuration migration ({})",
                path.display()
            ),
            Self::Changed(path) => write!(
                formatter,
                "{} changed after the migration review; create a new plan",
                path.display()
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Invalid(reason) | Self::Private(reason) => formatter.write_str(reason),
        }
    }
}

impl Error for ConfigMigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Busy(_) | Self::Changed(_) | Self::Invalid(_) | Self::Private(_) => None,
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigMigrationError> {
    let parent = path.parent().ok_or_else(|| {
        ConfigMigrationError::Invalid(format!("{} has no parent directory", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|source| ConfigMigrationError::Io {
        path: parent.to_owned(),
        source,
    })?;
    let mut file = atomic_write_file::AtomicWriteFile::open(path).map_err(|source| {
        ConfigMigrationError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    protect_open_file(file.as_file()).map_err(|source| ConfigMigrationError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(bytes)
        .map_err(|source| ConfigMigrationError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.commit().map_err(|source| ConfigMigrationError::Io {
        path: path.to_owned(),
        source,
    })
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ConfigMigrationError> {
    match bounded_file::read(path, MAX_CONFIG_BYTES) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(bounded_file::BoundedReadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(map_read_error(error)),
    }
}

fn restore_optional(path: &Path, bytes: Option<&[u8]>) -> Result<(), ConfigMigrationError> {
    match bytes {
        Some(bytes) => atomic_write(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ConfigMigrationError::Io {
                path: path.to_owned(),
                source,
            }),
        },
    }
}

fn remove_created(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = fs::remove_file(path);
    }
}

fn map_read_error(error: bounded_file::BoundedReadError) -> ConfigMigrationError {
    match error {
        bounded_file::BoundedReadError::TooLarge {
            path,
            actual,
            limit,
        } => ConfigMigrationError::Invalid(format!(
            "{} contains {actual} bytes, exceeding the {limit}-byte limit",
            path.display()
        )),
        bounded_file::BoundedReadError::Io { path, source } => {
            ConfigMigrationError::Io { path, source }
        }
    }
}

struct MigrationLock {
    file: fs::File,
}

impl MigrationLock {
    fn acquire(path: &Path) -> Result<Self, ConfigMigrationError> {
        let parent = path.parent().ok_or_else(|| {
            ConfigMigrationError::Invalid(format!("{} has no parent directory", path.display()))
        })?;
        fs::create_dir_all(parent).map_err(|source| ConfigMigrationError::Io {
            path: parent.to_owned(),
            source,
        })?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| ConfigMigrationError::Io {
                path: path.to_owned(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(fs::TryLockError::WouldBlock) => Err(ConfigMigrationError::Busy(path.to_owned())),
            Err(fs::TryLockError::Error(source)) => Err(ConfigMigrationError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }
}

impl Drop for MigrationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(unix)]
fn protect_open_file(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn protect_open_file(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode},
        shell::ShellConfig,
    };
    use std::ffi::OsString;
    use tempfile::tempdir;

    fn paths_and_v3() -> (tempfile::TempDir, XanaPaths, Vec<u8>) {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        let current = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".into(),
                base_url: "http://localhost:11434/v1".into(),
            },
            model: "qwen".into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap()
        .replacen("version = 4", "# keep this comment\nversion = 3", 1)
        .into_bytes();
        fs::write(paths.config_file(), &current).unwrap();
        (directory, paths, current)
    }

    #[test]
    fn plan_is_read_only_and_apply_preserves_backup_and_comments() {
        let (_directory, paths, original) = paths_and_v3();
        let plan = ConfigMigrationPlan::build(&paths).unwrap();
        assert!(plan.requires_apply());
        assert_eq!(plan.source_version, 3);
        assert_eq!(fs::read(paths.config_file()).unwrap(), original);

        let outcome = plan.apply(&paths).unwrap();

        let migrated = fs::read_to_string(paths.config_file()).unwrap();
        assert!(migrated.contains("# keep this comment\nversion = 4"));
        assert_eq!(fs::read(&outcome.backup_path).unwrap(), original);
        assert_eq!(outcome.initialized_private_records, 5);
        assert!(XanaConfig::parse(&migrated).is_ok());
    }

    #[test]
    fn rerun_is_idempotent_and_does_not_normalize_current_config() {
        let (_directory, paths, _) = paths_and_v3();
        ConfigMigrationPlan::build(&paths)
            .unwrap()
            .apply(&paths)
            .unwrap();
        let current = fs::read(paths.config_file()).unwrap();

        let plan = ConfigMigrationPlan::build(&paths).unwrap();
        assert!(!plan.requires_apply());
        let outcome = plan.apply(&paths).unwrap();

        assert!(!outcome.changed_config);
        assert_eq!(outcome.initialized_private_records, 0);
        assert_eq!(fs::read(paths.config_file()).unwrap(), current);
    }

    #[test]
    fn a_changed_config_invalidates_the_reviewed_plan() {
        let (_directory, paths, _) = paths_and_v3();
        let plan = ConfigMigrationPlan::build(&paths).unwrap();
        fs::write(paths.config_file(), b"changed").unwrap();

        assert!(matches!(
            plan.apply(&paths),
            Err(ConfigMigrationError::Changed(_))
        ));
        assert!(!paths.projects_file().exists());
    }

    #[test]
    fn a_held_migration_lock_fails_before_mutation() {
        let (_directory, paths, original) = paths_and_v3();
        let plan = ConfigMigrationPlan::build(&paths).unwrap();
        let held = MigrationLock::acquire(&paths.config_migration_lock_file()).unwrap();

        assert!(matches!(
            plan.apply(&paths),
            Err(ConfigMigrationError::Busy(_))
        ));
        assert_eq!(fs::read(paths.config_file()).unwrap(), original);
        drop(held);
    }
}
