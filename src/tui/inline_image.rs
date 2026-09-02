//! Positively detected, bounded terminal-image previews.
//!
//! Detection happens once after entering the alternate screen and before the
//! input stream starts. Decode, resize, and protocol encoding happen on a
//! blocking worker; rendering a prepared protocol is then cheap.

use crate::{
    artifact::{ArtifactRecord, ArtifactStore, MAX_ARTIFACT_BYTES},
    identity::ArtifactId,
    presentation::InlineImageChoice,
};
use image::ImageReader;
use ratatui::layout::Size;
use ratatui_image::{
    Resize,
    picker::{Capability, Picker, ProtocolType},
    protocol::Protocol,
};
use std::io::Cursor;
use tokio::task::JoinHandle;

const PREVIEW_CELLS: Size = Size::new(36, 10);
const MAX_DECODE_PIXELS: u64 = 40_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Availability {
    Available { protocol: &'static str },
    Unavailable { reason: String },
}

struct PreparedPreview {
    artifact_id: ArtifactId,
    protocol: Protocol,
    protocol_name: &'static str,
}

struct PendingPreview {
    artifact_id: ArtifactId,
    task: JoinHandle<Result<PreparedPreview, String>>,
}

pub(super) struct InlineImageController {
    picker: Option<Picker>,
    availability: Availability,
    pending: Option<PendingPreview>,
    ready: Option<PreparedPreview>,
}

pub(super) struct PreviewOutcome {
    pub(super) artifact_id: ArtifactId,
    pub(super) message: String,
}

impl InlineImageController {
    pub(super) fn detect(preference: InlineImageChoice) -> Self {
        if preference == InlineImageChoice::Off {
            return Self::unavailable("inline images are disabled by appearance.inline_image");
        }
        if multiplexer_is_unproven() {
            return Self::unavailable(
                "inline images fall back to metadata inside an unproven terminal multiplexer",
            );
        }
        match Picker::from_query_stdio() {
            Ok(picker) => {
                let has_dimensions = picker.capabilities().iter().any(|capability| {
                    matches!(capability, Capability::CellSize(Some((width, height))) if *width > 0 && *height > 0)
                });
                let protocol = picker.protocol_type();
                match classify(protocol, has_dimensions, false) {
                    Availability::Available { protocol } => Self {
                        picker: Some(picker),
                        availability: Availability::Available { protocol },
                        pending: None,
                        ready: None,
                    },
                    Availability::Unavailable { reason } => Self::unavailable(reason),
                }
            }
            Err(error) => Self::unavailable(format!(
                "terminal image capability query failed safely: {error}"
            )),
        }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            picker: None,
            availability: Availability::Unavailable {
                reason: reason.into(),
            },
            pending: None,
            ready: None,
        }
    }

    pub(super) fn capability_summary(&self) -> String {
        match &self.availability {
            Availability::Available { protocol } => {
                format!("inline image previews available via {protocol}")
            }
            Availability::Unavailable { reason } => reason.clone(),
        }
    }

    pub(super) fn reconcile(
        &mut self,
        record: Option<&ArtifactRecord>,
        store: &ArtifactStore,
    ) -> Option<String> {
        let record = record?;
        if !record.media_type.starts_with("image/") {
            return None;
        }
        if self
            .ready
            .as_ref()
            .is_some_and(|preview| preview.artifact_id == record.reference.id)
            || self
                .pending
                .as_ref()
                .is_some_and(|preview| preview.artifact_id == record.reference.id)
        {
            return None;
        }
        let Some(picker) = self.picker.clone() else {
            return Some(self.capability_summary());
        };
        let artifact_id = record.reference.id;
        let record = record.clone();
        let store = store.clone();
        self.ready = None;
        self.pending = Some(PendingPreview {
            artifact_id,
            task: tokio::task::spawn_blocking(move || {
                prepare_preview(store, record, picker, PREVIEW_CELLS)
            }),
        });
        Some("Preparing a bounded inline image preview…".to_owned())
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) async fn finish_if_ready(&mut self) -> Option<PreviewOutcome> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.task.is_finished())
        {
            return None;
        }
        let pending = self.pending.take()?;
        let artifact_id = pending.artifact_id;
        match pending.task.await {
            Ok(Ok(preview)) => {
                let message = format!(
                    "Inline preview ready via {} (bounded to {}x{} cells)",
                    preview.protocol_name, PREVIEW_CELLS.width, PREVIEW_CELLS.height
                );
                self.ready = Some(preview);
                Some(PreviewOutcome {
                    artifact_id,
                    message,
                })
            }
            Ok(Err(reason)) => Some(PreviewOutcome {
                artifact_id,
                message: format!("Inline preview unavailable: {reason}"),
            }),
            Err(error) => {
                crate::diagnostics::record_task_panic("tui-inline-image-preview");
                Some(PreviewOutcome {
                    artifact_id,
                    message: format!("Inline preview worker stopped: {error}"),
                })
            }
        }
    }

    pub(super) fn protocol_for(&self, artifact_id: ArtifactId) -> Option<&Protocol> {
        self.ready
            .as_ref()
            .filter(|preview| preview.artifact_id == artifact_id)
            .map(|preview| &preview.protocol)
    }
}

fn prepare_preview(
    store: ArtifactStore,
    record: ArtifactRecord,
    picker: Picker,
    size: Size,
) -> Result<PreparedPreview, String> {
    let bytes = store
        .read_bounded(&record, MAX_ARTIFACT_BYTES)
        .map_err(|error| format!("artifact verification failed: {error}"))?;
    let dimensions = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|error| format!("image type could not be detected: {error}"))?
        .into_dimensions()
        .map_err(|error| format!("image dimensions could not be read: {error}"))?;
    let pixels = u64::from(dimensions.0).saturating_mul(u64::from(dimensions.1));
    if pixels > MAX_DECODE_PIXELS {
        return Err(format!(
            "{}x{} image exceeds the {} pixel preview ceiling",
            dimensions.0, dimensions.1, MAX_DECODE_PIXELS
        ));
    }
    let image = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("image type could not be detected: {error}"))?
        .decode()
        .map_err(|error| format!("image decode failed: {error}"))?;
    let protocol_name = protocol_name(picker.protocol_type());
    let protocol = picker
        .new_protocol(image, size, Resize::Fit(None))
        .map_err(|error| format!("{protocol_name} encoding failed: {error}"))?;
    Ok(PreparedPreview {
        artifact_id: record.reference.id,
        protocol,
        protocol_name,
    })
}

fn classify(protocol: ProtocolType, has_dimensions: bool, multiplexer: bool) -> Availability {
    if multiplexer {
        return Availability::Unavailable {
            reason: "inline images fall back to metadata inside an unproven terminal multiplexer"
                .to_owned(),
        };
    }
    if !has_dimensions {
        return Availability::Unavailable {
            reason: "terminal cell dimensions were not positively detected; showing metadata"
                .to_owned(),
        };
    }
    match protocol {
        ProtocolType::Kitty | ProtocolType::Sixel | ProtocolType::Iterm2 => {
            Availability::Available {
                protocol: protocol_name(protocol),
            }
        }
        ProtocolType::Halfblocks => Availability::Unavailable {
            reason: "no terminal image protocol was positively detected; showing metadata"
                .to_owned(),
        },
    }
}

fn protocol_name(protocol: ProtocolType) -> &'static str {
    match protocol {
        ProtocolType::Kitty => "Kitty",
        ProtocolType::Sixel => "Sixel",
        ProtocolType::Iterm2 => "iTerm2",
        ProtocolType::Halfblocks => "half-block text",
    }
}

fn multiplexer_is_unproven() -> bool {
    ["TMUX", "STY"]
        .into_iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::PrincipalId;
    use image::{DynamicImage, ImageFormat};
    use tempfile::tempdir;

    #[test]
    fn only_real_protocol_plus_dimensions_enables_inline_images() {
        assert!(matches!(
            classify(ProtocolType::Kitty, true, false),
            Availability::Available { protocol: "Kitty" }
        ));
        assert!(matches!(
            classify(ProtocolType::Sixel, false, false),
            Availability::Unavailable { .. }
        ));
        assert!(matches!(
            classify(ProtocolType::Halfblocks, true, false),
            Availability::Unavailable { .. }
        ));
        assert!(matches!(
            classify(ProtocolType::Iterm2, true, true),
            Availability::Unavailable { .. }
        ));
    }

    #[test]
    fn preview_preparation_verifies_decodes_and_bounds_the_artifact() {
        let directory = tempdir().unwrap();
        let store = ArtifactStore::new(directory.path().to_owned());
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(2, 2)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        let (record, _) = store
            .put(&bytes.into_inner(), "image/png", PrincipalId::new())
            .unwrap();

        let preview =
            prepare_preview(store, record.clone(), Picker::halfblocks(), Size::new(4, 2)).unwrap();

        assert_eq!(preview.artifact_id, record.reference.id);
        assert_eq!(preview.protocol_name, "half-block text");
    }
}
