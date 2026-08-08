//! Incremental, bounded Server-Sent Events framing shared by native adapters.
//!
//! This module owns transport framing only. Provider-specific JSON and terminal
//! markers remain private to each adapter.

use std::{fmt, mem};

pub(super) const MAX_FRAME_BYTES: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SseEvent {
    pub(super) data: Vec<u8>,
}

#[derive(Debug, Default)]
pub(super) struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    frame_bytes: usize,
    frame_active: bool,
    data_seen: bool,
}

impl SseDecoder {
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        let mut remaining = chunk;

        while let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') {
            self.extend_line(&remaining[..newline])?;
            self.charge(1)?;
            self.finish_line(&mut events);
            remaining = &remaining[newline + 1..];
        }
        self.extend_line(remaining)?;
        Ok(events)
    }

    pub(super) fn finish(self) -> Result<(), SseError> {
        if !self.data_seen && !self.frame_active && self.line.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(SseError::IncompleteFrame {
                remaining_bytes: self.frame_bytes,
            })
        }
    }

    fn extend_line(&mut self, bytes: &[u8]) -> Result<(), SseError> {
        self.charge(bytes.len())?;
        self.line.extend_from_slice(bytes);
        Ok(())
    }

    fn charge(&mut self, bytes: usize) -> Result<(), SseError> {
        if self.frame_bytes.saturating_add(bytes) > MAX_FRAME_BYTES {
            return Err(SseError::FrameTooLarge {
                limit: MAX_FRAME_BYTES,
            });
        }
        self.frame_bytes += bytes;
        Ok(())
    }

    fn finish_line(&mut self, events: &mut Vec<SseEvent>) {
        let mut line = mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            if self.data_seen {
                events.push(SseEvent {
                    data: mem::take(&mut self.data),
                });
            }
            self.frame_bytes = 0;
            self.frame_active = false;
            self.data_seen = false;
            return;
        }

        self.frame_active = true;
        if line.starts_with(b":") {
            return;
        }
        let (field, mut value) = match line.iter().position(|byte| *byte == b':') {
            Some(separator) => (&line[..separator], &line[separator + 1..]),
            None => (line.as_slice(), &[][..]),
        };
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        if field == b"data" {
            if self.data_seen {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value);
            self.data_seen = true;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SseError {
    FrameTooLarge { limit: usize },
    IncompleteFrame { remaining_bytes: usize },
}

impl fmt::Display for SseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { limit } => {
                write!(f, "SSE frame exceeds the {limit}-byte limit")
            }
            Self::IncompleteFrame { remaining_bytes } => {
                write!(
                    f,
                    "stream ended with an incomplete {remaining_bytes}-byte SSE frame"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_is_incremental_multiline_and_line_ending_independent() {
        let input = b": ignored\r\ndata: hel\r\ndata: lo\r\n\r\ndata: bye\n\n";
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for byte in input {
            events.extend(decoder.push(&[*byte]).expect("one-byte chunk"));
        }
        decoder.finish().expect("complete stream");
        assert_eq!(
            events,
            vec![
                SseEvent {
                    data: b"hel\nlo".to_vec()
                },
                SseEvent {
                    data: b"bye".to_vec()
                }
            ]
        );
    }

    #[test]
    fn frame_limit_is_enforced_before_an_unbounded_append() {
        let mut decoder = SseDecoder::default();
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert_eq!(
            decoder.push(&oversized),
            Err(SseError::FrameTooLarge {
                limit: MAX_FRAME_BYTES
            })
        );
    }
}
