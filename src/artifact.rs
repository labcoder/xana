//! Immutable content-addressed artifact bytes and logical registrations.

use crate::{
    identity::{ArtifactId, PrincipalId},
    resource::MAX_RESOURCE_SOURCE_BYTES,
};
use fs2::FileExt as _;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{
    error::Error,
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;
const HASH_HEX_LEN: usize = 64;
const MAX_RECOVERY_ENTRIES: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub(crate) struct ContentHash(String);

impl ContentHash {
    pub(crate) fn for_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn parse(value: String) -> Result<Self, &'static str> {
        if value.len() != HASH_HEX_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("content hash must be 64 lowercase hexadecimal characters");
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ArtifactRef {
    pub(crate) id: ArtifactId,
    pub(crate) content_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactRecord {
    pub(crate) reference: ArtifactRef,
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) owner: PrincipalId,
}

#[derive(Debug, Clone)]
pub(crate) struct ArtifactStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedArtifactRange {
    pub(crate) offset: u64,
    pub(crate) total_byte_len: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated_after: bool,
}

/// Bounded result of reconciling abandoned artifact staging files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ArtifactRecoveryReport {
    pub(crate) removed: usize,
    pub(crate) retained_active: usize,
    pub(crate) ignored: usize,
}

impl ArtifactStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn put(
        &self,
        bytes: &[u8],
        media_type: &str,
        owner: PrincipalId,
    ) -> Result<(ArtifactRecord, bool), ArtifactError> {
        self.put_bounded(bytes, media_type, owner, MAX_ARTIFACT_BYTES)
    }

    /// Publish one resource under a caller-selected soft limit.
    ///
    /// Existing callers retain the historical 4 MiB bound through
    /// [`Self::put`]. Resource adapters may select a stricter kind/route limit,
    /// but no caller can raise it above Xana's compiled resource ceiling.
    pub(crate) fn put_bounded(
        &self,
        bytes: &[u8],
        media_type: &str,
        owner: PrincipalId,
        max_bytes: usize,
    ) -> Result<(ArtifactRecord, bool), ArtifactError> {
        if max_bytes == 0 || max_bytes > MAX_RESOURCE_SOURCE_BYTES {
            return Err(ArtifactError::InvalidLimit {
                value: max_bytes,
                ceiling: MAX_RESOURCE_SOURCE_BYTES,
            });
        }
        if bytes.len() > max_bytes {
            return Err(ArtifactError::TooLarge {
                actual: bytes.len(),
                limit: max_bytes,
            });
        }
        if media_type.trim().is_empty() {
            return Err(ArtifactError::InvalidMediaType);
        }

        fs::create_dir_all(&self.root).map_err(|source| ArtifactError::Io {
            path: self.root.clone(),
            source,
        })?;
        let content_hash = ContentHash::for_bytes(bytes);
        let final_path = self.path_for(&content_hash);
        let was_created = if final_path.exists() {
            self.verify_path(&final_path, &content_hash, bytes.len(), max_bytes)?;
            false
        } else {
            self.publish_create_new(&final_path, bytes, &content_hash, max_bytes)?
        };

        Ok((
            ArtifactRecord {
                reference: ArtifactRef {
                    id: ArtifactId::new(),
                    content_hash,
                },
                media_type: media_type.to_owned(),
                byte_len: bytes.len() as u64,
                owner,
            },
            was_created,
        ))
    }

    pub(crate) fn read_bounded(
        &self,
        artifact: &ArtifactRecord,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ArtifactError> {
        let declared = usize::try_from(artifact.byte_len).map_err(|_| ArtifactError::TooLarge {
            actual: usize::MAX,
            limit: max_bytes,
        })?;
        if declared > max_bytes {
            return Err(ArtifactError::TooLarge {
                actual: declared,
                limit: max_bytes,
            });
        }
        let path = self.path_for(&artifact.reference.content_hash);
        let mut file = fs::File::open(&path).map_err(|source| ArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        let mut bytes = Vec::with_capacity(declared.min(max_bytes));
        Read::by_ref(&mut file)
            .take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| ArtifactError::Io {
                path: path.clone(),
                source,
            })?;
        if bytes.len() > max_bytes {
            return Err(ArtifactError::TooLarge {
                actual: bytes.len(),
                limit: max_bytes,
            });
        }
        self.verify_bytes(&path, &bytes, artifact)?;
        Ok(bytes)
    }

    /// Read one bounded range while streaming and verifying the complete
    /// immutable artifact. Bytes outside the requested range are never kept in
    /// memory, and a path replacement or content mismatch fails closed.
    pub(crate) fn read_verified_range(
        &self,
        artifact: &ArtifactRecord,
        offset: u64,
        max_range_bytes: usize,
        max_artifact_bytes: usize,
    ) -> Result<VerifiedArtifactRange, ArtifactError> {
        if max_artifact_bytes == 0 || max_artifact_bytes > MAX_RESOURCE_SOURCE_BYTES {
            return Err(ArtifactError::InvalidLimit {
                value: max_artifact_bytes,
                ceiling: MAX_RESOURCE_SOURCE_BYTES,
            });
        }
        let declared = usize::try_from(artifact.byte_len).map_err(|_| ArtifactError::TooLarge {
            actual: usize::MAX,
            limit: max_artifact_bytes,
        })?;
        if declared > max_artifact_bytes {
            return Err(ArtifactError::TooLarge {
                actual: declared,
                limit: max_artifact_bytes,
            });
        }
        if offset > artifact.byte_len {
            return Err(ArtifactError::InvalidRange {
                offset,
                length: artifact.byte_len,
            });
        }
        let path = self.path_for(&artifact.reference.content_hash);
        let path_metadata = fs::symlink_metadata(&path).map_err(|source| ArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        if path_metadata.file_type().is_symlink() || !path_metadata.file_type().is_file() {
            return Err(ArtifactError::NotRegular { path });
        }
        let mut file = fs::File::open(&path).map_err(|source| ArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        let opened_identity = artifact_file_identity(&file, &path)?;
        if file
            .metadata()
            .map_err(|source| ArtifactError::Io {
                path: path.clone(),
                source,
            })?
            .len()
            != artifact.byte_len
        {
            return Err(ArtifactError::CorruptContent { path });
        }
        let range_len = u64::try_from(max_range_bytes).unwrap_or(u64::MAX);
        let requested_end = offset.saturating_add(range_len).min(artifact.byte_len);
        let capacity = usize::try_from(requested_end.saturating_sub(offset))
            .unwrap_or(max_range_bytes)
            .min(max_range_bytes);
        let mut range = Vec::with_capacity(capacity);
        let mut hasher = blake3::Hasher::new();
        let mut actual = 0_u64;
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            let read = file.read(&mut chunk).map_err(|source| ArtifactError::Io {
                path: path.clone(),
                source,
            })?;
            if read == 0 {
                break;
            }
            let chunk_start = actual;
            actual = actual
                .checked_add(read as u64)
                .ok_or(ArtifactError::ArithmeticOverflow("artifact byte count"))?;
            if actual > max_artifact_bytes as u64 {
                return Err(ArtifactError::TooLarge {
                    actual: usize::try_from(actual).unwrap_or(usize::MAX),
                    limit: max_artifact_bytes,
                });
            }
            hasher.update(&chunk[..read]);
            let copy_start = chunk_start.max(offset);
            let copy_end = actual.min(requested_end);
            if copy_start < copy_end {
                let start = usize::try_from(copy_start - chunk_start)
                    .map_err(|_| ArtifactError::ArithmeticOverflow("artifact range start"))?;
                let end = usize::try_from(copy_end - chunk_start)
                    .map_err(|_| ArtifactError::ArithmeticOverflow("artifact range end"))?;
                range.extend_from_slice(&chunk[start..end]);
            }
        }
        if actual != artifact.byte_len
            || hasher.finalize().to_hex().as_str() != artifact.reference.content_hash.as_str()
        {
            return Err(ArtifactError::CorruptContent { path });
        }
        if artifact_path_identity(&path)? != opened_identity {
            return Err(ArtifactError::ChangedDuringRead { path });
        }
        Ok(VerifiedArtifactRange {
            offset,
            total_byte_len: artifact.byte_len,
            bytes: range,
            truncated_after: requested_end < artifact.byte_len,
        })
    }

    pub(crate) fn verify_reference(
        &self,
        reference: &ArtifactRef,
        declared_len: u64,
        max_bytes: usize,
    ) -> Result<(), ArtifactError> {
        if declared_len > max_bytes as u64 {
            return Err(ArtifactError::TooLarge {
                actual: usize::try_from(declared_len).unwrap_or(usize::MAX),
                limit: max_bytes,
            });
        }
        let path = self.path_for(&reference.content_hash);
        let mut file = fs::File::open(&path).map_err(|source| ArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        let actual_len = file
            .metadata()
            .map_err(|source| ArtifactError::Io {
                path: path.clone(),
                source,
            })?
            .len();
        if actual_len != declared_len {
            return Err(ArtifactError::CorruptContent { path });
        }
        let mut hasher = blake3::Hasher::new();
        let mut remaining = declared_len;
        let mut chunk = [0_u8; 16 * 1024];
        while remaining > 0 {
            let limit =
                usize::try_from(remaining.min(chunk.len() as u64)).expect("chunk limit fits usize");
            let read = file
                .read(&mut chunk[..limit])
                .map_err(|source| ArtifactError::Io {
                    path: path.clone(),
                    source,
                })?;
            if read == 0 {
                return Err(ArtifactError::CorruptContent { path });
            }
            hasher.update(&chunk[..read]);
            remaining -= read as u64;
        }
        if hasher.finalize().to_hex().as_str() != reference.content_hash.as_str() {
            return Err(ArtifactError::CorruptContent { path });
        }
        Ok(())
    }

    pub(crate) fn verified_path(
        &self,
        artifact: &ArtifactRecord,
        max_bytes: usize,
    ) -> Result<PathBuf, ArtifactError> {
        self.verify_reference(&artifact.reference, artifact.byte_len, max_bytes)?;
        Ok(self.path_for(&artifact.reference.content_hash))
    }

    /// Streams one verified immutable artifact into a new caller-selected file.
    ///
    /// The destination is never overwritten. Xana hashes the complete source
    /// while copying, checks both path identities after I/O, and removes only
    /// the partial file created by this call if verification fails.
    pub(crate) fn copy_verified_create_new(
        &self,
        artifact: &ArtifactRecord,
        destination: &Path,
        max_bytes: usize,
    ) -> Result<(), ArtifactError> {
        if max_bytes == 0 || max_bytes > MAX_RESOURCE_SOURCE_BYTES {
            return Err(ArtifactError::InvalidLimit {
                value: max_bytes,
                ceiling: MAX_RESOURCE_SOURCE_BYTES,
            });
        }
        let declared = usize::try_from(artifact.byte_len).map_err(|_| ArtifactError::TooLarge {
            actual: usize::MAX,
            limit: max_bytes,
        })?;
        if declared > max_bytes {
            return Err(ArtifactError::TooLarge {
                actual: declared,
                limit: max_bytes,
            });
        }

        let source_path = self.path_for(&artifact.reference.content_hash);
        let source_metadata =
            fs::symlink_metadata(&source_path).map_err(|source| ArtifactError::Io {
                path: source_path.clone(),
                source,
            })?;
        if source_metadata.file_type().is_symlink() || !source_metadata.file_type().is_file() {
            return Err(ArtifactError::NotRegular { path: source_path });
        }
        let mut input = fs::File::open(&source_path).map_err(|source| ArtifactError::Io {
            path: source_path.clone(),
            source,
        })?;
        let source_identity = artifact_file_identity(&input, &source_path)?;
        if input
            .metadata()
            .map_err(|source| ArtifactError::Io {
                path: source_path.clone(),
                source,
            })?
            .len()
            != artifact.byte_len
        {
            return Err(ArtifactError::CorruptContent { path: source_path });
        }

        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|source| ArtifactError::Io {
                path: destination.to_owned(),
                source,
            })?;
        let destination_identity = artifact_file_identity(&output, destination)?;
        let result =
            (|| {
                let mut actual = 0_u64;
                let mut hasher = blake3::Hasher::new();
                let mut chunk = [0_u8; 16 * 1024];
                loop {
                    let read = input.read(&mut chunk).map_err(|source| ArtifactError::Io {
                        path: source_path.clone(),
                        source,
                    })?;
                    if read == 0 {
                        break;
                    }
                    actual = actual.checked_add(read as u64).ok_or(
                        ArtifactError::ArithmeticOverflow("artifact copy byte count"),
                    )?;
                    if actual > max_bytes as u64 {
                        return Err(ArtifactError::TooLarge {
                            actual: usize::try_from(actual).unwrap_or(usize::MAX),
                            limit: max_bytes,
                        });
                    }
                    hasher.update(&chunk[..read]);
                    output
                        .write_all(&chunk[..read])
                        .map_err(|source| ArtifactError::Io {
                            path: destination.to_owned(),
                            source,
                        })?;
                }
                output.flush().map_err(|source| ArtifactError::Io {
                    path: destination.to_owned(),
                    source,
                })?;
                if actual != artifact.byte_len
                    || hasher.finalize().to_hex().as_str()
                        != artifact.reference.content_hash.as_str()
                {
                    return Err(ArtifactError::CorruptContent {
                        path: source_path.clone(),
                    });
                }
                if artifact_path_identity(&source_path)? != source_identity {
                    return Err(ArtifactError::ChangedDuringRead {
                        path: source_path.clone(),
                    });
                }
                if artifact_path_identity(destination)? != destination_identity {
                    return Err(ArtifactError::ChangedDuringRead {
                        path: destination.to_owned(),
                    });
                }
                Ok(())
            })();
        drop(output);
        if result.is_err()
            && artifact_path_identity(destination)
                .is_ok_and(|identity| identity == destination_identity)
        {
            let _ = fs::remove_file(destination);
        }
        result
    }

    /// Removes only unlocked staging files created by [`Self::put_bounded`].
    ///
    /// Published artifacts are immutable hash-named files. A process loss can
    /// leave the UUID-named staging file behind before or after publication;
    /// it is never an authoritative artifact. The writer holds an exclusive
    /// lock, so recovery cannot remove another live writer's staging file.
    pub(crate) fn reconcile_partials(&self) -> Result<ArtifactRecoveryReport, ArtifactError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ArtifactRecoveryReport::default());
            }
            Err(source) => {
                return Err(ArtifactError::Io {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        let mut report = ArtifactRecoveryReport::default();
        for (index, entry) in entries.enumerate() {
            if index >= MAX_RECOVERY_ENTRIES {
                return Err(ArtifactError::RecoveryLimit {
                    path: self.root.clone(),
                    limit: MAX_RECOVERY_ENTRIES,
                });
            }
            let entry = entry.map_err(|source| ArtifactError::Io {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            if !is_staging_path(&path) {
                report.ignored = report.ignored.saturating_add(1);
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|source| ArtifactError::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                report.ignored = report.ignored.saturating_add(1);
                continue;
            }
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|source| ArtifactError::Io {
                    path: path.clone(),
                    source,
                })?;
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = fs2::FileExt::unlock(&file);
                    drop(file);
                    match fs::remove_file(&path) {
                        Ok(()) => report.removed = report.removed.saturating_add(1),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(source) => return Err(ArtifactError::Io { path, source }),
                    }
                }
                Err(error) if lock_is_held(&error) => {
                    report.retained_active = report.retained_active.saturating_add(1);
                }
                Err(source) => return Err(ArtifactError::Io { path, source }),
            }
        }
        Ok(report)
    }

    fn path_for(&self, content_hash: &ContentHash) -> PathBuf {
        self.root.join(content_hash.as_str())
    }

    fn publish_create_new(
        &self,
        final_path: &Path,
        bytes: &[u8],
        content_hash: &ContentHash,
        max_bytes: usize,
    ) -> Result<bool, ArtifactError> {
        let temp_path = self.root.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut temp = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .map_err(|source| ArtifactError::Io {
                    path: temp_path.clone(),
                    source,
                })?;
            temp.try_lock_exclusive()
                .map_err(|source| ArtifactError::Io {
                    path: temp_path.clone(),
                    source,
                })?;
            temp.write_all(bytes)
                .and_then(|()| temp.flush())
                .map_err(|source| ArtifactError::Io {
                    path: temp_path.clone(),
                    source,
                })?;
            drop(temp);

            match fs::hard_link(&temp_path, final_path) {
                Ok(()) => Ok(true),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                    self.verify_path(final_path, content_hash, bytes.len(), max_bytes)?;
                    Ok(false)
                }
                Err(source) => Err(ArtifactError::Io {
                    path: final_path.to_owned(),
                    source,
                }),
            }
        })();
        let _ = fs::remove_file(&temp_path);
        result
    }

    fn verify_path(
        &self,
        path: &Path,
        expected_hash: &ContentHash,
        expected_len: usize,
        max_bytes: usize,
    ) -> Result<(), ArtifactError> {
        let actual_len = fs::metadata(path)
            .map_err(|source| ArtifactError::Io {
                path: path.to_owned(),
                source,
            })?
            .len();
        if actual_len != expected_len as u64 || actual_len > max_bytes as u64 {
            return Err(ArtifactError::CorruptContent {
                path: path.to_owned(),
            });
        }
        let mut file = fs::File::open(path).map_err(|source| ArtifactError::Io {
            path: path.to_owned(),
            source,
        })?;
        let mut hasher = blake3::Hasher::new();
        let mut remaining = expected_len;
        let mut chunk = [0_u8; 16 * 1024];
        while remaining > 0 {
            let limit = remaining.min(chunk.len());
            let read = file
                .read(&mut chunk[..limit])
                .map_err(|source| ArtifactError::Io {
                    path: path.to_owned(),
                    source,
                })?;
            if read == 0 {
                return Err(ArtifactError::CorruptContent {
                    path: path.to_owned(),
                });
            }
            hasher.update(&chunk[..read]);
            remaining -= read;
        }
        if hasher.finalize().to_hex().as_str() != expected_hash.as_str() {
            return Err(ArtifactError::CorruptContent {
                path: path.to_owned(),
            });
        }
        Ok(())
    }

    fn verify_bytes(
        &self,
        path: &Path,
        bytes: &[u8],
        artifact: &ArtifactRecord,
    ) -> Result<(), ArtifactError> {
        if bytes.len() as u64 != artifact.byte_len
            || ContentHash::for_bytes(bytes) != artifact.reference.content_hash
        {
            return Err(ArtifactError::CorruptContent {
                path: path.to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum ArtifactError {
    InvalidMediaType,
    InvalidLimit { value: usize, ceiling: usize },
    TooLarge { actual: usize, limit: usize },
    InvalidRange { offset: u64, length: u64 },
    NotRegular { path: PathBuf },
    ChangedDuringRead { path: PathBuf },
    ArithmeticOverflow(&'static str),
    RecoveryLimit { path: PathBuf, limit: usize },
    CorruptContent { path: PathBuf },
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMediaType => write!(formatter, "artifact media type must not be blank"),
            Self::InvalidLimit { value, ceiling } => write!(
                formatter,
                "artifact byte limit {value} is invalid; compiled ceiling is {ceiling}"
            ),
            Self::TooLarge { actual, limit } => write!(
                formatter,
                "artifact contains {actual} bytes, exceeding the {limit}-byte limit"
            ),
            Self::InvalidRange { offset, length } => write!(
                formatter,
                "artifact range starts at {offset}, beyond its {length}-byte length"
            ),
            Self::NotRegular { path } => write!(
                formatter,
                "artifact content at {} is not a regular immutable file",
                path.display()
            ),
            Self::ChangedDuringRead { path } => write!(
                formatter,
                "artifact content at {} changed while it was being verified",
                path.display()
            ),
            Self::ArithmeticOverflow(operation) => {
                write!(formatter, "artifact {operation} overflowed")
            }
            Self::RecoveryLimit { path, limit } => write!(
                formatter,
                "artifact recovery at {} exceeds its {limit}-entry bound",
                path.display()
            ),
            Self::CorruptContent { path } => write!(
                formatter,
                "artifact content at {} does not match its immutable identity",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "artifact I/O at {} failed: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for ArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidMediaType
            | Self::InvalidLimit { .. }
            | Self::TooLarge { .. }
            | Self::InvalidRange { .. }
            | Self::NotRegular { .. }
            | Self::ChangedDuringRead { .. }
            | Self::ArithmeticOverflow(_)
            | Self::RecoveryLimit { .. }
            | Self::CorruptContent { .. } => None,
        }
    }
}

fn is_staging_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some(uuid) = name
        .strip_prefix('.')
        .and_then(|value| value.strip_suffix(".tmp"))
    else {
        return false;
    };
    uuid::Uuid::parse_str(uuid).is_ok()
}

fn lock_is_held(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(33)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(any(unix, windows))]
fn artifact_file_identity(
    file: &fs::File,
    path: &Path,
) -> Result<same_file::Handle, ArtifactError> {
    let cloned = file.try_clone().map_err(|source| ArtifactError::Io {
        path: path.to_owned(),
        source,
    })?;
    same_file::Handle::from_file(cloned).map_err(|source| ArtifactError::Io {
        path: path.to_owned(),
        source,
    })
}

#[cfg(not(any(unix, windows)))]
fn artifact_file_identity(file: &fs::File, path: &Path) -> Result<u64, ArtifactError> {
    file.metadata()
        .map(|metadata| metadata.len())
        .map_err(|source| ArtifactError::Io {
            path: path.to_owned(),
            source,
        })
}

#[cfg(any(unix, windows))]
fn artifact_path_identity(path: &Path) -> Result<same_file::Handle, ArtifactError> {
    same_file::Handle::from_path(path).map_err(|source| ArtifactError::Io {
        path: path.to_owned(),
        source,
    })
}

#[cfg(not(any(unix, windows)))]
fn artifact_path_identity(path: &Path) -> Result<u64, ArtifactError> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|source| ArtifactError::Io {
            path: path.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests;
