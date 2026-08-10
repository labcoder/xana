//! Lazy, long-lived ownership of the platform text clipboard.

#[derive(Default)]
pub(super) struct Clipboard {
    backend: Option<arboard::Clipboard>,
}

impl Clipboard {
    pub(super) fn set_text(&mut self, text: String) -> Result<(), String> {
        if self.backend.is_none() {
            self.backend = Some(
                arboard::Clipboard::new()
                    .map_err(|error| format!("clipboard is unavailable: {error}"))?,
            );
        }
        self.backend
            .as_mut()
            .expect("clipboard was initialized")
            .set_text(text)
            .map_err(|error| format!("could not copy conversation text: {error}"))
    }
}
