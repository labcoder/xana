//! Modern stateless MCP Streamable HTTP transport.

use super::{MCP_PROTOCOL_VERSION, ProtocolError, decode_notification};
use crate::{
    credential::SecretString,
    outbound::{
        OutboundItem, OutboundTransport, OutboundTransportFailure, RecipientIdentity, RecipientKind,
    },
    sse::{SseDecoder, SseError},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::StreamExt;
use reqwest::{Client, StatusCode, Url, header};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_HEADERS_BYTES: usize = 32 * 1024;
const MAX_SSE_EVENTS: usize = 256;
const MAX_HEADER_VALUE_BYTES: usize = 8192;
const MAX_RAW_HEADER_VALUE_BYTES: usize = 6000;
const MAX_TOOL_HEADERS: usize = 32;
const MAX_SCHEMA_WALK_DEPTH: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct McpHttpSecurity {
    pub(crate) allow_loopback_http: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpHttpEndpoint {
    url: Url,
    security: McpHttpSecurity,
}

impl McpHttpEndpoint {
    pub(crate) fn parse(value: &str, security: McpHttpSecurity) -> Result<Self, McpHttpError> {
        let url = Url::parse(value).map_err(|_| McpHttpError::InvalidEndpoint)?;
        if url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
        {
            return Err(McpHttpError::InvalidEndpoint);
        }
        match url.scheme() {
            "https" => {}
            "http" if security.allow_loopback_http && is_loopback_host(&url) => {}
            _ => return Err(McpHttpError::HttpsRequired),
        }
        Ok(Self { url, security })
    }

    pub(crate) fn canonical(&self) -> &str {
        self.url.as_str()
    }

    pub(crate) fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct McpHttpClient {
    endpoint: McpHttpEndpoint,
    client: Client,
}

impl McpHttpClient {
    pub(crate) async fn connect(endpoint: McpHttpEndpoint) -> Result<Self, McpHttpError> {
        let client = pinned_client(&endpoint.url, endpoint.security, REQUEST_TIMEOUT).await?;
        Ok(Self { endpoint, client })
    }

    pub(crate) fn endpoint(&self) -> &McpHttpEndpoint {
        &self.endpoint
    }

    pub(crate) async fn request(
        &self,
        body: &[u8],
        bearer: Option<&SecretString>,
        tool_headers: &McpHttpToolHeaders,
        cancellation: &CancellationToken,
    ) -> Result<McpHttpResponse, McpHttpError> {
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(McpHttpError::RequestTooLarge);
        }
        let envelope = RequestEnvelope::parse(body)?;
        let mut request = self
            .client
            .post(self.endpoint.url.clone())
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::CONTENT_TYPE, "application/json")
            .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
            .header("Mcp-Method", &envelope.method)
            .body(body.to_vec());
        if let Some(name) = &envelope.name {
            request = request.header("Mcp-Name", encode_header_value(name));
        }
        for (name, value) in &tool_headers.headers {
            request = request.header(name, value);
        }
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer.expose());
        }
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(McpHttpError::Cancelled),
            response = request.send() => response.map_err(classify_reqwest)?,
        };
        validate_response_headers(&response)?;
        match response.status() {
            StatusCode::UNAUTHORIZED => {
                return Err(McpHttpError::Unauthorized(parse_challenge_header(
                    response.headers().get(header::WWW_AUTHENTICATE),
                )));
            }
            StatusCode::FORBIDDEN => return Err(McpHttpError::InsufficientScope),
            StatusCode::TOO_MANY_REQUESTS => return Err(McpHttpError::RateLimited),
            status if status.is_redirection() => return Err(McpHttpError::RedirectRejected),
            status if !status.is_success() => return Err(McpHttpError::Http(status.as_u16())),
            _ => {}
        }
        if response.status() == StatusCode::ACCEPTED {
            return Err(McpHttpError::UnexpectedNotificationAck);
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        match content_type {
            Some("application/json") => {
                let bytes = collect_body(response, cancellation).await?;
                validate_final_response(&bytes, envelope.id)?;
                Ok(McpHttpResponse {
                    final_response: bytes,
                    notifications: Vec::new(),
                })
            }
            Some("text/event-stream") => collect_sse(response, envelope.id, cancellation).await,
            _ => Err(McpHttpError::UnsupportedMediaType),
        }
    }
}

pub(super) async fn pinned_client(
    url: &Url,
    security: McpHttpSecurity,
    timeout: Duration,
) -> Result<Client, McpHttpError> {
    if url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(McpHttpError::InvalidEndpoint);
    }
    match url.scheme() {
        "https" => {}
        "http" if security.allow_loopback_http && is_loopback_host(url) => {}
        _ => return Err(McpHttpError::HttpsRequired),
    }
    let host = url.host_str().ok_or(McpHttpError::InvalidEndpoint)?;
    let port = url
        .port_or_known_default()
        .ok_or(McpHttpError::InvalidEndpoint)?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| McpHttpError::Dns)?
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<SocketAddr>>();
    if addresses.is_empty() {
        return Err(McpHttpError::Dns);
    }
    if addresses
        .iter()
        .any(|address| !is_permitted_address(address.ip(), security.allow_loopback_http))
    {
        return Err(McpHttpError::AddressPolicy);
    }
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| McpHttpError::Client)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct McpHttpResponse {
    pub(crate) final_response: Vec<u8>,
    pub(crate) notifications: Vec<super::McpNotification>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct McpHttpToolHeaders {
    headers: BTreeMap<String, String>,
}

impl McpHttpToolHeaders {
    pub(crate) fn from_schema_and_arguments(
        schema: &Value,
        arguments: &Value,
    ) -> Result<Self, McpHttpError> {
        let mut discovered = BTreeMap::new();
        collect_header_annotations(schema, Vec::new(), 0, &mut discovered)?;
        if discovered.len() > MAX_TOOL_HEADERS {
            return Err(McpHttpError::ToolHeaderSchema);
        }
        let mut headers = BTreeMap::new();
        for (name, path) in discovered {
            if let Some(value) = value_at_path(arguments, &path) {
                if value.is_null() {
                    continue;
                }
                let value = primitive_header_value(value)?;
                headers.insert(format!("Mcp-Param-{name}"), encode_header_value(&value));
            }
        }
        Ok(Self { headers })
    }
}

#[derive(Debug)]
pub(crate) struct McpHttpOutboundTransport {
    client: McpHttpClient,
    recipient_digest: String,
    bearer: Option<SecretString>,
    headers: McpHttpToolHeaders,
    cancellation: CancellationToken,
}

impl McpHttpOutboundTransport {
    pub(crate) fn new(
        client: McpHttpClient,
        recipient: &RecipientIdentity,
        bearer: Option<SecretString>,
        headers: McpHttpToolHeaders,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            client,
            recipient_digest: recipient.identity_digest.clone(),
            bearer,
            headers,
            cancellation,
        }
    }
}

impl OutboundTransport for McpHttpOutboundTransport {
    type Receipt = McpHttpResponse;

    fn send<'a>(
        &'a mut self,
        recipient: &'a RecipientIdentity,
        items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        Box::pin(async move {
            if recipient.identity_digest != self.recipient_digest || items.len() != 1 {
                return Err(OutboundTransportFailure::Rejected);
            }
            self.client
                .request(
                    items[0].bytes(),
                    self.bearer.as_ref(),
                    &self.headers,
                    &self.cancellation,
                )
                .await
                .map_err(|error| match error {
                    McpHttpError::Cancelled => OutboundTransportFailure::Cancelled,
                    McpHttpError::Timeout => OutboundTransportFailure::TimedOut,
                    McpHttpError::Unauthorized(_)
                    | McpHttpError::InsufficientScope
                    | McpHttpError::RateLimited
                    | McpHttpError::RedirectRejected
                    | McpHttpError::Http(_) => OutboundTransportFailure::Rejected,
                    McpHttpError::Dns | McpHttpError::Connect | McpHttpError::AddressPolicy => {
                        OutboundTransportFailure::Unavailable
                    }
                    _ => OutboundTransportFailure::Protocol,
                })
        })
    }
}

pub(crate) fn mcp_http_recipient(
    connection: &str,
    endpoint: &McpHttpEndpoint,
    auth_issuer: Option<&str>,
    client_id: Option<&str>,
) -> Result<RecipientIdentity, crate::outbound::OutboundError> {
    let identity = format!(
        "protocol={MCP_PROTOCOL_VERSION}\nendpoint={}\nissuer={}\nclient_id={}",
        endpoint.canonical(),
        auth_issuer.unwrap_or("none"),
        client_id.unwrap_or("none")
    );
    RecipientIdentity::new(
        RecipientKind::McpHttp,
        connection,
        endpoint.canonical(),
        identity.as_bytes(),
    )
}

struct RequestEnvelope {
    id: u64,
    method: String,
    name: Option<String>,
}

impl RequestEnvelope {
    fn parse(bytes: &[u8]) -> Result<Self, McpHttpError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| McpHttpError::ProtocolShape)?;
        let object = value.as_object().ok_or(McpHttpError::ProtocolShape)?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(McpHttpError::ProtocolShape);
        }
        let id = object
            .get("id")
            .and_then(Value::as_u64)
            .ok_or(McpHttpError::ProtocolShape)?;
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .filter(|method| !method.is_empty() && method.len() <= 128)
            .ok_or(McpHttpError::ProtocolShape)?
            .to_owned();
        let params = object
            .get("params")
            .and_then(Value::as_object)
            .ok_or(McpHttpError::ProtocolShape)?;
        if params
            .get("_meta")
            .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
            .and_then(Value::as_str)
            != Some(MCP_PROTOCOL_VERSION)
        {
            return Err(McpHttpError::ProtocolShape);
        }
        let name = match method.as_str() {
            "tools/call" | "prompts/get" => params.get("name"),
            "resources/read" => params.get("uri"),
            _ => None,
        }
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= MAX_RAW_HEADER_VALUE_BYTES)
                .map(str::to_owned)
                .ok_or(McpHttpError::ProtocolShape)
        })
        .transpose()?;
        Ok(Self { id, method, name })
    }
}

async fn collect_body(
    response: reqwest::Response,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, McpHttpError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(McpHttpError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut output = Vec::new();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(McpHttpError::Cancelled),
            next = stream.next() => next,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(classify_reqwest)?;
        if output.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(McpHttpError::ResponseTooLarge);
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

async fn collect_sse(
    response: reqwest::Response,
    expected_id: u64,
    cancellation: &CancellationToken,
) -> Result<McpHttpResponse, McpHttpError> {
    let mut stream = response.bytes_stream();
    let mut decoder = SseDecoder::default();
    let mut notifications = Vec::new();
    let mut final_response = None;
    let mut total = 0_usize;
    let mut events = 0_usize;
    loop {
        let next = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(McpHttpError::Cancelled),
            next = stream.next() => next,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(classify_reqwest)?;
        total = total.saturating_add(chunk.len());
        if total > MAX_RESPONSE_BYTES {
            return Err(McpHttpError::ResponseTooLarge);
        }
        for event in decoder.push(&chunk).map_err(McpHttpError::Sse)? {
            events = events.saturating_add(1);
            if events > MAX_SSE_EVENTS {
                return Err(McpHttpError::EventLimit);
            }
            let value: Value =
                serde_json::from_slice(&event.data).map_err(|_| McpHttpError::ProtocolShape)?;
            if value.get("id").is_some() {
                if final_response.is_some() {
                    return Err(McpHttpError::ProtocolShape);
                }
                validate_final_response(&event.data, expected_id)?;
                final_response = Some(event.data);
            } else {
                notifications.push(decode_notification(&event.data)?);
            }
        }
    }
    decoder.finish().map_err(McpHttpError::Sse)?;
    Ok(McpHttpResponse {
        final_response: final_response.ok_or(McpHttpError::MissingFinalResponse)?,
        notifications,
    })
}

fn validate_final_response(bytes: &[u8], expected_id: u64) -> Result<(), McpHttpError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| McpHttpError::ProtocolShape)?;
    let object = value.as_object().ok_or(McpHttpError::ProtocolShape)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_u64) != Some(expected_id)
        || matches!(
            (object.contains_key("result"), object.contains_key("error")),
            (true, true) | (false, false)
        )
    {
        return Err(McpHttpError::ProtocolShape);
    }
    Ok(())
}

fn validate_response_headers(response: &reqwest::Response) -> Result<(), McpHttpError> {
    let bytes = response
        .headers()
        .iter()
        .fold(0_usize, |total, (name, value)| {
            total
                .saturating_add(name.as_str().len())
                .saturating_add(value.as_bytes().len())
        });
    if bytes > MAX_RESPONSE_HEADERS_BYTES {
        Err(McpHttpError::ResponseHeadersTooLarge)
    } else {
        Ok(())
    }
}

fn parse_challenge_header(value: Option<&header::HeaderValue>) -> Option<super::McpAuthChallenge> {
    let value = value?.to_str().ok()?;
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return None;
    }
    super::McpAuthChallenge::parse(value).ok()
}

fn collect_header_annotations(
    schema: &Value,
    path: Vec<String>,
    depth: usize,
    output: &mut BTreeMap<String, Vec<String>>,
) -> Result<(), McpHttpError> {
    if depth > MAX_SCHEMA_WALK_DEPTH {
        return Err(McpHttpError::ToolHeaderSchema);
    }
    let object = schema.as_object().ok_or(McpHttpError::ToolHeaderSchema)?;
    if let Some(name) = object.get("x-mcp-header") {
        let name = name.as_str().ok_or(McpHttpError::ToolHeaderSchema)?;
        if path.is_empty() || !valid_header_token(name) {
            return Err(McpHttpError::ToolHeaderSchema);
        }
        let primitive = object.get("type").and_then(Value::as_str);
        if !matches!(primitive, Some("string" | "integer" | "boolean")) {
            return Err(McpHttpError::ToolHeaderSchema);
        }
        let canonical = name.to_ascii_lowercase();
        if output.insert(canonical, path.clone()).is_some() {
            return Err(McpHttpError::ToolHeaderSchema);
        }
    }
    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or(McpHttpError::ToolHeaderSchema)?;
        for (name, child) in properties {
            let mut child_path = path.clone();
            child_path.push(name.clone());
            collect_header_annotations(child, child_path, depth + 1, output)?;
        }
    }
    for forbidden in [
        "items", "oneOf", "anyOf", "allOf", "not", "if", "then", "else", "$ref",
    ] {
        if contains_header_annotation(object.get(forbidden)) {
            return Err(McpHttpError::ToolHeaderSchema);
        }
    }
    Ok(())
}

fn contains_header_annotation(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Object(object)) => {
            object.contains_key("x-mcp-header")
                || object
                    .values()
                    .any(|value| contains_header_annotation(Some(value)))
        }
        Some(Value::Array(values)) => values
            .iter()
            .any(|value| contains_header_annotation(Some(value))),
        _ => false,
    }
}

fn value_at_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, segment| current.get(segment))
}

fn primitive_header_value(value: &Value) -> Result<String, McpHttpError> {
    match value {
        Value::String(value) if value.len() <= MAX_RAW_HEADER_VALUE_BYTES => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) if value.as_i64().is_some() || value.as_u64().is_some() => {
            let number = value
                .as_i64()
                .map(|value| value as i128)
                .or_else(|| value.as_u64().map(|value| value as i128))
                .ok_or(McpHttpError::ToolHeaderValue)?;
            if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&number) {
                return Err(McpHttpError::ToolHeaderValue);
            }
            Ok(number.to_string())
        }
        _ => Err(McpHttpError::ToolHeaderValue),
    }
}

fn encode_header_value(value: &str) -> String {
    let plain = !value.is_empty()
        && value.len() <= MAX_RAW_HEADER_VALUE_BYTES
        && value
            .as_bytes()
            .iter()
            .all(|byte| matches!(byte, 0x20..=0x7e | b'\t'))
        && value.trim() == value
        && !(value.starts_with("=?base64?") && value.ends_with("?="));
    if plain {
        value.to_owned()
    } else {
        format!("=?base64?{}?=", STANDARD.encode(value.as_bytes()))
    }
}

fn valid_header_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn classify_reqwest(error: reqwest::Error) -> McpHttpError {
    if error.is_timeout() {
        McpHttpError::Timeout
    } else if error.is_connect() {
        McpHttpError::Connect
    } else {
        McpHttpError::Transport
    }
}

fn is_loopback_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn is_permitted_address(address: IpAddr, allow_loopback: bool) -> bool {
    if address.is_loopback() {
        return allow_loopback;
    }
    match address {
        IpAddr::V4(address) => permitted_ipv4(address),
        IpAddr::V6(address) => permitted_ipv6(address),
    }
}

fn permitted_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !(address.is_unspecified()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        || octets[0] == 0
        || octets[0] >= 224
        || octets[0] == 100 && (64..=127).contains(&octets[1])
        || octets[0] == 198 && matches!(octets[1], 18 | 19))
}

fn permitted_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    !(address.is_unspecified()
        || address.is_multicast()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[derive(Debug)]
pub(crate) enum McpHttpError {
    InvalidEndpoint,
    HttpsRequired,
    Dns,
    AddressPolicy,
    Client,
    Connect,
    Timeout,
    Cancelled,
    Transport,
    RequestTooLarge,
    ResponseTooLarge,
    ResponseHeadersTooLarge,
    UnsupportedMediaType,
    UnexpectedNotificationAck,
    RedirectRejected,
    Unauthorized(Option<super::McpAuthChallenge>),
    InsufficientScope,
    RateLimited,
    Http(u16),
    ProtocolShape,
    MissingFinalResponse,
    EventLimit,
    ToolHeaderSchema,
    ToolHeaderValue,
    Sse(SseError),
    Protocol(ProtocolError),
}

impl From<ProtocolError> for McpHttpError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl fmt::Display for McpHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => formatter.write_str("MCP HTTP endpoint is invalid"),
            Self::HttpsRequired => formatter.write_str("MCP HTTP endpoint requires HTTPS"),
            Self::Dns => formatter.write_str("MCP HTTP endpoint could not be resolved"),
            Self::AddressPolicy => {
                formatter.write_str("MCP HTTP endpoint resolves to a blocked address")
            }
            Self::Client => formatter.write_str("MCP HTTP client could not be built"),
            Self::Connect => formatter.write_str("MCP HTTP endpoint is unavailable"),
            Self::Timeout => formatter.write_str("MCP HTTP request timed out"),
            Self::Cancelled => formatter.write_str("MCP HTTP request was cancelled"),
            Self::Transport => formatter.write_str("MCP HTTP transport failed"),
            Self::RequestTooLarge => formatter.write_str("MCP HTTP request exceeds its limit"),
            Self::ResponseTooLarge => formatter.write_str("MCP HTTP response exceeds its limit"),
            Self::ResponseHeadersTooLarge => {
                formatter.write_str("MCP HTTP response headers exceed their limit")
            }
            Self::UnsupportedMediaType => {
                formatter.write_str("MCP HTTP response media type is unsupported")
            }
            Self::UnexpectedNotificationAck => {
                formatter.write_str("MCP HTTP request received a notification acknowledgement")
            }
            Self::RedirectRejected => formatter.write_str("MCP HTTP redirect was rejected"),
            Self::Unauthorized(_) => {
                formatter.write_str("MCP HTTP authorization is required or invalid")
            }
            Self::InsufficientScope => {
                formatter.write_str("MCP HTTP credential has insufficient scope")
            }
            Self::RateLimited => formatter.write_str("MCP HTTP endpoint is rate limited"),
            Self::Http(status) => write!(formatter, "MCP HTTP endpoint returned status {status}"),
            Self::ProtocolShape => formatter.write_str("MCP HTTP message is invalid"),
            Self::MissingFinalResponse => {
                formatter.write_str("MCP HTTP stream ended without a final response")
            }
            Self::EventLimit => formatter.write_str("MCP HTTP stream exceeds its event limit"),
            Self::ToolHeaderSchema => formatter.write_str("MCP tool header schema is invalid"),
            Self::ToolHeaderValue => formatter.write_str("MCP tool header value is invalid"),
            Self::Sse(error) => write!(formatter, "MCP HTTP stream is invalid: {error}"),
            Self::Protocol(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for McpHttpError {}

#[cfg(test)]
mod tests;
