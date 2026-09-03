//! Explicit operating-system actions for verified immutable artifacts.

use crate::artifact::{ArtifactRecord, ArtifactStore};
use anyhow::{Context as _, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalArtifactAction {
    Open,
    Reveal,
}

/// Re-verifies one artifact before passing only its owned store path to the OS.
pub(crate) fn launch_verified(
    store: &ArtifactStore,
    artifact: &ArtifactRecord,
    max_bytes: usize,
    action: ExternalArtifactAction,
) -> Result<()> {
    let path = store
        .verified_path(artifact, max_bytes)
        .context("could not verify artifact before the OS action")?;
    launch_path(&path, action).context("could not start the OS artifact action")
}

pub(crate) fn extension_for_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/mp4" => "m4a",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "application/json" => "json",
        "application/lottie+json" => "lottie",
        "text/plain" => "txt",
        _ => "bin",
    }
}

fn launch_path(path: &Path, action: ExternalArtifactAction) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer.exe");
        if action == ExternalArtifactAction::Reveal {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        if action == ExternalArtifactAction::Reveal {
            command.arg("-R");
        }
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(if action == ExternalArtifactAction::Reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        });
        command
    };
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_extensions_are_bounded_and_never_use_untrusted_input() {
        assert_eq!(extension_for_media_type("image/webp"), "webp");
        assert_eq!(extension_for_media_type("video/webm"), "webm");
        assert_eq!(extension_for_media_type("../../escape"), "bin");
    }
}
