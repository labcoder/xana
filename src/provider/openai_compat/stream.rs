//! Provider-delta accumulation above the shared bounded SSE framing module.

use super::wire::{WireDelta, WireToolCallDelta};
use crate::{
    message::{ContentBlock, Message, Role, ToolCall},
    provider::sse,
};
use std::{collections::BTreeMap, error::Error, fmt};

const MAX_STREAMED_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAMED_TOOL_BYTES: usize = 256 * 1024;
const MAX_STREAMED_TOOL_CALLS: usize = 64;
#[cfg(test)]
const MAX_UNDECODED_BYTES: usize = sse::MAX_FRAME_BYTES;

#[derive(Debug, Default)]
pub(super) struct SseDecoder {
    decoder: sse::SseDecoder,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SseItem {
    Data(Vec<u8>),
    Done,
}

impl SseDecoder {
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseItem>, StreamError> {
        self.decoder
            .push(chunk)
            .map_err(StreamError::from_sse)?
            .into_iter()
            .map(|event| {
                let text = std::str::from_utf8(&event.data).map_err(StreamError::InvalidUtf8)?;
                Ok(if text.trim() == "[DONE]" {
                    SseItem::Done
                } else {
                    SseItem::Data(event.data)
                })
            })
            .collect()
    }

    pub(super) fn finish(self) -> Result<(), StreamError> {
        self.decoder.finish().map_err(StreamError::from_sse)
    }
}

#[derive(Debug, Default)]
pub(super) struct StreamAccumulator {
    text: String,
    tool_calls: BTreeMap<usize, PartialToolCall>,
    tool_bytes: usize,
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
            if self.text.len().saturating_add(text.len()) > MAX_STREAMED_TEXT_BYTES {
                return Err(StreamError::ResponseTextTooLarge {
                    limit: MAX_STREAMED_TEXT_BYTES,
                });
            }
            self.text.push_str(&text);
            fragments.push(text);
        }
        for tool_call in delta.tool_calls.unwrap_or_default() {
            self.apply_tool_call(tool_call)?;
        }
        Ok(fragments)
    }

    fn apply_tool_call(&mut self, delta: WireToolCallDelta) -> Result<(), StreamError> {
        if !self.tool_calls.contains_key(&delta.index)
            && self.tool_calls.len() >= MAX_STREAMED_TOOL_CALLS
        {
            return Err(StreamError::TooManyToolCalls {
                limit: MAX_STREAMED_TOOL_CALLS,
            });
        }
        let additional = delta.id.as_ref().map_or(0, String::len)
            + delta
                .function
                .as_ref()
                .and_then(|function| function.name.as_ref())
                .map_or(0, String::len)
            + delta
                .function
                .as_ref()
                .and_then(|function| function.arguments.as_ref())
                .map_or(0, String::len);
        if self.tool_bytes.saturating_add(additional) > MAX_STREAMED_TOOL_BYTES {
            return Err(StreamError::ToolDataTooLarge {
                limit: MAX_STREAMED_TOOL_BYTES,
            });
        }
        self.tool_bytes += additional;

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
        Ok(())
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
    MissingDone,
    TextAfterToolCall,
    ResponseTextTooLarge {
        limit: usize,
    },
    ToolDataTooLarge {
        limit: usize,
    },
    TooManyToolCalls {
        limit: usize,
    },
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

impl StreamError {
    fn from_sse(error: sse::SseError) -> Self {
        match error {
            sse::SseError::FrameTooLarge { limit } => Self::FrameTooLarge { limit },
            sse::SseError::IncompleteFrame { remaining_bytes } => {
                Self::IncompleteFrame { remaining_bytes }
            }
        }
    }
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
            Self::MissingDone => write!(f, "stream ended before the [DONE] marker"),
            Self::TextAfterToolCall => write!(
                f,
                "streamed text after a tool call cannot preserve internal content order"
            ),
            Self::ResponseTextTooLarge { limit } => {
                write!(f, "streamed response text exceeds the {limit}-byte limit")
            }
            Self::ToolDataTooLarge { limit } => {
                write!(f, "streamed tool data exceeds the {limit}-byte limit")
            }
            Self::TooManyToolCalls { limit } => {
                write!(f, "stream contains more than {limit} tool calls")
            }
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
            | Self::MissingDone
            | Self::TextAfterToolCall
            | Self::ResponseTextTooLarge { .. }
            | Self::ToolDataTooLarge { .. }
            | Self::TooManyToolCalls { .. }
            | Self::NonContiguousToolIndex { .. }
            | Self::MissingToolField { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
