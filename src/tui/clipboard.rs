//! Lazy, long-lived ownership of the platform clipboard.

use crate::{
    artifact::ArtifactStore,
    identity::PrincipalId,
    vision::{ImageAttachment, ingest_clipboard_image_from},
};

#[derive(Default)]
pub(super) struct Clipboard {
    backend: Option<arboard::Clipboard>,
}

impl Clipboard {
    fn backend(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.backend.is_none() {
            self.backend = Some(
                arboard::Clipboard::new()
                    .map_err(|error| format!("clipboard is unavailable: {error}"))?,
            );
        }
        Ok(self.backend.as_mut().expect("clipboard was initialized"))
    }

    pub(super) fn set_text(&mut self, text: String) -> Result<(), String> {
        self.backend()?
            .set_text(text)
            .map_err(|error| format!("could not copy conversation text: {error}"))
    }

    pub(super) fn get_image(
        &mut self,
        store: ArtifactStore,
        owner: PrincipalId,
    ) -> Result<ImageAttachment, String> {
        let backend = self.backend()?;
        ingest_clipboard_image_from(backend, store, owner)
    }
}
