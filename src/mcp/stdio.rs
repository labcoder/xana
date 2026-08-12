//! Owned, bounded MCP stdio process transport.

use super::{
    McpDiscoverResult, McpRequestId, ProtocolError, decode_discover_response, decode_notification,
    encode_notification, encode_request, negotiate,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    ffi::{OsStr, OsString},
    fmt, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_ENVIRONMENT_ENTRIES: usize = 64;
const MAX_ENVIRONMENT_KEY_BYTES: usize = 128;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 16 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_OUTSTANDING_REQUESTS: usize = 32;
const COMMAND_QUEUE_CAPACITY: usize = 64;
const FRAME_QUEUE_CAPACITY: usize = 32;
const ACTIVITY_QUEUE_CAPACITY: usize = 64;
const MAX_RETIRED_REQUESTS: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct McpArgument {
    value: OsString,
    sensitive: bool,
}

impl McpArgument {
    pub(crate) fn visible(value: impl Into<OsString>) -> Self {
        Self {
            value: value.into(),
            sensitive: false,
        }
    }

    pub(crate) fn sensitive(value: impl Into<OsString>) -> Self {
        Self {
            value: value.into(),
            sensitive: true,
        }
    }

    fn value(&self) -> &OsStr {
        &self.value
    }
}

impl fmt::Debug for McpArgument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.sensitive {
            formatter.write_str("McpArgument(<redacted>)")
        } else {
            formatter
                .debug_tuple("McpArgument")
                .field(&self.value)
                .finish()
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct McpEnvironmentValue(String);

impl McpEnvironmentValue {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for McpEnvironmentValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpEnvironmentValue(<redacted>)")
    }
}

impl Drop for McpEnvironmentValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct McpProcessConfig {
    pub(crate) program: PathBuf,
    pub(crate) arguments: Vec<McpArgument>,
    pub(crate) working_directory: PathBuf,
    pub(crate) environment: BTreeMap<String, McpEnvironmentValue>,
    pub(crate) request_timeout: Duration,
    pub(crate) shutdown_grace: Duration,
    pub(crate) max_outstanding_requests: usize,
}

impl McpProcessConfig {
    pub(crate) fn new(program: impl Into<PathBuf>, working_directory: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            working_directory: working_directory.into(),
            environment: BTreeMap::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            max_outstanding_requests: MAX_OUTSTANDING_REQUESTS,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), McpStdioError> {
        if self.program.as_os_str().is_empty() {
            return Err(McpStdioError::InvalidConfig("program cannot be empty"));
        }
        if !self.working_directory.is_absolute() {
            return Err(McpStdioError::InvalidConfig(
                "working directory must be absolute",
            ));
        }
        if self.arguments.len() > MAX_ARGUMENTS
            || self
                .arguments
                .iter()
                .any(|argument| os_len(argument.value()) > MAX_ARGUMENT_BYTES)
        {
            return Err(McpStdioError::InvalidConfig("argument limit exceeded"));
        }
        if self.environment.len() > MAX_ENVIRONMENT_ENTRIES {
            return Err(McpStdioError::InvalidConfig(
                "environment entry limit exceeded",
            ));
        }
        for (key, value) in &self.environment {
            if key.is_empty()
                || key.len() > MAX_ENVIRONMENT_KEY_BYTES
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || value.expose().len() > MAX_ENVIRONMENT_VALUE_BYTES
            {
                return Err(McpStdioError::InvalidConfig("environment entry is invalid"));
            }
        }
        if self.request_timeout.is_zero() || self.shutdown_grace.is_zero() {
            return Err(McpStdioError::InvalidConfig("timeouts must be positive"));
        }
        if self.max_outstanding_requests == 0
            || self.max_outstanding_requests > MAX_OUTSTANDING_REQUESTS
        {
            return Err(McpStdioError::InvalidConfig(
                "outstanding request limit is invalid",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for McpProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arguments = self
            .arguments
            .iter()
            .map(|argument| {
                if argument.sensitive {
                    OsString::from("<redacted>")
                } else {
                    argument.value.clone()
                }
            })
            .collect::<Vec<_>>();
        formatter
            .debug_struct("McpProcessConfig")
            .field("program", &self.program)
            .field("arguments", &arguments)
            .field("working_directory", &self.working_directory)
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .field("request_timeout", &self.request_timeout)
            .field("shutdown_grace", &self.shutdown_grace)
            .field("max_outstanding_requests", &self.max_outstanding_requests)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpProcessPhase {
    Starting,
    Discovering,
    Ready,
    Degraded,
    Stopping,
    Stopped,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpProcessHealth {
    pub(crate) phase: McpProcessPhase,
    pub(crate) process_id: Option<u32>,
    pub(crate) outstanding_requests: usize,
    pub(crate) stderr_bytes: usize,
    pub(crate) stderr_truncated: bool,
    pub(crate) last_failure: Option<McpFailureKind>,
}

impl McpProcessHealth {
    fn starting(process_id: Option<u32>) -> Self {
        Self {
            phase: McpProcessPhase::Starting,
            process_id,
            outstanding_requests: 0,
            stderr_bytes: 0,
            stderr_truncated: false,
            last_failure: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpFailureKind {
    Protocol,
    Input,
    Output,
    Timeout,
    Process,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum McpProcessActivity {
    Spawned { process_id: Option<u32> },
    DiscoveryStarted,
    Ready,
    RequestStarted { request_id: u64, method: String },
    Progress { progress: f64, total: Option<f64> },
    RequestCompleted { request_id: u64 },
    RequestCancelled { request_id: u64 },
    StderrDrained { bytes: usize, truncated: bool },
    Failed { kind: McpFailureKind },
    Stopped { forced: bool, success: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct McpStopReport {
    pub(crate) forced: bool,
    pub(crate) success: bool,
}

#[derive(Clone)]
pub(crate) struct McpStdioClient {
    inner: Arc<ClientInner>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpRawResponse {
    pub(crate) request_id: McpRequestId,
    pub(crate) bytes: Vec<u8>,
}

struct ClientInner {
    commands: mpsc::Sender<ActorCommand>,
    health: watch::Receiver<McpProcessHealth>,
    activity: broadcast::Sender<McpProcessActivity>,
    next_request_id: AtomicU64,
    shutdown: CancellationToken,
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl McpStdioClient {
    pub(crate) async fn spawn(config: McpProcessConfig) -> Result<Self, McpStdioError> {
        config.validate()?;
        let mut command = Command::new(&config.program);
        command
            .args(config.arguments.iter().map(McpArgument::value))
            .current_dir(&config.working_directory)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        add_minimal_environment(&mut command);
        for (key, value) in &config.environment {
            command.env(key, value.expose());
        }
        configure_process_group(&mut command);

        let mut child = command
            .spawn()
            .map_err(|error| McpStdioError::Spawn(error.kind()))?;
        let process_id = child.id();
        let stdin = child.stdin.take().ok_or(McpStdioError::PipeUnavailable)?;
        let stdout = child.stdout.take().ok_or(McpStdioError::PipeUnavailable)?;
        let stderr = child.stderr.take().ok_or(McpStdioError::PipeUnavailable)?;

        let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let (wire_tx, wire_rx) = mpsc::channel(FRAME_QUEUE_CAPACITY);
        let (health_tx, health) = watch::channel(McpProcessHealth::starting(process_id));
        let (activity, _) = broadcast::channel(ACTIVITY_QUEUE_CAPACITY);
        let shutdown = CancellationToken::new();
        let stdout_task = tokio::spawn(read_stdout(stdout, wire_tx.clone()));
        let stderr_task = tokio::spawn(read_stderr(stderr, wire_tx));
        let actor_activity = activity.clone();
        let actor_shutdown = shutdown.clone();
        tokio::spawn(run_actor(
            child,
            stdin,
            command_rx,
            wire_rx,
            health_tx,
            actor_activity,
            actor_shutdown,
            config,
            stdout_task,
            stderr_task,
        ));
        let _ = activity.send(McpProcessActivity::Spawned { process_id });
        Ok(Self {
            inner: Arc::new(ClientInner {
                commands,
                health,
                activity,
                next_request_id: AtomicU64::new(1),
                shutdown,
            }),
        })
    }

    pub(crate) fn health(&self) -> McpProcessHealth {
        self.inner.health.borrow().clone()
    }

    pub(crate) fn subscribe_activity(&self) -> broadcast::Receiver<McpProcessActivity> {
        self.inner.activity.subscribe()
    }

    pub(crate) async fn discover(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<McpDiscoverResult, McpStdioError> {
        let bytes = self
            .request_raw("server/discover", serde_json::json!({}), cancellation)
            .await?;
        let id = response_id(&bytes)?;
        let result = decode_discover_response(&bytes, McpRequestId::new(id)?)?;
        negotiate(&result)?;
        let (ready_tx, ready_rx) = oneshot::channel();
        self.inner
            .commands
            .send(ActorCommand::MarkReady { result: ready_tx })
            .await
            .map_err(|_| McpStdioError::Closed)?;
        ready_rx.await.map_err(|_| McpStdioError::Closed)?;
        Ok(result)
    }

    pub(crate) async fn request_raw(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, McpStdioError> {
        Ok(self
            .request_with_id(method, params, cancellation)
            .await?
            .bytes)
    }

    pub(crate) async fn request_with_id(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
    ) -> Result<McpRawResponse, McpStdioError> {
        let request_id = self.inner.next_request_id.fetch_add(1, Ordering::Relaxed);
        let id = McpRequestId::new(request_id)?;
        let encoded = encode_request(id, method, params)?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(McpStdioError::FrameTooLarge);
        }
        let (result_tx, result_rx) = oneshot::channel();
        let command = ActorCommand::Request {
            request_id,
            method: method.to_owned(),
            encoded,
            cancellation: cancellation.clone(),
            result: result_tx,
        };
        self.inner
            .commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => McpStdioError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => McpStdioError::Closed,
            })?;
        let bytes = result_rx.await.map_err(|_| McpStdioError::Closed)??;
        Ok(McpRawResponse {
            request_id: id,
            bytes,
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<McpStopReport, McpStdioError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.inner
            .commands
            .send(ActorCommand::Shutdown { result: result_tx })
            .await
            .map_err(|_| McpStdioError::Closed)?;
        result_rx.await.map_err(|_| McpStdioError::Closed)
    }
}

enum ActorCommand {
    Request {
        request_id: u64,
        method: String,
        encoded: Vec<u8>,
        cancellation: CancellationToken,
        result: oneshot::Sender<Result<Vec<u8>, McpStdioError>>,
    },
    MarkReady {
        result: oneshot::Sender<()>,
    },
    Shutdown {
        result: oneshot::Sender<McpStopReport>,
    },
}

enum WireEvent {
    Frame(Vec<u8>),
    StdoutFailure(io::ErrorKind),
    StdoutClosed,
    StderrDrained { bytes: usize, truncated: bool },
}

struct PendingRequest {
    deadline: Instant,
    cancellation: CancellationToken,
    result: oneshot::Sender<Result<Vec<u8>, McpStdioError>>,
}

#[allow(clippy::too_many_arguments)]
async fn run_actor(
    mut child: Child,
    stdin: ChildStdin,
    mut commands: mpsc::Receiver<ActorCommand>,
    mut wire: mpsc::Receiver<WireEvent>,
    health: watch::Sender<McpProcessHealth>,
    activity: broadcast::Sender<McpProcessActivity>,
    shutdown: CancellationToken,
    config: McpProcessConfig,
    stdout_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
) {
    let mut writer = BufWriter::new(stdin);
    let mut pending = BTreeMap::<u64, PendingRequest>::new();
    let mut retired = VecDeque::<u64>::new();
    let mut tick = interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut requested_stop = false;
    let mut stop_sender = None;
    let mut failure = None;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                requested_stop = true;
                break;
            }
            Some(command) = commands.recv() => {
                match command {
                    ActorCommand::Request { request_id, method, encoded, cancellation, result } => {
                        if cancellation.is_cancelled() {
                            let _ = result.send(Err(McpStdioError::Cancelled));
                            continue;
                        }
                        if pending.len() >= config.max_outstanding_requests {
                            let _ = result.send(Err(McpStdioError::OutstandingLimit));
                            continue;
                        }
                        if method == "server/discover" {
                            update_health(&health, |current| current.phase = McpProcessPhase::Discovering);
                            let _ = activity.send(McpProcessActivity::DiscoveryStarted);
                        }
                        if write_frame(&mut writer, &encoded).await.is_err() {
                            let _ = result.send(Err(McpStdioError::Output(io::ErrorKind::BrokenPipe)));
                            failure = Some(McpFailureKind::Output);
                            break;
                        }
                        let _ = activity.send(McpProcessActivity::RequestStarted {
                            request_id,
                            method,
                        });
                        pending.insert(request_id, PendingRequest {
                            deadline: Instant::now() + config.request_timeout,
                            cancellation,
                            result,
                        });
                        update_health(&health, |current| current.outstanding_requests = pending.len());
                    }
                    ActorCommand::MarkReady { result } => {
                        update_health(&health, |current| {
                            current.phase = McpProcessPhase::Ready;
                            current.last_failure = None;
                        });
                        let _ = activity.send(McpProcessActivity::Ready);
                        let _ = result.send(());
                    }
                    ActorCommand::Shutdown { result } => {
                        requested_stop = true;
                        stop_sender = Some(result);
                        break;
                    }
                }
            }
            Some(event) = wire.recv() => {
                match event {
                    WireEvent::Frame(frame) => {
                        if let Err(kind) = handle_frame(
                            frame,
                            &mut pending,
                            &mut retired,
                            &activity,
                        ) {
                            failure = Some(kind);
                            break;
                        }
                        update_health(&health, |current| current.outstanding_requests = pending.len());
                    }
                    WireEvent::StdoutFailure(kind) => {
                        fail_pending(&mut pending, McpStdioError::Input(kind));
                        failure = Some(McpFailureKind::Input);
                        break;
                    }
                    WireEvent::StdoutClosed => {
                        failure = Some(McpFailureKind::Process);
                        break;
                    }
                    WireEvent::StderrDrained { bytes, truncated } => {
                        update_health(&health, |current| {
                            current.stderr_bytes = bytes;
                            current.stderr_truncated = truncated;
                        });
                        let _ = activity.send(McpProcessActivity::StderrDrained { bytes, truncated });
                    }
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let cancelled = pending
                    .iter()
                    .filter(|(_, request)| request.cancellation.is_cancelled())
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                for id in cancelled {
                    if send_cancellation(&mut writer, id).await.is_err() {
                        failure = Some(McpFailureKind::Output);
                        break;
                    }
                    if let Some(request) = pending.remove(&id) {
                        let _ = request.result.send(Err(McpStdioError::Cancelled));
                        retire(id, &mut retired);
                        let _ = activity.send(McpProcessActivity::RequestCancelled { request_id: id });
                    }
                }
                if failure.is_some() {
                    break;
                }
                let expired = pending
                    .iter()
                    .filter(|(_, request)| request.deadline <= now)
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                for id in expired {
                    if let Some(request) = pending.remove(&id) {
                        let _ = request.result.send(Err(McpStdioError::Timeout));
                        retire(id, &mut retired);
                    }
                }
                update_health(&health, |current| current.outstanding_requests = pending.len());
                match child.try_wait() {
                    Ok(Some(_)) => {
                        failure = Some(McpFailureKind::Process);
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        failure = Some(McpFailureKind::Process);
                        break;
                    }
                }
            }
            else => {
                failure = Some(McpFailureKind::Process);
                break;
            }
        }
    }

    if let Some(kind) = failure {
        update_health(&health, |current| {
            current.phase = McpProcessPhase::Crashed;
            current.last_failure = Some(kind);
        });
        let _ = activity.send(McpProcessActivity::Failed { kind });
        fail_pending(&mut pending, McpStdioError::ProcessExited);
    } else {
        update_health(&health, |current| current.phase = McpProcessPhase::Stopping);
        fail_pending(&mut pending, McpStdioError::Cancelled);
    }
    drop(writer);
    let report = stop_child(&mut child, config.shutdown_grace).await;
    stdout_task.abort();
    stderr_task.abort();
    update_health(&health, |current| {
        current.phase = if failure.is_some() && !requested_stop {
            McpProcessPhase::Crashed
        } else {
            McpProcessPhase::Stopped
        };
        current.outstanding_requests = 0;
    });
    let _ = activity.send(McpProcessActivity::Stopped {
        forced: report.forced,
        success: report.success,
    });
    if let Some(sender) = stop_sender {
        let _ = sender.send(report);
    }
}

fn handle_frame(
    frame: Vec<u8>,
    pending: &mut BTreeMap<u64, PendingRequest>,
    retired: &mut VecDeque<u64>,
    activity: &broadcast::Sender<McpProcessActivity>,
) -> Result<(), McpFailureKind> {
    let value: Value = serde_json::from_slice(&frame).map_err(|_| McpFailureKind::Protocol)?;
    let object = value.as_object().ok_or(McpFailureKind::Protocol)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpFailureKind::Protocol);
    }
    if let Some(id) = object.get("id") {
        let id = id.as_u64().ok_or(McpFailureKind::Protocol)?;
        if matches!(
            (object.contains_key("result"), object.contains_key("error")),
            (true, true) | (false, false)
        ) {
            return Err(McpFailureKind::Protocol);
        }
        if let Some(request) = pending.remove(&id) {
            let _ = request.result.send(Ok(frame));
            let _ = activity.send(McpProcessActivity::RequestCompleted { request_id: id });
            return Ok(());
        }
        if retired.contains(&id) {
            return Ok(());
        }
        return Err(McpFailureKind::Protocol);
    }
    match decode_notification(&frame).map_err(|_| McpFailureKind::Protocol)? {
        super::McpNotification::Progress {
            progress, total, ..
        } => {
            let _ = activity.send(McpProcessActivity::Progress { progress, total });
        }
        super::McpNotification::Cancelled { request_id, .. } => {
            if let Ok(id) = request_id.parse::<u64>()
                && let Some(request) = pending.remove(&id)
            {
                let _ = request.result.send(Err(McpStdioError::Cancelled));
                retire(id, retired);
            }
        }
        super::McpNotification::ToolsChanged
        | super::McpNotification::ResourcesChanged
        | super::McpNotification::PromptsChanged => {}
    }
    Ok(())
}

fn fail_pending(pending: &mut BTreeMap<u64, PendingRequest>, error: McpStdioError) {
    for (_, request) in std::mem::take(pending) {
        let _ = request.result.send(Err(error.clone()));
    }
}

fn retire(id: u64, retired: &mut VecDeque<u64>) {
    retired.push_back(id);
    if retired.len() > MAX_RETIRED_REQUESTS {
        retired.pop_front();
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &[u8],
) -> Result<(), McpStdioError> {
    let write = async {
        writer.write_all(frame).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    };
    tokio::time::timeout(WRITE_TIMEOUT, write)
        .await
        .map_err(|_| McpStdioError::Timeout)?
        .map_err(|error| McpStdioError::Output(error.kind()))
}

async fn send_cancellation<W: AsyncWrite + Unpin>(
    writer: &mut W,
    request_id: u64,
) -> Result<(), McpStdioError> {
    let frame = encode_notification(
        "notifications/cancelled",
        serde_json::json!({"requestId": request_id, "reason": "cancelled by user"}),
    )?;
    write_frame(writer, &frame).await
}

async fn read_stdout(stdout: ChildStdout, events: mpsc::Sender<WireEvent>) {
    let mut reader = JsonLineReader::new(BufReader::new(stdout));
    loop {
        match reader.read_frame().await {
            Ok(Some(frame)) => {
                if events.send(WireEvent::Frame(frame)).await.is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = events.send(WireEvent::StdoutClosed).await;
                return;
            }
            Err(error) => {
                let _ = events.send(WireEvent::StdoutFailure(error.kind())).await;
                return;
            }
        }
    }
}

async fn read_stderr(mut stderr: ChildStderr, events: mpsc::Sender<WireEvent>) {
    let mut retained = 0_usize;
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => {
                let available = MAX_STDERR_BYTES.saturating_sub(retained);
                retained = retained.saturating_add(count.min(available));
                truncated |= count > available;
                let _ = events.try_send(WireEvent::StderrDrained {
                    bytes: retained,
                    truncated,
                });
            }
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    let _ = events
        .send(WireEvent::StderrDrained {
            bytes: retained,
            truncated,
        })
        .await;
}

struct JsonLineReader<R> {
    reader: R,
    buffered: Vec<u8>,
}

impl<R: AsyncRead + Unpin> JsonLineReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buffered: Vec::with_capacity(8192),
        }
    }

    async fn read_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(position) = self.buffered.iter().position(|byte| *byte == b'\n') {
                let mut frame = self.buffered.drain(..=position).collect::<Vec<_>>();
                frame.pop();
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                if frame.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "empty MCP frame",
                    ));
                }
                return Ok(Some(frame));
            }
            if self.buffered.len() > MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP frame exceeds limit",
                ));
            }
            let mut chunk = [0_u8; 8192];
            let count = self.reader.read(&mut chunk).await?;
            if count == 0 {
                if self.buffered.is_empty() {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial MCP frame",
                ));
            }
            if self.buffered.len().saturating_add(count) > MAX_FRAME_BYTES + 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP frame exceeds limit",
                ));
            }
            self.buffered.extend_from_slice(&chunk[..count]);
        }
    }
}

async fn stop_child(child: &mut Child, grace: Duration) -> McpStopReport {
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => McpStopReport {
            forced: false,
            success: status.success(),
        },
        Ok(Err(_)) => McpStopReport {
            forced: false,
            success: false,
        },
        Err(_) => {
            kill_process_tree(child).await;
            let status = child.wait().await.ok();
            McpStopReport {
                forced: true,
                success: status.is_some(),
            }
        }
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
async fn kill_process_tree(child: &mut Child) {
    if let Some(id) = child.id() {
        let _ = Command::new("kill")
            .args([OsString::from("-KILL"), OsString::from(format!("-{id}"))])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
    }
    let _ = child.kill().await;
}

#[cfg(windows)]
async fn kill_process_tree(child: &mut Child) {
    if let Some(id) = child.id() {
        let _ = Command::new("taskkill.exe")
            .args(["/PID", &id.to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
    }
    let _ = child.kill().await;
}

#[cfg(not(any(unix, windows)))]
async fn kill_process_tree(child: &mut Child) {
    let _ = child.kill().await;
}

fn add_minimal_environment(command: &mut Command) {
    #[cfg(windows)]
    const KEYS: &[&str] = &[
        "PATH",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
        "TEMP",
        "TMP",
    ];
    #[cfg(not(windows))]
    const KEYS: &[&str] = &["PATH", "LANG", "LC_ALL", "TMPDIR"];

    for key in KEYS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn response_id(bytes: &[u8]) -> Result<u64, McpStdioError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| McpStdioError::ProtocolShape)?;
    value
        .get("id")
        .and_then(Value::as_u64)
        .ok_or(McpStdioError::ProtocolShape)
}

fn update_health(
    sender: &watch::Sender<McpProcessHealth>,
    update: impl FnOnce(&mut McpProcessHealth),
) {
    let mut next = sender.borrow().clone();
    update(&mut next);
    sender.send_replace(next);
}

fn os_len(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpStdioError {
    InvalidConfig(&'static str),
    Spawn(io::ErrorKind),
    PipeUnavailable,
    QueueFull,
    OutstandingLimit,
    FrameTooLarge,
    Timeout,
    Cancelled,
    Closed,
    Input(io::ErrorKind),
    Output(io::ErrorKind),
    ProcessExited,
    ProtocolShape,
    Protocol(ProtocolError),
}

impl From<ProtocolError> for McpStdioError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl fmt::Display for McpStdioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => {
                write!(formatter, "invalid MCP process config: {reason}")
            }
            Self::Spawn(kind) => write!(formatter, "could not start MCP process ({kind:?})"),
            Self::PipeUnavailable => formatter.write_str("MCP process pipe is unavailable"),
            Self::QueueFull => formatter.write_str("MCP process command queue is full"),
            Self::OutstandingLimit => {
                formatter.write_str("MCP process outstanding request limit reached")
            }
            Self::FrameTooLarge => formatter.write_str("MCP process frame exceeds its limit"),
            Self::Timeout => formatter.write_str("MCP process request timed out"),
            Self::Cancelled => formatter.write_str("MCP process request was cancelled"),
            Self::Closed => formatter.write_str("MCP process transport is closed"),
            Self::Input(kind) => write!(formatter, "MCP process input failed ({kind:?})"),
            Self::Output(kind) => write!(formatter, "MCP process output failed ({kind:?})"),
            Self::ProcessExited => formatter.write_str("MCP process exited"),
            Self::ProtocolShape => formatter.write_str("MCP process returned an invalid response"),
            Self::Protocol(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for McpStdioError {}

#[cfg(test)]
mod tests;
