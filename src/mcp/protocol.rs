//! Private MCP 2026-07-28 JSON-RPC wire model.

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::{error::Error, fmt};

pub(crate) const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_PAGE_ITEMS: usize = 256;
const MAX_CURSOR_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 256;
const MAX_VERSION_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;
const MAX_URI_BYTES: usize = 4096;
const MAX_ARGUMENTS: usize = 128;
const MAX_ERROR_MESSAGE_BYTES: usize = 512;
const MAX_CAPABILITIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct McpRequestId(u64);

impl McpRequestId {
    pub(crate) fn new(value: u64) -> Result<Self, ProtocolError> {
        if value > i64::MAX as u64 {
            return Err(ProtocolError::InvalidRequestId);
        }
        Ok(Self(value))
    }

    fn value(self) -> Value {
        Value::from(self.0)
    }
}

pub(crate) fn encode_request(
    id: McpRequestId,
    method: &str,
    params: Value,
) -> Result<Vec<u8>, ProtocolError> {
    validate_method(method)?;
    let Value::Object(mut params) = params else {
        return Err(ProtocolError::InvalidParams);
    };
    if params.contains_key("_meta") {
        return Err(ProtocolError::ReservedMetadata);
    }
    params.insert("_meta".to_owned(), request_metadata());
    serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.value(),
        "method": method,
        "params": params,
    }))
    .map_err(|_| ProtocolError::Encode)
}

pub(crate) fn encode_notification(method: &str, params: Value) -> Result<Vec<u8>, ProtocolError> {
    validate_method(method)?;
    let Value::Object(mut params) = params else {
        return Err(ProtocolError::InvalidParams);
    };
    if params.contains_key("_meta") {
        return Err(ProtocolError::ReservedMetadata);
    }
    params.insert("_meta".to_owned(), request_metadata());
    serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
    .map_err(|_| ProtocolError::Encode)
}

fn request_metadata() -> Value {
    serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": {
            "name": "Xana",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpServerCapabilities {
    pub(crate) tools: Option<ListCapability>,
    pub(crate) resources: Option<ResourceCapability>,
    pub(crate) prompts: Option<ListCapability>,
    pub(crate) ignored: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListCapability {
    pub(crate) list_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceCapability {
    pub(crate) list_changed: bool,
    pub(crate) subscribe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpDiscoverResult {
    pub(crate) supported_versions: Vec<String>,
    pub(crate) capabilities: McpServerCapabilities,
    pub(crate) server_name: Option<String>,
    pub(crate) server_version: Option<String>,
    pub(crate) instructions_digest: Option<String>,
    pub(crate) instructions_bytes: usize,
    pub(crate) cache: McpCacheHint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpNegotiation {
    pub(crate) protocol_version: &'static str,
    pub(crate) capabilities: McpServerCapabilities,
    pub(crate) server_name: Option<String>,
    pub(crate) server_version: Option<String>,
    pub(crate) ignored_capabilities: Vec<String>,
}

pub(crate) fn negotiate(result: &McpDiscoverResult) -> Result<McpNegotiation, ProtocolError> {
    if !result
        .supported_versions
        .iter()
        .any(|version| version == MCP_PROTOCOL_VERSION)
    {
        return Err(ProtocolError::UnsupportedVersion {
            advertised: result.supported_versions.clone(),
        });
    }
    Ok(McpNegotiation {
        protocol_version: MCP_PROTOCOL_VERSION,
        capabilities: result.capabilities.clone(),
        server_name: result.server_name.clone(),
        server_version: result.server_version.clone(),
        ignored_capabilities: result.capabilities.ignored.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpCacheHint {
    pub(crate) ttl_ms: Option<u64>,
    pub(crate) scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Page<T> {
    pub(crate) items: Vec<T>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) cache: McpCacheHint,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct McpToolWire {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) input_schema: Value,
    #[serde(default)]
    pub(crate) output_schema: Option<Value>,
    #[serde(default)]
    pub(crate) annotations: Option<Value>,
    #[serde(default)]
    pub(crate) icons: Vec<Value>,
    #[serde(default, rename = "_meta")]
    pub(crate) meta: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct McpResourceWire {
    pub(crate) uri: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) mime_type: Option<String>,
    #[serde(default)]
    pub(crate) size: Option<u64>,
    #[serde(default)]
    pub(crate) annotations: Option<Value>,
    #[serde(default)]
    pub(crate) icons: Vec<Value>,
    #[serde(default, rename = "_meta")]
    pub(crate) meta: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct McpResourceTemplateWire {
    pub(crate) uri_template: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) mime_type: Option<String>,
    #[serde(default)]
    pub(crate) annotations: Option<Value>,
    #[serde(default)]
    pub(crate) icons: Vec<Value>,
    #[serde(default, rename = "_meta")]
    pub(crate) meta: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct McpPromptArgumentWire {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct McpPromptWire {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) arguments: Vec<McpPromptArgumentWire>,
    #[serde(default)]
    pub(crate) icons: Vec<Value>,
    #[serde(default, rename = "_meta")]
    pub(crate) meta: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum McpNotification {
    ToolsChanged,
    ResourcesChanged,
    PromptsChanged,
    Progress {
        token: String,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    },
    Cancelled {
        request_id: String,
        reason: Option<String>,
    },
}

pub(crate) fn decode_discover_response(
    bytes: &[u8],
    expected_id: McpRequestId,
) -> Result<McpDiscoverResult, ProtocolError> {
    let value = decode_result_value(bytes, expected_id)?;
    validate_complete(&value)?;
    let object = value.as_object().ok_or(ProtocolError::InvalidResult)?;
    let supported_versions = parse_string_array(
        object.get("supportedVersions"),
        "supportedVersions",
        32,
        MAX_VERSION_BYTES,
    )?;
    if supported_versions.is_empty() {
        return Err(ProtocolError::InvalidDiscovery(
            "supportedVersions cannot be empty",
        ));
    }
    let capabilities = parse_capabilities(object.get("capabilities"))?;
    let (server_name, server_version) = parse_server_info(object.get("_meta"))?;
    let instructions = object.get("instructions").and_then(Value::as_str);
    if instructions.is_some_and(|value| value.len() > MAX_DESCRIPTION_BYTES) {
        return Err(ProtocolError::FieldTooLarge("instructions"));
    }
    let cache = parse_cache_hint(object)?;
    Ok(McpDiscoverResult {
        supported_versions,
        capabilities,
        server_name,
        server_version,
        instructions_digest: instructions
            .map(|value| blake3::hash(value.as_bytes()).to_hex().to_string()),
        instructions_bytes: instructions.map_or(0, str::len),
        cache,
    })
}

pub(crate) fn decode_tool_page(
    bytes: &[u8],
    expected_id: McpRequestId,
) -> Result<Page<McpToolWire>, ProtocolError> {
    decode_page(bytes, expected_id, "tools")
}

pub(crate) fn decode_resource_page(
    bytes: &[u8],
    expected_id: McpRequestId,
) -> Result<Page<McpResourceWire>, ProtocolError> {
    decode_page(bytes, expected_id, "resources")
}

pub(crate) fn decode_resource_template_page(
    bytes: &[u8],
    expected_id: McpRequestId,
) -> Result<Page<McpResourceTemplateWire>, ProtocolError> {
    decode_page(bytes, expected_id, "resourceTemplates")
}

pub(crate) fn decode_prompt_page(
    bytes: &[u8],
    expected_id: McpRequestId,
) -> Result<Page<McpPromptWire>, ProtocolError> {
    decode_page(bytes, expected_id, "prompts")
}

fn decode_page<T: DeserializeOwned>(
    bytes: &[u8],
    expected_id: McpRequestId,
    field: &'static str,
) -> Result<Page<T>, ProtocolError> {
    let value = decode_result_value(bytes, expected_id)?;
    validate_complete(&value)?;
    let object = value.as_object().ok_or(ProtocolError::InvalidResult)?;
    let items_value = object
        .get(field)
        .ok_or(ProtocolError::MissingField(field))?;
    let item_count = items_value
        .as_array()
        .ok_or(ProtocolError::InvalidField(field))?
        .len();
    if item_count > MAX_PAGE_ITEMS {
        return Err(ProtocolError::PageItemLimit {
            actual: item_count,
            limit: MAX_PAGE_ITEMS,
        });
    }
    let items = serde_json::from_value(items_value.clone())
        .map_err(|_| ProtocolError::InvalidField(field))?;
    let next_cursor =
        parse_optional_string(object.get("nextCursor"), "nextCursor", MAX_CURSOR_BYTES)?;
    Ok(Page {
        items,
        next_cursor,
        cache: parse_cache_hint(object)?,
    })
}

pub(crate) fn decode_notification(bytes: &[u8]) -> Result<McpNotification, ProtocolError> {
    let value = decode_value(bytes)?;
    let object = value.as_object().ok_or(ProtocolError::InvalidMessage)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || object.contains_key("id") {
        return Err(ProtocolError::InvalidNotification);
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::InvalidNotification)?;
    match method {
        "notifications/tools/list_changed" => Ok(McpNotification::ToolsChanged),
        "notifications/resources/list_changed" => Ok(McpNotification::ResourcesChanged),
        "notifications/prompts/list_changed" => Ok(McpNotification::PromptsChanged),
        "notifications/progress" => parse_progress(object.get("params")),
        "notifications/cancelled" => parse_cancelled(object.get("params")),
        _ => Err(ProtocolError::UnsupportedNotification),
    }
}

fn parse_progress(params: Option<&Value>) -> Result<McpNotification, ProtocolError> {
    let params = params
        .and_then(Value::as_object)
        .ok_or(ProtocolError::InvalidNotification)?;
    let token = scalar_token(params.get("progressToken"))?;
    let progress = finite_number(params.get("progress"))?;
    let total = params
        .get("total")
        .map(|value| finite_number(Some(value)))
        .transpose()?;
    if progress < 0.0 || total.is_some_and(|total| total <= 0.0 || progress > total) {
        return Err(ProtocolError::InvalidNotification);
    }
    let message = parse_optional_string(params.get("message"), "message", MAX_ERROR_MESSAGE_BYTES)?;
    Ok(McpNotification::Progress {
        token,
        progress,
        total,
        message: message.map(|value| sanitize_text(&value, MAX_ERROR_MESSAGE_BYTES)),
    })
}

fn parse_cancelled(params: Option<&Value>) -> Result<McpNotification, ProtocolError> {
    let params = params
        .and_then(Value::as_object)
        .ok_or(ProtocolError::InvalidNotification)?;
    let request_id = scalar_token(params.get("requestId"))?;
    let reason = parse_optional_string(params.get("reason"), "reason", MAX_ERROR_MESSAGE_BYTES)?;
    Ok(McpNotification::Cancelled {
        request_id,
        reason: reason.map(|value| sanitize_text(&value, MAX_ERROR_MESSAGE_BYTES)),
    })
}

fn scalar_token(value: Option<&Value>) -> Result<String, ProtocolError> {
    let value = value.ok_or(ProtocolError::InvalidNotification)?;
    let rendered = match value {
        Value::String(value) if !value.is_empty() && value.len() <= MAX_NAME_BYTES => value.clone(),
        Value::Number(value) if value.is_i64() || value.is_u64() => value.to_string(),
        _ => return Err(ProtocolError::InvalidNotification),
    };
    Ok(sanitize_text(&rendered, MAX_NAME_BYTES))
}

fn finite_number(value: Option<&Value>) -> Result<f64, ProtocolError> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or(ProtocolError::InvalidNotification)
}

fn decode_result_value(bytes: &[u8], expected_id: McpRequestId) -> Result<Value, ProtocolError> {
    let value = decode_value(bytes)?;
    let object = value.as_object().ok_or(ProtocolError::InvalidMessage)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ProtocolError::InvalidJsonRpcVersion);
    }
    validate_response_id(object.get("id"), expected_id)?;
    match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(parse_remote_error(error)?),
        _ => Err(ProtocolError::InvalidResponseShape),
    }
}

fn decode_value(bytes: &[u8]) -> Result<Value, ProtocolError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            actual: bytes.len(),
            limit: MAX_FRAME_BYTES,
        });
    }
    serde_json::from_slice(bytes).map_err(|_| ProtocolError::MalformedJson)
}

fn validate_response_id(
    value: Option<&Value>,
    expected: McpRequestId,
) -> Result<(), ProtocolError> {
    if value == Some(&expected.value()) {
        Ok(())
    } else {
        Err(ProtocolError::ResponseIdMismatch)
    }
}

fn parse_remote_error(value: &Value) -> Result<ProtocolError, ProtocolError> {
    let object = value
        .as_object()
        .ok_or(ProtocolError::InvalidResponseShape)?;
    let code = object
        .get("code")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or(ProtocolError::InvalidResponseShape)?;
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::InvalidResponseShape)?;
    Ok(ProtocolError::Remote {
        code,
        message: sanitize_text(message, MAX_ERROR_MESSAGE_BYTES),
    })
}

fn validate_complete(value: &Value) -> Result<(), ProtocolError> {
    let result_type = value
        .as_object()
        .and_then(|object| object.get("resultType"))
        .and_then(Value::as_str)
        .unwrap_or("complete");
    if result_type == "complete" {
        Ok(())
    } else {
        Err(ProtocolError::UnsupportedResultType)
    }
}

fn parse_capabilities(value: Option<&Value>) -> Result<McpServerCapabilities, ProtocolError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(ProtocolError::InvalidDiscovery(
            "capabilities must be an object",
        ))?;
    if object.len() > MAX_CAPABILITIES {
        return Err(ProtocolError::FieldTooLarge("capabilities"));
    }
    let tools = object.get("tools").map(parse_list_capability).transpose()?;
    let resources = object
        .get("resources")
        .map(parse_resource_capability)
        .transpose()?;
    let prompts = object
        .get("prompts")
        .map(parse_list_capability)
        .transpose()?;
    let ignored = object
        .keys()
        .filter(|name| !matches!(name.as_str(), "tools" | "resources" | "prompts"))
        .map(|name| sanitize_text(name, MAX_NAME_BYTES))
        .collect();
    Ok(McpServerCapabilities {
        tools,
        resources,
        prompts,
        ignored,
    })
}

fn parse_list_capability(value: &Value) -> Result<ListCapability, ProtocolError> {
    let object = value.as_object().ok_or(ProtocolError::InvalidDiscovery(
        "capability must be an object",
    ))?;
    let list_changed = optional_bool(object, "listChanged")?;
    Ok(ListCapability { list_changed })
}

fn parse_resource_capability(value: &Value) -> Result<ResourceCapability, ProtocolError> {
    let object = value.as_object().ok_or(ProtocolError::InvalidDiscovery(
        "capability must be an object",
    ))?;
    Ok(ResourceCapability {
        list_changed: optional_bool(object, "listChanged")?,
        subscribe: optional_bool(object, "subscribe")?,
    })
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<bool, ProtocolError> {
    match object.get(key) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ProtocolError::InvalidDiscovery(
            "capability flag must be a boolean",
        )),
    }
}

fn parse_server_info(
    meta: Option<&Value>,
) -> Result<(Option<String>, Option<String>), ProtocolError> {
    let Some(meta) = meta else {
        return Ok((None, None));
    };
    let object = meta
        .as_object()
        .ok_or(ProtocolError::InvalidDiscovery("_meta must be an object"))?;
    let Some(info) = object.get("io.modelcontextprotocol/serverInfo") else {
        return Ok((None, None));
    };
    let info = info.as_object().ok_or(ProtocolError::InvalidDiscovery(
        "serverInfo must be an object",
    ))?;
    let name = parse_optional_string(info.get("name"), "serverInfo.name", MAX_NAME_BYTES)?
        .map(|value| sanitize_text(&value, MAX_NAME_BYTES));
    let version =
        parse_optional_string(info.get("version"), "serverInfo.version", MAX_VERSION_BYTES)?
            .map(|value| sanitize_text(&value, MAX_VERSION_BYTES));
    Ok((name, version))
}

fn parse_cache_hint(object: &Map<String, Value>) -> Result<McpCacheHint, ProtocolError> {
    let ttl_ms = object
        .get("ttlMs")
        .map(|value| value.as_u64().ok_or(ProtocolError::InvalidField("ttlMs")))
        .transpose()?;
    let scope = parse_optional_string(object.get("cacheScope"), "cacheScope", 64)?;
    Ok(McpCacheHint { ttl_ms, scope })
}

fn parse_string_array(
    value: Option<&Value>,
    field: &'static str,
    limit: usize,
    string_limit: usize,
) -> Result<Vec<String>, ProtocolError> {
    let values = value
        .and_then(Value::as_array)
        .ok_or(ProtocolError::InvalidField(field))?;
    if values.len() > limit {
        return Err(ProtocolError::FieldTooLarge(field));
    }
    values
        .iter()
        .map(|value| {
            let value = value.as_str().ok_or(ProtocolError::InvalidField(field))?;
            if value.is_empty() || value.len() > string_limit || value.chars().any(char::is_control)
            {
                return Err(ProtocolError::InvalidField(field));
            }
            Ok(value.to_owned())
        })
        .collect()
}

fn parse_optional_string(
    value: Option<&Value>,
    field: &'static str,
    limit: usize,
) -> Result<Option<String>, ProtocolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.as_str().ok_or(ProtocolError::InvalidField(field))?;
    if value.len() > limit {
        return Err(ProtocolError::FieldTooLarge(field));
    }
    Ok(Some(value.to_owned()))
}

fn validate_method(method: &str) -> Result<(), ProtocolError> {
    if method.is_empty()
        || method.len() > 128
        || method.chars().any(char::is_control)
        || method.starts_with("rpc.")
    {
        Err(ProtocolError::InvalidMethod)
    } else {
        Ok(())
    }
}

pub(super) fn sanitize_text(value: &str, max_bytes: usize) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let replacement = if character.is_control()
            || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            '\u{fffd}'
        } else {
            character
        };
        if output.len().saturating_add(replacement.len_utf8()) > max_bytes {
            break;
        }
        output.push(replacement);
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProtocolError {
    FrameTooLarge { actual: usize, limit: usize },
    MalformedJson,
    InvalidMessage,
    InvalidJsonRpcVersion,
    InvalidRequestId,
    ResponseIdMismatch,
    InvalidResponseShape,
    InvalidResult,
    InvalidMethod,
    InvalidParams,
    ReservedMetadata,
    Encode,
    Remote { code: i32, message: String },
    UnsupportedVersion { advertised: Vec<String> },
    UnsupportedResultType,
    UnsupportedNotification,
    InvalidNotification,
    InvalidDiscovery(&'static str),
    MissingField(&'static str),
    InvalidField(&'static str),
    FieldTooLarge(&'static str),
    PageItemLimit { actual: usize, limit: usize },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { actual, limit } => write!(
                formatter,
                "MCP frame contains {actual} bytes, exceeding the {limit}-byte limit"
            ),
            Self::MalformedJson => formatter.write_str("MCP peer returned malformed JSON"),
            Self::InvalidMessage => formatter.write_str("MCP peer returned an invalid message"),
            Self::InvalidJsonRpcVersion => formatter.write_str("MCP peer did not use JSON-RPC 2.0"),
            Self::InvalidRequestId => formatter.write_str("MCP request id is out of range"),
            Self::ResponseIdMismatch => {
                formatter.write_str("MCP response id does not match the request")
            }
            Self::InvalidResponseShape => {
                formatter.write_str("MCP response must contain exactly one result or error")
            }
            Self::InvalidResult => formatter.write_str("MCP result is invalid"),
            Self::InvalidMethod => formatter.write_str("MCP method is invalid"),
            Self::InvalidParams => formatter.write_str("MCP parameters must be an object"),
            Self::ReservedMetadata => formatter.write_str("MCP request metadata is owned by Xana"),
            Self::Encode => formatter.write_str("could not encode MCP request"),
            Self::Remote { code, message } => write!(formatter, "MCP error {code}: {message}"),
            Self::UnsupportedVersion { advertised } => write!(
                formatter,
                "MCP server does not advertise {MCP_PROTOCOL_VERSION}; advertised {}",
                advertised.join(", ")
            ),
            Self::UnsupportedResultType => {
                formatter.write_str("MCP result requires an unsupported extension or round trip")
            }
            Self::UnsupportedNotification => {
                formatter.write_str("MCP notification is outside Xana's supported subset")
            }
            Self::InvalidNotification => formatter.write_str("MCP notification is invalid"),
            Self::InvalidDiscovery(reason) => write!(formatter, "invalid MCP discovery: {reason}"),
            Self::MissingField(field) => write!(formatter, "MCP result is missing {field}"),
            Self::InvalidField(field) => write!(formatter, "MCP field {field} is invalid"),
            Self::FieldTooLarge(field) => write!(formatter, "MCP field {field} exceeds its limit"),
            Self::PageItemLimit { actual, limit } => write!(
                formatter,
                "MCP page contains {actual} items, exceeding the {limit}-item limit"
            ),
        }
    }
}

impl Error for ProtocolError {}
