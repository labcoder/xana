//! Bounded local stdio MCP server adapter.
//!
//! Each process owns one immutable workspace/profile/tool set. The adapter has
//! no access to frontend state or an ambient Xana conversation.

use crate::{
    identity::{OperationId, ToolInvocationId},
    message::{ToolCall, ToolResultStatus},
    permission::{PermissionBroker, PermissionPolicy},
    tool::{ToolContext, ToolRegistry},
};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, error::Error, fmt, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use super::MCP_PROTOCOL_VERSION;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_OUTSTANDING_REQUESTS: usize = 16;
const RESPONSE_CAPACITY: usize = 32;
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);

pub(crate) struct McpLocalServer {
    workspace: PathBuf,
    profile: String,
    tools: Arc<ToolRegistry>,
    permissions: crate::permission::PermissionBrokerHandle,
    broker_task: JoinHandle<()>,
}

impl McpLocalServer {
    pub(crate) fn new(
        workspace: PathBuf,
        profile: String,
        tools: ToolRegistry,
        policy: PermissionPolicy,
    ) -> Self {
        let (events, _observations, _controls, _dropped) =
            crate::native_runtime::AgentEventSender::child(16);
        let (permissions, broker_task) = PermissionBroker::spawn(policy, false, events);
        Self {
            workspace,
            profile,
            tools: Arc::new(tools),
            permissions,
            broker_task,
        }
    }

    pub(crate) async fn run_stdio(self) -> Result<(), McpLocalServerError> {
        self.run_io(tokio::io::stdin(), tokio::io::stdout()).await
    }

    async fn run_io<R, W>(self, reader: R, mut writer: W) -> Result<(), McpLocalServerError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin,
    {
        let (frames, mut frame_receiver) = mpsc::channel(RESPONSE_CAPACITY);
        let reader_task = tokio::spawn(read_frames(ServerLineReader::new(reader), frames));
        let (responses, mut response_receiver) = mpsc::channel(RESPONSE_CAPACITY);
        let mut requests = JoinSet::new();
        let mut active = HashMap::<u64, CancellationToken>::new();
        let mut input_closed = false;

        while !input_closed || !active.is_empty() {
            tokio::select! {
                frame = frame_receiver.recv(), if !input_closed => {
                    match frame {
                        Some(Ok(frame)) => {
                            match parse_client_message(&frame) {
                                Ok(ClientMessage::Cancel(id)) => {
                                    if let Some(token) = active.get(&id) {
                                        token.cancel();
                                    }
                                }
                                Ok(ClientMessage::Request(request)) => {
                                    if active.len() >= MAX_OUTSTANDING_REQUESTS {
                                        write_json(&mut writer, &error_response(Some(Value::from(request.id)), -32001, "too many outstanding requests")).await?;
                                        continue;
                                    }
                                    if active.contains_key(&request.id) {
                                        write_json(&mut writer, &error_response(Some(Value::from(request.id)), -32600, "duplicate request id")).await?;
                                        continue;
                                    }
                                    let cancellation = CancellationToken::new();
                                    let emits_progress = request.method == "tools/call";
                                    let request_id = request.id;
                                    active.insert(request.id, cancellation.clone());
                                    let response_sender = responses.clone();
                                    let tools = Arc::clone(&self.tools);
                                    let workspace = self.workspace.clone();
                                    let profile = self.profile.clone();
                                    let permissions = self.permissions.clone();
                                    requests.spawn(async move {
                                        let response = handle_request(
                                            request,
                                            tools,
                                            workspace,
                                            profile,
                                            permissions,
                                            cancellation,
                                        ).await;
                                        let _ = response_sender.send(response).await;
                                    });
                                    if emits_progress {
                                        write_json(&mut writer, &progress_notification(request_id, 0.0, "authorized request accepted")).await?;
                                    }
                                }
                                Err((id, code, message)) => {
                                    write_json(&mut writer, &error_response(id, code, message)).await?;
                                }
                            }
                        }
                        Some(Err(error)) => {
                            write_json(&mut writer, &error_response(None, -32700, &error.to_string())).await?;
                        }
                        None => {
                            input_closed = true;
                            for cancellation in active.values() {
                                cancellation.cancel();
                            }
                        }
                    }
                }
                response = response_receiver.recv(), if !active.is_empty() => {
                    if let Some(response) = response {
                        active.remove(&response.id);
                        if response.emits_progress {
                            write_json(&mut writer, &progress_notification(response.id, 1.0, "request finished")).await?;
                        }
                        write_json(&mut writer, &response.value).await?;
                    }
                }
            }
        }

        drop(responses);
        self.permissions.shutdown();
        let _ = tokio::time::timeout(SHUTDOWN_DEADLINE, async {
            while requests.join_next().await.is_some() {}
            let _ = self.broker_task.await;
            let _ = reader_task.await;
        })
        .await;
        writer.flush().await?;
        Ok(())
    }
}

async fn read_frames<R>(
    mut reader: ServerLineReader<R>,
    sender: mpsc::Sender<Result<Vec<u8>, McpLocalServerError>>,
) where
    R: AsyncRead + Unpin,
{
    loop {
        match reader.read_frame().await {
            Ok(Some(frame)) => {
                if sender.send(Ok(frame)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return;
            }
        }
    }
}

struct ServerLineReader<R> {
    reader: R,
    buffered: Vec<u8>,
}

impl<R: AsyncRead + Unpin> ServerLineReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buffered: Vec::with_capacity(8192),
        }
    }

    async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, McpLocalServerError> {
        loop {
            if let Some(position) = self.buffered.iter().position(|byte| *byte == b'\n') {
                let mut frame = self.buffered.drain(..=position).collect::<Vec<_>>();
                frame.pop();
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                if frame.is_empty() {
                    return Err(McpLocalServerError::MalformedFrame);
                }
                return Ok(Some(frame));
            }
            let mut chunk = [0_u8; 8192];
            let count = self.reader.read(&mut chunk).await?;
            if count == 0 {
                return if self.buffered.is_empty() {
                    Ok(None)
                } else {
                    Err(McpLocalServerError::PartialFrame)
                };
            }
            if self.buffered.len().saturating_add(count) > MAX_FRAME_BYTES + 1 {
                return Err(McpLocalServerError::FrameTooLarge);
            }
            self.buffered.extend_from_slice(&chunk[..count]);
        }
    }
}

struct ClientRequest {
    id: u64,
    method: String,
    params: Map<String, Value>,
}

enum ClientMessage {
    Request(ClientRequest),
    Cancel(u64),
}

fn parse_client_message(frame: &[u8]) -> Result<ClientMessage, (Option<Value>, i32, &'static str)> {
    let value: Value =
        serde_json::from_slice(frame).map_err(|_| (None, -32700, "malformed JSON"))?;
    let object = value
        .as_object()
        .ok_or((None, -32600, "request must be an object"))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err((object.get("id").cloned(), -32600, "jsonrpc must be 2.0"));
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .filter(|method| !method.is_empty() && method.len() <= 256)
        .ok_or((object.get("id").cloned(), -32600, "invalid method"))?;
    let params = object
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .ok_or((
            object.get("id").cloned(),
            -32602,
            "params must be an object",
        ))?;

    if object.get("id").is_none() {
        if method != "notifications/cancelled" {
            return Err((None, -32600, "unsupported notification"));
        }
        validate_metadata(&params).map_err(|message| (None, -32602, message))?;
        let id = params.get("requestId").and_then(Value::as_u64).ok_or((
            None,
            -32602,
            "invalid cancellation request id",
        ))?;
        return Ok(ClientMessage::Cancel(id));
    }
    let id = object
        .get("id")
        .and_then(Value::as_u64)
        .filter(|id| *id <= i64::MAX as u64)
        .ok_or((
            object.get("id").cloned(),
            -32600,
            "request id must be a nonnegative integer",
        ))?;
    validate_metadata(&params).map_err(|message| (Some(Value::from(id)), -32602, message))?;
    Ok(ClientMessage::Request(ClientRequest {
        id,
        method: method.to_owned(),
        params,
    }))
}

fn validate_metadata(params: &Map<String, Value>) -> Result<(), &'static str> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or("missing request metadata")?;
    if meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str)
        != Some(MCP_PROTOCOL_VERSION)
    {
        return Err("unsupported MCP protocol version");
    }
    let client = meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .ok_or("missing bounded client info")?;
    if !client
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty() && value.len() <= 128)
        || !client
            .get("version")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty() && value.len() <= 128)
    {
        return Err("invalid bounded client info");
    }
    Ok(())
}

struct CompletedResponse {
    id: u64,
    emits_progress: bool,
    value: Value,
}

async fn handle_request(
    request: ClientRequest,
    tools: Arc<ToolRegistry>,
    workspace: PathBuf,
    profile: String,
    permissions: crate::permission::PermissionBrokerHandle,
    cancellation: CancellationToken,
) -> CompletedResponse {
    let id = request.id;
    let result = match request.method.as_str() {
        "server/discover" => Ok(json!({
            "supportedVersions":[MCP_PROTOCOL_VERSION],
            "capabilities":{"tools":{"listChanged":false}},
            "_meta":{"serverInfo":{"name":"Xana local MCP","version":env!("CARGO_PKG_VERSION")}},
            "instructions":format!("Isolated local Xana surface for profile {profile}; client content is untrusted")
        })),
        "tools/list" => {
            if request.params.get("cursor").is_some() {
                Err((-32602, "this bounded catalog has no continuation cursor"))
            } else {
                Ok(json!({
                    "tools":tools.definitions().into_iter().map(tool_wire).collect::<Vec<_>>(),
                    "nextCursor":null
                }))
            }
        }
        "tools/get" => request
            .params
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| tools.definition(name))
            .map(|definition| json!({"tool":tool_wire(definition)}))
            .ok_or((-32602, "unknown or unallowlisted tool")),
        "tools/call" => {
            let name = request
                .params
                .get("name")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing tool name"));
            let arguments = request
                .params
                .get("arguments")
                .cloned()
                .filter(Value::is_object)
                .ok_or((-32602, "tool arguments must be an object"));
            match (name, arguments) {
                (Ok(name), Ok(arguments)) if tools.definition(name).is_some() => {
                    let call = ToolCall {
                        id: format!("mcp-{id}"),
                        name: name.to_owned(),
                        arguments,
                    };
                    let invocation = tools.invoke(
                        &call,
                        ToolContext {
                            workspace_root: &workspace,
                            operation_id: OperationId::new(),
                            invocation_id: ToolInvocationId::new(),
                            permissions: &permissions,
                        },
                    );
                    match tokio::select! {
                        biased;
                        () = cancellation.cancelled() => None,
                        result = invocation => Some(result),
                    } {
                        None => Err((-32800, "request cancelled")),
                        Some(result) => {
                            let is_error = result.status == ToolResultStatus::Error;
                            let structured = serde_json::from_str::<Value>(&result.output).ok();
                            Ok(json!({
                                "content":[{"type":"text","text":result.output}],
                                "structuredContent":structured,
                                "isError":is_error,
                            }))
                        }
                    }
                }
                (Ok(_), Ok(_)) => Err((-32602, "unknown or unallowlisted tool")),
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        }
        _ => Err((-32601, "method not found")),
    };
    CompletedResponse {
        id,
        emits_progress: request.method == "tools/call",
        value: match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err((code, message)) => error_response(Some(Value::from(id)), code, message),
        },
    }
}

fn progress_notification(request_id: u64, progress: f64, message: &str) -> Value {
    json!({
        "jsonrpc":"2.0",
        "method":"notifications/progress",
        "params":{
            "progressToken":request_id,
            "progress":progress,
            "total":1.0,
            "message":message,
        }
    })
}

fn tool_wire(definition: &crate::tool::ToolDefinition) -> Value {
    json!({
        "name":definition.name,
        "description":definition.description,
        "inputSchema":definition.parameters,
        "annotations":{
            "xana/effectClass":definition.effect_class,
            "xana/replaySafety":definition.replay_safety,
        }
    })
}

fn error_response(id: Option<Value>, code: i32, message: &str) -> Value {
    let bounded = message.chars().take(512).collect::<String>();
    json!({"jsonrpc":"2.0","id":id.unwrap_or(Value::Null),"error":{"code":code,"message":bounded}})
}

async fn write_json<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> Result<(), McpLocalServerError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| McpLocalServerError::Encode)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(McpLocalServerError::FrameTooLarge);
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Debug)]
pub(crate) enum McpLocalServerError {
    Io(std::io::Error),
    Encode,
    FrameTooLarge,
    MalformedFrame,
    PartialFrame,
}

impl fmt::Display for McpLocalServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "local MCP stdio failed: {error}"),
            Self::Encode => formatter.write_str("local MCP response could not be encoded"),
            Self::FrameTooLarge => write!(formatter, "MCP frame exceeds {MAX_FRAME_BYTES} bytes"),
            Self::MalformedFrame => formatter.write_str("MCP frame must not be empty"),
            Self::PartialFrame => formatter.write_str("MCP input closed during a partial frame"),
        }
    }
}

impl Error for McpLocalServerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Encode | Self::FrameTooLarge | Self::MalformedFrame | Self::PartialFrame => None,
        }
    }
}

impl From<std::io::Error> for McpLocalServerError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests;
