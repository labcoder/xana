//! Artifact-backed image input and bounded provider media resolution.

use crate::{
    artifact::{ArtifactRecord, ArtifactStore, MAX_ARTIFACT_BYTES},
    identity::PrincipalId,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt, fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

const MAX_IMAGE_PIXELS: u64 = 40_000_000;
pub(crate) const MAX_IMAGES_PER_TURN: usize = 8;
pub(crate) const MAX_IMAGE_BYTES_PER_TURN: u64 = 20 * 1024 * 1024;

pub(crate) struct ClipboardImageData {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) rgba: Vec<u8>,
}

pub(crate) trait ClipboardImageSource {
    fn image(&mut self) -> Result<ClipboardImageData, String>;
}

impl ClipboardImageSource for arboard::Clipboard {
    fn image(&mut self) -> Result<ClipboardImageData, String> {
        let image = self.get_image().map_err(|error| {
            format!(
                "clipboard does not contain a supported image: {error}; {}",
                clipboard_image_remediation()
            )
        })?;
        Ok(ClipboardImageData {
            width: image.width,
            height: image.height,
            rgba: image.bytes.into_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ImageRef {
    pub(crate) artifact: ArtifactRecord,
    pub(crate) media_type: String,
    pub(crate) byte_len: u64,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
}

pub(crate) fn ingest_clipboard_image(
    store: ArtifactStore,
    owner: PrincipalId,
) -> Result<ImageAttachment, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|error| {
        format!(
            "clipboard is unavailable: {error}; {}",
            clipboard_image_remediation()
        )
    })?;
    ingest_clipboard_image_from(&mut clipboard, store, owner)
}

pub(crate) fn ingest_clipboard_image_from(
    source: &mut dyn ClipboardImageSource,
    store: ArtifactStore,
    owner: PrincipalId,
) -> Result<ImageAttachment, String> {
    let image = source.image()?;
    let width = u32::try_from(image.width)
        .map_err(|_| "clipboard image width exceeds Xana's limit".to_owned())?;
    let height = u32::try_from(image.height)
        .map_err(|_| "clipboard image height exceeds Xana's limit".to_owned())?;
    let expected = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "clipboard image dimensions overflow".to_owned())?;
    if image.rgba.len() != expected || expected > 64 * 1024 * 1024 {
        return Err(
            "clipboard RGBA image is malformed or exceeds the 64 MiB decode bound".to_owned(),
        );
    }
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&image.rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|error| format!("could not encode clipboard image safely: {error}"))?;
    ImageIngestor::new(store, ImageLimits::default())
        .ingest_bytes("clipboard", &png, owner)
        .map_err(|error| format!("could not ingest clipboard image: {error}"))
}

fn clipboard_image_remediation() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "copy an image in an interactive Windows desktop session, then retry `/attach --clipboard`"
    }
    #[cfg(target_os = "macos")]
    {
        "allow terminal clipboard access, copy an image, then retry `/attach --clipboard`"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "use a graphical Wayland/X11 session with a supported clipboard, copy an image, then retry `/attach --clipboard`"
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        "copy a supported image in an interactive desktop session, then retry `/attach --clipboard`"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageAttachment {
    pub(crate) source_path: String,
    pub(crate) image: ImageRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DroppedImagePath {
    Workspace { relative: String },
    External { canonical: PathBuf },
}

#[derive(Debug, Default, Clone)]
pub(crate) struct PendingImages {
    queue: VecDeque<ImageAttachment>,
}

impl PendingImages {
    pub(crate) fn push(&mut self, attachment: ImageAttachment) -> bool {
        if self.queue.iter().any(|existing| {
            existing.image.artifact.reference.content_hash
                == attachment.image.artifact.reference.content_hash
        }) {
            return false;
        }
        self.queue.push_back(attachment);
        true
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
        self.ingest_bytes(source_path, &bytes, owner)
    }

    pub(crate) fn ingest_dropped_path(
        &self,
        workspace_root: &Path,
        source_path: &str,
        owner: PrincipalId,
    ) -> Result<ImageAttachment, ImageError> {
        match classify_dropped_image_path(workspace_root, source_path)? {
            DroppedImagePath::Workspace { relative } => {
                self.ingest_path(workspace_root, &relative, owner)
            }
            DroppedImagePath::External { .. } => Err(ImageError::OutsideWorkspace),
        }
    }

    pub(crate) fn ingest_approved_dropped_path(
        &self,
        workspace_root: &Path,
        source_path: &str,
        owner: PrincipalId,
    ) -> Result<ImageAttachment, ImageError> {
        match classify_dropped_image_path(workspace_root, source_path)? {
            DroppedImagePath::Workspace { relative } => {
                self.ingest_path(workspace_root, &relative, owner)
            }
            DroppedImagePath::External { canonical } => {
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
                self.ingest_bytes(source_path, &bytes, owner)
            }
        }
    }

    pub(crate) fn ingest_bytes(
        &self,
        source: &str,
        bytes: &[u8],
        owner: PrincipalId,
    ) -> Result<ImageAttachment, ImageError> {
        if bytes.len() > self.limits.max_bytes {
            return Err(ImageError::TooLarge {
                actual: bytes.len(),
                limit: self.limits.max_bytes,
            });
        }
        let metadata = inspect_image(bytes, self.limits)?;
        let (artifact, _) = self.store.put(bytes, metadata.media_type, owner)?;
        Ok(ImageAttachment {
            source_path: source.to_owned(),
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

pub(crate) fn classify_dropped_image_path(
    workspace_root: &Path,
    source_path: &str,
) -> Result<DroppedImagePath, ImageError> {
    let source = normalize_dropped_path(source_path)?;
    let root = workspace_root.canonicalize().map_err(ImageError::Io)?;
    let path = if source.is_absolute() {
        source
    } else {
        root.join(source)
    };
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() {
        return Err(ImageError::NotRegular);
    }
    let canonical = path.canonicalize().map_err(ImageError::Io)?;
    if let Ok(relative) = canonical.strip_prefix(&root) {
        Ok(DroppedImagePath::Workspace {
            relative: relative.to_string_lossy().into_owned(),
        })
    } else {
        Ok(DroppedImagePath::External { canonical })
    }
}

/// Finds image-looking paths in ordinary user text, preserving input order.
/// This is lexical intent detection only; callers still resolve every path and
/// enforce authority before reading bytes. One extra candidate is retained so
/// callers can report the per-turn count limit instead of silently truncating.
pub(crate) fn image_paths_in_text(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut start = None;
    let mut quote = None;
    for (index, character) in text.char_indices() {
        match (quote, character) {
            (None, '"' | '\'') => {
                quote = Some(character);
                start = Some(index + character.len_utf8());
            }
            (Some(expected), actual) if actual == expected => {
                if let Some(start) = start.take() {
                    candidates.push(&text[start..index]);
                }
                quote = None;
            }
            (None, character) if character.is_whitespace() => {
                if let Some(start) = start.take() {
                    candidates.push(&text[start..index]);
                }
            }
            (None, _) if start.is_none() => start = Some(index),
            _ => {}
        }
    }
    if let Some(start) = start {
        candidates.push(&text[start..]);
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let candidate = candidate.trim_matches(|character: char| {
                matches!(character, ',' | ';' | ':' | ')' | ']' | '}')
            });
            let extension = Path::new(candidate)
                .extension()
                .and_then(std::ffi::OsStr::to_str)?;
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif"
            )
            .then(|| candidate.to_owned())
        })
        .filter(|candidate| seen.insert(candidate.clone()))
        .take(MAX_IMAGES_PER_TURN + 1)
        .collect()
}

fn normalize_dropped_path(source: &str) -> Result<PathBuf, ImageError> {
    let source = source.trim();
    if source.is_empty() || source.contains(['\r', '\n']) {
        return Err(ImageError::InvalidPath);
    }
    let source = if source.len() >= 2
        && ((source.starts_with('"') && source.ends_with('"'))
            || (source.starts_with('\'') && source.ends_with('\'')))
    {
        &source[1..source.len() - 1]
    } else {
        source
    };
    let source = if let Some(path) = source.strip_prefix("file://") {
        if !path.starts_with('/') {
            return Err(ImageError::InvalidPath);
        }
        let mut path = percent_decode_path(path)?;
        #[cfg(windows)]
        if path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(2) == Some(&b':')
        {
            path.remove(0);
        }
        path
    } else {
        source.to_owned()
    };
    #[cfg(windows)]
    let source = msys_windows_path(&source);
    Ok(PathBuf::from(source))
}

fn percent_decode_path(source: &str) -> Result<String, ImageError> {
    let bytes = source.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let encoded = bytes
            .get(index + 1..index + 3)
            .ok_or(ImageError::InvalidPath)?;
        let high = hex_digit(encoded[0]).ok_or(ImageError::InvalidPath)?;
        let low = hex_digit(encoded[1]).ok_or(ImageError::InvalidPath)?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    if decoded.contains(&0) {
        return Err(ImageError::InvalidPath);
    }
    String::from_utf8(decoded).map_err(|_| ImageError::InvalidPath)
}

const fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(windows)]
fn msys_windows_path(source: &str) -> String {
    let bytes = source.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/' {
        format!(
            "{}:/{}",
            (bytes[1] as char).to_ascii_uppercase(),
            &source[3..]
        )
    } else {
        source.to_owned()
    }
}

struct ImageMetadata {
    media_type: &'static str,
    width: Option<u32>,
    height: Option<u32>,
}
fn inspect_image(bytes: &[u8], limits: ImageLimits) -> Result<ImageMetadata, ImageError> {
    let format = image::guess_format(bytes).map_err(|_| ImageError::UnsupportedFormat)?;
    let media_type = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        _ => return Err(ImageError::UnsupportedFormat),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut decode_limits = image::Limits::default();
    decode_limits.max_alloc = Some(limits.max_pixels.saturating_mul(4));
    reader.limits(decode_limits.clone());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| ImageError::Malformed)?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels == 0 || pixels > limits.max_pixels {
        return Err(ImageError::PixelLimit {
            actual: pixels,
            limit: limits.max_pixels,
        });
    }
    let mut validation = image::ImageReader::with_format(Cursor::new(bytes), format);
    decode_limits.max_image_width = Some(width);
    decode_limits.max_image_height = Some(height);
    validation.limits(decode_limits);
    validation.decode().map_err(|_| ImageError::Malformed)?;
    Ok(ImageMetadata {
        media_type,
        width: Some(width),
        height: Some(height),
    })
}

#[derive(Clone)]
pub(crate) struct MediaResolver {
    store: ArtifactStore,
    max_bytes: usize,
}
impl MediaResolver {
    pub(crate) fn new(store: ArtifactStore, max_bytes: usize) -> Self {
        Self { store, max_bytes }
    }
    pub(crate) fn resolve_openai_data_url(&self, image: &ImageRef) -> Result<String, ImageError> {
        let bytes = self.resolve_bytes(image)?;
        Ok(format!(
            "data:{};base64,{}",
            image.media_type,
            STANDARD.encode(bytes)
        ))
    }

    pub(crate) fn resolve_base64(&self, image: &ImageRef) -> Result<String, ImageError> {
        Ok(STANDARD.encode(self.resolve_bytes(image)?))
    }

    fn resolve_bytes(&self, image: &ImageRef) -> Result<Vec<u8>, ImageError> {
        self.store
            .read_bounded(&image.artifact, self.max_bytes)
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(
                &vec![0; width as usize * height as usize * 4],
                width,
                height,
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }

    struct FakeClipboard(Result<ClipboardImageData, String>);

    impl ClipboardImageSource for FakeClipboard {
        fn image(&mut self) -> Result<ClipboardImageData, String> {
            self.0
                .as_ref()
                .map(|image| ClipboardImageData {
                    width: image.width,
                    height: image.height,
                    rgba: image.rgba.clone(),
                })
                .map_err(Clone::clone)
        }
    }

    #[test]
    fn clipboard_adapter_publishes_bounded_rgba_as_an_immutable_png() {
        let directory = tempdir().unwrap();
        let store = ArtifactStore::new(directory.path().join("artifacts"));
        let owner = PrincipalId::new();
        let mut clipboard = FakeClipboard(Ok(ClipboardImageData {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        }));

        let attachment = ingest_clipboard_image_from(&mut clipboard, store.clone(), owner).unwrap();

        assert_eq!(attachment.source_path, "clipboard");
        assert_eq!(attachment.image.width, Some(2));
        assert_eq!(attachment.image.height, Some(1));
        let bytes = store
            .read_bounded(&attachment.image.artifact, MAX_ARTIFACT_BYTES)
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn malformed_clipboard_pixels_fail_before_artifact_publication() {
        let directory = tempdir().unwrap();
        let store = ArtifactStore::new(directory.path().join("artifacts"));
        let mut clipboard = FakeClipboard(Ok(ClipboardImageData {
            width: 2,
            height: 2,
            rgba: vec![0; 3],
        }));

        let error =
            ingest_clipboard_image_from(&mut clipboard, store, PrincipalId::new()).unwrap_err();

        assert!(error.contains("malformed"));
    }

    #[test]
    fn clipboard_unavailability_has_an_actionable_platform_remedy() {
        let remedy = clipboard_image_remediation();
        assert!(remedy.contains("clipboard") || remedy.contains("copy an image"));
        assert!(remedy.contains("/attach --clipboard"));
    }

    #[test]
    fn malformed_and_decompression_bomb_images_fail_before_artifact_publication() {
        let limits = ImageLimits {
            max_bytes: 1024,
            max_pixels: 4,
        };
        assert!(matches!(
            inspect_image(b"\xff\xd8\xff", limits),
            Err(ImageError::Malformed)
        ));
        assert!(matches!(
            inspect_image(&png(3, 3), limits),
            Err(ImageError::PixelLimit { .. })
        ));
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
    fn dropped_absolute_image_is_reduced_to_a_workspace_relative_reference() {
        let workspace = tempdir().unwrap();
        let image_path = workspace.path().join("dropped image.png");
        std::fs::write(&image_path, png(2, 2)).unwrap();
        let store = ArtifactStore::new(workspace.path().join("artifacts"));
        let ingestor = ImageIngestor::new(store, ImageLimits::default());

        #[cfg(windows)]
        let file_uri = format!("file:///{}", image_path.display());
        #[cfg(not(windows))]
        let file_uri = format!("file://{}", image_path.display());
        let file_uri = file_uri.replace('\\', "/").replace(' ', "%20");

        let attachment = ingestor
            .ingest_dropped_path(workspace.path(), &file_uri, PrincipalId::new())
            .unwrap();

        assert_eq!(attachment.source_path, "dropped image.png");
    }

    #[test]
    fn dropped_image_outside_workspace_is_rejected() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let image_path = outside.path().join("outside.png");
        std::fs::write(&image_path, png(2, 2)).unwrap();
        let store = ArtifactStore::new(workspace.path().join("artifacts"));

        assert!(matches!(
            ImageIngestor::new(store, ImageLimits::default()).ingest_dropped_path(
                workspace.path(),
                &image_path.display().to_string(),
                PrincipalId::new(),
            ),
            Err(ImageError::OutsideWorkspace)
        ));
    }

    #[test]
    fn explicitly_approved_external_image_is_imported_as_an_artifact_copy() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let image_path = outside.path().join("outside.png");
        std::fs::write(&image_path, png(2, 2)).unwrap();
        let store = ArtifactStore::new(workspace.path().join("artifacts"));

        let attachment = ImageIngestor::new(store, ImageLimits::default())
            .ingest_approved_dropped_path(
                workspace.path(),
                &image_path.display().to_string(),
                PrincipalId::new(),
            )
            .expect("approved external image");

        assert_eq!(attachment.image.media_type, "image/png");
        assert_eq!(attachment.image.width, Some(2));
        assert_eq!(attachment.source_path, image_path.display().to_string());
    }

    #[test]
    fn pending_queue_is_consumed_once_and_clear_is_visible() {
        let mut queue = PendingImages::default();
        assert_eq!(queue.len(), 0);
        assert!(queue.push(ImageAttachment {
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
        }));
        let drained = queue.take_for_turn();
        assert_eq!(drained.len(), 1);
        assert_eq!(queue.len(), 0);
        assert!(queue.push(drained.into_iter().next().unwrap()));
        let duplicate = queue.queue.front().unwrap().clone();
        assert!(!queue.push(duplicate));
        assert_eq!(queue.clear(), 1);
    }

    #[test]
    fn ordinary_text_detects_all_image_paths_in_input_order() {
        assert_eq!(
            image_paths_in_text(
                r#"C:\Users\xana\Downloads\first.png C:\Users\xana\Downloads\second.jpg compare these"#,
            ),
            vec![
                r#"C:\Users\xana\Downloads\first.png"#.to_owned(),
                r#"C:\Users\xana\Downloads\second.jpg"#.to_owned(),
            ]
        );
        assert_eq!(
            image_paths_in_text(
                r#"'C:\Users\xana\Downloads\first image.png' "C:\Users\xana\Downloads\second image.gif" 'C:\Users\xana\Downloads\first image.png'"#,
            ),
            vec![
                r#"C:\Users\xana\Downloads\first image.png"#.to_owned(),
                r#"C:\Users\xana\Downloads\second image.gif"#.to_owned(),
            ]
        );
    }
}
