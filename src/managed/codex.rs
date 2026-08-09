//! Codex app-server adapter using the vendor-owned JSONL protocol.

mod events;

use crate::{
    model::{DescriptorSource, ModelDescriptor, ReasoningEffort, ReasoningSummary},
    process_capture,
};
use events::normalize_notification;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashSet},
    error::Error,
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const TURN_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const INTERRUPT_COMPLETION_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_VERSION_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_EVENT_TEXT_BYTES: usize = 64 * 1024;
const MAX_TURN_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ITEM_DETAIL_BYTES: usize = 16 * 1024;
const MAX_MODELS: usize = 1024;
const MAX_MODEL_PAGES: usize = 32;
const MAX_PLAN_STEPS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexLaunchConfig {
    pub(crate) program: String,
    pub(crate) home: Option<PathBuf>,
}

impl Default for CodexLaunchConfig {
    fn default() -> Self {
        Self {
            program: "codex".into(),
            home: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccountStatus {
    LoggedOut,
    ApiKey,
    ChatGpt { plan: String },
    Other { kind: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginMode {
    Browser,
    DeviceCode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginInstructions {
    pub(crate) login_id: String,
    pub(crate) url: String,
    pub(crate) user_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedTurnInput {
    pub(crate) text: String,
    pub(crate) local_images: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedTurnResult {
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) final_text: String,
    pub(crate) usage: Option<ManagedTokenUsage>,
    pub(crate) interruption_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagedTokenUsage {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedTurnOptions {
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) reasoning_summary: Option<ReasoningSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedApprovalPolicy {
    OnRequest,
    Never,
}

impl ManagedApprovalPolicy {
    fn wire(self) -> &'static str {
        match self {
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedSandbox {
    WorkspaceWrite,
}

impl ManagedSandbox {
    fn wire(self) -> &'static str {
        match self {
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagedThreadPolicy {
    pub(crate) approval: ManagedApprovalPolicy,
    pub(crate) sandbox: ManagedSandbox,
    pub(crate) ephemeral: bool,
}

impl Default for ManagedThreadPolicy {
    fn default() -> Self {
        Self {
            approval: ManagedApprovalPolicy::OnRequest,
            sandbox: ManagedSandbox::WorkspaceWrite,
            ephemeral: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedItem {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) status: Option<String>,
    pub(crate) phase: Option<String>,
    pub(crate) label: String,
    pub(crate) details: String,
    pub(crate) text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedPlanStep {
    pub(crate) step: String,
    pub(crate) status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ManagedNotification {
    ThreadStarted {
        thread_id: String,
    },
    AssistantDelta {
        item_id: Option<String>,
        delta: String,
    },
    ReasoningSummaryDelta {
        item_id: Option<String>,
        summary_index: Option<usize>,
        delta: String,
    },
    ReasoningSummaryPartAdded {
        item_id: Option<String>,
        summary_index: Option<usize>,
    },
    ReasoningDelta {
        item_id: Option<String>,
        delta: String,
    },
    PlanDelta {
        item_id: Option<String>,
        delta: String,
    },
    PlanUpdated {
        explanation: Option<String>,
        steps: Vec<ManagedPlanStep>,
    },
    CommandOutputDelta {
        item_id: Option<String>,
        delta: String,
    },
    DiffUpdated(String),
    ItemStarted(ManagedItem),
    ItemCompleted(ManagedItem),
    ModelRerouted {
        from_model: String,
        to_model: String,
        reason: String,
    },
    TurnCompleted {
        turn_id: String,
        status: String,
        error: Option<String>,
    },
    TokenUsageUpdated {
        thread_id: String,
        turn_id: String,
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
    Warning(String),
    LoginCompleted {
        login_id: String,
        success: bool,
        error: Option<String>,
    },
    Other {
        method: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ApprovalRequest {
    pub(crate) item_id: Option<String>,
    pub(crate) method: String,
    pub(crate) available_decisions: BTreeSet<String>,
    pub(crate) reason: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecision {
    AcceptOnce,
    AcceptForSession,
    Decline,
    Cancel,
}

impl ApprovalDecision {
    fn wire(self) -> &'static str {
        match self {
            Self::AcceptOnce => "accept",
            Self::AcceptForSession => "acceptForSession",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

pub(crate) trait ManagedEventHandler {
    fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError>;
    fn approve<'a>(
        &'a mut self,
        request: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>>;
}

#[derive(Debug)]
pub(crate) enum CodexError {
    Spawn(String),
    Timeout(&'static str),
    ProcessExited,
    Io(String),
    FrameTooLarge,
    Protocol(String),
    Remote { code: Option<i64>, message: String },
    UnsupportedServerRequest(String),
    LoginFailed(String),
    RequestCancelled(&'static str),
    TurnInterrupted { turn_id: String, status: String },
}

impl fmt::Display for CodexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(reason) => write!(f, "could not start Codex app-server: {reason}"),
            Self::Timeout(operation) => write!(f, "Codex app-server timed out during {operation}"),
            Self::ProcessExited => f.write_str("Codex app-server exited unexpectedly"),
            Self::Io(reason) => write!(f, "Codex app-server I/O failed: {reason}"),
            Self::FrameTooLarge => f.write_str("Codex app-server frame exceeded the safety limit"),
            Self::Protocol(reason) => write!(f, "invalid Codex app-server protocol: {reason}"),
            Self::Remote { code, message } => match code {
                Some(code) => write!(f, "Codex app-server error {code}: {message}"),
                None => write!(f, "Codex app-server error: {message}"),
            },
            Self::UnsupportedServerRequest(method) => {
                write!(f, "unsupported Codex app-server callback {method:?}")
            }
            Self::LoginFailed(reason) => write!(f, "Codex login failed: {reason}"),
            Self::RequestCancelled(method) => {
                write!(f, "Codex app-server request {method} was cancelled")
            }
            Self::TurnInterrupted { turn_id, status } => {
                write!(
                    f,
                    "Codex turn {turn_id} ended after interruption with status {status}"
                )
            }
        }
    }
}

impl Error for CodexError {}

struct JsonLinePeer<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
    incoming: Vec<u8>,
}

impl<R, W> JsonLinePeer<R, W>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            incoming: Vec::new(),
        }
    }

    async fn send(&mut self, value: &Value) -> Result<(), CodexError> {
        let mut encoded =
            serde_json::to_vec(value).map_err(|error| CodexError::Protocol(error.to_string()))?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(CodexError::FrameTooLarge);
        }
        encoded.push(b'\n');
        self.writer
            .write_all(&encoded)
            .await
            .map_err(|error| CodexError::Io(error.to_string()))?;
        self.writer
            .flush()
            .await
            .map_err(|error| CodexError::Io(error.to_string()))
    }

    async fn receive(&mut self) -> Result<Value, CodexError> {
        loop {
            let available = self
                .reader
                .fill_buf()
                .await
                .map_err(|error| CodexError::Io(error.to_string()))?;
            if available.is_empty() {
                return Err(CodexError::ProcessExited);
            }
            let end = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if self.incoming.len().saturating_add(end) > MAX_FRAME_BYTES {
                return Err(CodexError::FrameTooLarge);
            }
            self.incoming.extend_from_slice(&available[..end]);
            let complete = available.get(end.saturating_sub(1)) == Some(&b'\n');
            self.reader.consume(end);
            if complete {
                break;
            }
        }
        let line = std::mem::take(&mut self.incoming);
        serde_json::from_slice(&line).map_err(|error| CodexError::Protocol(error.to_string()))
    }

    async fn request<H: ManagedEventHandler + ?Sized>(
        &mut self,
        method: &str,
        params: Value,
        handler: &mut H,
    ) -> Result<Value, CodexError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.send(&json!({"method": method, "id": id, "params": params}))
            .await?;
        loop {
            let message = self.receive().await?;
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                return response_result(message);
            }
            self.handle_incoming(message, handler).await?;
        }
    }

    async fn request_cancellable<H: ManagedEventHandler + ?Sized>(
        &mut self,
        method: &'static str,
        params: Value,
        cancellation: &CancellationToken,
        handler: &mut H,
    ) -> Result<Value, CodexError> {
        if cancellation.is_cancelled() {
            return Err(CodexError::RequestCancelled(method));
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let request = json!({"method": method, "id": id, "params": params});
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return Err(CodexError::RequestCancelled(method));
            }
            sent = self.send(&request) => sent?,
        }
        loop {
            let message = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    return Err(CodexError::RequestCancelled(method));
                }
                message = self.receive() => message?,
            };
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                return response_result(message);
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    return Err(CodexError::RequestCancelled(method));
                }
                handled = self.handle_incoming(message, handler) => handled?,
            }
        }
    }

    async fn handle_incoming<H: ManagedEventHandler + ?Sized>(
        &mut self,
        message: Value,
        handler: &mut H,
    ) -> Result<(), CodexError> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| CodexError::Protocol("message is missing method".into()))?
            .to_owned();
        if let Some(id) = message.get("id").cloned() {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let result = approval_response(&method, &params, handler).await?;
            self.send(&json!({"id": id, "result": result})).await
        } else {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            handler.notification(normalize_notification(method, params)?)
        }
    }

    async fn wait_for<H: ManagedEventHandler + ?Sized>(
        &mut self,
        mut complete: impl FnMut(&ManagedNotification) -> Result<bool, CodexError>,
        handler: &mut H,
    ) -> Result<(), CodexError> {
        loop {
            let message = self.receive().await?;
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if message.get("id").is_some() && method.is_some() {
                self.handle_incoming(message, handler).await?;
                continue;
            }
            let method = method.ok_or_else(|| {
                CodexError::Protocol("unexpected response while waiting for notification".into())
            })?;
            let notification = normalize_notification(
                method,
                message.get("params").cloned().unwrap_or(Value::Null),
            )?;
            let done = complete(&notification)?;
            handler.notification(notification)?;
            if done {
                return Ok(());
            }
        }
    }

    async fn wait_for_turn<H: ManagedEventHandler + ?Sized>(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        cancellation: Option<&CancellationToken>,
        mut complete: impl FnMut(&ManagedNotification) -> Result<bool, CodexError>,
        handler: &mut H,
    ) -> Result<bool, CodexError> {
        let mut cancellation_observed = false;
        let mut interrupt_response_id = None;
        let mut interruption_deadline = None;
        loop {
            let message = if !cancellation_observed {
                tokio::select! {
                    biased;
                    _ = wait_for_cancellation(cancellation) => {
                        cancellation_observed = true;
                        let deadline = tokio::time::Instant::now() + INTERRUPT_COMPLETION_TIMEOUT;
                        interruption_deadline = Some(deadline);
                        let id = self.next_id;
                        self.next_id = self.next_id.saturating_add(1);
                        tokio::time::timeout_at(deadline, self.send(&json!({
                                "method":"turn/interrupt",
                                "id":id,
                                "params":{"threadId":thread_id,"turnId":turn_id}
                            })))
                            .await
                            .map_err(|_| CodexError::Timeout("turn interruption"))??;
                        interrupt_response_id = Some(id);
                        continue;
                    }
                    message = self.receive() => message?,
                }
            } else {
                let deadline = interruption_deadline
                    .expect("observed cancellation establishes an interruption deadline");
                if tokio::time::Instant::now() >= deadline {
                    return Err(CodexError::Timeout("turn interruption"));
                }
                tokio::time::timeout_at(deadline, self.receive())
                    .await
                    .map_err(|_| CodexError::Timeout("turn interruption"))??
            };
            if interrupt_response_id.is_some()
                && message.get("id").and_then(Value::as_u64) == interrupt_response_id
            {
                response_result(message)?;
                interrupt_response_id = None;
                continue;
            }
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if message.get("id").is_some() && method.is_some() {
                if let Some(deadline) = interruption_deadline {
                    tokio::time::timeout_at(deadline, self.handle_incoming(message, handler))
                        .await
                        .map_err(|_| CodexError::Timeout("turn interruption"))??;
                } else {
                    self.handle_incoming(message, handler).await?;
                }
                continue;
            }
            let method = method.ok_or_else(|| {
                CodexError::Protocol("unexpected response while waiting for turn".into())
            })?;
            let notification = normalize_notification(
                method,
                message.get("params").cloned().unwrap_or(Value::Null),
            )?;
            let done = complete(&notification)?;
            handler.notification(notification)?;
            if done {
                return Ok(cancellation_observed);
            }
        }
    }
}

async fn wait_for_cancellation(cancellation: Option<&CancellationToken>) {
    match cancellation {
        Some(cancellation) => cancellation.cancelled().await,
        None => std::future::pending().await,
    }
}

fn response_result(message: Value) -> Result<Value, CodexError> {
    if let Some(error) = message.get("error") {
        return Err(CodexError::Remote {
            code: error.get("code").and_then(Value::as_i64),
            message: bounded_text(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error"),
                4096,
            ),
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| CodexError::Protocol("response is missing result".into()))
}

async fn approval_response<H: ManagedEventHandler + ?Sized>(
    method: &str,
    params: &Value,
    handler: &mut H,
) -> Result<Value, CodexError> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let decision = handler
                .approve(ApprovalRequest {
                    item_id: params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    method: method.to_owned(),
                    available_decisions: params
                        .get("availableDecisions")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .take(16)
                                .filter_map(Value::as_str)
                                .filter(|value| value.len() <= 64)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_else(|| {
                            ["accept", "acceptForSession", "decline", "cancel"]
                                .into_iter()
                                .map(str::to_owned)
                                .collect()
                        }),
                    reason: params
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(|value| bounded_text(value, 4096)),
                    command: params
                        .get("command")
                        .and_then(Value::as_str)
                        .map(|value| bounded_text(value, MAX_ITEM_DETAIL_BYTES)),
                    cwd: params
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(|value| bounded_text(value, 4096)),
                })
                .await?;
            Ok(json!({"decision": decision.wire()}))
        }
        _ => Err(CodexError::UnsupportedServerRequest(method.to_owned())),
    }
}

fn thread_start_params(
    model: &str,
    workspace: &Path,
    developer_instructions: &str,
    policy: ManagedThreadPolicy,
) -> Value {
    json!({
        "model": model,
        "cwd": workspace,
        "approvalPolicy": policy.approval.wire(),
        "sandbox": policy.sandbox.wire(),
        "ephemeral": policy.ephemeral,
        "serviceName": "xana",
        "developerInstructions": developer_instructions
    })
}

fn thread_resume_params(
    thread_id: &str,
    model: &str,
    workspace: &Path,
    developer_instructions: &str,
    policy: ManagedThreadPolicy,
) -> Value {
    json!({
        "threadId": thread_id,
        "model": model,
        "cwd": workspace,
        "approvalPolicy": policy.approval.wire(),
        "sandbox": policy.sandbox.wire(),
        "developerInstructions": developer_instructions
    })
}

fn turn_start_params(
    thread_id: &str,
    model: &str,
    options: &ManagedTurnOptions,
    input: Vec<Value>,
) -> Value {
    let mut params = serde_json::Map::from_iter([
        ("threadId".into(), Value::String(thread_id.to_owned())),
        ("input".into(), Value::Array(input)),
        ("model".into(), Value::String(model.to_owned())),
    ]);
    if let Some(effort) = &options.reasoning_effort {
        params.insert("effort".into(), Value::String(effort.clone()));
    }
    if let Some(summary) = options.reasoning_summary {
        params.insert(
            "summary".into(),
            Value::String(summary.as_wire().to_owned()),
        );
    }
    Value::Object(params)
}

fn thread_id_from_result(result: &Value, method: &str) -> Result<String, CodexError> {
    let id = result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| CodexError::Protocol(format!("{method} omitted thread id")))?;
    if id.is_empty() || id.len() > 4096 {
        return Err(CodexError::Protocol(format!(
            "{method} returned an invalid thread id"
        )));
    }
    Ok(id.to_owned())
}

fn model_descriptor_from_wire(value: &Value) -> Result<ModelDescriptor, CodexError> {
    let id = required_string(value, "id")?;
    let input_modalities = value
        .get("inputModalities")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| matches!(*value, "text" | "image"))
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_else(|| {
            ["text".to_owned(), "image".to_owned()]
                .into_iter()
                .collect()
        });
    let mut seen_efforts = BTreeSet::new();
    let reasoning_efforts = value
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(32)
        .filter_map(|effort| {
            let id = bounded_text(effort.get("reasoningEffort")?.as_str()?, 64);
            if !seen_efforts.insert(id.clone()) {
                return None;
            }
            Some(ReasoningEffort {
                id,
                description: bounded_text(
                    effort
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    1024,
                ),
            })
        })
        .collect::<Vec<_>>();
    let default_reasoning_effort = value
        .get("defaultReasoningEffort")
        .and_then(Value::as_str)
        .map(|value| bounded_text(value, 64));
    if let Some(default) = &default_reasoning_effort
        && !reasoning_efforts.is_empty()
        && !reasoning_efforts.iter().any(|effort| effort.id == *default)
    {
        return Err(CodexError::Protocol(format!(
            "model {id:?} names an unsupported default reasoning effort {default:?}"
        )));
    }
    Ok(ModelDescriptor {
        display_name: value
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_owned(),
        id,
        input_modalities,
        tools: Some(true),
        reasoning: Some(!reasoning_efforts.is_empty()),
        reasoning_efforts,
        default_reasoning_effort,
        context_tokens: None,
        max_output_tokens: None,
        source: DescriptorSource::ManagedRuntime,
        is_default: value
            .get("isDefault")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub(crate) struct CodexAppServer {
    child: Child,
    peer: JsonLinePeer<BufReader<ChildStdout>, BufWriter<ChildStdin>>,
    pub(crate) version: String,
    pub(crate) codex_home: PathBuf,
    protocol_usable: bool,
}

impl CodexAppServer {
    pub(crate) async fn spawn(config: &CodexLaunchConfig) -> Result<Self, CodexError> {
        let version = probe_version(config).await?;
        let mut command = Command::new(&config.program);
        command
            .arg("app-server")
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(home) = &config.home {
            command.env("CODEX_HOME", home);
        }
        let mut child = command
            .spawn()
            .map_err(|error| CodexError::Spawn(error.to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CodexError::Spawn("stdout was not piped".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| CodexError::Spawn("stdin was not piped".into()))?;
        let mut peer = JsonLinePeer::new(BufReader::new(stdout), BufWriter::new(stdin));
        let mut handler = RejectingHandler;
        let initialize = timeout(
            STARTUP_TIMEOUT,
            peer.request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "xana",
                        "title": "Xana",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": { "experimentalApi": false }
                }),
                &mut handler,
            ),
        )
        .await
        .map_err(|_| CodexError::Timeout("initialize"))??;
        let codex_home = initialize
            .get("codexHome")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| CodexError::Protocol("initialize omitted codexHome".into()))?;
        peer.send(&json!({"method": "initialized", "params": {}}))
            .await?;
        Ok(Self {
            child,
            peer,
            version,
            codex_home,
            protocol_usable: true,
        })
    }

    pub(crate) async fn account_status(&mut self) -> Result<AccountStatus, CodexError> {
        let mut handler = RejectingHandler;
        let result = self
            .request("account/read", json!({"refreshToken": false}), &mut handler)
            .await?;
        let Some(account) = result.get("account") else {
            return Ok(AccountStatus::LoggedOut);
        };
        if account.is_null() {
            return Ok(AccountStatus::LoggedOut);
        }
        let kind = account
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        Ok(match kind {
            "apiKey" => AccountStatus::ApiKey,
            "chatgpt" => AccountStatus::ChatGpt {
                plan: account
                    .get("planType")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
            },
            _ => AccountStatus::Other {
                kind: kind.to_owned(),
            },
        })
    }

    pub(crate) async fn begin_login(
        &mut self,
        mode: LoginMode,
    ) -> Result<LoginInstructions, CodexError> {
        let mut handler = RejectingHandler;
        let params = match mode {
            LoginMode::Browser => json!({"type": "chatgpt"}),
            LoginMode::DeviceCode => json!({"type": "chatgptDeviceCode"}),
        };
        let result = self
            .request("account/login/start", params, &mut handler)
            .await?;
        let login_id = required_string(&result, "loginId")?;
        match mode {
            LoginMode::Browser => Ok(LoginInstructions {
                login_id,
                url: required_string(&result, "authUrl")?,
                user_code: None,
            }),
            LoginMode::DeviceCode => Ok(LoginInstructions {
                login_id,
                url: required_string(&result, "verificationUrl")?,
                user_code: Some(required_string(&result, "userCode")?),
            }),
        }
    }

    pub(crate) async fn wait_for_login(
        &mut self,
        login_id: &str,
    ) -> Result<AccountStatus, CodexError> {
        let mut success = None;
        let mut handler = CapturingHandler;
        if !self.protocol_usable {
            return Err(CodexError::Protocol(
                "app-server connection is unavailable after a prior timeout".into(),
            ));
        }
        let completion = timeout(
            LOGIN_TIMEOUT,
            self.peer.wait_for(
                |notification| {
                    let ManagedNotification::LoginCompleted {
                        login_id: completed_login_id,
                        success: completed_success,
                        error,
                    } = notification
                    else {
                        return Ok(false);
                    };
                    if completed_login_id != login_id {
                        return Ok(false);
                    }
                    success = Some((*completed_success, error.clone()));
                    Ok(true)
                },
                &mut handler,
            ),
        )
        .await;
        let completion = match completion {
            Ok(completion) => completion,
            Err(_) => {
                self.protocol_usable = false;
                return Err(CodexError::Timeout("login completion"));
            }
        };
        if let Err(error) = completion {
            self.protocol_usable = false;
            return Err(error);
        }
        let (ok, reason) = success.unwrap_or((false, Some("missing completion state".into())));
        if !ok {
            return Err(CodexError::LoginFailed(
                reason.unwrap_or_else(|| "authorization was not completed".into()),
            ));
        }
        self.account_status().await
    }

    pub(crate) async fn logout(&mut self) -> Result<(), CodexError> {
        let mut handler = RejectingHandler;
        self.request("account/logout", Value::Null, &mut handler)
            .await?;
        Ok(())
    }

    pub(crate) async fn rate_limits(&mut self) -> Result<Value, CodexError> {
        let mut handler = RejectingHandler;
        self.request("account/rateLimits/read", Value::Null, &mut handler)
            .await
    }

    pub(crate) async fn models(&mut self) -> Result<Vec<ModelDescriptor>, CodexError> {
        let mut cursor = None::<String>;
        let mut models = Vec::new();
        let mut seen_cursors = HashSet::new();
        for _ in 0..MAX_MODEL_PAGES {
            let mut handler = RejectingHandler;
            let result = self
                .request(
                    "model/list",
                    json!({"cursor": cursor, "limit": 100, "includeHidden": false}),
                    &mut handler,
                )
                .await?;
            let data = result
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| CodexError::Protocol("model/list omitted data".into()))?;
            for value in data {
                if models.len() >= MAX_MODELS {
                    return Err(CodexError::Protocol(format!(
                        "model/list exceeds the {MAX_MODELS}-model limit"
                    )));
                }
                models.push(model_descriptor_from_wire(value)?);
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                models.sort_by(|left, right| left.id.cmp(&right.id));
                models.dedup_by(|left, right| left.id == right.id);
                return Ok(models);
            }
            if !seen_cursors.insert(cursor.clone().expect("cursor was checked above")) {
                return Err(CodexError::Protocol(
                    "model/list repeated a pagination cursor".into(),
                ));
            }
        }
        Err(CodexError::Protocol(format!(
            "model/list exceeds the {MAX_MODEL_PAGES}-page limit"
        )))
    }

    pub(crate) async fn start_thread<H: ManagedEventHandler>(
        &mut self,
        model: &str,
        workspace: &Path,
        developer_instructions: &str,
        handler: &mut H,
    ) -> Result<String, CodexError> {
        self.start_thread_with_policy(
            model,
            workspace,
            developer_instructions,
            ManagedThreadPolicy::default(),
            handler,
        )
        .await
    }

    pub(crate) async fn start_thread_with_policy<H: ManagedEventHandler + ?Sized>(
        &mut self,
        model: &str,
        workspace: &Path,
        developer_instructions: &str,
        policy: ManagedThreadPolicy,
        handler: &mut H,
    ) -> Result<String, CodexError> {
        let result = self
            .request(
                "thread/start",
                thread_start_params(model, workspace, developer_instructions, policy),
                handler,
            )
            .await?;
        thread_id_from_result(&result, "thread/start")
    }

    pub(crate) async fn resume_thread<H: ManagedEventHandler>(
        &mut self,
        thread_id: &str,
        model: &str,
        workspace: &Path,
        developer_instructions: &str,
        handler: &mut H,
    ) -> Result<String, CodexError> {
        let result = self
            .request(
                "thread/resume",
                thread_resume_params(
                    thread_id,
                    model,
                    workspace,
                    developer_instructions,
                    ManagedThreadPolicy::default(),
                ),
                handler,
            )
            .await?;
        thread_id_from_result(&result, "thread/resume")
    }

    pub(crate) async fn run_turn<H: ManagedEventHandler>(
        &mut self,
        thread_id: &str,
        model: &str,
        options: &ManagedTurnOptions,
        input: ManagedTurnInput,
        handler: &mut H,
    ) -> Result<ManagedTurnResult, CodexError> {
        self.run_turn_inner(thread_id, model, options, input, None, handler)
            .await
    }

    pub(crate) async fn run_turn_cancellable<H: ManagedEventHandler + ?Sized>(
        &mut self,
        thread_id: &str,
        model: &str,
        options: &ManagedTurnOptions,
        input: ManagedTurnInput,
        cancellation: &CancellationToken,
        handler: &mut H,
    ) -> Result<ManagedTurnResult, CodexError> {
        self.run_turn_inner(
            thread_id,
            model,
            options,
            input,
            Some(cancellation),
            handler,
        )
        .await
    }

    async fn run_turn_inner<H: ManagedEventHandler + ?Sized>(
        &mut self,
        thread_id: &str,
        model: &str,
        options: &ManagedTurnOptions,
        input: ManagedTurnInput,
        cancellation: Option<&CancellationToken>,
        handler: &mut H,
    ) -> Result<ManagedTurnResult, CodexError> {
        let mut user_input = vec![json!({"type": "text", "text": input.text})];
        user_input.extend(
            input
                .local_images
                .into_iter()
                .map(|path| json!({"type": "localImage", "path": path})),
        );
        let params = turn_start_params(thread_id, model, options, user_input);
        let result = match cancellation {
            Some(cancellation) => match timeout(
                REQUEST_TIMEOUT,
                self.peer
                    .request_cancellable("turn/start", params, cancellation, handler),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error @ CodexError::RequestCancelled(_))) => {
                    self.protocol_usable = false;
                    return Err(error);
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    self.protocol_usable = false;
                    return Err(CodexError::Timeout("turn/start"));
                }
            },
            None => self.request("turn/start", params, handler).await?,
        };
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| CodexError::Protocol("turn/start omitted turn id".into()))?
            .to_owned();
        let mut streamed_text = String::new();
        let mut final_text = None::<String>;
        let mut completed_status = None;
        let mut completed_error = None;
        let mut usage = None;
        if !self.protocol_usable {
            return Err(CodexError::Protocol(
                "app-server connection is unavailable after a prior timeout".into(),
            ));
        }
        let completion = timeout(
            TURN_TIMEOUT,
            self.peer.wait_for_turn(
                thread_id,
                &turn_id,
                cancellation,
                |notification| match notification {
                    ManagedNotification::AssistantDelta { delta, .. } => {
                        if streamed_text.len().saturating_add(delta.len()) > MAX_TURN_TEXT_BYTES {
                            return Err(CodexError::Protocol(format!(
                                "assistant output exceeds the {MAX_TURN_TEXT_BYTES}-byte turn limit"
                            )));
                        }
                        streamed_text.push_str(delta);
                        Ok(false)
                    }
                    ManagedNotification::ItemCompleted(item)
                        if item.kind == "agentMessage"
                            && item.phase.as_deref() != Some("commentary") =>
                    {
                        if let Some(text) = &item.text {
                            final_text = Some(text.clone());
                        }
                        Ok(false)
                    }
                    ManagedNotification::TurnCompleted {
                        turn_id: completed,
                        status,
                        error,
                    } if completed == &turn_id => {
                        completed_status = Some(status.clone());
                        completed_error = error.clone();
                        Ok(true)
                    }
                    ManagedNotification::TokenUsageUpdated {
                        thread_id: usage_thread,
                        turn_id: usage_turn,
                        input_tokens,
                        output_tokens,
                        total_tokens,
                    } if usage_thread == thread_id && usage_turn == &turn_id => {
                        usage = Some(ManagedTokenUsage {
                            input_tokens: *input_tokens,
                            output_tokens: *output_tokens,
                            total_tokens: *total_tokens,
                        });
                        Ok(false)
                    }
                    _ => Ok(false),
                },
                handler,
            ),
        )
        .await;
        let completion = match completion {
            Ok(completion) => completion,
            Err(_) => {
                self.protocol_usable = false;
                return Err(CodexError::Timeout("turn completion"));
            }
        };
        let interruption_requested = match completion {
            Ok(interruption_requested) => interruption_requested,
            Err(error) => {
                self.protocol_usable = false;
                return Err(error);
            }
        };
        let status = completed_status.unwrap_or_else(|| "unknown".into());
        if status != "completed" {
            if interruption_requested {
                return Err(CodexError::TurnInterrupted { turn_id, status });
            }
            return Err(CodexError::Remote {
                code: None,
                message: completed_error
                    .unwrap_or_else(|| format!("turn {turn_id} ended with status {status}")),
            });
        }
        Ok(ManagedTurnResult {
            thread_id: thread_id.to_owned(),
            turn_id,
            final_text: final_text.unwrap_or(streamed_text),
            usage,
            interruption_requested,
        })
    }

    async fn request<H: ManagedEventHandler + ?Sized>(
        &mut self,
        method: &'static str,
        params: Value,
        handler: &mut H,
    ) -> Result<Value, CodexError> {
        if !self.protocol_usable {
            return Err(CodexError::Protocol(
                "app-server connection is unavailable after a prior timeout".into(),
            ));
        }
        match timeout(REQUEST_TIMEOUT, self.peer.request(method, params, handler)).await {
            Ok(result) => result,
            Err(_) => {
                self.protocol_usable = false;
                Err(CodexError::Timeout(method))
            }
        }
    }

    pub(crate) async fn shutdown(mut self) -> Result<(), CodexError> {
        drop(self.peer);
        match timeout(Duration::from_secs(2), self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(CodexError::Io(error.to_string())),
            Err(_) => self
                .child
                .kill()
                .await
                .map_err(|error| CodexError::Io(error.to_string())),
        }
    }
}

async fn probe_version(config: &CodexLaunchConfig) -> Result<String, CodexError> {
    let mut command = Command::new(&config.program);
    command.arg("--version");
    if let Some(home) = &config.home {
        command.env("CODEX_HOME", home);
    }
    let output = timeout(
        STARTUP_TIMEOUT,
        process_capture::run(&mut command, MAX_VERSION_OUTPUT_BYTES),
    )
    .await
    .map_err(|_| CodexError::Timeout("version probe"))?
    .map_err(|error| CodexError::Spawn(error.to_string()))?;
    if !output.status.success() {
        return Err(CodexError::Spawn(format!(
            "version probe exited with {}",
            output.status
        )));
    }
    if output.stdout_truncated {
        return Err(CodexError::Protocol(format!(
            "version output exceeds the {MAX_VERSION_OUTPUT_BYTES}-byte limit"
        )));
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| CodexError::Protocol("version output is not UTF-8".into()))?;
    let version = version.trim();
    if !version.starts_with("codex-cli ") {
        return Err(CodexError::Protocol(format!(
            "unexpected version output {version:?}"
        )));
    }
    Ok(version.to_owned())
}

fn required_string(value: &Value, field: &str) -> Result<String, CodexError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CodexError::Protocol(format!("response omitted {field}")))
}

fn bounded_json_summary(value: &Value) -> String {
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| "<unavailable>".into());
    if rendered.len() <= MAX_ITEM_DETAIL_BYTES {
        return rendered;
    }
    let mut end = MAX_ITEM_DETAIL_BYTES;
    while !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… [details truncated]", &rendered[..end])
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

struct RejectingHandler;
impl ManagedEventHandler for RejectingHandler {
    fn notification(&mut self, _: ManagedNotification) -> Result<(), CodexError> {
        Ok(())
    }
    fn approve<'a>(
        &'a mut self,
        _: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        Box::pin(async { Ok(ApprovalDecision::Decline) })
    }
}

#[derive(Default)]
struct CapturingHandler;
impl ManagedEventHandler for CapturingHandler {
    fn notification(&mut self, _: ManagedNotification) -> Result<(), CodexError> {
        Ok(())
    }
    fn approve<'a>(
        &'a mut self,
        _: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        Box::pin(async { Ok(ApprovalDecision::Decline) })
    }
}

#[cfg(test)]
mod tests;
