//! Immutable content-addressed artifact bytes and logical registrations.

use crate::{
    bounded_file,
    identity::{ArtifactId, PrincipalId},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{
    error::Error,
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;
const HASH_HEX_LEN: usize = 64;

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
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::TooLarge {
                actual: bytes.len(),
                limit: MAX_ARTIFACT_BYTES,
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
            self.verify_path(&final_path, &content_hash, bytes.len())?;
            false
        } else {
            self.publish_create_new(&final_path, bytes, &content_hash)?
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

    fn path_for(&self, content_hash: &ContentHash) -> PathBuf {
        self.root.join(content_hash.as_str())
    }

    fn publish_create_new(
        &self,
        final_path: &Path,
        bytes: &[u8],
        content_hash: &ContentHash,
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
                    self.verify_path(final_path, content_hash, bytes.len())?;
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
    ) -> Result<(), ArtifactError> {
        let actual_len = fs::metadata(path)
            .map_err(|source| ArtifactError::Io {
                path: path.to_owned(),
                source,
            })?
            .len();
        if actual_len != expected_len as u64 || actual_len > MAX_ARTIFACT_BYTES as u64 {
            return Err(ArtifactError::CorruptContent {
                path: path.to_owned(),
            });
        }
        let bytes = bounded_file::read(path, MAX_ARTIFACT_BYTES).map_err(|error| match error {
            bounded_file::BoundedReadError::TooLarge { .. } => ArtifactError::CorruptContent {
                path: path.to_owned(),
            },
            bounded_file::BoundedReadError::Io { path, source } => {
                ArtifactError::Io { path, source }
            }
        })?;
        if bytes.len() != expected_len || ContentHash::for_bytes(&bytes) != *expected_hash {
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
    TooLarge { actual: usize, limit: usize },
    CorruptContent { path: PathBuf },
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMediaType => write!(formatter, "artifact media type must not be blank"),
            Self::TooLarge { actual, limit } => write!(
                formatter,
                "artifact contains {actual} bytes, exceeding the {limit}-byte limit"
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
            Self::InvalidMediaType | Self::TooLarge { .. } | Self::CorruptContent { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
