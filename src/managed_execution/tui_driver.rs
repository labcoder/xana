//! Bounded actor that lets the TUI observe and control a Codex-owned inner loop.

use super::{ManagedChatConfig, ManagedThreadState, ensure_thread_loaded, initial_managed_thread};
use crate::{
    frontend::ManagedClientEvent,
    identity::{ConversationId, OperationId},
    managed::{
        codex::{
            AccountStatus, ApprovalDecision, ApprovalRequest, CodexAppServer, CodexError,
            ManagedEventHandler, ManagedNotification, ManagedTurnInput, ManagedTurnOptions,
        },
        thread_store::ManagedThreadStore,
    },
    model_catalog::{ModelDescriptor, ModelManager, ModelSelection},
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
    SelectModel {
        model: String,
        reply: oneshot::Sender<Result<ModelSelection, String>>,
    },
    SetReasoning {
        effort: Option<String>,
        reply: oneshot::Sender<Result<ModelSelection, String>>,
    },
    Clear,
    Archive {
        thread_id: String,
        reply: oneshot::Sender<Result<bool, String>>,
    },
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
        server: CodexAppServer,
        models: ModelManager,
        config: ManagedChatConfig,
        workspace_host: Arc<WorkspaceHost>,
        conversation: ConversationRef,
    ) -> Result<Self, CodexError> {
        Self::start_inner(
            server,
            models,
            config,
            Some(workspace_host),
            conversation.clone(),
            false,
        )
        .await
        .map(|(driver, _)| driver)
    }

    /// Starts the same managed frontend actor while an application execution
    /// host owns workspace leases. The Codex thread is opened eagerly so the
    /// host receives a durable Conversation identity before admitting a Run.
    pub(crate) async fn start_hosted(
        server: CodexAppServer,
        models: ModelManager,
        config: ManagedChatConfig,
        conversation: ConversationRef,
    ) -> Result<(Self, ConversationRef), CodexError> {
        Self::start_inner(server, models, config, None, conversation, true).await
    }

    async fn start_inner(
        mut server: CodexAppServer,
        models: ModelManager,
        config: ManagedChatConfig,
        workspace_host: Option<Arc<WorkspaceHost>>,
        mut conversation: ConversationRef,
        eager_thread: bool,
    ) -> Result<(Self, ConversationRef), CodexError> {
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
        let selected_model = config.model.clone();
        let mut store =
            ManagedThreadStore::open(&config.data_root, &config.connection, &config.workspace)
                .map_err(|error| CodexError::Io(error.to_string()))?;
        let (initial_thread, mut thread) = initial_managed_thread(
            &conversation,
            &store,
            &config.connection,
            config.identity_version,
        )?;
        let version = server.version.clone();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let initial_thread = if eager_thread {
            let mut handler = TuiManagedHandler::new(event_tx.clone());
            let thread_id =
                ensure_thread_loaded(&mut server, &mut thread, &mut store, &config, &mut handler)
                    .await?;
            conversation = ConversationRef::Managed {
                conversation_id: thread.conversation_id(),
                connection: config.connection.clone(),
                thread_id: thread_id.clone(),
            };
            send_event(&event_tx, ManagedTuiEvent::ThreadOpened(thread_id.clone())).await?;
            Some(thread_id)
        } else {
            initial_thread
        };
        let active = Arc::new(Mutex::new(None));
        let task_active = Arc::clone(&active);
        let task = tokio::spawn(run_actor(
            server,
            models,
            config,
            workspace_host,
            conversation.clone(),
            store,
            thread,
            command_rx,
            event_tx,
            task_active,
        ));
        Ok((
            Self {
                commands: command_tx,
                events: event_rx,
                active,
                task,
                models: available,
                initial_thread,
                version,
                selected_model,
            },
            conversation,
        ))
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

    pub(crate) async fn select_model(&self, model: String) -> Result<ModelSelection, String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ManagedTuiCommand::SelectModel { model, reply })
            .await
            .map_err(|_| "managed runtime stopped".to_owned())?;
        response
            .await
            .map_err(|_| "managed runtime stopped before selecting the model".to_owned())?
    }

    pub(crate) async fn set_reasoning(
        &self,
        effort: Option<String>,
    ) -> Result<ModelSelection, String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ManagedTuiCommand::SetReasoning { effort, reply })
            .await
            .map_err(|_| "managed runtime stopped".to_owned())?;
        response
            .await
            .map_err(|_| "managed runtime stopped before updating reasoning".to_owned())?
    }

    pub(crate) async fn clear(&self) -> Result<(), String> {
        self.commands
            .send(ManagedTuiCommand::Clear)
            .await
            .map_err(|_| "managed runtime stopped".to_owned())
    }

    pub(crate) async fn archive(&self, thread_id: String) -> Result<bool, String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ManagedTuiCommand::Archive { thread_id, reply })
            .await
            .map_err(|_| "managed runtime stopped".to_owned())?;
        response
            .await
            .map_err(|_| "managed runtime stopped before archiving the conversation".to_owned())?
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
    workspace_host: Option<Arc<WorkspaceHost>>,
    mut conversation: ConversationRef,
    mut store: ManagedThreadStore,
    mut thread: ManagedThreadState,
    mut commands: mpsc::Receiver<ManagedTuiCommand>,
    events: mpsc::Sender<ManagedTuiEvent>,
    active: Arc<Mutex<Option<ActiveTurn>>>,
) -> Result<(), CodexError> {
    let mut selection = config.selection.clone();
    let mut memory_maintenance = None;
    while let Some(command) = commands.recv().await {
        match command {
            ManagedTuiCommand::Submit {
                operation_id,
                input,
                images,
            } => {
                if images.is_empty() && crate::memory::parse_natural(&input).is_some() {
                    let result = super::local_memory_reply(
                        config.memory.as_ref(),
                        thread.conversation_id(),
                        &input,
                    )
                    .await;
                    let (text, error) = match result {
                        Ok(text) => (text, None),
                        Err(error) => (format!("Memory control failed: {error}"), Some(error)),
                    };
                    send_event(
                        &events,
                        ManagedTuiEvent::Notification(ManagedClientEvent::AssistantDelta(text)),
                    )
                    .await?;
                    send_event(
                        &events,
                        ManagedTuiEvent::Notification(ManagedClientEvent::TurnCompleted {
                            status: if error.is_some() {
                                "failed"
                            } else {
                                "completed"
                            }
                            .into(),
                            error: error.clone(),
                        }),
                    )
                    .await?;
                    send_event(
                        &events,
                        ManagedTuiEvent::TurnFinished {
                            operation_id,
                            error,
                        },
                    )
                    .await?;
                    continue;
                }
                let lease = if let Some(workspace_host) = workspace_host.as_ref() {
                    match workspace_host
                        .acquire_foreground_root(conversation.clone())
                        .await
                    {
                        Ok(lease) => Some(lease),
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
                    }
                } else {
                    None
                };
                let image_urls = match images
                    .iter()
                    .map(|image| {
                        crate::vision::MediaResolver::new(
                            config.artifact_store.clone(),
                            crate::artifact::MAX_ARTIFACT_BYTES,
                        )
                        .resolve_openai_data_url(&image.image)
                        .map_err(anyhow::Error::from)
                    })
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
                    conversation_id: thread.conversation_id(),
                    connection: config.connection.clone(),
                    thread_id: thread_id.clone(),
                };
                server.set_usage_identity(thread.conversation_id().to_string(), operation_id);
                let _foreground = match super::memory_context::foreground(config.memory.as_ref()) {
                    Ok(lease) => lease,
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
                let input = match super::memory_context::prepare(
                    config.memory.as_ref(),
                    thread.conversation_id(),
                    &input,
                )
                .await
                {
                    Ok(text) => text,
                    Err(error) => {
                        clear_active(&active, operation_id);
                        send_event(
                            &events,
                            ManagedTuiEvent::TurnFinished {
                                operation_id,
                                error: Some(error),
                            },
                        )
                        .await?;
                        drop(lease);
                        continue;
                    }
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
                            image_urls,
                        },
                        &cancellation,
                        &mut handler,
                    )
                    .await;
                if let Err(error) = &result {
                    let diagnostic = super::failure::diagnostic(
                        error,
                        Some(operation_id),
                        thread.conversation_id(),
                        &config,
                    );
                    crate::diagnostics::emit_terminal(diagnostic.clone());
                    send_event(
                        &events,
                        ManagedTuiEvent::Notification(ManagedClientEvent::TerminalDiagnostic(
                            diagnostic,
                        )),
                    )
                    .await?;
                }
                clear_active(&active, operation_id);
                drop(_foreground);
                super::memory_context::maintain(config.memory.as_ref(), &mut memory_maintenance);
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
            ManagedTuiCommand::SelectModel { model, reply } => {
                let result = models
                    .select(&config.connection, &model)
                    .map_err(|error| error.to_string());
                if let Ok(next) = &result {
                    selection = next.clone();
                    config.model.clone_from(&next.model);
                }
                let _ = reply.send(result);
            }
            ManagedTuiCommand::SetReasoning { effort, reply } => {
                let result = models
                    .update_reasoning_effort(effort)
                    .map_err(|error| error.to_string());
                if let Ok(next) = &result {
                    selection = next.clone();
                }
                let _ = reply.send(result);
            }
            ManagedTuiCommand::Clear => {
                store
                    .set_thread(None, None, None)
                    .map_err(|error| CodexError::Io(error.to_string()))?;
                let conversation_id = ConversationId::new();
                thread = ManagedThreadState::New { conversation_id };
                conversation = ConversationRef::NewManaged {
                    conversation_id,
                    connection: config.connection.clone(),
                };
                send_event(&events, ManagedTuiEvent::Cleared).await?;
            }
            ManagedTuiCommand::Archive { thread_id, reply } => {
                let result = store
                    .archive_thread(&thread_id)
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
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
