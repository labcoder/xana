//! Codex app-server adapter using the vendor-owned JSONL protocol.

use crate::model::{DescriptorSource, ModelDescriptor};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
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

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const TURN_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

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
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ManagedNotification {
    TextDelta(String),
    ItemStarted {
        item_id: String,
        kind: String,
        summary: String,
    },
    TurnCompleted {
        turn_id: String,
        status: String,
    },
    Warning(String),
    Other {
        method: String,
        params: Value,
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
    fn approve(&mut self, request: ApprovalRequest) -> Result<ApprovalDecision, CodexError>;
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
        }
    }
}

impl Error for CodexError {}

struct JsonLinePeer<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
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
        let mut line = Vec::new();
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
            if line.len().saturating_add(end) > MAX_FRAME_BYTES {
                return Err(CodexError::FrameTooLarge);
            }
            line.extend_from_slice(&available[..end]);
            let complete = available.get(end.saturating_sub(1)) == Some(&b'\n');
            self.reader.consume(end);
            if complete {
                break;
            }
        }
        serde_json::from_slice(&line).map_err(|error| CodexError::Protocol(error.to_string()))
    }

    async fn request<H: ManagedEventHandler>(
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

    async fn handle_incoming<H: ManagedEventHandler>(
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
            let result = approval_response(&method, &params, handler)?;
            self.send(&json!({"id": id, "result": result})).await
        } else {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            handler.notification(normalize_notification(method, params)?)
        }
    }

    async fn wait_for<H: ManagedEventHandler>(
        &mut self,
        mut complete: impl FnMut(&ManagedNotification) -> bool,
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
            let done = complete(&notification);
            handler.notification(notification)?;
            if done {
                return Ok(());
            }
        }
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

fn approval_response<H: ManagedEventHandler>(
    method: &str,
    params: &Value,
    handler: &mut H,
) -> Result<Value, CodexError> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let decision = handler.approve(ApprovalRequest {
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
                            .filter_map(Value::as_str)
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
                    .map(str::to_owned),
                command: params
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                cwd: params.get("cwd").and_then(Value::as_str).map(str::to_owned),
            })?;
            Ok(json!({"decision": decision.wire()}))
        }
        _ => Err(CodexError::UnsupportedServerRequest(method.to_owned())),
    }
}

fn normalize_notification(
    method: String,
    params: Value,
) -> Result<ManagedNotification, CodexError> {
    Ok(match method.as_str() {
        "item/agentMessage/delta" => ManagedNotification::TextDelta(
            params
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        "item/started" => {
            let item = params
                .get("item")
                .ok_or_else(|| CodexError::Protocol("item/started omitted item".into()))?;
            ManagedNotification::ItemStarted {
                item_id: required_string(item, "id")?,
                kind: required_string(item, "type")?,
                summary: bounded_json_summary(item),
            }
        }
        "turn/completed" => ManagedNotification::TurnCompleted {
            turn_id: params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status: params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        },
        "warning" | "error" => ManagedNotification::Warning(bounded_text(
            params
                .get("message")
                .or_else(|| params.pointer("/error/message"))
                .and_then(Value::as_str)
                .unwrap_or("Codex reported a warning"),
            4096,
        )),
        _ => ManagedNotification::Other { method, params },
    })
}

pub(crate) struct CodexAppServer {
    child: Child,
    peer: JsonLinePeer<BufReader<ChildStdout>, BufWriter<ChildStdin>>,
    pub(crate) version: String,
    pub(crate) codex_home: PathBuf,
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
        timeout(
            LOGIN_TIMEOUT,
            self.peer.wait_for(
                |notification| {
                    let ManagedNotification::Other { method, params } = notification else {
                        return false;
                    };
                    if method != "account/login/completed"
                        || params.get("loginId").and_then(Value::as_str) != Some(login_id)
                    {
                        return false;
                    }
                    success = Some((
                        params
                            .get("success")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        params
                            .get("error")
                            .and_then(Value::as_str)
                            .map(|error| bounded_text(error, 4096)),
                    ));
                    true
                },
                &mut handler,
            ),
        )
        .await
        .map_err(|_| CodexError::Timeout("login completion"))??;
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
        loop {
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
                let id = required_string(value, "id")?;
                let input_modalities = value
                    .get("inputModalities")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|value| matches!(*value, "text" | "image"))
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                models.push(ModelDescriptor {
                    display_name: value
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or(&id)
                        .to_owned(),
                    id,
                    input_modalities,
                    tools: Some(true),
                    reasoning: Some(
                        value
                            .get("supportedReasoningEfforts")
                            .and_then(Value::as_array)
                            .is_some_and(|values| !values.is_empty()),
                    ),
                    context_tokens: None,
                    max_output_tokens: None,
                    source: DescriptorSource::ManagedRuntime,
                    is_default: value
                        .get("isDefault")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        models.sort_by(|left, right| left.id.cmp(&right.id));
        models.dedup_by(|left, right| left.id == right.id);
        Ok(models)
    }

    pub(crate) async fn run_turn<H: ManagedEventHandler>(
        &mut self,
        model: &str,
        workspace: &Path,
        thread_id: Option<&str>,
        input: ManagedTurnInput,
        handler: &mut H,
    ) -> Result<ManagedTurnResult, CodexError> {
        let thread_id = match thread_id {
            Some(id) => id.to_owned(),
            None => {
                let result = self
                    .request(
                        "thread/start",
                        json!({
                            "model": model,
                            "cwd": workspace,
                            "approvalPolicy": "on-request",
                            "sandbox": "workspace-write",
                            "ephemeral": false
                        }),
                        handler,
                    )
                    .await?;
                result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| CodexError::Protocol("thread/start omitted thread id".into()))?
                    .to_owned()
            }
        };
        let mut user_input = vec![json!({"type": "text", "text": input.text})];
        user_input.extend(
            input
                .local_images
                .into_iter()
                .map(|path| json!({"type": "localImage", "path": path})),
        );
        let result = self
            .request(
                "turn/start",
                json!({"threadId": thread_id, "input": user_input, "model": model}),
                handler,
            )
            .await?;
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| CodexError::Protocol("turn/start omitted turn id".into()))?
            .to_owned();
        let mut final_text = String::new();
        let mut completed_status = None;
        timeout(
            TURN_TIMEOUT,
            self.peer.wait_for(
                |notification| match notification {
                    ManagedNotification::TextDelta(delta) => {
                        final_text.push_str(delta);
                        false
                    }
                    ManagedNotification::TurnCompleted {
                        turn_id: completed,
                        status,
                    } if completed == &turn_id => {
                        completed_status = Some(status.clone());
                        true
                    }
                    _ => false,
                },
                handler,
            ),
        )
        .await
        .map_err(|_| CodexError::Timeout("turn completion"))??;
        let status = completed_status.unwrap_or_else(|| "unknown".into());
        if status != "completed" {
            return Err(CodexError::Remote {
                code: None,
                message: format!("turn {turn_id} ended with status {status}"),
            });
        }
        Ok(ManagedTurnResult {
            thread_id,
            turn_id,
            final_text,
        })
    }

    async fn request<H: ManagedEventHandler>(
        &mut self,
        method: &'static str,
        params: Value,
        handler: &mut H,
    ) -> Result<Value, CodexError> {
        timeout(REQUEST_TIMEOUT, self.peer.request(method, params, handler))
            .await
            .map_err(|_| CodexError::Timeout(method))?
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
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = &config.home {
        command.env("CODEX_HOME", home);
    }
    let output = timeout(STARTUP_TIMEOUT, command.output())
        .await
        .map_err(|_| CodexError::Timeout("version probe"))?
        .map_err(|error| CodexError::Spawn(error.to_string()))?;
    if !output.status.success() {
        return Err(CodexError::Spawn(format!(
            "version probe exited with {}",
            output.status
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
    const MAX_SUMMARY_BYTES: usize = 16 * 1024;
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| "<unavailable>".into());
    if rendered.len() <= MAX_SUMMARY_BYTES {
        return rendered;
    }
    let mut end = MAX_SUMMARY_BYTES;
    while !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… [proposal truncated]", &rendered[..end])
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
    fn approve(&mut self, _: ApprovalRequest) -> Result<ApprovalDecision, CodexError> {
        Ok(ApprovalDecision::Decline)
    }
}

#[derive(Default)]
struct CapturingHandler;
impl ManagedEventHandler for CapturingHandler {
    fn notification(&mut self, _: ManagedNotification) -> Result<(), CodexError> {
        Ok(())
    }
    fn approve(&mut self, _: ApprovalRequest) -> Result<ApprovalDecision, CodexError> {
        Ok(ApprovalDecision::Decline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{BufReader, duplex, sink, split};

    #[derive(Default)]
    struct TestHandler {
        notifications: Vec<ManagedNotification>,
        approvals: usize,
    }
    impl ManagedEventHandler for TestHandler {
        fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError> {
            self.notifications.push(notification);
            Ok(())
        }
        fn approve(&mut self, _: ApprovalRequest) -> Result<ApprovalDecision, CodexError> {
            self.approvals += 1;
            Ok(ApprovalDecision::AcceptOnce)
        }
    }

    #[tokio::test]
    async fn fake_jsonl_child_maps_notification_and_approval() {
        let (client, server) = duplex(16 * 1024);
        let (client_read, client_write) = split(client);
        let (server_read, mut server_write) = split(server);
        let server_task = tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let id = request["id"].clone();
            server_write
                .write_all(
                    b"{\"method\":\"item/agentMessage/delta\",\"params\":{\"delta\":\"hi\"}}\n",
                )
                .await
                .unwrap();
            server_write.write_all(b"{\"method\":\"item/commandExecution/requestApproval\",\"id\":99,\"params\":{\"command\":\"echo hi\"}}\n").await.unwrap();
            let approval: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(approval["result"]["decision"], "accept");
            server_write
                .write_all(format!("{{\"id\":{id},\"result\":{{\"ok\":true}}}}\n").as_bytes())
                .await
                .unwrap();
        });
        let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
        let mut handler = TestHandler::default();
        let result = peer.request("test", json!({}), &mut handler).await.unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(handler.approvals, 1);
        assert_eq!(
            handler.notifications,
            vec![ManagedNotification::TextDelta("hi".into())]
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_incoming_frame_fails_before_json_decoding() {
        let bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
        let reader = BufReader::new(bytes.as_slice());
        let mut peer = JsonLinePeer::new(reader, sink());

        assert!(matches!(
            peer.receive().await,
            Err(CodexError::FrameTooLarge)
        ));
    }

    #[tokio::test]
    async fn remote_error_response_is_typed_and_bounded_by_the_frame_reader() {
        let input = br#"{"id":1,"error":{"code":401,"message":"authentication failed"}}
"#;
        let mut peer = JsonLinePeer::new(BufReader::new(input.as_slice()), sink());
        let mut handler = TestHandler::default();

        let error = peer
            .request("account/read", json!({}), &mut handler)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CodexError::Remote {
                code: Some(401),
                ..
            }
        ));
    }

    #[test]
    fn item_started_keeps_a_bounded_approval_summary() {
        let notification = normalize_notification(
            "item/started".into(),
            json!({
                "item": {
                    "id": "item-1",
                    "type": "fileChange",
                    "changes": [{"path": "src/lib.rs", "kind": "update"}]
                }
            }),
        )
        .unwrap();

        let ManagedNotification::ItemStarted {
            item_id,
            kind,
            summary,
        } = notification
        else {
            panic!("expected item-start notification");
        };
        assert_eq!(item_id, "item-1");
        assert_eq!(kind, "fileChange");
        assert!(summary.contains("src/lib.rs"));
        assert!(summary.len() <= 16 * 1024 + 64);
    }

    #[test]
    fn account_and_error_debug_paths_contain_no_tokens() {
        let error = CodexError::Remote {
            code: Some(401),
            message: "authentication failed".into(),
        };
        assert!(!format!("{error:?}").contains("access_token"));
        assert_eq!(
            AccountStatus::ChatGpt {
                plan: "plus".into()
            },
            AccountStatus::ChatGpt {
                plan: "plus".into()
            }
        );
    }
}
