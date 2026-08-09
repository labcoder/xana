use super::{
    descriptor::{DescriptorLease, RuntimeDescriptor, discover},
    hub::ObservationHub,
    protocol::{
        ClientFrame, ClientHello, ClientRole, HostEvent, HostObservation, HostSnapshot,
        HostSnapshotSeed, LOCAL_HOST_PROTOCOL_VERSION, MAX_WIRE_BYTES, ServerFrame, command_kind,
        decode_client_frame, decode_server_frame, encode_frame,
    },
};
use crate::frontend::ClientCommandResult;
use futures::{SinkExt, StreamExt};
use std::{
    error::Error,
    fmt,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
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
    shutdown: CancellationToken,
}

impl LocalHostServer {
    pub(crate) async fn bind(
        runtime_root: &Path,
        workspace: &Path,
        bind: IpAddr,
        port: u16,
        seed: HostSnapshotSeed,
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
        Ok(Self {
            listener,
            endpoint,
            host_id,
            workspace,
            descriptor,
            capability_hash,
            hub: ObservationHub::new(HostSnapshot::new(host_id, seed)),
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
                    let hub = self.hub.clone();
                    let shutdown = self.shutdown.child_token();
                    let capability_hash = self.capability_hash;
                    let workspace_id = super::protocol::workspace_identity(&self.workspace);
                    let endpoint = self.endpoint;
                    let host_id = self.host_id;
                    clients.spawn(async move {
                        let result = serve_client(
                            stream,
                            endpoint,
                            host_id,
                            workspace_id,
                            capability_hash,
                            hub.clone(),
                            shutdown,
                        )
                        .await;
                        eprintln!(
                            "xana serve: client disconnected ({} remaining)",
                            hub.subscriber_count()
                        );
                        result
                    });
                }
                Some(joined) = clients.join_next(), if !clients.is_empty() => {
                    if let Err(error) = joined {
                        eprintln!("xana serve: client task failed: {error}");
                    }
                }
            }
        }
        self.shutdown.cancel();
        while let Some(joined) = clients.join_next().await {
            if let Err(error) = joined {
                eprintln!("xana serve: client task failed during shutdown: {error}");
            }
        }
        Ok(())
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the tungstenite handshake callback requires its concrete HTTP error response"
)]
async fn serve_client(
    stream: TcpStream,
    endpoint: SocketAddr,
    host_id: Uuid,
    workspace_id: String,
    capability_hash: [u8; 32],
    hub: ObservationHub,
    shutdown: CancellationToken,
) -> Result<(), LocalHostError> {
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
    let mut subscription = hub.subscribe().map_err(LocalHostError::Invalid)?;
    let client_id = subscription.client_id;
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
        &ServerFrame::Snapshot(subscription.snapshot.clone()),
    )
    .await?;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            observation = subscription.observations.recv() => {
                let Some(observation) = observation else { break; };
                send_frame(&mut writer, &ServerFrame::Observation(observation)).await?;
            }
            incoming = reader.next() => {
                let Some(incoming) = incoming else { break; };
                let incoming = incoming.map_err(|error| LocalHostError::Transport(error.to_string()))?;
                match parse_client_message(incoming)? {
                    ClientFrame::RequestSnapshot => {
                        hub.unsubscribe(client_id);
                        subscription = hub.subscribe_as(client_id).map_err(LocalHostError::Invalid)?;
                        send_frame(&mut writer, &ServerFrame::Snapshot(subscription.snapshot.clone())).await?;
                    }
                    ClientFrame::Command(command) => {
                        let kind = command_kind(&command);
                        let result = ClientCommandResult::rejected(
                            command.id,
                            "observer clients cannot mutate Xana runtime state",
                        );
                        send_frame(&mut writer, &ServerFrame::CommandResult(result)).await?;
                        let _ = hub.publish(HostEvent::ObserverCommandRejected { command: kind });
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
    if hello.role != ClientRole::Observer {
        return Err("this local-host version accepts observer attachment only".into());
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
    writer
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|error| LocalHostError::Transport(error.to_string()))
}

pub(crate) struct AttachedObserver {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    snapshot: HostSnapshot,
    expected_sequence: u64,
}

impl AttachedObserver {
    pub(crate) fn snapshot(&self) -> &HostSnapshot {
        &self.snapshot
    }

    pub(crate) async fn next(&mut self) -> Result<HostObservation, LocalHostError> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or(LocalHostError::Closed)?
                .map_err(|error| LocalHostError::Transport(error.to_string()))?;
            let frame = match message {
                Message::Text(encoded) => {
                    decode_server_frame(&encoded).map_err(LocalHostError::Invalid)?
                }
                Message::Ping(data) => {
                    self.socket
                        .send(Message::Pong(data))
                        .await
                        .map_err(|error| LocalHostError::Transport(error.to_string()))?;
                    continue;
                }
                Message::Pong(_) => continue,
                Message::Close(_) => return Err(LocalHostError::Closed),
                Message::Binary(_) | Message::Frame(_) => {
                    return Err(LocalHostError::Invalid(
                        "local-host server sent a non-text protocol frame".into(),
                    ));
                }
            };
            match frame {
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
                ServerFrame::Snapshot(snapshot) => {
                    self.expected_sequence = snapshot.sequence.saturating_add(1);
                    self.snapshot = snapshot;
                }
                ServerFrame::ProtocolError { message, .. } => {
                    return Err(LocalHostError::Invalid(message));
                }
                ServerFrame::CommandResult(_) | ServerFrame::Pong => continue,
            }
        }
    }
}

pub(crate) async fn connect_observer(
    runtime_root: &Path,
    workspace: &Path,
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
        role: ClientRole::Observer,
    });
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
        ServerFrame::Snapshot(snapshot)
            if snapshot.version == LOCAL_HOST_PROTOCOL_VERSION
                && snapshot.host_id == descriptor.host_id
                && snapshot.workspace_id == descriptor.workspace_id =>
        {
            let expected_sequence = snapshot.sequence.saturating_add(1);
            Ok(AttachedObserver {
                socket,
                snapshot,
                expected_sequence,
            })
        }
        ServerFrame::ProtocolError { message, .. } => Err(LocalHostError::Invalid(message)),
        _ => Err(LocalHostError::Invalid(
            "local-host did not provide a matching initial snapshot".into(),
        )),
    }
}
