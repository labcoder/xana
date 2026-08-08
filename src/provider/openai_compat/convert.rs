//! Conversion between Xana's conversation model and private wire values.

use super::wire::{
    WireContentPart, WireFunctionCall, WireFunctionDefinition, WireImageUrl, WireMessage,
    WireMessageContent, WireRole, WireToolCall, WireToolDefinition, WireToolKind,
};
use crate::{
    message::{ContentBlock, Message, Role, ToolCall, ToolResultStatus},
    tool::ToolDefinition,
    vision::MediaResolver,
};
use std::{error::Error, fmt};

#[derive(Debug)]
pub(super) enum MessageConversionError {
    TextAfterToolCall,
    UnsupportedImage,
    InvalidToolArguments {
        tool_call_id: String,
        source: serde_json::Error,
    },
    UnexpectedResponseRole(WireRole),
    InvalidMessageShape {
        role: Role,
        detail: &'static str,
    },
}

impl fmt::Display for MessageConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextAfterToolCall => {
                write!(
                    f,
                    "text after a tool call cannot be represented by this adapter"
                )
            }
            Self::UnsupportedImage => {
                f.write_str("image input requires a media resolver at the wire edge")
            }
            Self::InvalidToolArguments {
                tool_call_id,
                source,
            } => {
                write!(
                    f,
                    "tool call {tool_call_id:?} contained invalid JSON arguments: {source}"
                )
            }
            Self::UnexpectedResponseRole(role) => {
                write!(f, "provider returned unexpected response role {role:?}")
            }
            Self::InvalidMessageShape { role, detail } => {
                write!(f, "internal {role:?} message is invalid: {detail}")
            }
        }
    }
}

impl Error for MessageConversionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidToolArguments { source, .. } => Some(source),
            Self::TextAfterToolCall
            | Self::UnsupportedImage
            | Self::UnexpectedResponseRole(_)
            | Self::InvalidMessageShape { .. } => None,
        }
    }
}

impl<'a> From<&'a ToolDefinition> for WireToolDefinition<'a> {
    fn from(definition: &'a ToolDefinition) -> Self {
        Self {
            kind: WireToolKind::Function,
            function: WireFunctionDefinition {
                name: definition.name,
                description: definition.description,
                parameters: &definition.parameters,
            },
        }
    }
}

impl From<Role> for WireRole {
    fn from(role: Role) -> Self {
        match role {
            Role::System => Self::System,
            Role::User => Self::User,
            Role::Assistant => Self::Assistant,
            Role::Tool => Self::Tool,
        }
    }
}

impl From<&ToolCall> for WireToolCall {
    fn from(tool_call: &ToolCall) -> Self {
        Self {
            id: tool_call.id.clone(),
            kind: WireToolKind::Function,
            function: WireFunctionCall {
                name: tool_call.name.clone(),
                arguments: tool_call.arguments.to_string(),
            },
        }
    }
}

impl TryFrom<WireToolCall> for ToolCall {
    type Error = MessageConversionError;

    fn try_from(wire: WireToolCall) -> Result<Self, Self::Error> {
        let WireToolCall {
            id,
            kind: _,
            function,
        } = wire;

        let arguments = serde_json::from_str(&function.arguments).map_err(|source| {
            MessageConversionError::InvalidToolArguments {
                tool_call_id: id.clone(),
                source,
            }
        })?;

        Ok(Self {
            id,
            name: function.name,
            arguments,
        })
    }
}

impl TryFrom<WireMessage> for Message {
    type Error = MessageConversionError;

    fn try_from(wire: WireMessage) -> Result<Self, Self::Error> {
        if wire.role != WireRole::Assistant {
            return Err(MessageConversionError::UnexpectedResponseRole(wire.role));
        }

        let mut content = Vec::new();

        if let Some(wire_content) = wire.content {
            match wire_content {
                WireMessageContent::Text(text) => content.push(ContentBlock::Text(text)),
                WireMessageContent::Parts(parts) => {
                    for part in parts {
                        match part {
                            WireContentPart::Text { text } => {
                                content.push(ContentBlock::Text(text));
                            }
                            WireContentPart::ImageUrl { .. } => {
                                return Err(MessageConversionError::UnsupportedImage);
                            }
                        }
                    }
                }
            }
        }

        for tool_call in wire.tool_calls.unwrap_or_default() {
            content.push(ContentBlock::ToolCall(ToolCall::try_from(tool_call)?));
        }

        Ok(Self {
            role: Role::Assistant,
            content,
        })
    }
}

impl TryFrom<&Message> for WireMessage {
    type Error = MessageConversionError;

    fn try_from(message: &Message) -> Result<Self, Self::Error> {
        if message.role == Role::Tool {
            return match message.content.as_slice() {
                [ContentBlock::ToolResult(result)] => {
                    let content = match result.status {
                        ToolResultStatus::Success => result.output.clone(),
                        ToolResultStatus::Error => format!("ERROR: {}", result.output),
                    };

                    Ok(Self {
                        role: WireRole::Tool,
                        content: Some(WireMessageContent::Text(content)),
                        tool_calls: None,
                        tool_call_id: Some(result.call_id.clone()),
                    })
                }
                _ => Err(MessageConversionError::InvalidMessageShape {
                    role: message.role,
                    detail: "tool messages require exactly one tool-result block",
                }),
            };
        }

        convert_message(message, None)
    }
}

pub(super) fn convert_message(
    message: &Message,
    media: Option<&MediaResolver>,
) -> Result<WireMessage, MessageConversionError> {
    let mut text: Option<String> = None;
    let mut parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut saw_tool_call = false;

    for block in &message.content {
        match block {
            ContentBlock::Text(part) => {
                if saw_tool_call {
                    return Err(MessageConversionError::TextAfterToolCall);
                }

                match &mut text {
                    Some(existing) => existing.push_str(part),
                    None => text = Some(part.clone()),
                }
                parts.push(WireContentPart::Text { text: part.clone() });
            }
            ContentBlock::ToolCall(tool_call) => {
                saw_tool_call = true;
                tool_calls.push(WireToolCall::from(tool_call));
            }
            ContentBlock::Image(image) => {
                if message.role != Role::User || saw_tool_call {
                    return Err(MessageConversionError::UnsupportedImage);
                }
                let resolver = media.ok_or(MessageConversionError::UnsupportedImage)?;
                let url = resolver
                    .resolve_openai_data_url(image)
                    .map_err(|_| MessageConversionError::UnsupportedImage)?;
                parts.push(WireContentPart::ImageUrl {
                    image_url: WireImageUrl { url },
                });
            }
            ContentBlock::ToolResult(_) => {
                return Err(MessageConversionError::InvalidMessageShape {
                    role: message.role,
                    detail: "user and assistant messages cannot contain tool results",
                });
            }
        }
    }

    let has_image = parts
        .iter()
        .any(|part| matches!(part, WireContentPart::ImageUrl { .. }));
    Ok(WireMessage {
        role: message.role.into(),
        content: if has_image {
            Some(WireMessageContent::Parts(parts))
        } else {
            text.map(WireMessageContent::Text)
        },
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        tool_call_id: None,
    })
}
