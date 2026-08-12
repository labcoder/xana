//! Bounded Model Context Protocol client domain and private wire adapter.
//!
//! The modern MCP core is stateless. Configured server identity, negotiated
//! discovery facts, catalog metadata, and transport lifetime therefore remain
//! separate values. Tools, resources, and prompts intentionally have distinct
//! APIs and authority semantics.

#![allow(dead_code)] // Transport and application consumers arrive in the following M3 tickets.

mod application;
mod catalog;
mod http;
mod oauth;
mod protocol;
mod stdio;

#[allow(unused_imports)]
pub(crate) use application::{
    McpApplication, McpApplicationError, McpGuardedTransport, McpHttpConnection,
    McpPrimitiveTransport, McpPromptPreview, McpResourceDocument, McpTransportResponse,
};

#[allow(unused_imports)] // Shared facade grows transport consumers in M3-12 through M3-15.
pub(crate) use catalog::{
    CatalogLimits, IndexReport, McpCatalog, McpCatalogError, McpCatalogSource, McpPaginationGuard,
    McpPrimitiveAllowlist, McpPromptSummary, McpResourceSummary, McpServerExposure,
    McpServerReadiness, McpToolDefinition, McpToolSummary,
};
#[allow(unused_imports)] // Application integration arrives in M3-14.
pub(crate) use http::{
    McpHttpClient, McpHttpEndpoint, McpHttpError, McpHttpOutboundTransport, McpHttpResponse,
    McpHttpSecurity, McpHttpToolHeaders, mcp_http_recipient,
};
#[allow(unused_imports)] // Setup and command consumers arrive in M3-14/M3-23.
pub(crate) use oauth::{
    McpAuthChallenge, McpOAuthClient, McpOAuthDiscovery, McpOAuthError, McpOAuthFlow,
    McpOAuthMetadata, McpOAuthReference, McpOAuthStore, McpOAuthToken,
    McpProtectedResourceMetadata, OAuthCallback,
};
#[allow(unused_imports)] // Shared facade grows transport consumers in M3-12 through M3-15.
pub(crate) use protocol::{
    MCP_PROTOCOL_VERSION, McpCacheHint, McpDiscoverResult, McpNegotiation, McpNotification,
    McpPromptMessage, McpPromptResult, McpPromptRole, McpPromptWire, McpRequestId,
    McpResourceReadResult, McpResourceTemplateWire, McpResourceWire, McpServerCapabilities,
    McpToolCallResult, McpToolWire, Page, ProtocolError, decode_discover_response,
    decode_notification, decode_prompt_page, decode_prompt_result, decode_resource_page,
    decode_resource_read_result, decode_resource_template_page, decode_tool_call_result,
    decode_tool_definition_result, decode_tool_page, encode_notification, encode_request,
    negotiate,
};
#[allow(unused_imports)] // Application integration arrives in M3-14.
pub(crate) use stdio::{
    McpArgument, McpEnvironmentValue, McpProcessActivity, McpProcessConfig, McpProcessHealth,
    McpProcessPhase, McpRawResponse, McpStdioClient, McpStdioError, McpStopReport,
};

#[cfg(test)]
mod tests;
