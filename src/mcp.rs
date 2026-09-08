//! Bounded Model Context Protocol client domain and private wire adapter.
//!
//! The modern MCP core is stateless. Configured server identity, negotiated
//! discovery facts, catalog metadata, and transport lifetime therefore remain
//! separate values. Tools, resources, and prompts intentionally have distinct
//! APIs and authority semantics.

mod application;
mod catalog;
mod http;
mod oauth;
mod protocol;
mod server;
mod stdio;

pub(crate) use application::{
    McpApplication, McpApplicationError, McpGuardedTransport, McpHttpConnection,
    McpPrimitiveTransport, McpTransportResponse,
};

pub(crate) use catalog::{
    CatalogLimits, IndexReport, McpCatalog, McpCatalogError, McpCatalogSource, McpPaginationGuard,
    McpPrimitiveAllowlist, McpPromptSummary, McpResourceSummary, McpServerExposure,
    McpToolDefinition, McpToolSummary,
};
pub(crate) use http::{
    EXA_SEARCH_PROTOCOL_VERSION, McpHttpBudget, McpHttpClient, McpHttpEndpoint, McpHttpError,
    McpHttpSecurity, McpHttpToolHeaders, mcp_http_recipient, pinned_client,
    resolve_pinned_addresses,
};
pub(crate) use oauth::{
    McpAuthChallenge, McpOAuthClient, McpOAuthError, McpOAuthFlow, McpOAuthReference,
    McpOAuthSession, McpOAuthStore,
};
pub(crate) use protocol::{
    MCP_PROTOCOL_VERSION, McpDiscoverResult, McpNotification, McpPromptResult, McpPromptRole,
    McpRequestId, McpToolCallResult, McpToolWire, Page, ProtocolError, decode_discover_response,
    decode_notification, decode_prompt_page, decode_prompt_result, decode_resource_page,
    decode_resource_read_result, decode_resource_template_page, decode_tool_call_result,
    decode_tool_definition_result, decode_tool_page, encode_notification, encode_request,
    negotiate,
};
pub(crate) use server::McpLocalServer;
pub(crate) use stdio::{
    McpArgument, McpEnvironmentValue, McpProcessConfig, McpRawResponse, McpStdioClient,
};

#[cfg(test)]
mod tests;
