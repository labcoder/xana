//! Reviewable, failure-safe migration for shared configuration and private records.
//!
//! Private records are made durable first; the atomic config version update is
//! the final authoritative marker. A retry after interruption is idempotent.

use crate::{
    bounded_file,
    config::{CONFIG_VERSION, ConfigTransactionLock, XanaConfig},
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
        self.apply_with(paths, &FilesystemMigrationWriter)
    }

    fn apply_with(
        self,
        paths: &XanaPaths,
        writer: &dyn MigrationWriter,
    ) -> Result<ConfigMigrationOutcome, ConfigMigrationError> {
        let _lock = ConfigTransactionLock::acquire(&self.config_path)
            .map_err(|error| ConfigMigrationError::Private(error.to_string()))?;
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

        let backup_path = self.config_path.with_extension("toml.bak");
        let previous_backup = read_optional(&backup_path)?;
        let created = ensure_interoperable_records(paths)
            .map_err(|error| ConfigMigrationError::Private(error.to_string()))?;
        if let Err(error) = writer.write(&backup_path, &self.original, MigrationWrite::Backup) {
            return Err(with_rollback_failures(error, remove_created(&created)));
        }
        if let Err(error) = writer.write(&self.config_path, &self.migrated, MigrationWrite::Config)
        {
            let restore = writer.restore(&backup_path, previous_backup.as_deref());
            let mut failures = remove_created(&created);
            if let Err(restore) = restore {
                failures.push(restore.to_string());
            }
            return Err(with_rollback_failures(error, failures));
        }

        Ok(ConfigMigrationOutcome {
            changed_config: self.original != self.migrated,
            initialized_private_records: created.len(),
            backup_path,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MigrationWrite {
    Backup,
    Config,
}

trait MigrationWriter {
    fn write(
        &self,
        path: &Path,
        bytes: &[u8],
        stage: MigrationWrite,
    ) -> Result<(), ConfigMigrationError>;

    fn restore(&self, path: &Path, bytes: Option<&[u8]>) -> Result<(), ConfigMigrationError>;
}

struct FilesystemMigrationWriter;

impl MigrationWriter for FilesystemMigrationWriter {
    fn write(
        &self,
        path: &Path,
        bytes: &[u8],
        _stage: MigrationWrite,
    ) -> Result<(), ConfigMigrationError> {
        atomic_write(path, bytes)
    }

    fn restore(&self, path: &Path, bytes: Option<&[u8]>) -> Result<(), ConfigMigrationError> {
        restore_optional(path, bytes)
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
    Changed(PathBuf),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Invalid(String),
    Private(String),
    Rollback {
        operation: Box<ConfigMigrationError>,
        failures: Vec<String>,
    },
}

impl fmt::Display for ConfigMigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed(path) => write!(
                formatter,
                "{} changed after the migration review; create a new plan",
                path.display()
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Invalid(reason) | Self::Private(reason) => formatter.write_str(reason),
            Self::Rollback {
                operation,
                failures,
            } => write!(
                formatter,
                "{operation}; rollback was incomplete: {}",
                failures.join("; ")
            ),
        }
    }
}

impl Error for ConfigMigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Rollback { operation, .. } => Some(operation.as_ref()),
            Self::Changed(_) | Self::Invalid(_) | Self::Private(_) => None,
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

fn remove_created(paths: &[PathBuf]) -> Vec<String> {
    let mut failures = Vec::new();
    for path in paths.iter().rev() {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => failures.push(format!("{}: {source}", path.display())),
        }
    }
    failures
}

fn with_rollback_failures(
    operation: ConfigMigrationError,
    failures: Vec<String>,
) -> ConfigMigrationError {
    if failures.is_empty() {
        operation
    } else {
        ConfigMigrationError::Rollback {
            operation: Box::new(operation),
            failures,
        }
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

    struct FaultWriter {
        fail_config: Option<io::ErrorKind>,
        fail_restore: bool,
    }

    impl MigrationWriter for FaultWriter {
        fn write(
            &self,
            path: &Path,
            bytes: &[u8],
            stage: MigrationWrite,
        ) -> Result<(), ConfigMigrationError> {
            if stage == MigrationWrite::Config
                && let Some(kind) = self.fail_config
            {
                return Err(ConfigMigrationError::Io {
                    path: path.to_owned(),
                    source: io::Error::new(kind, "injected final config commit failure"),
                });
            }
            atomic_write(path, bytes)
        }

        fn restore(&self, path: &Path, bytes: Option<&[u8]>) -> Result<(), ConfigMigrationError> {
            if self.fail_restore {
                return Err(ConfigMigrationError::Io {
                    path: path.to_owned(),
                    source: io::Error::other("injected backup restore failure"),
                });
            }
            restore_optional(path, bytes)
        }
    }

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
        assert_eq!(outcome.initialized_private_records, 7);
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
        let held = ConfigTransactionLock::acquire(paths.config_file()).unwrap();

        let error = plan.apply(&paths).unwrap_err();
        assert!(error.to_string().contains("another Xana process"));
        assert_eq!(fs::read(paths.config_file()).unwrap(), original);
        drop(held);
    }

    #[test]
    fn backup_failure_removes_every_record_created_by_the_attempt() {
        let (_directory, paths, original) = paths_and_v3();
        let plan = ConfigMigrationPlan::build(&paths).unwrap();
        fs::create_dir(paths.config_file().with_extension("toml.bak")).unwrap();

        assert!(plan.apply(&paths).is_err());

        assert_eq!(fs::read(paths.config_file()).unwrap(), original);
        let records = inspect_interoperable_records(&paths);
        assert!(
            records
                .iter()
                .all(|record| record.status == PrivateRecordStatus::Missing),
            "{records:?}"
        );
    }

    #[test]
    fn cleanup_reports_failure_but_still_attempts_every_created_path() {
        let directory = tempdir().unwrap();
        let removable = directory.path().join("removable.json");
        let invalid = directory.path().join("directory.json");
        fs::write(&removable, b"temporary").unwrap();
        fs::create_dir(&invalid).unwrap();

        let failures = remove_created(&[removable.clone(), invalid]);

        assert_eq!(failures.len(), 1);
        assert!(!removable.exists(), "later cleanup was not attempted");
    }

    #[test]
    fn final_commit_io_failures_restore_backup_config_and_private_state() {
        for kind in [
            io::ErrorKind::WriteZero,
            io::ErrorKind::Interrupted,
            io::ErrorKind::Other,
        ] {
            let (_directory, paths, original) = paths_and_v3();
            let backup = paths.config_file().with_extension("toml.bak");
            fs::write(&backup, b"prior backup").unwrap();
            let writer = FaultWriter {
                fail_config: Some(kind),
                fail_restore: false,
            };

            assert!(
                ConfigMigrationPlan::build(&paths)
                    .unwrap()
                    .apply_with(&paths, &writer)
                    .is_err()
            );
            assert_eq!(fs::read(paths.config_file()).unwrap(), original);
            assert_eq!(fs::read(backup).unwrap(), b"prior backup");
            assert!(
                inspect_interoperable_records(&paths)
                    .iter()
                    .all(|record| record.status == PrivateRecordStatus::Missing)
            );
        }
    }

    #[test]
    fn backup_restore_failure_is_reported_after_every_private_cleanup() {
        let (_directory, paths, original) = paths_and_v3();
        fs::write(
            paths.config_file().with_extension("toml.bak"),
            b"prior backup",
        )
        .unwrap();
        let writer = FaultWriter {
            fail_config: Some(io::ErrorKind::WriteZero),
            fail_restore: true,
        };

        let error = ConfigMigrationPlan::build(&paths)
            .unwrap()
            .apply_with(&paths, &writer)
            .unwrap_err();

        assert!(matches!(error, ConfigMigrationError::Rollback { .. }));
        assert_eq!(fs::read(paths.config_file()).unwrap(), original);
        assert!(
            inspect_interoperable_records(&paths)
                .iter()
                .all(|record| record.status == PrivateRecordStatus::Missing)
        );
    }

    #[test]
    fn abandoning_a_reviewed_plan_before_apply_mutates_nothing() {
        let (_directory, paths, original) = paths_and_v3();
        drop(ConfigMigrationPlan::build(&paths).unwrap());

        assert_eq!(fs::read(paths.config_file()).unwrap(), original);
        assert!(!paths.config_file().with_extension("toml.bak").exists());
        assert!(
            inspect_interoperable_records(&paths)
                .iter()
                .all(|record| record.status == PrivateRecordStatus::Missing)
        );
    }
}
