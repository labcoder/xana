//! Bounded Model Context Protocol client domain and private wire adapter.
//!
//! The modern MCP core is stateless. Configured server identity, negotiated
//! discovery facts, catalog metadata, and transport lifetime therefore remain
//! separate values. Tools, resources, and prompts intentionally have distinct
//! APIs and authority semantics.

#![allow(dead_code)] // Transport and application consumers arrive in the following M3 tickets.

mod catalog;
mod protocol;
mod stdio;

#[allow(unused_imports)] // Shared facade grows transport consumers in M3-12 through M3-15.
pub(crate) use catalog::{
    CatalogLimits, IndexReport, McpCatalog, McpCatalogError, McpCatalogSource, McpPaginationGuard,
    McpPrimitiveAllowlist, McpServerExposure, McpServerReadiness, McpToolDefinition,
};
#[allow(unused_imports)] // Shared facade grows transport consumers in M3-12 through M3-15.
pub(crate) use protocol::{
    MCP_PROTOCOL_VERSION, McpCacheHint, McpDiscoverResult, McpNegotiation, McpNotification,
    McpPromptWire, McpRequestId, McpResourceTemplateWire, McpResourceWire, McpServerCapabilities,
    McpToolWire, Page, ProtocolError, decode_discover_response, decode_notification,
    decode_prompt_page, decode_resource_page, decode_resource_template_page, decode_tool_page,
    encode_notification, encode_request, negotiate,
};
#[allow(unused_imports)] // Application integration arrives in M3-14.
pub(crate) use stdio::{
    McpArgument, McpEnvironmentValue, McpProcessActivity, McpProcessConfig, McpProcessHealth,
    McpProcessPhase, McpStdioClient, McpStdioError, McpStopReport,
};

#[cfg(test)]
mod tests;
