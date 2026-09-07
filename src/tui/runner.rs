//! Shared TUI lifecycle over native and managed execution-owner adapters.
//!
//! The runner owns terminal input, bounded frame cadence, follow-up dispatch,
//! and shutdown ordering. Execution owners translate their own events and
//! effects without leaking provider-specific state into the view model.

use super::{
    PreparedTui, TuiRunOutcome, clipboard,
    effects::{dispatch_effect, dispatch_managed_effect},
    input::TerminalInput,
    session,
    state::{TuiState, UpdateEffect},
    terminal_input_action, view,
};
use crate::{
    app::{ChatExit, ChatHeader},
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    managed::{codex::ApprovalDecision, codex::CodexAppServer},
    managed_execution::{ManagedChatConfig, ManagedTuiDriver, ManagedTuiEvent},
    model_catalog::ModelManager,
    native_runtime::{AgentEvent, OperationState, RuntimeHandle},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use futures::FutureExt;
use std::{ops::Range, panic::AssertUnwindSafe, path::Path, sync::Arc, time::Duration};
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

    fn artifact_store(&self) -> &crate::artifact::ArtifactStore;
}

async fn run<Owner: ExecutionOwner>(
    mut prepared: PreparedTui,
    mut state: TuiState,
    mut owner: Owner,
    mut session_preferences: session::SessionPreferenceStore,
    mut composer_history: crate::terminal_productivity::ComposerHistoryStore,
) -> Result<TuiRunOutcome> {
    let outcome = drive(
        &mut prepared,
        &mut state,
        &mut owner,
        &mut session_preferences,
        &mut composer_history,
    )
    .await;
    let shutdown = owner.shutdown(&state).await;

    match (outcome, shutdown) {
        (Ok(exit), Ok(())) => Ok(TuiRunOutcome {
            exit,
            continuation: state.into_continuation(),
        }),
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
    composer_history: &mut crate::terminal_productivity::ComposerHistoryStore,
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
    let mut inline_image_poll = tokio::time::interval(FRAME_INTERVAL);
    inline_image_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut dirty = true;
    let (completion_sender, mut completion_receiver) = mpsc::channel(1);
    let mut completion_generation = 0_u64;
    let mut completion_cancellation: Option<CancellationToken> = None;
    let mut completion_task: Option<JoinHandle<()>> = None;

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
                let inline_preview = current_artifact_id(state)
                    .and_then(|artifact_id| prepared.inline_images.protocol_for(artifact_id));
                prepared
                    .terminal
                    .terminal_mut()
                .draw(|frame| view::render_with_inline(frame, state, prepared.profile, inline_preview))
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
                    while let Some(entry) = state.take_pending_history_entry() {
                        if let Err(error) = composer_history.record(&entry) {
                            state.push_activity(format!("composer history was not saved: {error:#}"));
                        }
                    }
                    if let UpdateEffect::CompleteFile { query, replacement } = effect {
                        if let Some(cancellation) = completion_cancellation.take() {
                            cancellation.cancel();
                        }
                        if let Some(task) = completion_task.take() {
                            task.abort();
                        }
                        completion_generation = completion_generation.saturating_add(1);
                        let generation = completion_generation;
                        let workspace = state.workspace.clone();
                        let cancellation = CancellationToken::new();
                        completion_cancellation = Some(cancellation.clone());
                        let sender = completion_sender.clone();
                        completion_task = Some(tokio::task::spawn_blocking(move || {
                            let result = crate::terminal_productivity::complete_workspace_paths(
                                &workspace,
                                &query,
                                crate::terminal_productivity::MAX_FILE_COMPLETIONS,
                                &cancellation,
                            )
                            .map_err(|error| format!("{error:#}"));
                            let _ = sender.blocking_send(FileCompletionFinished {
                                generation,
                                query,
                                replacement,
                                result,
                            });
                        }));
                        dirty = true;
                        continue;
                    }
                    if let UpdateEffect::ControlCommand { family, arguments } = &effect
                        && inline_control_command(family, arguments)
                    {
                        let title = format!("/{family} {arguments}");
                        match crate::app::run_tui_control_command(
                            &prepared.paths,
                            family,
                            arguments,
                        )
                        .await
                        {
                            Ok(output) => state.show_command_result(
                                title,
                                if output.trim().is_empty() {
                                    "Command completed successfully.".to_owned()
                                } else {
                                    output
                                },
                            ),
                            Err(error) => state.show_command_error(title, format!("{error:#}")),
                        }
                        continue;
                    }
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
                    if let Some(status) = prepared.inline_images.reconcile(
                        previewed_artifact(state),
                        owner.artifact_store(),
                    ) {
                        state.set_inline_preview_status(status);
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
            _ = inline_image_poll.tick(), if prepared.inline_images.is_pending() => {
                if let Some(outcome) = prepared.inline_images.finish_if_ready().await {
                    state.finish_inline_preview(outcome.artifact_id, outcome.message);
                    dirty = true;
                }
            }
            completion = completion_receiver.recv() => {
                let Some(completion) = completion else {
                    continue;
                };
                if completion.generation != completion_generation {
                    continue;
                }
                completion_cancellation = None;
                if let Some(task) = completion_task.take() {
                    let _ = task.await;
                }
                match completion.result {
                    Ok(choices) => state.show_file_completions(
                        completion.query,
                        completion.replacement,
                        choices,
                    ),
                    Err(reason) => state.fail_file_completion(reason),
                }
                dirty = true;
            }
        }
    };

    if let Some(cancellation) = completion_cancellation {
        cancellation.cancel();
    }
    if let Some(task) = completion_task {
        let _ = task.await;
    }

    Ok(exit)
}

struct FileCompletionFinished {
    generation: u64,
    query: String,
    replacement: Range<usize>,
    result: std::result::Result<Vec<String>, String>,
}

fn previewed_artifact(state: &TuiState) -> Option<&crate::artifact::ArtifactRecord> {
    match &state.overlay {
        Some(super::state::Overlay::Artifact {
            artifact,
            preview: Some(_),
            ..
        }) => Some(&artifact.record),
        _ => None,
    }
}

fn current_artifact_id(state: &TuiState) -> Option<crate::identity::ArtifactId> {
    previewed_artifact(state).map(|record| record.reference.id)
}

fn inline_control_command(family: &str, arguments: &str) -> bool {
    let action = arguments.split_whitespace().next().unwrap_or("list");
    matches!(
        (family, action),
        ("mcp", "list") | ("profile", "create") | ("conversation", "search")
    )
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
                        .send(crate::native_runtime::RuntimeCommand::SubmitDerivedTurn {
                            operation_id,
                            input: prepared.model_input,
                            owner_input: prepared.owner_input,
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
                state: OperationState::Finished(_),
                ..
            } | AgentEvent::OperationFailed { .. }
        );
        state.apply_runtime(&event);
        state.sync_client_snapshot(self.client.snapshot());
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

    fn artifact_store(&self) -> &crate::artifact::ArtifactStore {
        &self.header.artifact_store
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
        let lease = match self
            .workspace_host
            .acquire_foreground_root(self.conversation.clone())
            .await
        {
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
    pending_memory: Option<ManagedMemoryApproval>,
}

struct ManagedMemoryApproval {
    request: crate::permission::PermissionRequest,
    reply: oneshot::Sender<crate::permission::ControllerDecision>,
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
            ManagedTuiEvent::MemoryAudit(fact) => {
                state.apply_runtime(&crate::native_runtime::AgentEvent::PermissionAudited { fact })
            }
            ManagedTuiEvent::Approval { request, reply } => {
                if let Some(stale) = self.pending_approval.replace(reply) {
                    let _ = stale.send(ApprovalDecision::Cancel);
                }
                state.open_managed_approval(request);
            }
            ManagedTuiEvent::MemoryApproval { request, reply } => {
                if state.active_operation != Some(request.operation_id) {
                    let _ = reply.send(crate::permission::ControllerDecision::Deny);
                } else {
                    state.apply_runtime(&crate::native_runtime::AgentEvent::PermissionRequested {
                        request: request.clone(),
                    });
                    self.pending_memory = Some(ManagedMemoryApproval { request, reply });
                }
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
                self.pending_memory = None;
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
        if let UpdateEffect::DecideNativeApproval {
            operation_id,
            invocation_id,
            decision,
        } = effect
        {
            let matches = self.pending_memory.as_ref().is_some_and(|pending| {
                pending.request.operation_id == operation_id
                    && pending.request.invocation_id == invocation_id
            });
            if matches {
                let pending = self.pending_memory.take().expect("matched pending request");
                let _ = pending.reply.send(decision);
            } else {
                state.status = "Memory approval is no longer pending".into();
            }
            return Ok(None);
        }
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
        self.pending_memory = None;
        self.driver.shutdown().await.map_err(anyhow::Error::new)
    }

    fn artifact_store(&self) -> &crate::artifact::ArtifactStore {
        &self.artifact_store
    }
}

pub(crate) async fn run_native(
    mut prepared: PreparedTui,
    runtime: RuntimeHandle,
    header: &ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
) -> Result<TuiRunOutcome> {
    let seed = ClientSnapshotSeed {
        session_id: header.session_id,
        connection: header.provider_name.clone(),
        execution_owner: "native".to_owned(),
        model: header.model.clone(),
        reasoning_effort: None,
        host_location: crate::frontend::semantic::HostLocationV1::Embedded,
        approval_policy: header.permission_mode.as_str().to_owned(),
        children: header.children.clone(),
        resource_policy: header.resource_policy.clone(),
    };
    let client = EmbeddedClient::from_runtime(runtime, seed);
    let mut state = TuiState::from_client(
        &client,
        prepared.preferences.composer,
        prepared.preferences.activity.into(),
        conversation.clone(),
    );
    match workspace_host.conversation_history_page(&conversation, None, 1) {
        Ok(Some(page)) => state.seed_saved_history_cursor(page.total),
        Ok(None) => {}
        Err(error) => state.set_status(format!("Saved history paging is unavailable: {error}")),
    }
    state.set_inline_image_capability(prepared.inline_images.capability_summary());
    if let Some(continuation) = prepared.continuation.take() {
        state.restore_continuation(continuation);
    }
    let frontend_dir = prepared
        .preferences_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let session_preferences =
        session::SessionPreferenceStore::load(frontend_dir, &header.workspace_root);
    state.set_rail_expanded(session_preferences.rail_expanded());
    state.refresh_sessions(workspace_host.snapshot()?);
    let (composer_history, history) = crate::terminal_productivity::ComposerHistoryStore::open(
        &prepared.paths,
        &header.workspace_root,
    )?;
    state.install_composer_history(history.entries, history.warning);

    state.disclose_learning();
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
        composer_history,
    )
    .await
}

pub(crate) async fn run_managed(
    mut prepared: PreparedTui,
    server: CodexAppServer,
    models: ModelManager,
    config: ManagedChatConfig,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
) -> Result<TuiRunOutcome> {
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
    state.set_inline_image_capability(prepared.inline_images.capability_summary());
    if let Some(continuation) = prepared.continuation.take() {
        state.restore_continuation(continuation);
    }
    state.set_status(format!("Managed Codex app-server {} ready", driver.version));
    let frontend_dir = prepared
        .preferences_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let session_preferences = session::SessionPreferenceStore::load(frontend_dir, &workspace);
    state.set_rail_expanded(session_preferences.rail_expanded());
    state.refresh_sessions(workspace_host.snapshot()?);
    let (composer_history, history) =
        crate::terminal_productivity::ComposerHistoryStore::open(&prepared.paths, &workspace)?;
    state.install_composer_history(history.entries, history.warning);

    state.disclose_learning();
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
            pending_memory: None,
        },
        session_preferences,
        composer_history,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::inline_control_command;

    #[test]
    fn only_bounded_noninteractive_commands_stay_inside_the_tui() {
        assert!(inline_control_command("mcp", "list"));
        assert!(inline_control_command(
            "profile",
            "create review --connection ollama --model qwen3:8b"
        ));
        assert!(!inline_control_command("mcp", "read server resource"));
        assert!(!inline_control_command("profile", "delete review --yes"));
        assert!(!inline_control_command("image", "generate a portrait"));
    }
}
