//! Allocation-free JSON size accounting for supervisor-owned projections.

#[derive(Default)]
struct EncodedLength(usize);

impl std::io::Write for EncodedLength {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn encoded_len(value: &impl serde::Serialize) -> Option<usize> {
    let mut length = EncodedLength::default();
    serde_json::to_writer(&mut length, value).ok()?;
    Some(length.0)
}
