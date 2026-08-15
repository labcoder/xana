//! Shared TUI lifecycle over native and managed execution-owner adapters.
//!
//! The runner owns terminal input, bounded frame cadence, follow-up dispatch,
//! and shutdown ordering. Execution owners translate their own events and
//! effects without leaking provider-specific state into the view model.

use super::{
    PreparedTui, clipboard,
    effects::{dispatch_effect, dispatch_managed_effect},
    input::TerminalInput,
    session,
    state::{TuiState, UpdateEffect},
    terminal_input_action, view,
};
use crate::{
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    managed::{codex::ApprovalDecision, codex::CodexAppServer},
    managed_execution::{ManagedChatConfig, ManagedTuiDriver, ManagedTuiEvent},
    model_catalog::ModelManager,
    native_runtime::{AgentEvent, OperationState, RuntimeHandle},
    plain_terminal::{ChatExit, ChatHeader},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use futures::FutureExt;
use std::{panic::AssertUnwindSafe, path::Path, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const WORK_INDICATOR_INTERVAL: Duration = Duration::from_millis(250);

trait ExecutionOwner {
    type Event;

    async fn next_event(&mut self) -> Result<Self::Event>;

    async fn apply_event(&mut self, state: &mut TuiState, event: Self::Event) -> Result<()>;

    async fn dispatch(
        &mut self,
        effect: UpdateEffect,
        state: &mut TuiState,
        preferences_path: &Path,
        session_preferences: &mut session::SessionPreferenceStore,
        clipboard: &mut clipboard::Clipboard,
    ) -> Result<Option<ChatExit>>;

    async fn shutdown(self, state: &TuiState) -> Result<()>;
}

async fn run<Owner: ExecutionOwner>(
    mut prepared: PreparedTui,
    mut state: TuiState,
    mut owner: Owner,
    mut session_preferences: session::SessionPreferenceStore,
) -> Result<ChatExit> {
    let outcome = drive(
        &mut prepared,
        &mut state,
        &mut owner,
        &mut session_preferences,
    )
    .await;
    let shutdown = owner.shutdown(&state).await;

    match (outcome, shutdown) {
        (Ok(exit), Ok(())) => Ok(exit),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.context("could not shut down TUI execution owner")),
        (Err(error), Err(shutdown_error)) => Err(error.context(format!(
            "TUI execution-owner shutdown also failed: {shutdown_error:#}"
        ))),
    }
}

async fn drive<Owner: ExecutionOwner>(
    prepared: &mut PreparedTui,
    state: &mut TuiState,
    owner: &mut Owner,
    session_preferences: &mut session::SessionPreferenceStore,
) -> Result<ChatExit> {
    let mut input = TerminalInput::new();
    let mut terminal_area = prepared.terminal.terminal_mut().size()?;
    let mut frames = tokio::time::interval(FRAME_INTERVAL);
    frames.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut work_indicator = tokio::time::interval_at(
        Instant::now() + WORK_INDICATOR_INTERVAL,
        WORK_INDICATOR_INTERVAL,
    );
    work_indicator.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut dirty = true;

    let exit = loop {
        if let Some(effect) = state.next_followup() {
            dirty = true;
            if let Some(exit) = owner
                .dispatch(
                    effect,
                    state,
                    &prepared.preferences_path,
                    session_preferences,
                    &mut prepared.clipboard,
                )
                .await?
            {
                break exit;
            }
        }

        tokio::select! {
            biased;
            _ = frames.tick(), if dirty => {
                prepared
                    .terminal
                    .terminal_mut()
                .draw(|frame| view::render(frame, state, prepared.profile))
                    .context("could not draw Xana TUI")?;
                terminal_area = prepared.terminal.terminal_mut().size()?;
                dirty = false;
            }
            terminal_event = input.next() => {
                dirty = true;
                let Some(terminal_event) = terminal_event else {
                    let exit = owner
                        .dispatch(
                            UpdateEffect::Quit,
                            state,
                            &prepared.preferences_path,
                            session_preferences,
                            &mut prepared.clipboard,
                        )
                        .await?;
                    break exit.unwrap_or(ChatExit::Quit);
                };
                let terminal_event = terminal_event.context("terminal input failed")?;
                if let Some(action) = terminal_input_action(terminal_event, state, terminal_area.into()) {
                    let effect = state.update_input(action);
                    if let Some(exit) = owner
                        .dispatch(
                            effect,
                            state,
                            &prepared.preferences_path,
                            session_preferences,
                            &mut prepared.clipboard,
                        )
                        .await?
                    {
                        break exit;
                    }
                }
            }
            owner_event = owner.next_event() => {
                dirty = true;
                owner.apply_event(state, owner_event?).await?;
            }
            _ = work_indicator.tick(), if !prepared.profile.reduced_motion && state.busy && state.active_operation.is_some() => {
                dirty |= state.advance_work_indicator();
            }
        }
    };

    Ok(exit)
}

struct NativeOwner<'a> {
    client: EmbeddedClient,
    header: &'a ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
    active_root: Option<ActiveRootLease>,
    vision_events: mpsc::Receiver<NativeVisionEvent>,
    vision_sender: mpsc::Sender<NativeVisionEvent>,
    vision_cancellation: Option<(crate::identity::OperationId, CancellationToken)>,
    vision_task: Option<JoinHandle<()>>,
}

enum NativeOwnerEvent {
    Runtime(Box<AgentEvent>),
    Vision(Box<NativeVisionEvent>),
}

enum NativeVisionEvent {
    Prepared {
        operation_id: crate::identity::OperationId,
        prepared: crate::app::vision::PreparedVisionTurn,
    },
    Failed {
        operation_id: crate::identity::OperationId,
        input: String,
        images: Vec<crate::vision::ImageAttachment>,
        route: String,
        reason: String,
    },
}

impl ExecutionOwner for NativeOwner<'_> {
    type Event = NativeOwnerEvent;

    async fn next_event(&mut self) -> Result<Self::Event> {
        tokio::select! {
            event = self.client.next_event() => event.map(Box::new).map(NativeOwnerEvent::Runtime).map_err(anyhow::Error::new),
            event = self.vision_events.recv() => event
                .map(Box::new)
                .map(NativeOwnerEvent::Vision)
                .ok_or_else(|| anyhow::anyhow!("vision preparation channel stopped")),
        }
    }

    async fn apply_event(&mut self, state: &mut TuiState, event: Self::Event) -> Result<()> {
        let event = match event {
            NativeOwnerEvent::Vision(event) => match *event {
                NativeVisionEvent::Prepared {
                    operation_id,
                    prepared,
                } => {
                    self.finish_vision_task().await;
                    self.vision_cancellation = None;
                    state.finish_vision_preparation(&prepared.receipt);
                    let result = self
                        .client
                        .send(crate::native_runtime::RuntimeCommand::SubmitTurn {
                            operation_id,
                            input: prepared.model_input,
                        })
                        .await
                        .context("native TUI runtime stopped after vision preparation")?;
                    if !result.accepted {
                        self.active_root = None;
                        state.fail_vision_preparation(
                            result
                                .reason
                                .unwrap_or_else(|| "prepared vision turn was rejected".to_owned()),
                        );
                    }
                    return Ok(());
                }
                NativeVisionEvent::Failed {
                    operation_id,
                    input,
                    images,
                    route,
                    reason,
                } => {
                    self.finish_vision_task().await;
                    self.vision_cancellation = None;
                    self.active_root = None;
                    state.fail_vision_preparation(format!(
                        "Vision specialist failed before turn {operation_id}: {reason}"
                    ));
                    state.restore_submission(
                        input,
                        images,
                        Some(route),
                        "Vision specialist failed; draft and images restored".to_owned(),
                    );
                    return Ok(());
                }
            },
            NativeOwnerEvent::Runtime(event) => *event,
        };
        let terminal = matches!(
            event,
            AgentEvent::OperationStateChanged {
                state: OperationState::Finished(_) | OperationState::Suspended,
                ..
            } | AgentEvent::OperationFailed { .. }
        );
        state.apply_runtime(&event);
        if terminal {
            self.active_root = None;
        }
        if let Ok(snapshot) = self.workspace_host.snapshot() {
            state.refresh_sessions(snapshot);
        }
        Ok(())
    }

    async fn dispatch(
        &mut self,
        effect: UpdateEffect,
        state: &mut TuiState,
        preferences_path: &Path,
        session_preferences: &mut session::SessionPreferenceStore,
        clipboard: &mut clipboard::Clipboard,
    ) -> Result<Option<ChatExit>> {
        match effect {
            UpdateEffect::Submit {
                operation_id,
                input,
                images,
                vision_route,
            } if !images.is_empty() => {
                let descriptor = self
                    .header
                    .models
                    .descriptor(&self.header.provider_name, &self.header.model)
                    .context("could not resolve selected model capabilities")?;
                let plan = match self.header.vision.route_turn(
                    descriptor.input_modalities.contains("image"),
                    vision_route.as_deref(),
                ) {
                    Ok(crate::app::vision::VisionTurnRoute::Native) => {
                        return dispatch_effect(
                            UpdateEffect::Submit {
                                operation_id,
                                input,
                                images,
                                vision_route: None,
                            },
                            state,
                            &self.client,
                            self.header,
                            &self.workspace_host,
                            &self.conversation,
                            &mut self.active_root,
                            preferences_path,
                            session_preferences,
                            clipboard,
                        )
                        .await;
                    }
                    Ok(crate::app::vision::VisionTurnRoute::SpecialistDenied(plan)) => {
                        state.restore_submission(
                            input,
                            images,
                            Some(plan.route.name),
                            "Vision specialist use is denied by the active profile".to_owned(),
                        );
                        return Ok(None);
                    }
                    Ok(crate::app::vision::VisionTurnRoute::SpecialistAllowed(plan)) => {
                        return self
                            .start_vision(operation_id, input, images, *plan, None, state)
                            .await;
                    }
                    Ok(crate::app::vision::VisionTurnRoute::SpecialistApprovalRequired(plan)) => {
                        *plan
                    }
                    Err(error) => {
                        state.restore_submission(
                            input,
                            images,
                            vision_route,
                            format!(
                                "Image input is unsupported for {}/{} and no usable vision specialist is available: {error:#}. Run `xana connect vision` or select an image-capable model.",
                                self.header.provider_name, self.header.model
                            ),
                        );
                        return Ok(None);
                    }
                };
                state.request_vision_approval(operation_id, input, images, plan);
                Ok(None)
            }
            UpdateEffect::PrepareVision {
                operation_id,
                input,
                images,
                plan,
                decision,
            } => {
                self.start_vision(operation_id, input, images, *plan, Some(decision), state)
                    .await
            }
            UpdateEffect::Interrupt { operation_id }
                if self
                    .vision_cancellation
                    .as_ref()
                    .is_some_and(|(active, _)| *active == operation_id) =>
            {
                if let Some((_, cancellation)) = self.vision_cancellation.take() {
                    cancellation.cancel();
                }
                state.set_status("Cancelling vision specialist preparation…");
                Ok(None)
            }
            effect => {
                dispatch_effect(
                    effect,
                    state,
                    &self.client,
                    self.header,
                    &self.workspace_host,
                    &self.conversation,
                    &mut self.active_root,
                    preferences_path,
                    session_preferences,
                    clipboard,
                )
                .await
            }
        }
    }

    async fn shutdown(mut self, _state: &TuiState) -> Result<()> {
        if let Some((_, cancellation)) = self.vision_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(mut task) = self.vision_task.take()
            && tokio::time::timeout(Duration::from_millis(750), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }
}

impl NativeOwner<'_> {
    async fn start_vision(
        &mut self,
        operation_id: crate::identity::OperationId,
        input: String,
        images: Vec<crate::vision::ImageAttachment>,
        plan: crate::app::vision::VisionPlan,
        decision: Option<crate::outbound::OutboundApprovalDecision>,
        state: &mut TuiState,
    ) -> Result<Option<ChatExit>> {
        let lease = match self.workspace_host.acquire_root(self.conversation.clone()) {
            Ok(lease) => lease,
            Err(error) => {
                state.restore_submission(
                    input,
                    images,
                    Some(plan.route.name),
                    format!("could not start vision turn: {error}"),
                );
                return Ok(None);
            }
        };
        let cancellation = CancellationToken::new();
        self.vision_cancellation = Some((operation_id, cancellation.clone()));
        self.active_root = Some(lease);
        state.mark_submitted(operation_id, input.clone());
        state.set_status(format!(
            "Analyzing {} image(s) via {}…",
            images.len(),
            plan.route.name
        ));
        let service = self.header.vision.clone();
        let source_images = images
            .iter()
            .map(|attachment| attachment.image.clone())
            .collect();
        let route = plan.route.name.clone();
        let events = self.vision_sender.clone();
        let task = tokio::spawn(async move {
            let execution = AssertUnwindSafe(service.execute(
                operation_id,
                input.clone(),
                source_images,
                plan,
                decision,
                cancellation,
            ))
            .catch_unwind()
            .await;
            let event = match execution {
                Ok(Ok(prepared)) => NativeVisionEvent::Prepared {
                    operation_id,
                    prepared,
                },
                Ok(Err(error)) => NativeVisionEvent::Failed {
                    operation_id,
                    input,
                    images,
                    route,
                    reason: format!("{error:#}"),
                },
                Err(_) => {
                    crate::diagnostics::record_task_panic("vision-specialist");
                    NativeVisionEvent::Failed {
                        operation_id,
                        input,
                        images,
                        route,
                        reason: "vision specialist task panicked; a crash report was recorded"
                            .to_owned(),
                    }
                }
            };
            let _ = events.send(event).await;
        });
        self.vision_task = Some(task);
        Ok(None)
    }

    async fn finish_vision_task(&mut self) {
        if let Some(task) = self.vision_task.take()
            && let Err(error) = task.await
        {
            crate::diagnostics::record_task_panic("vision-specialist");
            debug_assert!(error.is_cancelled() || error.is_panic());
        }
    }
}

struct ManagedOwner {
    driver: ManagedTuiDriver,
    workspace_host: Arc<WorkspaceHost>,
    workspace: std::path::PathBuf,
    artifact_store: crate::artifact::ArtifactStore,
    owner: crate::identity::PrincipalId,
    connection: String,
    pending_approval: Option<oneshot::Sender<ApprovalDecision>>,
}

impl ExecutionOwner for ManagedOwner {
    type Event = ManagedTuiEvent;

    async fn next_event(&mut self) -> Result<Self::Event> {
        self.driver.next_event().await.ok_or_else(|| {
            crate::diagnostics::emit(
                crate::diagnostics::DiagnosticFact::new(
                    crate::config::DiagnosticLevel::Error,
                    crate::config::DiagnosticTarget::Runtime,
                    crate::diagnostics::EventKind::ManagedRuntimeExited,
                    crate::diagnostics::EventOutcome::Failed,
                )
                .subject(&self.connection),
            );
            anyhow::anyhow!("Codex managed runtime stopped while the TUI was attached")
        })
    }

    async fn apply_event(&mut self, state: &mut TuiState, event: Self::Event) -> Result<()> {
        match event {
            ManagedTuiEvent::Notification(event) => state.apply_managed_event(&event),
            ManagedTuiEvent::Approval { request, reply } => {
                if let Some(stale) = self.pending_approval.replace(reply) {
                    let _ = stale.send(ApprovalDecision::Cancel);
                }
                state.open_managed_approval(request);
            }
            ManagedTuiEvent::ThreadOpened(thread_id) => {
                state.set_managed_thread(&self.connection, thread_id);
            }
            ManagedTuiEvent::TurnFinished {
                operation_id,
                error,
            } => {
                state.finish_managed_turn(operation_id, error);
                self.pending_approval = None;
            }
            ManagedTuiEvent::Cleared => state.managed_cleared(&self.connection),
        }
        if let Ok(snapshot) = self.workspace_host.snapshot() {
            state.refresh_sessions(snapshot);
        }
        Ok(())
    }

    async fn dispatch(
        &mut self,
        effect: UpdateEffect,
        state: &mut TuiState,
        preferences_path: &Path,
        session_preferences: &mut session::SessionPreferenceStore,
        clipboard: &mut clipboard::Clipboard,
    ) -> Result<Option<ChatExit>> {
        dispatch_managed_effect(
            effect,
            state,
            &self.driver,
            &self.workspace_host,
            &self.workspace,
            &self.artifact_store,
            self.owner,
            preferences_path,
            session_preferences,
            &mut self.pending_approval,
            clipboard,
        )
        .await
    }

    async fn shutdown(mut self, state: &TuiState) -> Result<()> {
        if let Some(operation_id) = state.active_operation {
            self.driver.interrupt(operation_id);
        }
        if let Some(reply) = self.pending_approval.take() {
            let _ = reply.send(ApprovalDecision::Cancel);
        }
        self.driver.shutdown().await.map_err(anyhow::Error::new)
    }
}

pub(crate) async fn run_native(
    prepared: PreparedTui,
    runtime: RuntimeHandle,
    header: &ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
) -> Result<ChatExit> {
    let seed = ClientSnapshotSeed {
        session_id: header.session_id,
        connection: header.provider_name.clone(),
        execution_owner: "native".to_owned(),
        model: header.model.clone(),
        reasoning_effort: None,
        children: header.children.clone(),
    };
    let client = EmbeddedClient::from_runtime(runtime, seed);
    let mut state = TuiState::from_client(
        &client,
        prepared.preferences.composer,
        prepared.preferences.activity.into(),
        conversation.clone(),
    );
    let frontend_dir = prepared
        .preferences_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let session_preferences =
        session::SessionPreferenceStore::load(frontend_dir, &header.workspace_root);
    state.set_rail_expanded(session_preferences.rail_expanded());
    state.refresh_sessions(workspace_host.snapshot()?);

    let (vision_sender, vision_events) = mpsc::channel(1);
    run(
        prepared,
        state,
        NativeOwner {
            client,
            header,
            workspace_host,
            conversation,
            active_root: None,
            vision_events,
            vision_sender,
            vision_cancellation: None,
            vision_task: None,
        },
        session_preferences,
    )
    .await
}

pub(crate) async fn run_managed(
    prepared: PreparedTui,
    server: CodexAppServer,
    models: ModelManager,
    config: ManagedChatConfig,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
) -> Result<ChatExit> {
    let connection = config.connection.clone();
    let workspace = config.workspace.clone();
    let artifact_store = config.artifact_store.clone();
    let principal = config.owner;
    let workspace_host = Arc::new(workspace_host);
    let driver = ManagedTuiDriver::start(
        server,
        models,
        config,
        Arc::clone(&workspace_host),
        conversation.clone(),
    )
    .await?;
    let session = driver
        .initial_thread
        .clone()
        .unwrap_or_else(|| "new".to_owned());
    let initial_model = driver.selected_model.clone();
    let mut state = TuiState::from_managed(
        connection.clone(),
        initial_model,
        session,
        prepared.preferences.composer,
        prepared.preferences.activity.into(),
        conversation,
    );
    state.set_status(format!("Managed Codex app-server {} ready", driver.version));
    let frontend_dir = prepared
        .preferences_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let session_preferences = session::SessionPreferenceStore::load(frontend_dir, &workspace);
    state.set_rail_expanded(session_preferences.rail_expanded());
    state.refresh_sessions(workspace_host.snapshot()?);

    run(
        prepared,
        state,
        ManagedOwner {
            driver,
            workspace_host,
            workspace,
            artifact_store,
            owner: principal,
            connection,
            pending_approval: None,
        },
        session_preferences,
    )
    .await
}
