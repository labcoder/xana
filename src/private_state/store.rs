use super::schema::{
    EndpointTrustDocument, OutboundDecisionDocument, PRIVATE_RECORD_VERSION, PackageStateDocument,
    ProjectBindingsDocument, ProjectRegistryDocument,
};
use crate::{bounded_file, paths::XanaPaths};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

const MAX_PRIVATE_RECORD_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrivateRecordStatus {
    Healthy,
    Missing,
    Invalid,
    Unsupported,
    Indeterminate,
}

impl PrivateRecordStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::Unsupported => "unsupported",
            Self::Indeterminate => "indeterminate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrivateRecordInspection {
    pub(crate) name: &'static str,
    pub(crate) version: Option<u32>,
    pub(crate) status: PrivateRecordStatus,
}

#[derive(Debug)]
pub(crate) enum PrivateStateError {
    Busy(PathBuf),
    Io { path: PathBuf, source: io::Error },
    TooLarge { path: PathBuf, actual: u64 },
    Decode { path: PathBuf, reason: String },
    UnsupportedVersion { path: PathBuf, found: u32 },
    Encode(serde_json::Error),
    Invalid(String),
}

impl fmt::Display for PrivateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(path) => write!(
                formatter,
                "private Xana state is being changed by another process ({})",
                path.display()
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::TooLarge { path, actual } => write!(
                formatter,
                "{} contains {actual} bytes, exceeding the {MAX_PRIVATE_RECORD_BYTES}-byte limit",
                path.display()
            ),
            Self::Decode { path, reason } => {
                write!(formatter, "could not decode {}: {reason}", path.display())
            }
            Self::UnsupportedVersion { path, found } => write!(
                formatter,
                "{} uses unsupported private-record version {found}; expected {PRIVATE_RECORD_VERSION}",
                path.display()
            ),
            Self::Encode(source) => {
                write!(formatter, "could not encode private Xana state: {source}")
            }
            Self::Invalid(reason) => formatter.write_str(reason),
        }
    }
}

impl Error for PrivateStateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Encode(source) => Some(source),
            Self::Busy(_)
            | Self::TooLarge { .. }
            | Self::Decode { .. }
            | Self::UnsupportedVersion { .. }
            | Self::Invalid(_) => None,
        }
    }
}

pub(crate) fn inspect_interoperable_records(paths: &XanaPaths) -> Vec<PrivateRecordInspection> {
    vec![
        inspect::<ProjectRegistryDocument>("projects", &paths.projects_file()),
        inspect::<ProjectBindingsDocument>("project bindings", &paths.project_bindings_file()),
        inspect::<PackageStateDocument>("package state", &paths.package_state_file()),
        inspect::<EndpointTrustDocument>("endpoint trust", &paths.endpoint_trust_file()),
        inspect::<OutboundDecisionDocument>("outbound decisions", &paths.outbound_decisions_file()),
    ]
}

pub(crate) fn ensure_interoperable_records(
    paths: &XanaPaths,
) -> Result<Vec<PathBuf>, PrivateStateError> {
    let mut created = Vec::new();
    ensure_one(
        &paths.projects_file(),
        &ProjectRegistryDocument::default(),
        &mut created,
    )?;
    ensure_one(
        &paths.project_bindings_file(),
        &ProjectBindingsDocument::default(),
        &mut created,
    )?;
    ensure_one(
        &paths.package_state_file(),
        &PackageStateDocument::default(),
        &mut created,
    )?;
    ensure_one(
        &paths.endpoint_trust_file(),
        &EndpointTrustDocument::default(),
        &mut created,
    )?;
    ensure_one(
        &paths.outbound_decisions_file(),
        &OutboundDecisionDocument::default(),
        &mut created,
    )?;
    Ok(created)
}

pub(crate) fn read_document<T: DeserializeOwned>(path: &Path) -> Result<T, PrivateStateError> {
    let bytes = bounded_file::read(path, MAX_PRIVATE_RECORD_BYTES).map_err(map_read_error)?;
    let document: T =
        serde_json::from_slice(&bytes).map_err(|error| PrivateStateError::Decode {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| PrivateStateError::Decode {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    let version = value.get("version").and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(PRIVATE_RECORD_VERSION)) {
        return Err(PrivateStateError::UnsupportedVersion {
            path: path.to_owned(),
            found: version
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
        });
    }
    Ok(document)
}

#[derive(Debug)]
pub(crate) enum UpdateDocumentError<E> {
    State(PrivateStateError),
    Update(E),
}

pub(crate) fn update_document<T, R, E>(
    path: &Path,
    update: impl FnOnce(&mut T) -> Result<R, E>,
) -> Result<R, UpdateDocumentError<E>>
where
    T: DeserializeOwned + Serialize,
{
    let _lock = RecordLock::acquire(path).map_err(UpdateDocumentError::State)?;
    let mut document = read_document(path).map_err(UpdateDocumentError::State)?;
    let output = update(&mut document).map_err(UpdateDocumentError::Update)?;
    atomic_write_json(path, &document).map_err(UpdateDocumentError::State)?;
    Ok(output)
}

fn ensure_one<T: DeserializeOwned + Serialize>(
    path: &Path,
    empty: &T,
    created: &mut Vec<PathBuf>,
) -> Result<(), PrivateStateError> {
    match read_document::<T>(path) {
        Ok(_) => Ok(()),
        Err(PrivateStateError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            atomic_write_json(path, empty)?;
            created.push(path.to_owned());
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn inspect<T: DeserializeOwned>(name: &'static str, path: &Path) -> PrivateRecordInspection {
    let (version, status) = match bounded_file::read(path, MAX_PRIVATE_RECORD_BYTES) {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => {
                let version = value
                    .get("version")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok());
                let status = if version != Some(PRIVATE_RECORD_VERSION) {
                    PrivateRecordStatus::Unsupported
                } else if serde_json::from_slice::<T>(&bytes).is_ok() {
                    PrivateRecordStatus::Healthy
                } else {
                    PrivateRecordStatus::Invalid
                };
                (version, status)
            }
            Err(_) => (None, PrivateRecordStatus::Invalid),
        },
        Err(bounded_file::BoundedReadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            (None, PrivateRecordStatus::Missing)
        }
        Err(bounded_file::BoundedReadError::TooLarge { .. }) => {
            (None, PrivateRecordStatus::Invalid)
        }
        Err(_) => (None, PrivateRecordStatus::Indeterminate),
    };
    PrivateRecordInspection {
        name,
        version,
        status,
    }
}

fn atomic_write_json<T: Serialize>(path: &Path, document: &T) -> Result<(), PrivateStateError> {
    let parent = path.parent().ok_or_else(|| {
        PrivateStateError::Invalid(format!("{} has no parent directory", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|source| PrivateStateError::Io {
        path: parent.to_owned(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(document).map_err(PrivateStateError::Encode)?;
    if bytes.len() > MAX_PRIVATE_RECORD_BYTES {
        return Err(PrivateStateError::TooLarge {
            path: path.to_owned(),
            actual: bytes.len() as u64,
        });
    }
    let mut file =
        atomic_write_file::AtomicWriteFile::open(path).map_err(|source| PrivateStateError::Io {
            path: path.to_owned(),
            source,
        })?;
    protect_open_file(file.as_file()).map_err(|source| PrivateStateError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(&bytes)
        .map_err(|source| PrivateStateError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.commit().map_err(|source| PrivateStateError::Io {
        path: path.to_owned(),
        source,
    })
}

fn map_read_error(error: bounded_file::BoundedReadError) -> PrivateStateError {
    match error {
        bounded_file::BoundedReadError::TooLarge { path, actual, .. } => {
            PrivateStateError::TooLarge { path, actual }
        }
        bounded_file::BoundedReadError::Io { path, source } => {
            PrivateStateError::Io { path, source }
        }
    }
}

struct RecordLock {
    file: fs::File,
}

impl RecordLock {
    fn acquire(document_path: &Path) -> Result<Self, PrivateStateError> {
        let lock_path = document_path.with_extension("json.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| PrivateStateError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| PrivateStateError::Io {
                path: lock_path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(fs::TryLockError::WouldBlock) => Err(PrivateStateError::Busy(lock_path)),
            Err(fs::TryLockError::Error(source)) => Err(PrivateStateError::Io {
                path: lock_path,
                source,
            }),
        }
    }
}

impl Drop for RecordLock {
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
    use crate::paths::XanaPaths;
    use std::ffi::OsString;
    use tempfile::tempdir;

    #[test]
    fn initialization_is_idempotent_and_inspection_is_redacted() {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();

        let first = ensure_interoperable_records(&paths).unwrap();
        let second = ensure_interoperable_records(&paths).unwrap();

        assert_eq!(first.len(), 5);
        assert!(second.is_empty());
        assert!(
            inspect_interoperable_records(&paths)
                .iter()
                .all(|item| item.status == PrivateRecordStatus::Healthy
                    && item.version == Some(PRIVATE_RECORD_VERSION))
        );
    }

    #[test]
    fn corrupt_or_future_records_are_never_replaced() {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        fs::create_dir_all(paths.projects_file().parent().unwrap()).unwrap();
        fs::write(paths.projects_file(), b"not-json").unwrap();

        assert!(ensure_interoperable_records(&paths).is_err());
        assert_eq!(fs::read(paths.projects_file()).unwrap(), b"not-json");

        fs::write(
            paths.projects_file(),
            br#"{"version":99,"projects":{},"conversation_memberships":{}}"#,
        )
        .unwrap();
        assert!(matches!(
            read_document::<ProjectRegistryDocument>(&paths.projects_file()),
            Err(PrivateStateError::UnsupportedVersion { found: 99, .. })
        ));
    }

    #[test]
    fn locked_updates_fail_without_overwriting() {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        ensure_interoperable_records(&paths).unwrap();
        let held = RecordLock::acquire(&paths.projects_file()).unwrap();

        let result = update_document::<ProjectRegistryDocument, _, PrivateStateError>(
            &paths.projects_file(),
            |_| Ok(()),
        );

        assert!(matches!(
            result,
            Err(UpdateDocumentError::State(PrivateStateError::Busy(_)))
        ));
        drop(held);
    }
}
