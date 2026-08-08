//! OpenAI Chat Completions-compatible adapter.
//!
//! Wire structs, conversion rules, and HTTP transport live in private child
//! modules. Only the client and structured adapter errors cross this facade.

mod client;
mod convert;
mod stream;
mod wire;

pub(crate) use client::OpenAiCompatClient;

#[cfg(test)]
use crate::message::{Message, Role, ToolCall};
#[cfg(test)]
use client::{OpenAiCompatErrorKind, chat_endpoint};
#[cfg(test)]
use convert::MessageConversionError;
#[cfg(test)]
use wire::{
    WireChatRequest, WireChatResponse, WireFunctionCall, WireMessage, WireMessageContent, WireRole,
    WireToolCall, WireToolDefinition, WireToolKind,
};

#[cfg(test)]
mod tests;
