use super::{
    descriptor::{DescriptorLease, RuntimeDescriptor, discover},
    execution::HostedExecutionHandle,
    hub::ObservationHub,
    protocol::{
        ClientFrame, ClientHello, ClientRole, ControlResult, HostEvent, HostObservation,
        HostSnapshot, HostSnapshotSeed, LOCAL_HOST_PROTOCOL_VERSION, MAX_WIRE_BYTES, ServerFrame,
        command_kind, decode_client_frame, decode_server_frame, encode_frame,
    },
};
use crate::frontend::ClientCommandResult;
use futures::{SinkExt, StreamExt};
use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, accept_hdr_async_with_config, connect_async_with_config,
    tungstenite::{
        Message,
        handshake::server::{ErrorResponse, Request, Response},
        protocol::WebSocketConfig,
    },
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroize;

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const HOST_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const HOST_SHUTDOWN_HARD: Duration = Duration::from_secs(5);
const MAX_CLIENTS: usize = 32;
const MAX_INBOUND_FRAMES_PER_SECOND: usize = 256;
pub(crate) const CONTROLLER_RECONNECT_GRACE: Duration = Duration::from_secs(3);
type ExecutionFactory = Box<dyn FnOnce(ObservationHub) -> HostedExecutionHandle>;

pub(crate) struct ControlledExecution {
    pub(crate) conversation: String,
    pub(crate) frontend: Option<crate::frontend::ClientSnapshot>,
    pub(crate) artifacts: Option<crate::artifact::ArtifactStore>,
    pub(crate) factory: ExecutionFactory,
}

impl ControlledExecution {
    pub(crate) fn new(
        conversation: String,
        frontend: Option<crate::frontend::ClientSnapshot>,
        artifacts: Option<crate::artifact::ArtifactStore>,
        factory: impl FnOnce(ObservationHub) -> HostedExecutionHandle + 'static,
    ) -> Self {
        Self {
            conversation,
            frontend,
            artifacts,
            factory: Box::new(factory),
        }
    }
}

#[derive(Clone)]
struct ClientService {
    endpoint: SocketAddr,
    host_id: Uuid,
    workspace_id: String,
    capability_hash: [u8; 32],
    hub: ObservationHub,
    execution: Option<HostedExecutionHandle>,
    shutdown: CancellationToken,
}

#[derive(Debug)]
pub(crate) enum LocalHostError {
    Invalid(String),
    Io(std::io::Error),
    Transport(String),
    SequenceGap { expected: u64, received: u64 },
    Closed,
}

impl fmt::Display for LocalHostError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => output.write_str(reason),
            Self::Io(error) => write!(output, "local-host I/O failed: {error}"),
            Self::Transport(reason) => write!(output, "local-host transport failed: {reason}"),
            Self::SequenceGap { expected, received } => write!(
                output,
                "local-host observation gap: expected sequence {expected}, received {received}; reconnect for a fresh snapshot"
            ),
            Self::Closed => output.write_str("local-host connection closed"),
        }
    }
}

impl Error for LocalHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) struct LocalHostServer {
    listener: TcpListener,
    endpoint: SocketAddr,
    host_id: Uuid,
    workspace: PathBuf,
    descriptor: DescriptorLease,
    capability_hash: [u8; 32],
    hub: ObservationHub,
    execution: Option<HostedExecutionHandle>,
    shutdown: CancellationToken,
}

impl LocalHostServer {
    #[cfg(test)]
    pub(crate) async fn bind(
        runtime_root: &Path,
        workspace: &Path,
        bind: IpAddr,
        port: u16,
        seed: HostSnapshotSeed,
    ) -> Result<Self, LocalHostError> {
        Self::bind_inner(runtime_root, workspace, bind, port, seed, None).await
    }

    pub(crate) async fn bind_controlled(
        runtime_root: &Path,
        workspace: &Path,
        bind: IpAddr,
        port: u16,
        seed: HostSnapshotSeed,
        controlled: ControlledExecution,
    ) -> Result<Self, LocalHostError> {
        Self::bind_inner(runtime_root, workspace, bind, port, seed, Some(controlled)).await
    }

    async fn bind_inner(
        runtime_root: &Path,
        workspace: &Path,
        bind: IpAddr,
        port: u16,
        seed: HostSnapshotSeed,
        controlled: Option<ControlledExecution>,
    ) -> Result<Self, LocalHostError> {
        if !bind.is_loopback() {
            return Err(LocalHostError::Invalid(
                "Course 1 `xana serve` accepts only loopback bind addresses".into(),
            ));
        }
        let workspace = workspace.canonicalize().map_err(LocalHostError::Io)?;
        if seed.workspace_id != super::protocol::workspace_identity(&workspace) {
            return Err(LocalHostError::Invalid(
                "local-host snapshot belongs to another workspace".into(),
            ));
        }
        let listener = TcpListener::bind(SocketAddr::new(bind, port))
            .await
            .map_err(LocalHostError::Io)?;
        let endpoint = listener.local_addr().map_err(LocalHostError::Io)?;
        let host_id = Uuid::new_v4();
        let mut capability = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let capability_hash = *blake3::hash(capability.as_bytes()).as_bytes();
        let descriptor_value =
            RuntimeDescriptor::new(host_id, workspace.clone(), endpoint, capability.clone());
        let descriptor = DescriptorLease::create(runtime_root, &descriptor_value)
            .map_err(LocalHostError::Invalid)?;
        capability.zeroize();
        drop(descriptor_value);
        let snapshot = controlled.as_ref().map_or_else(
            || HostSnapshot::new(host_id, seed.clone()),
            |controlled| {
                let snapshot = HostSnapshot::new(host_id, seed.clone())
                    .with_controllable_conversation(controlled.conversation.clone());
                controlled
                    .frontend
                    .clone()
                    .map_or(snapshot.clone(), |frontend| {
                        snapshot.with_frontend(frontend)
                    })
            },
        );
        let artifact_access = controlled.as_ref().and_then(|controlled| {
            controlled.artifacts.clone().map(|store| {
                super::artifact_access::ArtifactAccess::new(store, controlled.frontend.as_ref())
            })
        });
        let hub = ObservationHub::with_artifacts(snapshot, artifact_access);
        let execution = controlled.map(|controlled| (controlled.factory)(hub.clone()));
        Ok(Self {
            listener,
            endpoint,
            host_id,
            workspace,
            descriptor,
            capability_hash,
            hub,
            execution,
            shutdown: CancellationToken::new(),
        })
    }

    pub(crate) fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }

    pub(crate) fn descriptor_path(&self) -> &Path {
        self.descriptor.path()
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub(crate) async fn run(self) -> Result<(), LocalHostError> {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => break,
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(LocalHostError::Io)?;
                    if clients.len() >= MAX_CLIENTS {
                        eprintln!("xana serve: rejected client because the {MAX_CLIENTS}-client bound is full");
                        drop(stream);
                        continue;
                    }
                    let service = ClientService {
                        endpoint: self.endpoint,
                        host_id: self.host_id,
                        workspace_id: super::protocol::workspace_identity(&self.workspace),
                        capability_hash: self.capability_hash,
                        hub: self.hub.clone(),
                        execution: self.execution.clone(),
                        shutdown: self.shutdown.child_token(),
                    };
                    clients.spawn(async move {
                        let hub = service.hub.clone();
                        let result = serve_client(stream, service).await;
                        eprintln!(
                            "xana serve: client disconnected ({} remaining)",
                            hub.subscriber_count()
                        );
                        result
                    });
                }
                Some(joined) = clients.join_next(), if !clients.is_empty() => {
                    report_client_result(joined, "while serving");
                }
            }
        }
        self.shutdown.cancel();
        let shutdown_started = Instant::now();
        let graceful_deadline = shutdown_started + HOST_SHUTDOWN_GRACE;
        let hard_deadline = shutdown_started + HOST_SHUTDOWN_HARD;
        if let Some(execution) = &self.execution {
            let graceful = async {
                execution.fail_closed().await;
                execution.shutdown().await;
            };
            if timeout_at(graceful_deadline, graceful).await.is_err() {
                execution.abort();
            }
        }
        while !clients.is_empty() && Instant::now() < graceful_deadline {
            match timeout_at(graceful_deadline, clients.join_next()).await {
                Ok(Some(joined)) => report_client_result(joined, "during shutdown"),
                Ok(None) | Err(_) => break,
            }
        }
        if !clients.is_empty() {
            clients.abort_all();
        }
        while !clients.is_empty() && Instant::now() < hard_deadline {
            match timeout_at(hard_deadline, clients.join_next()).await {
                Ok(Some(joined)) => report_client_result(joined, "during forced shutdown"),
                Ok(None) | Err(_) => break,
            }
        }
        if let Some(execution) = &self.execution
            && timeout_at(hard_deadline, execution.wait_closed())
                .await
                .is_err()
        {
            return Err(LocalHostError::Transport(
                "host-owned execution did not confirm cleanup within five seconds".into(),
            ));
        }
        Ok(())
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the tungstenite handshake callback requires its concrete HTTP error response"
)]
async fn serve_client(stream: TcpStream, service: ClientService) -> Result<(), LocalHostError> {
    let ClientService {
        endpoint,
        host_id,
        workspace_id,
        capability_hash,
        hub,
        execution,
        shutdown,
    } = service;
    let socket = accept_hdr_async_with_config(
        stream,
        move |request: &Request, response: Response| {
            validate_origin(request, endpoint.port()).map(|()| response)
        },
        Some(websocket_config()),
    )
    .await
    .map_err(|error| LocalHostError::Transport(error.to_string()))?;
    let (mut writer, mut reader) = socket.split();
    let first = tokio::time::timeout(AUTH_TIMEOUT, reader.next())
        .await
        .map_err(|_| LocalHostError::Invalid("local-host authentication timed out".into()))?
        .ok_or(LocalHostError::Closed)?
        .map_err(|error| LocalHostError::Transport(error.to_string()))?;
    let mut hello = match parse_client_message(first)? {
        ClientFrame::Hello(hello) => hello,
        _ => {
            send_frame(
                &mut writer,
                &ServerFrame::ProtocolError {
                    code: "authentication_required".into(),
                    message: "the first local-host frame must authenticate".into(),
                },
            )
            .await?;
            return Ok(());
        }
    };
    let authentication = validate_hello(&hello, host_id, &workspace_id, &capability_hash);
    let client_id = Uuid::new_v4();
    let requested_role = hello.role;
    let reconnect = hello.controller_reconnect.take();
    hello.capability.zeroize();
    if let Err(reason) = authentication {
        send_frame(
            &mut writer,
            &ServerFrame::ProtocolError {
                code: "authentication_failed".into(),
                message: reason,
            },
        )
        .await?;
        return Ok(());
    }
    let mut role = match (requested_role, reconnect) {
        (ClientRole::Observer, None) => ClientRole::Observer,
        (ClientRole::Controller, Some(mut reconnect)) if execution.is_some() => {
            let result = hub.reconnect_controller(client_id, &reconnect);
            reconnect.zeroize();
            result.map_err(LocalHostError::Invalid)?;
            ClientRole::Controller
        }
        (ClientRole::Controller, _) => {
            return Err(LocalHostError::Invalid(
                "controller reconnect requires a valid reconnect capability".into(),
            ));
        }
        (ClientRole::Observer, Some(mut reconnect)) => {
            reconnect.zeroize();
            return Err(LocalHostError::Invalid(
                "observer attachment cannot present controller authority".into(),
            ));
        }
    };
    let mut subscription = hub
        .subscribe_as(client_id)
        .map_err(LocalHostError::Invalid)?;
    let _subscriber = SubscriberGuard {
        hub: hub.clone(),
        client_id,
    };
    eprintln!(
        "xana serve: observer authenticated ({} attached)",
        hub.subscriber_count()
    );
    send_frame(
        &mut writer,
        &ServerFrame::Snapshot {
            snapshot: Box::new(subscription.snapshot.clone()),
            role,
        },
    )
    .await?;
    let mut inbound_budget = InboundBudget::new();
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                let _ = send_frame(&mut writer, &ServerFrame::HostShuttingDown {
                    graceful_ms: HOST_SHUTDOWN_GRACE.as_millis() as u64,
                    hard_ms: HOST_SHUTDOWN_HARD.as_millis() as u64,
                }).await;
                break;
            },
            observation = subscription.observations.recv() => {
                let Some(observation) = observation else { break; };
                send_frame(&mut writer, &ServerFrame::Observation(observation)).await?;
            }
            incoming = reader.next() => {
                let Some(incoming) = incoming else { break; };
                let incoming = incoming.map_err(|error| LocalHostError::Transport(error.to_string()))?;
                if !inbound_budget.allow() {
                    send_frame(
                        &mut writer,
                        &ServerFrame::ProtocolError {
                            code: "rate_limited".into(),
                            message: format!(
                                "local-host clients may send at most {MAX_INBOUND_FRAMES_PER_SECOND} frames per second"
                            ),
                        },
                    )
                    .await?;
                    break;
                }
                match parse_client_message(incoming)? {
                    ClientFrame::RequestSnapshot => {
                        hub.unsubscribe(client_id);
                        subscription = hub.subscribe_as(client_id).map_err(LocalHostError::Invalid)?;
                        role = if hub.is_controller(client_id) {
                            ClientRole::Controller
                        } else {
                            ClientRole::Observer
                        };
                        send_frame(&mut writer, &ServerFrame::Snapshot {
                            snapshot: Box::new(subscription.snapshot.clone()),
                            role,
                        }).await?;
                    }
                    ClientFrame::AcquireControl { request_id, conversation, takeover } => {
                        let result = if execution.is_none() {
                            ControlResult::rejected(request_id, "this host has no controllable execution")
                        } else {
                            match hub.acquire_controller(client_id, &conversation, takeover) {
                                Ok(reconnect) => {
                                    role = ClientRole::Controller;
                                    ControlResult::accepted(request_id, reconnect)
                                }
                                Err(reason) => ControlResult::rejected(request_id, reason),
                            }
                        };
                        send_frame(&mut writer, &ServerFrame::ControlResult(result)).await?;
                    }
                    ClientFrame::ReleaseControl { request_id } => {
                        let result = if hub.release_controller(client_id) {
                            role = ClientRole::Observer;
                            if let Some(execution) = &execution {
                                execution.fail_closed().await;
                            }
                            ControlResult::released(request_id)
                        } else {
                            ControlResult::rejected(request_id, "this client does not own controller authority")
                        };
                        send_frame(&mut writer, &ServerFrame::ControlResult(result)).await?;
                    }
                    ClientFrame::DecideManagedApproval { request_id, approval_id, decision } => {
                        let result = if role == ClientRole::Controller && hub.is_controller(client_id) {
                            match &execution {
                                Some(execution) => match execution.managed_approval(approval_id, decision).await {
                                    Ok(()) => ControlResult::released(request_id),
                                    Err(reason) => ControlResult::rejected(request_id, reason),
                                },
                                None => ControlResult::rejected(request_id, "hosted execution is unavailable"),
                            }
                        } else {
                            ControlResult::rejected(request_id, "observer clients cannot approve managed work")
                        };
                        send_frame(&mut writer, &ServerFrame::ControlResult(result)).await?;
                    }
                    ClientFrame::GetArtifact { request_id, artifact_id, max_preview_bytes } => {
                        let result = hub.fetch_artifact(request_id, artifact_id, max_preview_bytes);
                        send_frame(&mut writer, &ServerFrame::ArtifactResult(result)).await?;
                    }
                    ClientFrame::Command(command) => {
                        let kind = command_kind(&command);
                        let command_id = command.id;
                        let result = if role == ClientRole::Controller && hub.is_controller(client_id) {
                            match &execution {
                                Some(execution) => execution.command(command).await.unwrap_or_else(|reason| {
                                    ClientCommandResult::rejected(command_id, reason)
                                }),
                                None => ClientCommandResult::rejected(command_id, "hosted execution is unavailable"),
                            }
                        } else {
                            ClientCommandResult::rejected(
                                command_id,
                                "observer clients cannot mutate Xana runtime state",
                            )
                        };
                        send_frame(&mut writer, &ServerFrame::CommandResult(result)).await?;
                        if role != ClientRole::Controller || !hub.is_controller(client_id) {
                            let _ = hub.publish(HostEvent::ObserverCommandRejected { command: kind });
                        }
                    }
                    ClientFrame::Ping => send_frame(&mut writer, &ServerFrame::Pong).await?,
                    ClientFrame::Hello(_) => {
                        send_frame(
                            &mut writer,
                            &ServerFrame::ProtocolError {
                                code: "duplicate_hello".into(),
                                message: "local-host authentication is performed once per connection".into(),
                            },
                        ).await?;
                        break;
                    }
                }
            }
        }
    }
    if let Some(generation) = hub.disconnect_controller(client_id) {
        let grace_hub = hub.clone();
        let grace_execution = execution.clone();
        let grace_shutdown = shutdown.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = grace_shutdown.cancelled() => {}
                () = tokio::time::sleep(CONTROLLER_RECONNECT_GRACE) => {
                    if grace_hub.expire_controller(generation)
                        && let Some(execution) = grace_execution
                    {
                        execution.fail_closed().await;
                    }
                }
            }
        });
    }
    let _ = writer.close().await;
    Ok(())
}

struct SubscriberGuard {
    hub: ObservationHub,
    client_id: Uuid,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.client_id);
    }
}

pub(super) fn validate_hello(
    hello: &ClientHello,
    host_id: Uuid,
    workspace_id: &str,
    capability_hash: &[u8; 32],
) -> Result<(), String> {
    if hello.version != LOCAL_HOST_PROTOCOL_VERSION {
        return Err("unsupported local-host protocol version".into());
    }
    if hello.host_id != host_id {
        return Err("local-host capability is invalid or expired".into());
    }
    if hello.workspace_id != workspace_id {
        return Err("local-host attachment targets another workspace".into());
    }
    let received = blake3::hash(hello.capability.as_bytes());
    if !constant_time_equal(received.as_bytes(), capability_hash) {
        return Err("local-host capability is invalid or expired".into());
    }
    Ok(())
}

pub(super) fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |different, (left, right)| different | (left ^ right))
        == 0
}

#[allow(
    clippy::result_large_err,
    reason = "the tungstenite handshake callback requires its concrete HTTP error response"
)]
fn validate_origin(request: &Request, _host_port: u16) -> Result<(), ErrorResponse> {
    let Some(origin) = request.headers().get("origin") else {
        return Ok(());
    };
    let Ok(origin) = origin.to_str() else {
        return Err(forbidden_origin());
    };
    if origin_is_loopback(origin) {
        Ok(())
    } else {
        Err(forbidden_origin())
    }
}

pub(super) fn origin_is_loopback(origin: &str) -> bool {
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https")
        || authority.is_empty()
        || authority.contains(['/', '@', '?', '#'])
    {
        return false;
    }
    let host = if let Some(after_bracket) = authority.strip_prefix('[') {
        let Some((inside, suffix)) = after_bracket.split_once(']') else {
            return false;
        };
        if !valid_optional_port(suffix) {
            return false;
        }
        format!("[{inside}]")
    } else {
        let (host, suffix) = authority
            .split_once(':')
            .map_or((authority, ""), |(host, _)| {
                (host, &authority[host.len()..])
            });
        if !valid_optional_port(suffix) || host.contains(':') {
            return false;
        }
        host.to_owned()
    };
    let normalized = host.to_ascii_lowercase();
    normalized == "localhost"
        || normalized
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn valid_optional_port(suffix: &str) -> bool {
    suffix.is_empty()
        || suffix
            .strip_prefix(':')
            .is_some_and(|port| port.parse::<u16>().is_ok())
}

fn forbidden_origin() -> ErrorResponse {
    let mut response = ErrorResponse::new(Some("browser Origin is not loopback".into()));
    *response.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
    response
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_WIRE_BYTES))
        .max_frame_size(Some(MAX_WIRE_BYTES))
}

fn parse_client_message(message: Message) -> Result<ClientFrame, LocalHostError> {
    match message {
        Message::Text(encoded) => decode_client_frame(&encoded).map_err(LocalHostError::Invalid),
        Message::Close(_) => Err(LocalHostError::Closed),
        Message::Ping(_) | Message::Pong(_) => Ok(ClientFrame::Ping),
        Message::Binary(_) | Message::Frame(_) => Err(LocalHostError::Invalid(
            "local-host clients must send bounded UTF-8 JSON text frames".into(),
        )),
    }
}

async fn send_frame<S>(writer: &mut S, frame: &ServerFrame) -> Result<(), LocalHostError>
where
    S: futures::Sink<Message> + Unpin,
    S::Error: fmt::Display,
{
    let encoded = encode_frame(frame).map_err(LocalHostError::Invalid)?;
    timeout(
        CLIENT_WRITE_TIMEOUT,
        writer.send(Message::Text(encoded.into())),
    )
    .await
    .map_err(|_| LocalHostError::Transport("local-host client write timed out".into()))?
    .map_err(|error| LocalHostError::Transport(error.to_string()))
}

struct InboundBudget {
    started: Instant,
    frames: usize,
}

impl InboundBudget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            frames: 0,
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.started) >= Duration::from_secs(1) {
            self.started = now;
            self.frames = 0;
        }
        self.frames = self.frames.saturating_add(1);
        self.frames <= MAX_INBOUND_FRAMES_PER_SECOND
    }
}

fn report_client_result(
    joined: Result<Result<(), LocalHostError>, tokio::task::JoinError>,
    context: &str,
) {
    match joined {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("xana serve: client disconnected {context}: {error}"),
        Err(error) if error.is_cancelled() => {}
        Err(error) => eprintln!("xana serve: client task failed {context}: {error}"),
    }
}

pub(crate) struct AttachedObserver {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    snapshot: HostSnapshot,
    expected_sequence: u64,
    role: ClientRole,
    controller_reconnect: Option<String>,
    buffered: VecDeque<HostObservation>,
}

impl AttachedObserver {
    pub(crate) fn snapshot(&self) -> &HostSnapshot {
        &self.snapshot
    }

    pub(crate) fn is_controller(&self) -> bool {
        self.role == ClientRole::Controller
    }

    pub(crate) fn take_controller_reconnect(&mut self) -> Option<String> {
        self.controller_reconnect.take()
    }

    pub(crate) async fn acquire_control(
        &mut self,
        conversation: String,
        takeover: bool,
    ) -> Result<(), LocalHostError> {
        let request_id = super::protocol::ControlRequestId::new();
        self.send_client_frame(&ClientFrame::AcquireControl {
            request_id,
            conversation,
            takeover,
        })
        .await?;
        loop {
            match self.read_server_frame().await? {
                ServerFrame::ControlResult(mut result) if result.request_id == request_id => {
                    if !result.accepted {
                        return Err(LocalHostError::Invalid(
                            result
                                .reason
                                .take()
                                .unwrap_or_else(|| "controller acquisition was rejected".into()),
                        ));
                    }
                    self.controller_reconnect = result.controller_reconnect.take();
                    self.role = ClientRole::Controller;
                    return Ok(());
                }
                ServerFrame::Observation(observation) => self.buffer_observation(observation)?,
                ServerFrame::ProtocolError { message, .. } => {
                    return Err(LocalHostError::Invalid(message));
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn release_control(&mut self) -> Result<(), LocalHostError> {
        let request_id = super::protocol::ControlRequestId::new();
        self.send_client_frame(&ClientFrame::ReleaseControl { request_id })
            .await?;
        loop {
            match self.read_server_frame().await? {
                ServerFrame::ControlResult(mut result) if result.request_id == request_id => {
                    if !result.accepted {
                        return Err(LocalHostError::Invalid(
                            result
                                .reason
                                .take()
                                .unwrap_or_else(|| "controller release was rejected".into()),
                        ));
                    }
                    self.controller_reconnect = None;
                    self.role = ClientRole::Observer;
                    return Ok(());
                }
                ServerFrame::Observation(observation) => self.buffer_observation(observation)?,
                ServerFrame::ProtocolError { message, .. } => {
                    return Err(LocalHostError::Invalid(message));
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn send_command(
        &mut self,
        command: crate::runtime::RuntimeCommand,
    ) -> Result<ClientCommandResult, LocalHostError> {
        let command = crate::frontend::ClientCommand::new(command);
        let command_id = command.id;
        self.send_client_frame(&ClientFrame::Command(command))
            .await?;
        loop {
            match self.read_server_frame().await? {
                ServerFrame::CommandResult(result) if result.command_id == command_id => {
                    return Ok(result);
                }
                ServerFrame::Observation(observation) => self.buffer_observation(observation)?,
                ServerFrame::ProtocolError { message, .. } => {
                    return Err(LocalHostError::Invalid(message));
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn get_artifact(
        &mut self,
        artifact_id: crate::identity::ArtifactId,
    ) -> Result<super::protocol::ArtifactResult, LocalHostError> {
        let request_id = super::protocol::ArtifactRequestId::new();
        self.send_client_frame(&ClientFrame::GetArtifact {
            request_id,
            artifact_id,
            max_preview_bytes: super::artifact_access::MAX_ARTIFACT_PREVIEW_BYTES,
        })
        .await?;
        loop {
            match self.read_server_frame().await? {
                ServerFrame::ArtifactResult(result) if result.request_id == request_id => {
                    return Ok(result);
                }
                ServerFrame::Observation(observation) => self.buffer_observation(observation)?,
                ServerFrame::ProtocolError { message, .. } => {
                    return Err(LocalHostError::Invalid(message));
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn next(&mut self) -> Result<HostObservation, LocalHostError> {
        if let Some(observation) = self.buffered.pop_front() {
            return Ok(observation);
        }
        loop {
            match self.read_server_frame().await? {
                ServerFrame::Observation(observation) => {
                    if observation.version != LOCAL_HOST_PROTOCOL_VERSION {
                        return Err(LocalHostError::Invalid(
                            "local-host observation uses an unsupported version".into(),
                        ));
                    }
                    if observation.sequence != self.expected_sequence {
                        return Err(LocalHostError::SequenceGap {
                            expected: self.expected_sequence,
                            received: observation.sequence,
                        });
                    }
                    self.expected_sequence = self.expected_sequence.saturating_add(1);
                    return Ok(observation);
                }
                ServerFrame::Snapshot { snapshot, role } => {
                    self.expected_sequence = snapshot.sequence.saturating_add(1);
                    self.snapshot = *snapshot;
                    self.role = role;
                }
                ServerFrame::ProtocolError { message, .. } => {
                    return Err(LocalHostError::Invalid(message));
                }
                ServerFrame::HostShuttingDown { .. } => return Err(LocalHostError::Closed),
                ServerFrame::CommandResult(_)
                | ServerFrame::ControlResult(_)
                | ServerFrame::ArtifactResult(_)
                | ServerFrame::Pong => continue,
            }
        }
    }

    fn buffer_observation(&mut self, observation: HostObservation) -> Result<(), LocalHostError> {
        self.validate_observation(&observation)?;
        if self.buffered.len() >= super::hub::OBSERVER_QUEUE_CAPACITY {
            return Err(LocalHostError::SequenceGap {
                expected: self.expected_sequence,
                received: observation.sequence,
            });
        }
        self.buffered.push_back(observation);
        Ok(())
    }

    fn validate_observation(
        &mut self,
        observation: &HostObservation,
    ) -> Result<(), LocalHostError> {
        if observation.version != LOCAL_HOST_PROTOCOL_VERSION {
            return Err(LocalHostError::Invalid(
                "local-host observation uses an unsupported version".into(),
            ));
        }
        if observation.sequence != self.expected_sequence {
            return Err(LocalHostError::SequenceGap {
                expected: self.expected_sequence,
                received: observation.sequence,
            });
        }
        self.expected_sequence = self.expected_sequence.saturating_add(1);
        Ok(())
    }

    async fn send_client_frame(&mut self, frame: &ClientFrame) -> Result<(), LocalHostError> {
        let encoded = serde_json::to_string(frame).map_err(|error| {
            LocalHostError::Invalid(format!("could not encode local-host request: {error}"))
        })?;
        if encoded.len() > MAX_WIRE_BYTES {
            return Err(LocalHostError::Invalid(
                "local-host request exceeds its byte bound".into(),
            ));
        }
        self.socket
            .send(Message::Text(encoded.into()))
            .await
            .map_err(|error| LocalHostError::Transport(error.to_string()))
    }

    async fn read_server_frame(&mut self) -> Result<ServerFrame, LocalHostError> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or(LocalHostError::Closed)?
                .map_err(|error| LocalHostError::Transport(error.to_string()))?;
            match message {
                Message::Text(encoded) => {
                    return decode_server_frame(&encoded).map_err(LocalHostError::Invalid);
                }
                Message::Ping(data) => {
                    self.socket
                        .send(Message::Pong(data))
                        .await
                        .map_err(|error| LocalHostError::Transport(error.to_string()))?;
                }
                Message::Pong(_) => {}
                Message::Close(_) => return Err(LocalHostError::Closed),
                Message::Binary(_) | Message::Frame(_) => {
                    return Err(LocalHostError::Invalid(
                        "local-host server sent a non-text protocol frame".into(),
                    ));
                }
            }
        }
    }
}

pub(crate) async fn connect_observer(
    runtime_root: &Path,
    workspace: &Path,
) -> Result<AttachedObserver, LocalHostError> {
    connect_client(runtime_root, workspace, ClientRole::Observer, None).await
}

pub(crate) async fn reconnect_controller(
    runtime_root: &Path,
    workspace: &Path,
    reconnect: String,
) -> Result<AttachedObserver, LocalHostError> {
    connect_client(
        runtime_root,
        workspace,
        ClientRole::Controller,
        Some(reconnect),
    )
    .await
}

async fn connect_client(
    runtime_root: &Path,
    workspace: &Path,
    role: ClientRole,
    controller_reconnect: Option<String>,
) -> Result<AttachedObserver, LocalHostError> {
    let mut descriptor = discover(runtime_root, workspace).map_err(LocalHostError::Invalid)?;
    let endpoint = format!("ws://{}", descriptor.endpoint);
    let (mut socket, _) = connect_async_with_config(endpoint, Some(websocket_config()), false)
        .await
        .map_err(|error| {
            LocalHostError::Transport(format!(
                "could not connect to the discovered foreground host; its descriptor may be stale: {error}"
            ))
        })?;
    let hello = ClientFrame::Hello(ClientHello {
        version: LOCAL_HOST_PROTOCOL_VERSION,
        host_id: descriptor.host_id,
        workspace_id: descriptor.workspace_id.clone(),
        capability: std::mem::take(&mut descriptor.capability),
        controller_reconnect,
        role,
    });
    let retained_reconnect = match &hello {
        ClientFrame::Hello(hello) => hello.controller_reconnect.clone(),
        _ => None,
    };
    let mut encoded = serde_json::to_string(&hello).map_err(|error| {
        LocalHostError::Invalid(format!("could not encode attach request: {error}"))
    })?;
    socket
        .send(Message::Text(encoded.clone().into()))
        .await
        .map_err(|error| LocalHostError::Transport(error.to_string()))?;
    encoded.zeroize();
    drop(hello);
    let message = socket
        .next()
        .await
        .ok_or(LocalHostError::Closed)?
        .map_err(|error| LocalHostError::Transport(error.to_string()))?;
    let Message::Text(encoded) = message else {
        return Err(LocalHostError::Invalid(
            "local-host did not begin with a snapshot".into(),
        ));
    };
    match decode_server_frame(&encoded).map_err(LocalHostError::Invalid)? {
        ServerFrame::Snapshot { snapshot, role }
            if snapshot.version == LOCAL_HOST_PROTOCOL_VERSION
                && snapshot.host_id == descriptor.host_id
                && snapshot.workspace_id == descriptor.workspace_id =>
        {
            let expected_sequence = snapshot.sequence.saturating_add(1);
            Ok(AttachedObserver {
                socket,
                snapshot: *snapshot,
                expected_sequence,
                role,
                controller_reconnect: retained_reconnect,
                buffered: VecDeque::new(),
            })
        }
        ServerFrame::ProtocolError { message, .. } => Err(LocalHostError::Invalid(message)),
        _ => Err(LocalHostError::Invalid(
            "local-host did not provide a matching initial snapshot".into(),
        )),
    }
}

impl Drop for AttachedObserver {
    fn drop(&mut self) {
        if let Some(reconnect) = &mut self.controller_reconnect {
            reconnect.zeroize();
        }
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;
    use futures::Sink;
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    struct PendingSink;

    impl Sink<Message> for PendingSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn outbound_frames_and_inbound_rate_have_exact_bounds() {
        let mut budget = InboundBudget::new();
        for _ in 0..MAX_INBOUND_FRAMES_PER_SECOND {
            assert!(budget.allow());
        }
        assert!(!budget.allow());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(budget.allow());

        let error = send_frame(&mut PendingSink, &ServerFrame::Pong)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("write timed out"));
    }
}
