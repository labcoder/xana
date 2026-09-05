//! Codex app-server adapter using the vendor-owned JSONL protocol.

mod approvals;
mod events;
mod thread_policy;

use crate::{
    model_catalog::{DescriptorSource, ModelDescriptor, ReasoningEffort, ReasoningSummary},
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
use thread_policy::checked_thread_id;
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
// Up to 20 MiB of accepted image bytes become ~27 MiB of base64. Keep vendor
// replies bounded independently; never decrypt attachments into temporary files.
const MAX_OUTGOING_FRAME_BYTES: usize = 32 * 1024 * 1024;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginCancellation {
    Cancelled,
    NotFound,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ManagedTurnInput {
    pub(crate) text: String,
    pub(crate) image_urls: Vec<String>,
}

impl std::fmt::Debug for ManagedTurnInput {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("ManagedTurnInput")
            .field("text_bytes", &self.text.len())
            .field("image_count", &self.image_urls.len())
            .finish()
    }
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
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) output_tokens: u64,
    pub(crate) reasoning_tokens: Option<u64>,
    pub(crate) total_tokens: u64,
    pub(crate) context_input_tokens: Option<u64>,
    pub(crate) context_window_tokens: Option<u64>,
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
        cached_input_tokens: Option<u64>,
        output_tokens: u64,
        reasoning_tokens: Option<u64>,
        total_tokens: u64,
        context_input_tokens: Option<u64>,
        context_window_tokens: Option<u64>,
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
    awaiting_response: bool,
}

struct TurnInterruption {
    response_id: Option<u64>,
    deadline: tokio::time::Instant,
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
            awaiting_response: false,
        }
    }

    async fn send(&mut self, value: &Value) -> Result<(), CodexError> {
        let mut encoded =
            serde_json::to_vec(value).map_err(|error| CodexError::Protocol(error.to_string()))?;
        if encoded.len() > MAX_OUTGOING_FRAME_BYTES {
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
        self.awaiting_response = true;
        self.send(&json!({"method": method, "id": id, "params": params}))
            .await?;
        loop {
            let message = self.receive().await?;
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && message.get("method").is_none()
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                return self.finish_response(message);
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
        self.awaiting_response = true;
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
                && message.get("method").is_none()
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                return self.finish_response(message);
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

    fn finish_response(&mut self, message: Value) -> Result<Value, CodexError> {
        let result = response_result(message);
        // Only a complete, correlated response resolves the RPC. A Remote
        // error returned by a controller callback is not such a response.
        if result.is_ok() || matches!(result, Err(CodexError::Remote { .. })) {
            self.awaiting_response = false;
        }
        result
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
        if message.get("id").is_some() {
            Err(CodexError::Protocol(format!(
                "server request {} arrived outside an acknowledged active turn",
                bounded_text(&method, 256)
            )))
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
        let mut interruption: Option<TurnInterruption> = None;
        let mut terminal_seen = false;
        let mut seen_approvals = HashSet::new();
        loop {
            let message = if let Some(interruption) = &interruption {
                let deadline = interruption.deadline;
                if tokio::time::Instant::now() >= deadline {
                    return Err(CodexError::Timeout("turn interruption"));
                }
                tokio::time::timeout_at(deadline, self.receive())
                    .await
                    .map_err(|_| CodexError::Timeout("turn interruption"))??
            } else {
                tokio::select! {
                    biased;
                    _ = wait_for_cancellation(cancellation) => {
                        interruption = Some(self.interrupt_turn(thread_id, turn_id).await?);
                        continue;
                    }
                    message = self.receive() => message?,
                }
            };
            if let Some(interruption) = interruption.as_mut()
                && interruption.response_id.is_some()
                && message.get("method").is_none()
                && message.get("id").and_then(Value::as_u64) == interruption.response_id
            {
                response_result(message)?;
                interruption.response_id = None;
                if terminal_seen {
                    return Ok(true);
                }
                continue;
            }
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(id) = message.get("id")
                && let Some(method) = method.as_deref()
            {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                approvals::validate_scope(id, &params, thread_id, turn_id, &mut seen_approvals)?;
                let request = approvals::decode(method, &params)?;
                let result = if interruption.is_some() {
                    approvals::cancel(&request)?
                } else {
                    // Only the controller future is dropped here; no response
                    // bytes have been written. A stale oneshot answer can no
                    // longer authorize work after cancellation wins.
                    tokio::select! {
                        biased;
                        _ = wait_for_cancellation(cancellation) => {
                            interruption = Some(self.interrupt_turn(thread_id, turn_id).await?);
                            approvals::cancel(&request)?
                        }
                        result = approvals::answer(&request, handler) => result?,
                    }
                };
                let response = json!({"id":id,"result":result});
                if let Some(interruption) = &interruption {
                    tokio::time::timeout_at(interruption.deadline, self.send(&response))
                        .await
                        .map_err(|_| CodexError::Timeout("turn interruption"))??;
                } else {
                    // A cancelled write may have emitted a partial JSONL
                    // frame. The caller must retire this connection rather
                    // than try to send an interrupt on an uncertain stream.
                    tokio::select! {
                        biased;
                        _ = wait_for_cancellation(cancellation) => {
                            return Err(CodexError::RequestCancelled("approval response"));
                        }
                        sent = self.send(&response) => sent?,
                    }
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
            let done = !terminal_seen && complete(&notification)?;
            handler.notification(notification)?;
            if done {
                terminal_seen = true;
                if interruption
                    .as_ref()
                    .is_none_or(|state| state.response_id.is_none())
                {
                    return Ok(interruption.is_some());
                }
            }
        }
    }

    async fn interrupt_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<TurnInterruption, CodexError> {
        let deadline = tokio::time::Instant::now() + INTERRUPT_COMPLETION_TIMEOUT;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        tokio::time::timeout_at(
            deadline,
            self.send(&json!({
                "method":"turn/interrupt",
                "id":id,
                "params":{"threadId":thread_id,"turnId":turn_id}
            })),
        )
        .await
        .map_err(|_| CodexError::Timeout("turn interruption"))??;
        Ok(TurnInterruption {
            response_id: Some(id),
            deadline,
        })
    }
}

async fn wait_for_cancellation(cancellation: Option<&CancellationToken>) {
    match cancellation {
        Some(cancellation) => cancellation.cancelled().await,
        None => std::future::pending().await,
    }
}

fn response_result(message: Value) -> Result<Value, CodexError> {
    match (message.get("result"), message.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => {
            let code = error.get("code").and_then(Value::as_i64);
            let text = error.get("message").and_then(Value::as_str);
            match (code, text) {
                (Some(code), Some(text)) => Err(CodexError::Remote {
                    code: Some(code),
                    message: bounded_text(text, 4096),
                }),
                _ => Err(CodexError::Protocol("malformed error response".into())),
            }
        }
        _ => Err(CodexError::Protocol(
            "response must contain exactly one result or error".into(),
        )),
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
        "approvalsReviewer": "user",
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
        "approvalsReviewer": "user",
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
        output_modalities: ["text".to_owned()].into_iter().collect(),
        tools: Some(true),
        reasoning: Some(!reasoning_efforts.is_empty()),
        reasoning_efforts,
        default_reasoning_effort,
        context_tokens: None,
        max_output_tokens: None,
        pricing: crate::model_catalog::ModelPricing::default(),
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
    usage_budget: Option<crate::usage_budget::UsageBudget>,
    usage_operation: Option<crate::identity::OperationId>,
}

impl CodexAppServer {
    pub(crate) fn set_usage_budget(&mut self, budget: Option<crate::usage_budget::UsageBudget>) {
        self.usage_budget = budget;
    }

    pub(crate) fn set_usage_identity(
        &mut self,
        root: String,
        operation: crate::identity::OperationId,
    ) {
        if let Some(budget) = &mut self.usage_budget {
            budget.rebind_root(root);
        }
        self.usage_operation = Some(operation);
    }
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
            usage_budget: None,
            usage_operation: None,
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
        self.ensure_usable()?;
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

    pub(crate) async fn cancel_login(
        &mut self,
        login_id: &str,
    ) -> Result<LoginCancellation, CodexError> {
        let mut handler = RejectingHandler;
        let result = self
            .request(
                "account/login/cancel",
                json!({"loginId": login_id}),
                &mut handler,
            )
            .await?;
        match required_string(&result, "status")?.as_str() {
            "canceled" => Ok(LoginCancellation::Cancelled),
            "notFound" => Ok(LoginCancellation::NotFound),
            status => Err(CodexError::Protocol(format!(
                "account/login/cancel returned unknown status {status:?}"
            ))),
        }
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
        let thread = checked_thread_id(&result, "thread/start", workspace, policy, None);
        if thread.is_err() {
            self.protocol_usable = false;
        }
        thread
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
        let thread = checked_thread_id(
            &result,
            "thread/resume",
            workspace,
            ManagedThreadPolicy::default(),
            Some(thread_id),
        );
        if thread.is_err() {
            self.protocol_usable = false;
        }
        thread
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
        self.ensure_usable()?;
        let operation = self
            .usage_operation
            .take()
            .unwrap_or_else(crate::identity::OperationId::new);
        let reservation = self
            .usage_budget
            .as_ref()
            .map(|budget| {
                budget
                    .with_model(model)
                    .with_reasoning(options.reasoning_effort.clone())
                    .admit(
                        operation,
                        crate::identity::StepId::new(),
                        crate::context::estimate_tokens(&input.text) as u64,
                    )
            })
            .transpose()
            .map_err(|error| CodexError::Protocol(format!("usage admission: {error}")))?;
        let mut user_input = vec![json!({"type": "text", "text": input.text})];
        user_input.extend(
            input
                .image_urls
                .into_iter()
                .map(|url| json!({"type": "image", "url": url})),
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
                Ok(Err(error)) => return Err(self.failed_request(error)),
                Err(_) => {
                    return Err(self.retire(CodexError::Timeout("turn/start")));
                }
            },
            None => self.request("turn/start", params, handler).await?,
        };
        let turn_id = match result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 4096)
        {
            Some(id) => id.to_owned(),
            None => {
                return Err(self.retire(CodexError::Protocol(
                    "turn/start returned a missing or invalid turn id".into(),
                )));
            }
        };
        let mut streamed_text = String::new();
        let mut final_text = None::<String>;
        let mut completed_status = None;
        let mut completed_error = None;
        let mut usage = None;
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
                        cached_input_tokens,
                        output_tokens,
                        reasoning_tokens,
                        total_tokens,
                        context_input_tokens,
                        context_window_tokens,
                    } if usage_thread == thread_id && usage_turn == &turn_id => {
                        usage = Some(ManagedTokenUsage {
                            input_tokens: *input_tokens,
                            cached_input_tokens: *cached_input_tokens,
                            output_tokens: *output_tokens,
                            reasoning_tokens: *reasoning_tokens,
                            total_tokens: *total_tokens,
                            context_input_tokens: *context_input_tokens,
                            context_window_tokens: *context_window_tokens,
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
                return Err(self.retire(CodexError::Timeout("turn completion")));
            }
        };
        let interruption_requested = match completion {
            Ok(interruption_requested) => interruption_requested,
            Err(error) => {
                return Err(self.retire(error));
            }
        };
        let status = completed_status.unwrap_or_else(|| "unknown".into());
        if let Some(reservation) = reservation {
            reservation
                .settle(crate::usage_budget::Receipt {
                    cumulative: usage
                        .as_ref()
                        .map(|usage| crate::usage_budget::CumulativeUsage {
                            counter: thread_id.to_owned(),
                            total_tokens: usage.total_tokens,
                        }),
                    total_tokens: None, // tokenUsage.total includes earlier turns, including before restart.
                    reported_cost_microunits: None,
                    outcome: if status == "completed" {
                        crate::usage_budget::Outcome::Completed
                    } else if interruption_requested || status == "interrupted" {
                        crate::usage_budget::Outcome::Interrupted
                    } else {
                        crate::usage_budget::Outcome::Failed
                    },
                })
                .map_err(|error| CodexError::Protocol(format!("usage settlement: {error}")))?;
        }
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
        self.ensure_usable()?;
        match timeout(REQUEST_TIMEOUT, self.peer.request(method, params, handler)).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(self.failed_request(error)),
            Err(_) => Err(self.retire(CodexError::Timeout(method))),
        }
    }

    fn failed_request(&mut self, error: CodexError) -> CodexError {
        if self.peer.awaiting_response {
            self.retire(error)
        } else {
            error
        }
    }

    fn retire(&mut self, error: CodexError) -> CodexError {
        self.protocol_usable = false;
        match self.child.start_kill() {
            Ok(()) => error,
            Err(stop_error) => CodexError::Io(format!(
                "{error}; could not stop the failed app-server: {stop_error}"
            )),
        }
    }

    fn ensure_usable(&self) -> Result<(), CodexError> {
        if !self.protocol_usable {
            return Err(CodexError::Protocol(
                "app-server connection is unavailable after a prior protocol or policy failure; restart the managed connection".into(),
            ));
        }
        Ok(())
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
