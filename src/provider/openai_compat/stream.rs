//! Incremental SSE framing and provider-delta accumulation.

use super::wire::{WireDelta, WireToolCallDelta};
use crate::message::{ContentBlock, Message, Role, ToolCall};
use std::{collections::BTreeMap, error::Error, fmt};

const MAX_UNDECODED_BYTES: usize = 256 * 1024;

#[derive(Debug, Default)]
pub(super) struct SseDecoder {
    buffer: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SseItem {
    Data(Vec<u8>),
    Done,
}

impl SseDecoder {
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseItem>, StreamError> {
        self.buffer.extend_from_slice(chunk);
        let mut items = Vec::new();

        while let Some((frame_end, delimiter_len)) = next_frame(&self.buffer) {
            if frame_end > MAX_UNDECODED_BYTES {
                return Err(StreamError::FrameTooLarge {
                    limit: MAX_UNDECODED_BYTES,
                });
            }
            let frame = self.buffer[..frame_end].to_vec();
            self.buffer.drain(..frame_end + delimiter_len);
            if let Some(item) = parse_frame(&frame)? {
                items.push(item);
            }
        }

        if self.buffer.len() > MAX_UNDECODED_BYTES {
            return Err(StreamError::FrameTooLarge {
                limit: MAX_UNDECODED_BYTES,
            });
        }
        Ok(items)
    }

    pub(super) fn finish(self) -> Result<(), StreamError> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(StreamError::IncompleteFrame {
                remaining_bytes: self.buffer.len(),
            })
        }
    }
}

fn next_frame(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|window| window == b"\n\n");
    let crlf = bytes.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

fn parse_frame(frame: &[u8]) -> Result<Option<SseItem>, StreamError> {
    let text = std::str::from_utf8(frame).map_err(StreamError::InvalidUtf8)?;
    let mut data = Vec::new();

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }

    if data.is_empty() {
        return Ok(None);
    }
    let data = data.join("\n");
    if data.trim() == "[DONE]" {
        Ok(Some(SseItem::Done))
    } else {
        Ok(Some(SseItem::Data(data.into_bytes())))
    }
}

#[derive(Debug, Default)]
pub(super) struct StreamAccumulator {
    text: String,
    tool_calls: BTreeMap<usize, PartialToolCall>,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    pub(super) fn apply(&mut self, delta: WireDelta) -> Result<Vec<String>, StreamError> {
        let mut fragments = Vec::new();
        if let Some(text) = delta.content.filter(|text| !text.is_empty()) {
            if !self.tool_calls.is_empty() {
                return Err(StreamError::TextAfterToolCall);
            }
            self.text.push_str(&text);
            fragments.push(text);
        }
        for tool_call in delta.tool_calls.unwrap_or_default() {
            self.apply_tool_call(tool_call);
        }
        Ok(fragments)
    }

    fn apply_tool_call(&mut self, delta: WireToolCallDelta) {
        let partial = self.tool_calls.entry(delta.index).or_default();
        if let Some(id) = delta.id {
            partial.id.push_str(&id);
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                partial.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                partial.arguments.push_str(&arguments);
            }
        }
    }

    pub(super) fn finish(self) -> Result<Message, StreamError> {
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(ContentBlock::Text(self.text));
        }

        for (expected_index, (index, partial)) in self.tool_calls.into_iter().enumerate() {
            if index != expected_index {
                return Err(StreamError::NonContiguousToolIndex {
                    expected: expected_index,
                    found: index,
                });
            }
            if partial.id.is_empty() {
                return Err(StreamError::MissingToolField { index, field: "id" });
            }
            if partial.name.is_empty() {
                return Err(StreamError::MissingToolField {
                    index,
                    field: "name",
                });
            }
            let arguments = serde_json::from_str(&partial.arguments)
                .map_err(|source| StreamError::InvalidToolArguments { index, source })?;
            content.push(ContentBlock::ToolCall(ToolCall {
                id: partial.id,
                name: partial.name,
                arguments,
            }));
        }

        Ok(Message {
            role: Role::Assistant,
            content,
        })
    }
}

#[derive(Debug)]
pub(super) enum StreamError {
    FrameTooLarge {
        limit: usize,
    },
    IncompleteFrame {
        remaining_bytes: usize,
    },
    InvalidUtf8(std::str::Utf8Error),
    InvalidJson(serde_json::Error),
    MissingChoice,
    TextAfterToolCall,
    NonContiguousToolIndex {
        expected: usize,
        found: usize,
    },
    MissingToolField {
        index: usize,
        field: &'static str,
    },
    InvalidToolArguments {
        index: usize,
        source: serde_json::Error,
    },
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { limit } => {
                write!(f, "stream frame exceeds the {limit}-byte limit")
            }
            Self::IncompleteFrame { remaining_bytes } => write!(
                f,
                "stream ended with an incomplete {remaining_bytes}-byte frame"
            ),
            Self::InvalidUtf8(_) => write!(f, "stream frame is not valid UTF-8"),
            Self::InvalidJson(_) => write!(f, "stream data is not valid response JSON"),
            Self::MissingChoice => write!(f, "stream response contained no choice"),
            Self::TextAfterToolCall => write!(
                f,
                "streamed text after a tool call cannot preserve internal content order"
            ),
            Self::NonContiguousToolIndex { expected, found } => write!(
                f,
                "streamed tool index {found} was not the expected contiguous index {expected}"
            ),
            Self::MissingToolField { index, field } => {
                write!(f, "streamed tool call {index} is missing {field}")
            }
            Self::InvalidToolArguments { index, .. } => {
                write!(f, "streamed tool call {index} has invalid JSON arguments")
            }
        }
    }
}

impl Error for StreamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidUtf8(source) => Some(source),
            Self::InvalidJson(source) | Self::InvalidToolArguments { source, .. } => Some(source),
            Self::FrameTooLarge { .. }
            | Self::IncompleteFrame { .. }
            | Self::MissingChoice
            | Self::TextAfterToolCall
            | Self::NonContiguousToolIndex { .. }
            | Self::MissingToolField { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
