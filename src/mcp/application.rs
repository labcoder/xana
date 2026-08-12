//! Application-facing MCP composition.
//!
//! This module turns private protocol transports into Xana capabilities while
//! preserving the semantic split between effectful tools, explicitly read
//! resources, and explicitly invoked prompt templates.

use super::{
    CatalogLimits, McpCatalog, McpCatalogError, McpCatalogSource, McpHttpClient, McpHttpError,
    McpHttpToolHeaders, McpNotification, McpPromptResult, McpPromptRole, McpPromptSummary,
    McpRawResponse, McpRequestId, McpResourceSummary, McpServerExposure, McpStdioClient,
    McpToolCallResult, McpToolDefinition, McpToolSummary, McpToolWire, ProtocolError,
    decode_discover_response, decode_prompt_page, decode_prompt_result, decode_resource_page,
    decode_resource_read_result, decode_resource_template_page, decode_tool_call_result,
    decode_tool_definition_result, decode_tool_page, encode_request, negotiate,
};
use crate::{
    config::OutboundDataClass,
    credential::SecretString,
    identity::OperationId,
    outbound::{
        OutboundApprovalController, OutboundApprovalDecision, OutboundApprovalRequest,
        OutboundAuditEvent, OutboundGuard, OutboundItem, OutboundPolicyLayers, OutboundRequest,
        OutboundTransport, OutboundTransportFailure, RecipientIdentity,
    },
    permission::PermissionScope,
    tool::{
        EffectClass, PlannedToolInvocation, RegistryError, ReplaySafety, Tool, ToolDefinition,
        ToolExecutionContext, ToolRegistry,
    },
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_RENDERED_RESULT_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct McpTransportResponse {
    pub(crate) request_id: McpRequestId,
    pub(crate) bytes: Vec<u8>,
    pub(crate) notifications: Vec<McpNotification>,
}

pub(crate) trait McpPrimitiveTransport: Send + Sync {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        tool_schema: Option<Value>,
        operation_id: OperationId,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpTransportResponse, McpApplicationError>>;
}

impl McpPrimitiveTransport for McpStdioClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        _tool_schema: Option<Value>,
        _operation_id: OperationId,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpTransportResponse, McpApplicationError>> {
        Box::pin(async move {
            let McpRawResponse { request_id, bytes } = self
                .request_with_id(method, params, cancellation)
                .await
                .map_err(|error| McpApplicationError::Transport(error.to_string()))?;
            Ok(McpTransportResponse {
                request_id,
                bytes,
                notifications: Vec::new(),
            })
        })
    }
}

pub(crate) struct McpHttpConnection {
    client: McpHttpClient,
    bearer: Option<Arc<SecretString>>,
    next_request_id: AtomicU64,
}

impl McpHttpConnection {
    pub(crate) fn new(client: McpHttpClient, bearer: Option<SecretString>) -> Self {
        Self {
            client,
            bearer: bearer.map(Arc::new),
            next_request_id: AtomicU64::new(1),
        }
    }
}

impl fmt::Debug for McpHttpConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpConnection")
            .field("endpoint", &self.client.endpoint().canonical())
            .field("bearer", &self.bearer.as_ref().map(|_| "REDACTED"))
            .finish_non_exhaustive()
    }
}

impl McpPrimitiveTransport for McpHttpConnection {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        tool_schema: Option<Value>,
        _operation_id: OperationId,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpTransportResponse, McpApplicationError>> {
        Box::pin(async move {
            let request_id =
                McpRequestId::new(self.next_request_id.fetch_add(1, Ordering::Relaxed))?;
            let headers = match tool_schema {
                Some(schema) => McpHttpToolHeaders::from_schema_and_arguments(
                    &schema,
                    params.get("arguments").unwrap_or(&Value::Null),
                )?,
                None => McpHttpToolHeaders::default(),
            };
            let body = encode_request(request_id, method, params)?;
            let response = self
                .client
                .request(&body, self.bearer.as_deref(), &headers, cancellation)
                .await?;
            Ok(McpTransportResponse {
                request_id,
                bytes: response.final_response,
                notifications: response.notifications,
            })
        })
    }
}

pub(crate) struct McpGuardedTransport {
    inner: Arc<dyn McpPrimitiveTransport>,
    guard: OutboundGuard,
    recipient: RecipientIdentity,
    policy: OutboundPolicyLayers,
}

impl McpGuardedTransport {
    pub(crate) fn new(
        inner: Arc<dyn McpPrimitiveTransport>,
        guard: OutboundGuard,
        recipient: RecipientIdentity,
        policy: OutboundPolicyLayers,
    ) -> Self {
        Self {
            inner,
            guard,
            recipient,
            policy,
        }
    }
}

impl McpPrimitiveTransport for McpGuardedTransport {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        tool_schema: Option<Value>,
        operation_id: OperationId,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpTransportResponse, McpApplicationError>> {
        Box::pin(async move {
            let bytes = serde_json::to_vec(&json!({"method": method, "params": params}))
                .map_err(|_| McpApplicationError::InvalidArguments)?;
            let class = if matches!(method, "tools/call" | "prompts/get") {
                OutboundDataClass::PromptText
            } else {
                OutboundDataClass::WorkspaceMetadata
            };
            let item = OutboundItem::new(
                class,
                format!("MCP {method}"),
                None,
                "profile MCP primitive",
                bytes,
            )?;
            let request = OutboundRequest::new(
                operation_id,
                self.recipient.clone(),
                format!("MCP {method}"),
                vec![item],
            )?;
            let mut controller = PreapprovedController;
            let mut audit = Vec::<OutboundAuditEvent>::new();
            let mut send = GuardedSend {
                inner: Arc::clone(&self.inner),
                method: method.to_owned(),
                params,
                tool_schema,
                operation_id,
                cancellation: cancellation.clone(),
                expected_recipient_digest: self.recipient.identity_digest.clone(),
            };
            self.guard
                .dispatch(
                    request,
                    &self.policy,
                    Some(&mut controller),
                    &mut send,
                    &mut audit,
                )
                .await
                .map_err(|error| McpApplicationError::Egress(error.to_string()))
        })
    }
}

struct PreapprovedController;

impl OutboundApprovalController for PreapprovedController {
    fn decide<'a>(
        &'a mut self,
        _request: &'a OutboundApprovalRequest,
    ) -> BoxFuture<'a, OutboundApprovalDecision> {
        Box::pin(async { OutboundApprovalDecision::AllowOnce })
    }
}

struct GuardedSend {
    inner: Arc<dyn McpPrimitiveTransport>,
    method: String,
    params: Value,
    tool_schema: Option<Value>,
    operation_id: OperationId,
    cancellation: CancellationToken,
    expected_recipient_digest: String,
}

impl OutboundTransport for GuardedSend {
    type Receipt = McpTransportResponse;

    fn send<'a>(
        &'a mut self,
        recipient: &'a RecipientIdentity,
        _items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        Box::pin(async move {
            if recipient.identity_digest != self.expected_recipient_digest {
                return Err(OutboundTransportFailure::Rejected);
            }
            self.inner
                .request(
                    &self.method,
                    self.params.clone(),
                    self.tool_schema.clone(),
                    self.operation_id,
                    &self.cancellation,
                )
                .await
                .map_err(|error| match error {
                    McpApplicationError::Transport(_) => OutboundTransportFailure::Unavailable,
                    _ => OutboundTransportFailure::Protocol,
                })
        })
    }
}

struct McpServerRuntime {
    transport: Arc<dyn McpPrimitiveTransport>,
    recipient_identity_digest: String,
}

pub(crate) struct McpApplication {
    catalog: RwLock<McpCatalog>,
    servers: BTreeMap<String, McpServerRuntime>,
}

impl McpApplication {
    pub(crate) fn new(native_tool_names: impl IntoIterator<Item = String>) -> Self {
        Self {
            catalog: RwLock::new(McpCatalog::new(CatalogLimits::default(), native_tool_names)),
            servers: BTreeMap::new(),
        }
    }

    pub(crate) fn add_server(
        &mut self,
        exposure: McpServerExposure,
        transport: Arc<dyn McpPrimitiveTransport>,
    ) -> Result<(), McpApplicationError> {
        let recipient_identity_digest = exposure.configured_identity_digest.clone();
        let server = exposure.server.clone();
        self.catalog.get_mut().add_server(exposure)?;
        if self
            .servers
            .insert(
                server,
                McpServerRuntime {
                    transport,
                    recipient_identity_digest,
                },
            )
            .is_some()
        {
            return Err(McpApplicationError::DuplicateServer);
        }
        Ok(())
    }

    pub(crate) async fn refresh(
        &self,
        server: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), McpApplicationError> {
        let runtime = self.runtime(server)?;
        let discovery = runtime
            .transport
            .request(
                "server/discover",
                json!({}),
                None,
                OperationId::new(),
                cancellation,
            )
            .await?;
        let discovery = decode_discover_response(&discovery.bytes, discovery.request_id)?;
        let capabilities = negotiate(&discovery)?.capabilities;
        if capabilities.tools.is_some() {
            self.refresh_tools(server, runtime, cancellation).await?;
        }
        if capabilities.resources.is_some() {
            self.refresh_resources(server, runtime, cancellation)
                .await?;
            self.refresh_resource_templates(server, runtime, cancellation)
                .await?;
        }
        if capabilities.prompts.is_some() {
            self.refresh_prompts(server, runtime, cancellation).await?;
        }
        Ok(())
    }

    pub(crate) async fn activate_tools(
        &self,
        registry: &mut ToolRegistry,
        cancellation: &CancellationToken,
    ) -> Result<usize, McpApplicationError> {
        let summaries = self.catalog.read().await.tool_summaries();
        let mut activated = 0;
        for summary in summaries {
            let runtime = self.runtime(&summary.server)?;
            let response = runtime
                .transport
                .request(
                    "tools/get",
                    json!({"name": summary.source_name}),
                    None,
                    OperationId::new(),
                    cancellation,
                )
                .await?;
            let wire = decode_tool_definition_result(&response.bytes, response.request_id)?;
            let mut source = ExactToolSource(Some(wire));
            let definition = self
                .catalog
                .read()
                .await
                .inspect_tool_exact(&summary.server, &summary.source_name, &mut source)
                .await?;
            registry.register_boxed(Box::new(McpRemoteTool {
                definition,
                transport: Arc::clone(&runtime.transport),
                recipient_identity_digest: runtime.recipient_identity_digest.clone(),
            }))?;
            activated += 1;
        }
        Ok(activated)
    }

    pub(crate) async fn tools(&self, query: &str, limit: usize) -> Vec<McpToolSummary> {
        self.catalog
            .read()
            .await
            .search_tools(query, limit)
            .into_iter()
            .cloned()
            .collect()
    }

    pub(crate) async fn resources(&self, query: &str, limit: usize) -> Vec<McpResourceSummary> {
        self.catalog
            .read()
            .await
            .search_resources(query, limit)
            .into_iter()
            .cloned()
            .collect()
    }

    pub(crate) async fn prompts(&self, query: &str, limit: usize) -> Vec<McpPromptSummary> {
        self.catalog
            .read()
            .await
            .search_prompts(query, limit)
            .into_iter()
            .cloned()
            .collect()
    }

    pub(crate) async fn read_resource(
        &self,
        server: &str,
        uri: &str,
        cancellation: &CancellationToken,
    ) -> Result<McpResourceDocument, McpApplicationError> {
        let summary = self.catalog.read().await.resource_summary(server, uri)?;
        let response = self
            .runtime(server)?
            .transport
            .request(
                "resources/read",
                json!({"uri": uri}),
                None,
                OperationId::new(),
                cancellation,
            )
            .await?;
        let result = decode_resource_read_result(&response.bytes, response.request_id)?;
        Ok(McpResourceDocument {
            server: server.to_owned(),
            uri: summary.uri,
            untrusted: true,
            contents: result.contents,
        })
    }

    pub(crate) async fn invoke_prompt(
        &self,
        server: &str,
        name: &str,
        arguments: BTreeMap<String, String>,
        cancellation: &CancellationToken,
    ) -> Result<McpPromptPreview, McpApplicationError> {
        let summary = self.catalog.read().await.prompt_summary(server, name)?;
        if arguments.len() > 128
            || arguments.iter().any(|(key, value)| {
                !summary.argument_names.contains(key)
                    || value.len() > 16 * 1024
                    || value.chars().any(char::is_control)
            })
        {
            return Err(McpApplicationError::InvalidArguments);
        }
        let response = self
            .runtime(server)?
            .transport
            .request(
                "prompts/get",
                json!({"name": name, "arguments": arguments}),
                None,
                OperationId::new(),
                cancellation,
            )
            .await?;
        let McpPromptResult {
            description,
            messages,
        } = decode_prompt_result(&response.bytes, response.request_id)?;
        Ok(McpPromptPreview {
            server: server.to_owned(),
            name: name.to_owned(),
            description,
            messages: messages
                .into_iter()
                .map(|message| McpPromptPreviewMessage {
                    role: match message.role {
                        McpPromptRole::User => "user",
                        McpPromptRole::Assistant => "assistant",
                    }
                    .to_owned(),
                    content: message.content,
                })
                .collect(),
            untrusted: true,
        })
    }

    fn runtime(&self, server: &str) -> Result<&McpServerRuntime, McpApplicationError> {
        self.servers
            .get(server)
            .ok_or_else(|| McpApplicationError::UnknownServer(server.to_owned()))
    }

    async fn refresh_tools(
        &self,
        server: &str,
        runtime: &McpServerRuntime,
        cancellation: &CancellationToken,
    ) -> Result<(), McpApplicationError> {
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let response = runtime
                .transport
                .request(
                    "tools/list",
                    cursor_params(cursor.as_deref()),
                    None,
                    OperationId::new(),
                    cancellation,
                )
                .await?;
            let page = decode_tool_page(&response.bytes, response.request_id)?;
            cursor = page.next_cursor.clone();
            self.catalog.write().await.index_tool_page(server, page)?;
            if cursor.is_none() {
                return Ok(());
            }
        }
        Err(McpApplicationError::PageLimit)
    }

    async fn refresh_resources(
        &self,
        server: &str,
        runtime: &McpServerRuntime,
        cancellation: &CancellationToken,
    ) -> Result<(), McpApplicationError> {
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let response = runtime
                .transport
                .request(
                    "resources/list",
                    cursor_params(cursor.as_deref()),
                    None,
                    OperationId::new(),
                    cancellation,
                )
                .await?;
            let page = decode_resource_page(&response.bytes, response.request_id)?;
            cursor = page.next_cursor.clone();
            self.catalog
                .write()
                .await
                .index_resource_page(server, page)?;
            if cursor.is_none() {
                return Ok(());
            }
        }
        Err(McpApplicationError::PageLimit)
    }

    async fn refresh_resource_templates(
        &self,
        server: &str,
        runtime: &McpServerRuntime,
        cancellation: &CancellationToken,
    ) -> Result<(), McpApplicationError> {
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let response = runtime
                .transport
                .request(
                    "resources/templates/list",
                    cursor_params(cursor.as_deref()),
                    None,
                    OperationId::new(),
                    cancellation,
                )
                .await?;
            let page = decode_resource_template_page(&response.bytes, response.request_id)?;
            cursor = page.next_cursor.clone();
            self.catalog
                .write()
                .await
                .index_resource_template_page(server, page)?;
            if cursor.is_none() {
                return Ok(());
            }
        }
        Err(McpApplicationError::PageLimit)
    }

    async fn refresh_prompts(
        &self,
        server: &str,
        runtime: &McpServerRuntime,
        cancellation: &CancellationToken,
    ) -> Result<(), McpApplicationError> {
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let response = runtime
                .transport
                .request(
                    "prompts/list",
                    cursor_params(cursor.as_deref()),
                    None,
                    OperationId::new(),
                    cancellation,
                )
                .await?;
            let page = decode_prompt_page(&response.bytes, response.request_id)?;
            cursor = page.next_cursor.clone();
            self.catalog.write().await.index_prompt_page(server, page)?;
            if cursor.is_none() {
                return Ok(());
            }
        }
        Err(McpApplicationError::PageLimit)
    }
}

fn cursor_params(cursor: Option<&str>) -> Value {
    cursor.map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}))
}

struct ExactToolSource(Option<McpToolWire>);

impl McpCatalogSource for ExactToolSource {
    fn load_tool<'a>(
        &'a mut self,
        _server: &'a str,
        _source_name: &'a str,
    ) -> BoxFuture<'a, Result<McpToolWire, McpCatalogError>> {
        Box::pin(async move { self.0.take().ok_or(McpCatalogError::SourceUnavailable) })
    }
}

struct McpRemoteTool {
    definition: McpToolDefinition,
    transport: Arc<dyn McpPrimitiveTransport>,
    recipient_identity_digest: String,
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Tool for McpRemoteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.definition.summary.qualified_name.clone(),
            contract_version: 1,
            description: format!(
                "MCP tool from server {}: {}",
                self.definition.summary.server, self.definition.summary.description_preview
            ),
            parameters: self.definition.input_schema.clone(),
            effect_class: EffectClass::Network,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &Path,
    ) -> Result<PlannedToolInvocation, String> {
        if !arguments.is_object() {
            return Err("MCP tool arguments must be an object".into());
        }
        let encoded = serde_json::to_vec(arguments)
            .map_err(|_| "MCP tool arguments could not be encoded".to_owned())?;
        if encoded.len() > MAX_ARGUMENT_BYTES {
            return Err(format!(
                "MCP tool arguments exceed the {MAX_ARGUMENT_BYTES}-byte limit"
            ));
        }
        Ok(PlannedToolInvocation::new(
            arguments.clone(),
            PermissionScope::External {
                recipient_identity_digest: self.recipient_identity_digest.clone(),
                operation: self.definition.summary.qualified_name.clone(),
            },
            arguments.clone(),
        ))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let arguments = planned.executable::<Value>(&self.definition.summary.qualified_name)?;
            // A cancelled agent turn drops this future. Tie that drop to the
            // transport request so stdio actors do not continue abandoned work.
            let cancellation = CancelOnDrop(CancellationToken::new());
            let response = self
                .transport
                .request(
                    "tools/call",
                    json!({
                        "name": self.definition.summary.source_name,
                        "arguments": arguments,
                    }),
                    Some(self.definition.input_schema.clone()),
                    _context.operation_id,
                    &cancellation.0,
                )
                .await
                .map_err(|error| error.to_string())?;
            let result = decode_tool_call_result(&response.bytes, response.request_id)
                .map_err(|error| error.to_string())?;
            render_tool_result(
                &self.definition.summary.server,
                &self.definition.summary.source_name,
                result,
            )
        })
    }
}

fn render_tool_result(
    server: &str,
    tool: &str,
    result: McpToolCallResult,
) -> Result<String, String> {
    let value = json!({
        "source": {"kind":"mcp_tool","server":server,"tool":tool},
        "untrusted": true,
        "is_error": result.is_error,
        "content": result.content,
        "structured_content": result.structured_content,
    });
    let encoded = serde_json::to_string(&value)
        .map_err(|_| "MCP tool result could not be encoded".to_owned())?;
    if encoded.len() > MAX_RENDERED_RESULT_BYTES {
        return Err(format!(
            "MCP tool result exceeds the {MAX_RENDERED_RESULT_BYTES}-byte limit"
        ));
    }
    if result.is_error {
        Err(encoded)
    } else {
        Ok(encoded)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpResourceDocument {
    pub(crate) server: String,
    pub(crate) uri: String,
    pub(crate) untrusted: bool,
    pub(crate) contents: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpPromptPreviewMessage {
    pub(crate) role: String,
    pub(crate) content: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpPromptPreview {
    pub(crate) server: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) messages: Vec<McpPromptPreviewMessage>,
    pub(crate) untrusted: bool,
}

#[derive(Debug)]
pub(crate) enum McpApplicationError {
    UnknownServer(String),
    DuplicateServer,
    InvalidArguments,
    PageLimit,
    Transport(String),
    Protocol(ProtocolError),
    Catalog(McpCatalogError),
    Registry(RegistryError),
    Http(McpHttpError),
    Egress(String),
}

impl fmt::Display for McpApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownServer(server) => write!(formatter, "unknown MCP server {server:?}"),
            Self::DuplicateServer => formatter.write_str("MCP server is already registered"),
            Self::InvalidArguments => formatter.write_str("MCP arguments are invalid"),
            Self::PageLimit => formatter.write_str("MCP catalog exceeded its page limit"),
            Self::Transport(reason) => write!(formatter, "MCP transport failed: {reason}"),
            Self::Protocol(error) => write!(formatter, "MCP protocol failed: {error}"),
            Self::Catalog(error) => write!(formatter, "MCP catalog failed: {error}"),
            Self::Registry(error) => write!(formatter, "MCP capability activation failed: {error}"),
            Self::Http(error) => write!(formatter, "MCP HTTP failed: {error}"),
            Self::Egress(reason) => write!(formatter, "MCP egress was denied: {reason}"),
        }
    }
}

impl Error for McpApplicationError {}

impl From<ProtocolError> for McpApplicationError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<McpCatalogError> for McpApplicationError {
    fn from(value: McpCatalogError) -> Self {
        Self::Catalog(value)
    }
}

impl From<RegistryError> for McpApplicationError {
    fn from(value: RegistryError) -> Self {
        Self::Registry(value)
    }
}

impl From<McpHttpError> for McpApplicationError {
    fn from(value: McpHttpError) -> Self {
        Self::Http(value)
    }
}

impl From<crate::outbound::OutboundError> for McpApplicationError {
    fn from(value: crate::outbound::OutboundError) -> Self {
        Self::Egress(value.to_string())
    }
}

#[cfg(test)]
mod tests;
