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
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{sync::oneshot, time::MissedTickBehavior};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

trait ExecutionOwner {
    type Event;

    async fn next_event(&mut self) -> Option<Self::Event>;

    fn apply_event(&mut self, state: &mut TuiState, event: Self::Event) -> Result<()>;

    async fn dispatch(
        &mut self,
        effect: UpdateEffect,
        state: &mut TuiState,
        preferences_path: &Path,
        session_preferences: &mut session::SessionPreferenceStore,
        clipboard: &mut clipboard::Clipboard,
    ) -> Result<Option<ChatExit>>;

    async fn shutdown(self, state: &TuiState) -> Result<()>;

    fn stopped_message() -> &'static str;
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
                let Some(owner_event) = owner_event else {
                    anyhow::bail!(Owner::stopped_message());
                };
                owner.apply_event(state, owner_event)?;
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
}

impl ExecutionOwner for NativeOwner<'_> {
    type Event = AgentEvent;

    async fn next_event(&mut self) -> Option<Self::Event> {
        self.client.next_event().await
    }

    fn apply_event(&mut self, state: &mut TuiState, event: Self::Event) -> Result<()> {
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

    async fn shutdown(self, _state: &TuiState) -> Result<()> {
        Ok(())
    }

    fn stopped_message() -> &'static str {
        "Xana's foreground runtime stopped while the TUI was attached"
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

    async fn next_event(&mut self) -> Option<Self::Event> {
        self.driver.next_event().await
    }

    fn apply_event(&mut self, state: &mut TuiState, event: Self::Event) -> Result<()> {
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

    fn stopped_message() -> &'static str {
        "Codex managed runtime stopped while the TUI was attached"
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

    run(
        prepared,
        state,
        NativeOwner {
            client,
            header,
            workspace_host,
            conversation,
            active_root: None,
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
