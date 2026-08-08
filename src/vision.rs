//! Artifact-backed image input and bounded provider media resolution.

#![allow(dead_code)]

use crate::{
    artifact::{ArtifactRecord, ArtifactStore, MAX_ARTIFACT_BYTES},
    identity::PrincipalId,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    collections::VecDeque,
    error::Error,
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_IMAGE_PIXELS: u64 = 40_000_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ImageRef {
    pub(crate) artifact: ArtifactRecord,
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageAttachment {
    pub(crate) source_path: String,
    pub(crate) image: ImageRef,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct PendingImages {
    queue: VecDeque<ImageAttachment>,
}

impl PendingImages {
    pub(crate) fn push(&mut self, attachment: ImageAttachment) {
        self.queue.push_back(attachment);
    }
    pub(crate) fn clear(&mut self) -> usize {
        let count = self.queue.len();
        self.queue.clear();
        count
    }
    pub(crate) fn take_for_turn(&mut self) -> Vec<ImageAttachment> {
        self.queue.drain(..).collect()
    }
    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageLimits {
    pub(crate) max_bytes: usize,
    pub(crate) max_pixels: u64,
}
impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_ARTIFACT_BYTES,
            max_pixels: MAX_IMAGE_PIXELS,
        }
    }
}

#[derive(Debug)]
pub(crate) enum ImageError {
    InvalidPath,
    OutsideWorkspace,
    NotRegular,
    Io(std::io::Error),
    TooLarge { actual: usize, limit: usize },
    UnsupportedFormat,
    Malformed,
    PixelLimit { actual: u64, limit: u64 },
    Artifact(crate::artifact::ArtifactError),
}
impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => {
                f.write_str("image path must be a non-empty workspace-relative path")
            }
            Self::OutsideWorkspace => f.write_str("image path resolves outside the workspace"),
            Self::NotRegular => f.write_str("image path is not a regular file"),
            Self::Io(error) => error.fmt(f),
            Self::TooLarge { actual, limit } => {
                write!(f, "image is {actual} bytes; limit is {limit}")
            }
            Self::UnsupportedFormat => f.write_str("unsupported image format"),
            Self::Malformed => f.write_str("malformed image header"),
            Self::PixelLimit { actual, limit } => {
                write!(f, "image has {actual} pixels; limit is {limit}")
            }
            Self::Artifact(error) => error.fmt(f),
        }
    }
}
impl Error for ImageError {}
impl From<std::io::Error> for ImageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<crate::artifact::ArtifactError> for ImageError {
    fn from(error: crate::artifact::ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

pub(crate) struct ImageIngestor {
    store: ArtifactStore,
    limits: ImageLimits,
}
impl ImageIngestor {
    pub(crate) fn new(store: ArtifactStore, limits: ImageLimits) -> Self {
        Self { store, limits }
    }
    pub(crate) fn ingest_path(
        &self,
        workspace_root: &Path,
        source_path: &str,
        owner: PrincipalId,
    ) -> Result<ImageAttachment, ImageError> {
        if source_path.trim().is_empty() {
            return Err(ImageError::InvalidPath);
        }
        let relative = PathBuf::from(source_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(ImageError::InvalidPath);
        }
        let root = workspace_root.canonicalize().map_err(ImageError::Io)?;
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            return Err(ImageError::NotRegular);
        }
        let canonical = path.canonicalize().map_err(ImageError::Io)?;
        if !canonical.starts_with(&root) {
            return Err(ImageError::OutsideWorkspace);
        }
        let mut file = fs::File::open(&canonical)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take((self.limits.max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > self.limits.max_bytes {
            return Err(ImageError::TooLarge {
                actual: bytes.len(),
                limit: self.limits.max_bytes,
            });
        }
        let metadata = inspect_image(&bytes, self.limits)?;
        let (artifact, _) = self.store.put(&bytes, metadata.media_type, owner)?;
        Ok(ImageAttachment {
            source_path: source_path.to_owned(),
            image: ImageRef {
                artifact,
                media_type: metadata.media_type.to_owned(),
                byte_len: bytes.len() as u64,
                width: metadata.width,
                height: metadata.height,
            },
        })
    }
}

struct ImageMetadata {
    media_type: &'static str,
    width: Option<u32>,
    height: Option<u32>,
}
fn inspect_image(bytes: &[u8], limits: ImageLimits) -> Result<ImageMetadata, ImageError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        if bytes.len() < 24 {
            return Err(ImageError::Malformed);
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if pixels > limits.max_pixels {
            return Err(ImageError::PixelLimit {
                actual: pixels,
                limit: limits.max_pixels,
            });
        }
        return Ok(ImageMetadata {
            media_type: "image/png",
            width: Some(width),
            height: Some(height),
        });
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok(ImageMetadata {
            media_type: "image/jpeg",
            width: None,
            height: None,
        });
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        if bytes.len() < 10 {
            return Err(ImageError::Malformed);
        }
        let width = u32::from(u16::from_le_bytes(bytes[6..8].try_into().unwrap()));
        let height = u32::from(u16::from_le_bytes(bytes[8..10].try_into().unwrap()));
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if pixels > limits.max_pixels {
            return Err(ImageError::PixelLimit {
                actual: pixels,
                limit: limits.max_pixels,
            });
        }
        return Ok(ImageMetadata {
            media_type: "image/gif",
            width: Some(width),
            height: Some(height),
        });
    }
    Err(ImageError::UnsupportedFormat)
}

pub(crate) struct MediaResolver {
    store: ArtifactStore,
    max_bytes: usize,
}
impl MediaResolver {
    pub(crate) fn new(store: ArtifactStore, max_bytes: usize) -> Self {
        Self { store, max_bytes }
    }
    pub(crate) fn resolve_openai_data_url(&self, image: &ImageRef) -> Result<String, ImageError> {
        let bytes = self.store.read_bounded(&image.artifact, self.max_bytes)?;
        Ok(format!(
            "data:{};base64,{}",
            image.media_type,
            STANDARD.encode(bytes)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 13]);
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes
    }

    #[test]
    fn image_ingest_is_bounded_and_content_addressed() {
        let workspace = tempdir().unwrap();
        let image_path = workspace.path().join("photo.png");
        std::fs::write(&image_path, png(2, 3)).unwrap();
        let artifact_root = workspace.path().join("artifacts");
        let owner = PrincipalId::new();
        let ingestor = ImageIngestor::new(
            ArtifactStore::new(artifact_root.clone()),
            ImageLimits::default(),
        );
        let attachment = ingestor
            .ingest_path(workspace.path(), "photo.png", owner)
            .unwrap();
        assert_eq!(attachment.image.media_type, "image/png");
        assert_eq!(attachment.image.width, Some(2));
        assert_eq!(attachment.image.height, Some(3));
        let url = MediaResolver::new(ArtifactStore::new(artifact_root), 100)
            .resolve_openai_data_url(&attachment.image)
            .unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn pending_queue_is_consumed_once_and_clear_is_visible() {
        let mut queue = PendingImages::default();
        assert_eq!(queue.len(), 0);
        queue.push(ImageAttachment {
            source_path: "photo.png".into(),
            image: ImageRef {
                artifact: ArtifactRecord {
                    reference: crate::artifact::ArtifactRef {
                        id: crate::identity::ArtifactId::new(),
                        content_hash: crate::artifact::ContentHash::for_bytes(b"x"),
                    },
                    media_type: "image/png".into(),
                    byte_len: 1,
                    owner: crate::identity::PrincipalId::new(),
                },
                media_type: "image/png".into(),
                byte_len: 1,
                width: None,
                height: None,
            },
        });
        let drained = queue.take_for_turn();
        assert_eq!(drained.len(), 1);
        assert_eq!(queue.len(), 0);
        queue.push(drained.into_iter().next().unwrap());
        assert_eq!(queue.clear(), 1);
    }
}
