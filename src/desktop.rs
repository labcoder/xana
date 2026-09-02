//! Repository-private Desktop application boundary.
//!
//! This module is public only so the `xana-desktop` workspace member can use
//! it. It is not a stable SDK. Presentation code receives bounded projections
//! and typed intent; provider adapters, credentials, tools, paths, and runtime
//! ownership stay in this package.

use crate::{
    app::{ChatExit, ChatHeader},
    frontend::{
        ClientCommand, ClientEvent, ClientObservation, ClientSnapshot, ClientSnapshotSeed,
        EmbeddedClient, FRONTEND_PROTOCOL_VERSION,
    },
    identity::{OperationId, ToolInvocationId},
    message::{ContentBlock, Message, Role},
    native_runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
    paths::XanaPaths,
    permission::{ControllerDecision, PermissionRequest, PermissionScope},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use std::{
    ffi::OsString,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc as std_mpsc},
    thread,
    time::Duration,
};
use tokio::sync::mpsc;

/// The repository-private Desktop protocol version.
pub const PROTOCOL_VERSION: u16 = FRONTEND_PROTOCOL_VERSION;

const COMMAND_CAPACITY: usize = 32;
const UPDATE_CAPACITY: usize = 256;
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CRITICAL_UPDATE_GRACE: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PUBLIC_TEXT_BYTES: usize = 256 * 1024;

type StartupSender = std_mpsc::SyncSender<Result<DesktopSnapshot, DesktopError>>;

/// Owned process-edge inputs used to start the bundled Desktop runtime.
#[derive(Debug, Clone)]
pub struct DesktopLaunch {
    workspace: PathBuf,
    xana_home: Option<OsString>,
}

impl DesktopLaunch {
    /// Captures the current workspace and optional `XANA_HOME` value once.
    pub fn from_process() -> Result<Self, DesktopError> {
        let workspace = std::env::current_dir().map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::WorkspaceUnavailable,
                format!("could not resolve the Desktop workspace: {error}"),
            )
        })?;
        Ok(Self {
            workspace,
            xana_home: std::env::var_os("XANA_HOME"),
        })
    }

    /// Creates an explicit launch without reading process-global workspace state.
    pub fn new(workspace: impl Into<PathBuf>, xana_home: Option<OsString>) -> Self {
        Self {
            workspace: workspace.into(),
            xana_home,
        }
    }
}

/// Stable semantic failure categories crossing the Desktop boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopErrorCode {
    WorkspaceUnavailable,
    ConfigurationUnavailable,
    StateInvalid,
    HostBusy,
    ProtocolMismatch,
    CommandRejected,
    RuntimeUnavailable,
    RuntimeCrashed,
    ObserverStalled,
    UnsupportedExecutionOwner,
}

impl DesktopErrorCode {
    /// Stable machine-readable code for logging and future transport adapters.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceUnavailable => "workspace_unavailable",
            Self::ConfigurationUnavailable => "configuration_unavailable",
            Self::StateInvalid => "state_invalid",
            Self::HostBusy => "host_busy",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::CommandRejected => "command_rejected",
            Self::RuntimeUnavailable => "runtime_unavailable",
            Self::RuntimeCrashed => "runtime_crashed",
            Self::ObserverStalled => "observer_stalled",
            Self::UnsupportedExecutionOwner => "unsupported_execution_owner",
        }
    }
}

/// Redacted, user-presentable Desktop boundary error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopError {
    pub code: DesktopErrorCode,
    pub message: String,
}

impl DesktopError {
    fn new(code: DesktopErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: bounded_text(message.into(), MAX_PUBLIC_TEXT_BYTES),
        }
    }
}

impl fmt::Display for DesktopError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for DesktopError {}

/// Opaque runtime operation identity retained by the Desktop application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DesktopOperationId(OperationId);

impl DesktopOperationId {
    /// Creates an application-owned correlation id.
    pub fn new() -> Self {
        Self(OperationId::new())
    }
}

impl Default for DesktopOperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DesktopOperationId {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(output)
    }
}

/// Opaque permission correlation retained by the Desktop application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DesktopPermissionId {
    operation_id: OperationId,
    invocation_id: ToolInvocationId,
}

/// One accepted application-side command enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopCommandReceipt {
    pub command_id: u64,
    pub operation_id: Option<DesktopOperationId>,
}

/// A bounded atomic projection of current runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSnapshot {
    pub version: u16,
    pub sequence: u64,
    pub session_id: String,
    pub connection: String,
    pub execution_owner: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub conversation: Vec<DesktopMessage>,
    pub conversation_truncated: bool,
    pub active_operation: Option<DesktopOperationId>,
    pub pending_approval_count: usize,
    pub activity_count: usize,
    pub artifact_count: usize,
}

/// Presentation-safe conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopMessage {
    pub id: String,
    pub role: DesktopRole,
    pub content: Vec<DesktopContent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Typed message content without artifact bytes or filesystem authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopContent {
    Text(String),
    Image {
        artifact_id: String,
        media_type: String,
        byte_len: u64,
        width: Option<u32>,
        height: Option<u32>,
    },
    ToolCall {
        name: String,
    },
    ToolResult {
        succeeded: bool,
        output: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOperationState {
    Running,
    Suspended,
    Completed,
    Failed,
    Declined,
    Interrupted,
}

/// One ordered, bounded runtime observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopObservation {
    pub version: u16,
    pub sequence: u64,
    pub event: DesktopEvent,
}

/// Minimal semantic vocabulary used by the M4 walking skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    OperationState {
        operation_id: DesktopOperationId,
        state: DesktopOperationState,
    },
    AssistantDelta {
        operation_id: DesktopOperationId,
        text: String,
    },
    ReasoningDelta {
        operation_id: DesktopOperationId,
        text: String,
    },
    MessageFinal {
        operation_id: DesktopOperationId,
        message: DesktopMessage,
    },
    PermissionRequired {
        permission_id: DesktopPermissionId,
        tool: String,
        effect: String,
        scope: String,
    },
    PermissionResolved {
        permission_id: DesktopPermissionId,
    },
    Usage {
        operation_id: DesktopOperationId,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        requests: u64,
    },
    ConversationCleared,
    Activity {
        label: String,
    },
    Error(DesktopError),
}

impl DesktopEvent {
    fn replaceable(&self) -> bool {
        matches!(
            self,
            Self::AssistantDelta { .. } | Self::ReasoningDelta { .. }
        )
    }
}

/// Updates delivered to one Desktop projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopUpdate {
    Snapshot(DesktopSnapshot),
    Observation(DesktopObservation),
    CommandResult {
        command_id: u64,
        accepted: bool,
        error: Option<DesktopError>,
    },
    ResyncRequired {
        expected_sequence: u64,
        received_sequence: u64,
    },
    BackendStopped {
        expected: bool,
        error: Option<DesktopError>,
    },
}

/// The Desktop-side controller and bounded observation receiver.
pub struct DesktopClient {
    commands: mpsc::Sender<BridgeCommand>,
    updates: mpsc::Receiver<DesktopUpdate>,
    next_command_id: std::sync::atomic::AtomicU64,
    backend: Option<thread::JoinHandle<()>>,
    backend_done: std_mpsc::Receiver<()>,
    initial_snapshot: DesktopSnapshot,
}

impl DesktopClient {
    /// Starts Xana's matching runtime inside this process.
    pub fn launch(launch: DesktopLaunch) -> Result<Self, DesktopError> {
        let workspace = launch.workspace.canonicalize().map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::WorkspaceUnavailable,
                format!(
                    "could not canonicalize Desktop workspace {}: {error}",
                    launch.workspace.display()
                ),
            )
        })?;
        let paths = XanaPaths::resolve(launch.xana_home).map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::ConfigurationUnavailable,
                format!("could not resolve Xana paths: {error}"),
            )
        })?;
        let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (updates, update_receiver) = mpsc::channel(UPDATE_CAPACITY);
        let (startup_sender, startup_receiver) = std_mpsc::sync_channel(1);
        let startup = StartupSignal::new(startup_sender);
        let (done_sender, done_receiver) = std_mpsc::sync_channel(1);
        let bridge = Bridge {
            commands: command_receiver,
            updates: updates.clone(),
            startup: startup.clone(),
        };
        let failure_updates = updates.clone();
        let backend = thread::Builder::new()
            .name("xana-desktop-runtime".to_owned())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_backend(paths, workspace, bridge)
                }));
                match result {
                    Ok(Ok(())) => {
                        startup.fail_if_pending(DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "Desktop runtime stopped before publishing its initial snapshot",
                        ));
                    }
                    Ok(Err(error)) => {
                        let error = classify_backend_error(&error);
                        startup.fail_if_pending(error.clone());
                        let _ = failure_updates.blocking_send(DesktopUpdate::BackendStopped {
                            expected: false,
                            error: Some(error),
                        });
                    }
                    Err(_) => {
                        let error = DesktopError::new(
                            DesktopErrorCode::RuntimeCrashed,
                            "Desktop runtime thread panicked; durable state remains recoverable",
                        );
                        startup.fail_if_pending(error.clone());
                        let _ = failure_updates.blocking_send(DesktopUpdate::BackendStopped {
                            expected: false,
                            error: Some(error),
                        });
                    }
                }
                let _ = done_sender.send(());
            })
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeUnavailable,
                    format!("could not create the Desktop runtime thread: {error}"),
                )
            })?;
        let initial_snapshot =
            startup_receiver
                .recv_timeout(START_TIMEOUT)
                .map_err(|error| {
                    DesktopError::new(
                        DesktopErrorCode::RuntimeUnavailable,
                        format!("Desktop runtime did not become ready within 30 seconds: {error}"),
                    )
                })??;
        if initial_snapshot.version != PROTOCOL_VERSION {
            return Err(DesktopError::new(
                DesktopErrorCode::ProtocolMismatch,
                format!(
                    "Desktop protocol {} does not match runtime protocol {}",
                    PROTOCOL_VERSION, initial_snapshot.version
                ),
            ));
        }
        Ok(Self {
            commands,
            updates: update_receiver,
            next_command_id: std::sync::atomic::AtomicU64::new(1),
            backend: Some(backend),
            backend_done: done_receiver,
            initial_snapshot,
        })
    }

    pub fn initial_snapshot(&self) -> &DesktopSnapshot {
        &self.initial_snapshot
    }

    pub fn submit(&self, input: impl Into<String>) -> Result<DesktopCommandReceipt, DesktopError> {
        let operation_id = DesktopOperationId(OperationId::new());
        let command_id = self.enqueue(BridgeCommandValue::Submit {
            operation_id,
            input: input.into(),
        })?;
        Ok(DesktopCommandReceipt {
            command_id,
            operation_id: Some(operation_id),
        })
    }

    pub fn clear(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::Clear)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    pub fn interrupt(
        &self,
        operation_id: DesktopOperationId,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::Interrupt { operation_id })
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: Some(operation_id),
            })
    }

    pub fn decide_permission(
        &self,
        permission_id: DesktopPermissionId,
        allow_once: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::DecidePermission {
            permission_id,
            allow_once,
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: Some(DesktopOperationId(permission_id.operation_id)),
        })
    }

    pub fn request_snapshot(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::RequestSnapshot)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Drains one already-delivered update without blocking GPUI's render thread.
    pub fn try_next(&mut self) -> Result<Option<DesktopUpdate>, DesktopError> {
        match self.updates.try_recv() {
            Ok(update) => Ok(Some(update)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Err(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                "Desktop runtime update stream closed",
            )),
        }
    }

    /// Requests clean shutdown and verifies the runtime thread terminates.
    pub fn shutdown(mut self) -> Result<(), DesktopError> {
        let _ = self.enqueue(BridgeCommandValue::Shutdown);
        self.backend_done
            .recv_timeout(SHUTDOWN_TIMEOUT)
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeUnavailable,
                    format!("Desktop runtime did not stop within 10 seconds: {error}"),
                )
            })?;
        if let Some(backend) = self.backend.take() {
            backend.join().map_err(|_| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeCrashed,
                    "Desktop runtime thread panicked during shutdown",
                )
            })?;
        }
        Ok(())
    }

    fn enqueue(&self, value: BridgeCommandValue) -> Result<u64, DesktopError> {
        use std::sync::atomic::Ordering;
        let command_id = self.next_command_id.fetch_add(1, Ordering::Relaxed);
        self.commands
            .try_send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id,
                value,
            })
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeUnavailable,
                    format!("Desktop command queue is unavailable: {error}"),
                )
            })?;
        Ok(command_id)
    }
}

impl Drop for DesktopClient {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let command_id = self.next_command_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.try_send(BridgeCommand {
            version: PROTOCOL_VERSION,
            command_id,
            value: BridgeCommandValue::Shutdown,
        });
    }
}

#[derive(Clone)]
struct StartupSignal(Arc<Mutex<Option<StartupSender>>>);

impl StartupSignal {
    fn new(sender: StartupSender) -> Self {
        Self(Arc::new(Mutex::new(Some(sender))))
    }

    fn ready(&self, snapshot: DesktopSnapshot) {
        if let Ok(mut sender) = self.0.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(Ok(snapshot));
        }
    }

    fn fail_if_pending(&self, error: DesktopError) {
        if let Ok(mut sender) = self.0.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(Err(error));
        }
    }
}

pub(crate) struct Bridge {
    commands: mpsc::Receiver<BridgeCommand>,
    updates: mpsc::Sender<DesktopUpdate>,
    startup: StartupSignal,
}

#[derive(Debug)]
struct BridgeCommand {
    version: u16,
    command_id: u64,
    value: BridgeCommandValue,
}

#[derive(Debug)]
enum BridgeCommandValue {
    Submit {
        operation_id: DesktopOperationId,
        input: String,
    },
    Clear,
    Interrupt {
        operation_id: DesktopOperationId,
    },
    DecidePermission {
        permission_id: DesktopPermissionId,
        allow_once: bool,
    },
    RequestSnapshot,
    Shutdown,
}

fn run_backend(paths: XanaPaths, workspace: PathBuf, bridge: Bridge) -> anyhow::Result<()> {
    let _diagnostics = crate::diagnostics::DiagnosticRuntime::start(&paths)
        .ok()
        .flatten();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| anyhow::anyhow!("could not create Desktop async runtime: {error}"))?;
    runtime.block_on(crate::app::run_desktop(paths, workspace, bridge))
}

pub(crate) async fn run_native(
    runtime: RuntimeHandle,
    header: &ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
    bridge: Bridge,
) -> anyhow::Result<ChatExit> {
    let reasoning_effort = header
        .models
        .selected()
        .ok()
        .and_then(|selection| selection.reasoning_effort);
    let seed = ClientSnapshotSeed {
        session_id: header.session_id,
        connection: header.provider_name.clone(),
        execution_owner: "native".to_owned(),
        model: header.model.clone(),
        reasoning_effort,
        children: header.children.clone(),
    };
    bridge
        .serve_native(
            EmbeddedClient::from_runtime(runtime, seed),
            workspace_host,
            conversation,
        )
        .await?;
    Ok(ChatExit::Quit)
}

pub(crate) fn reject_managed(bridge: &Bridge, connection: &str) -> DesktopError {
    let error = DesktopError::new(
        DesktopErrorCode::UnsupportedExecutionOwner,
        format!(
            "Desktop managed-runtime projection for connection {connection} is not available in the M4 walking skeleton"
        ),
    );
    bridge.startup.fail_if_pending(error.clone());
    error
}

impl Bridge {
    async fn serve_native(
        mut self,
        client: EmbeddedClient,
        workspace_host: WorkspaceHost,
        conversation: ConversationRef,
    ) -> Result<(), DesktopError> {
        let (owner, mut observer) = client.into_parts();
        let mut snapshot = observer.snapshot().clone();
        self.startup.ready(project_snapshot(&snapshot));
        let mut active_root: Option<ActiveRootLease> = None;

        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        let _ = owner.send(ClientCommand::new(RuntimeCommand::Shutdown)).await;
                        break;
                    };
                    if command.version != FRONTEND_PROTOCOL_VERSION {
                        self.publish_command_result(
                            command.command_id,
                            Err(DesktopError::new(
                                DesktopErrorCode::ProtocolMismatch,
                                format!(
                                    "Desktop command protocol {} does not match runtime protocol {}",
                                    command.version, FRONTEND_PROTOCOL_VERSION
                                ),
                            )),
                        ).await?;
                        continue;
                    }
                    let should_stop = self.handle_command(
                        command,
                        &owner,
                        &workspace_host,
                        &conversation,
                        &snapshot,
                        &mut active_root,
                    ).await?;
                    if should_stop {
                        break;
                    }
                }
                observation = observer.next() => {
                    let observation = observation.map_err(|error| {
                        DesktopError::new(DesktopErrorCode::RuntimeUnavailable, error.to_string())
                    })?;
                    if observation.version != FRONTEND_PROTOCOL_VERSION {
                        return Err(DesktopError::new(
                            DesktopErrorCode::ProtocolMismatch,
                            format!(
                                "runtime observation protocol {} does not match Desktop protocol {}",
                                observation.version, FRONTEND_PROTOCOL_VERSION
                            ),
                        ));
                    }
                    let expected = snapshot.sequence.saturating_add(1);
                    if observation.sequence != expected {
                        self.publish_critical(DesktopUpdate::ResyncRequired {
                            expected_sequence: expected,
                            received_sequence: observation.sequence,
                        }).await?;
                    }
                    if observation_ends_root(&observation) {
                        active_root = None;
                    }
                    snapshot.apply(&observation.event, observation.sequence);
                    let projected = DesktopObservation {
                        version: observation.version,
                        sequence: observation.sequence,
                        event: project_event(&observation.event, &snapshot.session_id),
                    };
                    let replaceable = projected.event.replaceable();
                    self.publish(DesktopUpdate::Observation(projected), replaceable).await?;
                }
            }
        }

        active_root = None;
        drop(active_root);
        self.publish_critical(DesktopUpdate::BackendStopped {
            expected: true,
            error: None,
        })
        .await?;
        Ok(())
    }

    async fn handle_command(
        &self,
        command: BridgeCommand,
        owner: &crate::frontend::EmbeddedOwner,
        workspace_host: &WorkspaceHost,
        conversation: &ConversationRef,
        snapshot: &ClientSnapshot,
        active_root: &mut Option<ActiveRootLease>,
    ) -> Result<bool, DesktopError> {
        let command_id = command.command_id;
        match command.value {
            BridgeCommandValue::RequestSnapshot => {
                self.publish_critical(DesktopUpdate::Snapshot(project_snapshot(snapshot)))
                    .await?;
                self.publish_command_result(command_id, Ok(())).await?;
                Ok(false)
            }
            BridgeCommandValue::Shutdown => {
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::Shutdown))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime rejected Desktop shutdown because it is unavailable",
                        )
                    })
                    .and_then(command_result);
                self.publish_command_result(command_id, result).await?;
                Ok(true)
            }
            BridgeCommandValue::Submit {
                operation_id,
                input,
            } => {
                if active_root.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::HostBusy,
                            "a root operation is already active",
                        )),
                    )
                    .await?;
                    return Ok(false);
                }
                let lease = workspace_host
                    .acquire_root(conversation.clone())
                    .map_err(|error| {
                        DesktopError::new(DesktopErrorCode::HostBusy, error.to_string())
                    });
                let lease = match lease {
                    Ok(lease) => lease,
                    Err(error) => {
                        self.publish_command_result(command_id, Err(error)).await?;
                        return Ok(false);
                    }
                };
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::SubmitTurn {
                        operation_id: operation_id.0,
                        input,
                    }))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                if result.is_ok() {
                    *active_root = Some(lease);
                }
                self.publish_command_result(command_id, result).await?;
                Ok(false)
            }
            BridgeCommandValue::Clear => {
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::ClearConversation))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                self.publish_command_result(command_id, result).await?;
                Ok(false)
            }
            BridgeCommandValue::Interrupt { operation_id } => {
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::InterruptOperation {
                        operation_id: operation_id.0,
                    }))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                self.publish_command_result(command_id, result).await?;
                Ok(false)
            }
            BridgeCommandValue::DecidePermission {
                permission_id,
                allow_once,
            } => {
                let decision = if allow_once {
                    ControllerDecision::AllowOnce
                } else {
                    ControllerDecision::Deny
                };
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::DecidePermission {
                        operation_id: permission_id.operation_id,
                        invocation_id: permission_id.invocation_id,
                        decision,
                    }))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                self.publish_command_result(command_id, result).await?;
                Ok(false)
            }
        }
    }

    async fn publish_command_result(
        &self,
        command_id: u64,
        result: Result<(), DesktopError>,
    ) -> Result<(), DesktopError> {
        self.publish_critical(DesktopUpdate::CommandResult {
            command_id,
            accepted: result.is_ok(),
            error: result.err(),
        })
        .await
    }

    async fn publish(&self, update: DesktopUpdate, replaceable: bool) -> Result<(), DesktopError> {
        match self.updates.try_send(update) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) if replaceable => Ok(()),
            Err(mpsc::error::TrySendError::Full(update)) => self.publish_critical(update).await,
            Err(mpsc::error::TrySendError::Closed(_)) => Err(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                "Desktop projection detached",
            )),
        }
    }

    async fn publish_critical(&self, update: DesktopUpdate) -> Result<(), DesktopError> {
        match tokio::time::timeout(CRITICAL_UPDATE_GRACE, self.updates.send(update)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                "Desktop projection detached",
            )),
            Err(_) => Err(DesktopError::new(
                DesktopErrorCode::ObserverStalled,
                "Desktop did not receive a critical update within 5 seconds",
            )),
        }
    }
}

fn command_result(result: crate::frontend::ClientCommandResult) -> Result<(), DesktopError> {
    if result.accepted {
        Ok(())
    } else {
        Err(DesktopError::new(
            DesktopErrorCode::CommandRejected,
            result
                .reason
                .unwrap_or_else(|| "runtime rejected the Desktop command".to_owned()),
        ))
    }
}

fn observation_ends_root(observation: &ClientObservation) -> bool {
    matches!(
        observation.event,
        ClientEvent::Runtime(ref event)
            if matches!(
                event.as_ref(),
                AgentEvent::OperationStateChanged {
                    state: OperationState::Finished(_),
                    ..
                } | AgentEvent::OperationFailed { .. }
            )
    )
}

fn project_snapshot(snapshot: &ClientSnapshot) -> DesktopSnapshot {
    DesktopSnapshot {
        version: snapshot.version,
        sequence: snapshot.sequence,
        session_id: snapshot.session_id.to_string(),
        connection: snapshot.connection.clone(),
        execution_owner: snapshot.execution_owner.clone(),
        model: snapshot.model.clone(),
        reasoning_effort: snapshot.reasoning_effort.clone(),
        conversation: project_messages(snapshot.session_id.to_string(), &snapshot.conversation),
        conversation_truncated: snapshot.conversation_truncated,
        active_operation: snapshot.active_operation.map(DesktopOperationId),
        pending_approval_count: snapshot.pending_approval_count,
        activity_count: snapshot.activity_count,
        artifact_count: snapshot.artifact_count,
    }
}

fn project_messages(session_id: String, messages: &[Message]) -> Vec<DesktopMessage> {
    let mut duplicate_counts = std::collections::HashMap::<String, usize>::new();
    messages
        .iter()
        .map(|message| {
            let encoded = serde_json::to_vec(message).unwrap_or_default();
            let digest = blake3::hash(&encoded).to_hex().to_string();
            let duplicate = duplicate_counts.entry(digest.clone()).or_default();
            let id = format!("{session_id}:{digest}:{duplicate}");
            *duplicate = duplicate.saturating_add(1);
            project_message(id, message)
        })
        .collect()
}

fn project_message(id: String, message: &Message) -> DesktopMessage {
    DesktopMessage {
        id,
        role: match message.role {
            Role::System => DesktopRole::System,
            Role::User => DesktopRole::User,
            Role::Assistant => DesktopRole::Assistant,
            Role::Tool => DesktopRole::Tool,
        },
        content: message
            .content
            .iter()
            .map(|content| match content {
                ContentBlock::Text(text) => {
                    DesktopContent::Text(bounded_text(text.clone(), MAX_PUBLIC_TEXT_BYTES))
                }
                ContentBlock::Image(image) => DesktopContent::Image {
                    artifact_id: image.artifact.reference.id.to_string(),
                    media_type: image.media_type.clone(),
                    byte_len: image.byte_len,
                    width: image.width,
                    height: image.height,
                },
                ContentBlock::ToolCall(call) => DesktopContent::ToolCall {
                    name: call.name.clone(),
                },
                ContentBlock::ToolResult(result) => DesktopContent::ToolResult {
                    succeeded: matches!(result.status, crate::message::ToolResultStatus::Success),
                    output: bounded_text(result.output.clone(), MAX_PUBLIC_TEXT_BYTES),
                },
            })
            .collect(),
    }
}

fn project_event(event: &ClientEvent, session_id: &crate::identity::SessionId) -> DesktopEvent {
    match event {
        ClientEvent::Runtime(event) => match event.as_ref() {
            AgentEvent::OperationStateChanged {
                operation_id,
                state,
            } => DesktopEvent::OperationState {
                operation_id: DesktopOperationId(*operation_id),
                state: project_operation_state(*state),
            },
            AgentEvent::AssistantTextDelta {
                operation_id, text, ..
            } => DesktopEvent::AssistantDelta {
                operation_id: DesktopOperationId(*operation_id),
                text: bounded_text(text.clone(), MAX_PUBLIC_TEXT_BYTES),
            },
            AgentEvent::ProviderReasoningDelta {
                operation_id, text, ..
            } => DesktopEvent::ReasoningDelta {
                operation_id: DesktopOperationId(*operation_id),
                text: bounded_text(text.clone(), MAX_PUBLIC_TEXT_BYTES),
            },
            AgentEvent::PermissionRequested { request } => project_permission(request),
            AgentEvent::PermissionAudited { fact } => DesktopEvent::PermissionResolved {
                permission_id: DesktopPermissionId {
                    operation_id: fact.request.operation_id,
                    invocation_id: fact.request.invocation_id,
                },
            },
            AgentEvent::AssistantMessage {
                operation_id,
                message,
            } => DesktopEvent::MessageFinal {
                operation_id: DesktopOperationId(*operation_id),
                message: project_message(format!("{session_id}:{operation_id}:final"), message),
            },
            AgentEvent::UsageObserved {
                operation_id,
                usage,
            } => DesktopEvent::Usage {
                operation_id: DesktopOperationId(*operation_id),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                requests: usage.requests,
            },
            AgentEvent::OperationFailed {
                operation_id,
                reason,
            } => DesktopEvent::Error(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                format!("operation {operation_id} failed: {reason}"),
            )),
            AgentEvent::ConversationCleared => DesktopEvent::ConversationCleared,
            AgentEvent::CommandRejected { reason } => DesktopEvent::Error(DesktopError::new(
                DesktopErrorCode::CommandRejected,
                reason.clone(),
            )),
            AgentEvent::ToolFinished { .. } => DesktopEvent::Activity {
                label: "Tool finished".to_owned(),
            },
            AgentEvent::InvocationIntentCommitted { .. } => DesktopEvent::Activity {
                label: "Tool invocation committed".to_owned(),
            },
            AgentEvent::InvocationResultCommitted { .. } => DesktopEvent::Activity {
                label: "Tool result committed".to_owned(),
            },
            AgentEvent::ChildLifecycleChanged { .. } => DesktopEvent::Activity {
                label: "Child lifecycle changed".to_owned(),
            },
            AgentEvent::ChildActivity { .. } => DesktopEvent::Activity {
                label: "Child activity updated".to_owned(),
            },
            AgentEvent::ChildReportCommitted { .. } => DesktopEvent::Activity {
                label: "Child report committed".to_owned(),
            },
            AgentEvent::ChildListSnapshot { .. } => DesktopEvent::Activity {
                label: "Child list refreshed".to_owned(),
            },
            AgentEvent::ChildInspectionSnapshot { .. } => DesktopEvent::Activity {
                label: "Child inspection refreshed".to_owned(),
            },
            AgentEvent::ChildCancellationRequested { .. } => DesktopEvent::Activity {
                label: "Child cancellation requested".to_owned(),
            },
            AgentEvent::ExternalAgentActivity { .. } => DesktopEvent::Activity {
                label: "External agent activity updated".to_owned(),
            },
        },
        ClientEvent::Managed(_) => DesktopEvent::Error(DesktopError::new(
            DesktopErrorCode::UnsupportedExecutionOwner,
            "managed event reached the native Desktop adapter",
        )),
        ClientEvent::PayloadOmitted {
            kind,
            encoded_bytes,
            limit,
        } => DesktopEvent::Activity {
            label: format!(
                "Oversized {kind} payload omitted ({encoded_bytes} bytes; limit {limit})"
            ),
        },
    }
}

fn project_operation_state(state: OperationState) -> DesktopOperationState {
    match state {
        OperationState::Running => DesktopOperationState::Running,
        OperationState::Suspended => DesktopOperationState::Suspended,
        OperationState::Finished(OperationOutcome::Completed) => DesktopOperationState::Completed,
        OperationState::Finished(OperationOutcome::Failed) => DesktopOperationState::Failed,
        OperationState::Finished(OperationOutcome::Declined) => DesktopOperationState::Declined,
        OperationState::Finished(OperationOutcome::Interrupted) => {
            DesktopOperationState::Interrupted
        }
    }
}

fn project_permission(request: &PermissionRequest) -> DesktopEvent {
    DesktopEvent::PermissionRequired {
        permission_id: DesktopPermissionId {
            operation_id: request.operation_id,
            invocation_id: request.invocation_id,
        },
        tool: request.tool_name.clone(),
        effect: format!("{:?}", request.effect_class).to_ascii_lowercase(),
        scope: permission_scope_label(&request.scope),
    }
}

fn permission_scope_label(scope: &PermissionScope) -> String {
    match scope {
        PermissionScope::WorkspacePath { canonical_path } => {
            format!("workspace path {}", canonical_path.display())
        }
        PermissionScope::ExternalPath { canonical_path } => {
            format!("external path {}", canonical_path.display())
        }
        PermissionScope::Command {
            shell,
            canonical_cwd,
            command,
        } => format!("{shell} in {}: {command}", canonical_cwd.display()),
        PermissionScope::External { operation, .. } => {
            format!("external recipient for {operation}")
        }
        PermissionScope::BuiltInResource { id } => format!("built-in resource {id}"),
        PermissionScope::Unscoped => "unscoped".to_owned(),
    }
}

fn classify_backend_error(error: &anyhow::Error) -> DesktopError {
    let message = format!("{error:#}");
    let code = if message.contains("not initialized") || message.contains("config") {
        DesktopErrorCode::ConfigurationUnavailable
    } else if message.contains("already has an active Xana root") {
        DesktopErrorCode::HostBusy
    } else if message.contains("invalid") || message.contains("corrupt") {
        DesktopErrorCode::StateInvalid
    } else {
        DesktopErrorCode::RuntimeUnavailable
    };
    DesktopError::new(code, message)
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::Agent,
        context::ContextBudget,
        identity::StepId,
        permission::{PermissionPolicy, PolicyDecision},
        prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
        provider::{ConversationalProvider, DeltaSink, ProviderError},
        tool::{ToolDefinition, ToolRegistry},
    };
    use anyhow::Result;
    use futures::future::BoxFuture;

    type BridgeChannels = (
        Bridge,
        mpsc::Sender<BridgeCommand>,
        mpsc::Receiver<DesktopUpdate>,
        std_mpsc::Receiver<Result<DesktopSnapshot, DesktopError>>,
    );

    struct ScriptedProvider;

    impl ConversationalProvider for ScriptedProvider {
        fn stream_message<'a>(
            &'a self,
            _messages: &'a [Message],
            _tools: &'a [&'a ToolDefinition],
            step_id: StepId,
            deltas: &'a dyn DeltaSink,
        ) -> BoxFuture<'a, Result<Message, ProviderError>> {
            Box::pin(async move {
                deltas.text_delta(step_id, "hello ");
                deltas.text_delta(step_id, "from Desktop");
                Ok(Message::text(Role::Assistant, "hello from Desktop"))
            })
        }
    }

    fn scripted_client(workspace: &std::path::Path) -> EmbeddedClient {
        let tools = ToolRegistry::new();
        let definitions = tools.definitions();
        let environment = PromptEnvironment {
            connection: "test-connection".to_owned(),
            model: "test-model".to_owned(),
            operating_system: "test".to_owned(),
            working_directory: workspace.to_owned(),
            configured_shell: "test shell".to_owned(),
            surface: PromptSurface::Cli,
        };
        let prompt = assemble_snapshot(PromptInputs {
            tool_definitions: &definitions,
            environment: &environment,
            product_documentation: None,
            project_sources: &[],
            budget: ContextBudget {
                total_tokens: 16_384,
                conversation_reserve_tokens: 4_096,
            },
        })
        .unwrap();
        let agent = Agent::new(
            Box::new(ScriptedProvider),
            tools,
            workspace.to_owned(),
            prompt,
            2,
        );
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), workspace).unwrap();
        EmbeddedClient::from_runtime(
            RuntimeHandle::spawn(agent, policy, true),
            ClientSnapshotSeed {
                session_id: crate::identity::SessionId::new(),
                connection: "scripted".to_owned(),
                execution_owner: "native".to_owned(),
                model: "test-model".to_owned(),
                reasoning_effort: None,
                children: Vec::new(),
            },
        )
    }

    fn bridge_channels() -> BridgeChannels {
        let (command_sender, commands) = mpsc::channel(COMMAND_CAPACITY);
        let (updates, update_receiver) = mpsc::channel(UPDATE_CAPACITY);
        let (startup_sender, startup_receiver) = std_mpsc::sync_channel(1);
        (
            Bridge {
                commands,
                updates,
                startup: StartupSignal::new(startup_sender),
            },
            command_sender,
            update_receiver,
            startup_receiver,
        )
    }

    #[test]
    fn public_message_projection_never_exposes_image_paths_or_bytes() {
        let message = Message::text(Role::Assistant, "bounded answer");
        let projected = project_message("stable".to_owned(), &message);

        assert_eq!(projected.id, "stable");
        assert_eq!(
            projected.content,
            vec![DesktopContent::Text("bounded answer".to_owned())]
        );
    }

    #[test]
    fn invalid_command_version_is_a_semantic_protocol_failure() {
        let error = DesktopError::new(
            DesktopErrorCode::ProtocolMismatch,
            "Desktop command protocol 0 does not match runtime protocol 2",
        );

        assert_eq!(error.code.as_str(), "protocol_mismatch");
        assert!(error.message.contains("does not match"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_embedded_turn_streams_then_publishes_authoritative_final() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let client = scripted_client(&workspace);
        let (bridge, commands, mut updates, startup) = bridge_channels();
        let runtime = tokio::spawn(bridge.serve_native(client, host, ConversationRef::NewNative));

        let initial = startup
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(initial.connection, "scripted");

        let operation_id = DesktopOperationId::new();
        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 1,
                value: BridgeCommandValue::Submit {
                    operation_id,
                    input: "hello".to_owned(),
                },
            })
            .await
            .unwrap();

        let mut accepted = false;
        let mut delta = String::new();
        let mut final_text = None;
        let mut completed = false;
        while !(accepted && !delta.is_empty() && final_text.is_some() && completed) {
            let update = tokio::time::timeout(Duration::from_secs(2), updates.recv())
                .await
                .unwrap()
                .unwrap();
            match update {
                DesktopUpdate::CommandResult {
                    command_id: 1,
                    accepted: true,
                    ..
                } => accepted = true,
                DesktopUpdate::Observation(DesktopObservation {
                    event: DesktopEvent::AssistantDelta { text, .. },
                    ..
                }) => delta.push_str(&text),
                DesktopUpdate::Observation(DesktopObservation {
                    event: DesktopEvent::MessageFinal { message, .. },
                    ..
                }) => {
                    final_text = message
                        .content
                        .into_iter()
                        .find_map(|content| match content {
                            DesktopContent::Text(text) => Some(text),
                            _ => None,
                        });
                }
                DesktopUpdate::Observation(DesktopObservation {
                    event:
                        DesktopEvent::OperationState {
                            state: DesktopOperationState::Completed,
                            ..
                        },
                    ..
                }) => completed = true,
                _ => {}
            }
        }
        assert_eq!(delta, "hello from Desktop");
        assert_eq!(final_text.as_deref(), Some("hello from Desktop"));

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 2,
                value: BridgeCommandValue::Shutdown,
            })
            .await
            .unwrap();
        runtime.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mismatched_command_is_rejected_without_stopping_the_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();
        let client = scripted_client(&workspace);
        let (bridge, commands, mut updates, startup) = bridge_channels();
        let runtime = tokio::spawn(bridge.serve_native(client, host, ConversationRef::NewNative));
        startup
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION.saturating_sub(1),
                command_id: 7,
                value: BridgeCommandValue::Clear,
            })
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(1), updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            update,
            DesktopUpdate::CommandResult {
                command_id: 7,
                accepted: false,
                error: Some(DesktopError {
                    code: DesktopErrorCode::ProtocolMismatch,
                    ..
                }),
                ..
            }
        ));

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 8,
                value: BridgeCommandValue::Shutdown,
            })
            .await
            .unwrap();
        runtime.await.unwrap().unwrap();
    }
}
