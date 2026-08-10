//! Bounded actor that lets the TUI observe and control a Codex-owned inner loop.

use super::{
    ManagedChatConfig, ManagedThreadState, checked_original_path, ensure_thread_loaded,
    initial_managed_thread,
};
use crate::{
    frontend::ManagedClientEvent,
    identity::OperationId,
    managed::{
        codex::{
            AccountStatus, ApprovalDecision, ApprovalRequest, CodexAppServer, CodexError,
            ManagedEventHandler, ManagedNotification, ManagedTurnInput, ManagedTurnOptions,
        },
        thread_store::ManagedThreadStore,
    },
    model::{ModelDescriptor, ModelManager},
    vision::ImageAttachment,
    workspace_host::{ConversationRef, WorkspaceHost},
};
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 256;

pub(crate) enum ManagedTuiEvent {
    Notification(ManagedClientEvent),
    Approval {
        request: ApprovalRequest,
        reply: oneshot::Sender<ApprovalDecision>,
    },
    ThreadOpened(String),
    TurnFinished {
        operation_id: OperationId,
        error: Option<String>,
    },
    Cleared,
}

enum ManagedTuiCommand {
    Submit {
        operation_id: OperationId,
        input: String,
        images: Vec<ImageAttachment>,
    },
    SelectModel(String),
    SetReasoning(Option<String>),
    Clear,
    Shutdown,
}

#[derive(Clone)]
struct ActiveTurn {
    operation_id: OperationId,
    cancellation: CancellationToken,
}

pub(crate) struct ManagedTuiDriver {
    commands: mpsc::Sender<ManagedTuiCommand>,
    events: mpsc::Receiver<ManagedTuiEvent>,
    active: Arc<Mutex<Option<ActiveTurn>>>,
    task: JoinHandle<Result<(), CodexError>>,
    pub(crate) models: Vec<ModelDescriptor>,
    pub(crate) initial_thread: Option<String>,
    pub(crate) version: String,
    pub(crate) selected_model: String,
}

impl ManagedTuiDriver {
    pub(crate) async fn start(
        mut server: CodexAppServer,
        models: ModelManager,
        mut config: ManagedChatConfig,
        workspace_host: Arc<WorkspaceHost>,
        conversation: ConversationRef,
    ) -> Result<Self, CodexError> {
        if matches!(server.account_status().await?, AccountStatus::LoggedOut) {
            return Err(CodexError::LoginFailed(format!(
                "Codex is logged out; run `xana connection login {}` first",
                config.connection
            )));
        }
        let available = server.models().await?;
        models
            .write_managed_cache(&config.connection, &available)
            .map_err(|error| CodexError::Io(error.to_string()))?;
        if !available.iter().any(|model| model.id == config.model) {
            return Err(CodexError::Protocol(format!(
                "Codex does not advertise model {:?}; refresh or choose an advertised model",
                config.model
            )));
        }
        let selection = models
            .selected()
            .map_err(|error| CodexError::Protocol(error.to_string()))?;
        config.model = selection.model;
        let selected_model = config.model.clone();
        let store =
            ManagedThreadStore::open(&config.data_root, &config.connection, &config.workspace)
                .map_err(|error| CodexError::Io(error.to_string()))?;
        let (initial_thread, thread) = initial_managed_thread(
            &conversation,
            &store,
            &config.connection,
            config.identity_version,
        )?;
        let version = server.version.clone();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let active = Arc::new(Mutex::new(None));
        let task_active = Arc::clone(&active);
        let task = tokio::spawn(run_actor(
            server,
            models,
            config,
            workspace_host,
            conversation,
            store,
            thread,
            command_rx,
            event_tx,
            task_active,
        ));
        Ok(Self {
            commands: command_tx,
            events: event_rx,
            active,
            task,
            models: available,
            initial_thread,
            version,
            selected_model,
        })
    }

    pub(crate) async fn submit(
        &self,
        operation_id: OperationId,
        input: String,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        self.commands
            .send(ManagedTuiCommand::Submit {
                operation_id,
                input,
                images,
            })
            .await
            .map_err(|_| "managed runtime stopped".to_owned())
    }

    pub(crate) fn interrupt(&self, operation_id: OperationId) -> bool {
        let active = self
            .active
            .lock()
            .expect("managed active-turn lock poisoned");
        let Some(active) = active.as_ref() else {
            return false;
        };
        if active.operation_id != operation_id {
            return false;
        }
        active.cancellation.cancel();
        true
    }

    pub(crate) async fn select_model(&self, model: String) -> Result<(), String> {
        self.commands
            .send(ManagedTuiCommand::SelectModel(model))
            .await
            .map_err(|_| "managed runtime stopped".to_owned())
    }

    pub(crate) async fn set_reasoning(&self, effort: Option<String>) -> Result<(), String> {
        self.commands
            .send(ManagedTuiCommand::SetReasoning(effort))
            .await
            .map_err(|_| "managed runtime stopped".to_owned())
    }

    pub(crate) async fn clear(&self) -> Result<(), String> {
        self.commands
            .send(ManagedTuiCommand::Clear)
            .await
            .map_err(|_| "managed runtime stopped".to_owned())
    }

    pub(crate) async fn next_event(&mut self) -> Option<ManagedTuiEvent> {
        self.events.recv().await
    }

    pub(crate) async fn shutdown(self) -> Result<(), CodexError> {
        let _ = self.commands.send(ManagedTuiCommand::Shutdown).await;
        self.task
            .await
            .map_err(|error| CodexError::Io(format!("managed TUI task failed: {error}")))?
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_actor(
    mut server: CodexAppServer,
    models: ModelManager,
    mut config: ManagedChatConfig,
    workspace_host: Arc<WorkspaceHost>,
    mut conversation: ConversationRef,
    mut store: ManagedThreadStore,
    mut thread: ManagedThreadState,
    mut commands: mpsc::Receiver<ManagedTuiCommand>,
    events: mpsc::Sender<ManagedTuiEvent>,
    active: Arc<Mutex<Option<ActiveTurn>>>,
) -> Result<(), CodexError> {
    let mut selection = models
        .selected()
        .map_err(|error| CodexError::Protocol(error.to_string()))?;
    while let Some(command) = commands.recv().await {
        match command {
            ManagedTuiCommand::Submit {
                operation_id,
                input,
                images,
            } => {
                let lease = match workspace_host.acquire_root(conversation.clone()) {
                    Ok(lease) => lease,
                    Err(error) => {
                        send_event(
                            &events,
                            ManagedTuiEvent::TurnFinished {
                                operation_id,
                                error: Some(error.to_string()),
                            },
                        )
                        .await?;
                        continue;
                    }
                };
                let local_images = match images
                    .iter()
                    .map(|image| checked_original_path(&config.workspace, &image.source_path))
                    .collect::<anyhow::Result<Vec<_>>>()
                {
                    Ok(images) => images,
                    Err(error) => {
                        send_event(
                            &events,
                            ManagedTuiEvent::TurnFinished {
                                operation_id,
                                error: Some(error.to_string()),
                            },
                        )
                        .await?;
                        drop(lease);
                        continue;
                    }
                };
                let cancellation = CancellationToken::new();
                *active.lock().expect("managed active-turn lock poisoned") = Some(ActiveTurn {
                    operation_id,
                    cancellation: cancellation.clone(),
                });
                let mut handler = TuiManagedHandler::new(events.clone());
                let thread_id = match ensure_thread_loaded(
                    &mut server,
                    &mut thread,
                    &mut store,
                    &config,
                    &mut handler,
                )
                .await
                {
                    Ok(thread_id) => thread_id,
                    Err(error) => {
                        clear_active(&active, operation_id);
                        send_event(
                            &events,
                            ManagedTuiEvent::TurnFinished {
                                operation_id,
                                error: Some(error.to_string()),
                            },
                        )
                        .await?;
                        drop(lease);
                        continue;
                    }
                };
                send_event(&events, ManagedTuiEvent::ThreadOpened(thread_id.clone())).await?;
                conversation = ConversationRef::Managed {
                    connection: config.connection.clone(),
                    thread_id: thread_id.clone(),
                };
                let result = server
                    .run_turn_cancellable(
                        &thread_id,
                        &config.model,
                        &ManagedTurnOptions {
                            reasoning_effort: selection.reasoning_effort.clone(),
                            reasoning_summary: selection.reasoning_summary,
                        },
                        ManagedTurnInput {
                            text: input,
                            local_images,
                        },
                        &cancellation,
                        &mut handler,
                    )
                    .await;
                clear_active(&active, operation_id);
                send_event(
                    &events,
                    ManagedTuiEvent::TurnFinished {
                        operation_id,
                        error: result.err().map(|error| error.to_string()),
                    },
                )
                .await?;
                drop(lease);
            }
            ManagedTuiCommand::SelectModel(model) => {
                selection = models
                    .select(&config.connection, &model)
                    .map_err(|error| CodexError::Protocol(error.to_string()))?;
                config.model = model;
            }
            ManagedTuiCommand::SetReasoning(effort) => {
                selection = models
                    .update_reasoning_effort(effort)
                    .map_err(|error| CodexError::Protocol(error.to_string()))?;
            }
            ManagedTuiCommand::Clear => {
                store
                    .set_thread(None, None)
                    .map_err(|error| CodexError::Io(error.to_string()))?;
                thread = ManagedThreadState::New;
                conversation = ConversationRef::NewManaged {
                    connection: config.connection.clone(),
                };
                send_event(&events, ManagedTuiEvent::Cleared).await?;
            }
            ManagedTuiCommand::Shutdown => break,
        }
    }
    server.shutdown().await
}

fn clear_active(active: &Mutex<Option<ActiveTurn>>, operation_id: OperationId) {
    let mut active = active.lock().expect("managed active-turn lock poisoned");
    if active
        .as_ref()
        .is_some_and(|turn| turn.operation_id == operation_id)
    {
        *active = None;
    }
}

async fn send_event(
    events: &mpsc::Sender<ManagedTuiEvent>,
    event: ManagedTuiEvent,
) -> Result<(), CodexError> {
    events
        .send(event)
        .await
        .map_err(|_| CodexError::RequestCancelled("managed TUI detached"))
}

struct TuiManagedHandler {
    events: mpsc::Sender<ManagedTuiEvent>,
}

impl TuiManagedHandler {
    fn new(events: mpsc::Sender<ManagedTuiEvent>) -> Self {
        Self { events }
    }
}

impl ManagedEventHandler for TuiManagedHandler {
    fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError> {
        let Some(event) = ManagedClientEvent::from_notification(notification) else {
            return Ok(());
        };
        self.events
            .try_send(ManagedTuiEvent::Notification(event))
            .map_err(|error| {
                CodexError::Protocol(format!("managed TUI event queue unavailable: {error}"))
            })
    }

    fn approve<'a>(
        &'a mut self,
        request: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        Box::pin(async move {
            let (reply, decision) = oneshot::channel();
            self.events
                .send(ManagedTuiEvent::Approval { request, reply })
                .await
                .map_err(|_| CodexError::RequestCancelled("managed approval frontend"))?;
            decision
                .await
                .map_err(|_| CodexError::RequestCancelled("managed approval decision"))
        })
    }
}
