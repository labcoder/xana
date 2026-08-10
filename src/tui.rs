//! Native full-screen terminal frontend over Xana's embedded client contract.
//!
//! Crossterm/Ratatui types stop at this module. The state/update layer consumes
//! provider-neutral snapshots and runtime events and emits runtime commands.

mod activity;
mod command;
mod composer;
mod lifecycle;
mod model;
mod rich_text;
mod session;
mod view;

use crate::{
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    managed::codex::ApprovalDecision,
    managed_terminal::{ManagedChatConfig, ManagedTuiDriver, ManagedTuiEvent},
    model::ModelManager,
    presentation::{ComposerPreset, PresentationPreferences, ResolvedPresentation},
    runtime::{AgentEvent, OperationState, RuntimeCommand, RuntimeHandle},
    terminal::{ChatExit, ChatHeader},
    vision::{ImageIngestor, ImageLimits},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures::StreamExt;
use lifecycle::TerminalSession;
use model::{ArtifactAction, InputAction, MoveDirection, TuiState, UpdateEffect};
use std::{io, path::PathBuf, sync::Arc};
use tokio::sync::oneshot;

pub(crate) struct PreparedTui {
    terminal: TerminalSession,
    profile: ResolvedPresentation,
    preferences: PresentationPreferences,
    preferences_path: PathBuf,
}

impl PreparedTui {
    pub(crate) const fn profile(&self) -> ResolvedPresentation {
        self.profile
    }
}

pub(crate) fn prepare(
    profile: ResolvedPresentation,
    preferences: PresentationPreferences,
    preferences_path: PathBuf,
) -> io::Result<PreparedTui> {
    let mut terminal = TerminalSession::enter()?;
    let state = TuiState::starting(preferences.composer);
    terminal
        .terminal_mut()
        .draw(|frame| view::render(frame, &state, profile))?;
    Ok(PreparedTui {
        terminal,
        profile,
        preferences,
        preferences_path,
    })
}

pub(crate) async fn run_native(
    mut prepared: PreparedTui,
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
    let mut client = EmbeddedClient::from_runtime(runtime, seed);
    let mut state = TuiState::from_client(
        &client,
        prepared.preferences.composer,
        prepared.preferences.activity.into(),
        conversation.clone(),
    );
    let frontend_dir = prepared
        .preferences_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut session_preferences =
        session::SessionPreferenceStore::load(frontend_dir, &header.workspace_root);
    state.set_rail_expanded(session_preferences.rail_expanded());
    state.refresh_sessions(workspace_host.snapshot()?);
    let mut events = EventStream::new();
    let mut _active_root: Option<ActiveRootLease> = None;

    loop {
        if let Some(effect) = state.next_followup()
            && let Some(exit) = dispatch_effect(
                effect,
                &mut state,
                &client,
                header,
                &workspace_host,
                &conversation,
                &mut _active_root,
                &prepared.preferences_path,
                &mut session_preferences,
            )
            .await?
        {
            return Ok(exit);
        }
        let terminal_area = prepared.terminal.terminal_mut().size()?;
        prepared
            .terminal
            .terminal_mut()
            .draw(|frame| view::render(frame, &state, prepared.profile))
            .context("could not draw Xana TUI")?;

        tokio::select! {
            terminal_event = events.next() => {
                let Some(terminal_event) = terminal_event else {
                    let _ = client.send(RuntimeCommand::Shutdown).await;
                    return Ok(ChatExit::Quit);
                };
                let terminal_event = terminal_event.context("terminal input failed")?;
                if let Some(action) = input_action(terminal_event, &state, terminal_area.into()) {
                    let effect = state.update_input(action);
                    if let Some(exit) = dispatch_effect(
                        effect,
                        &mut state,
                        &client,
                        header,
                        &workspace_host,
                        &conversation,
                        &mut _active_root,
                        &prepared.preferences_path,
                        &mut session_preferences,
                    )
                    .await?
                    {
                        return Ok(exit);
                    }
                }
            }
            runtime_event = client.next_event() => {
                let Some(runtime_event) = runtime_event else {
                    anyhow::bail!("Xana's foreground runtime stopped while the TUI was attached");
                };
                let terminal = matches!(
                    runtime_event,
                    AgentEvent::OperationStateChanged {
                        state: OperationState::Finished(_) | OperationState::Suspended,
                        ..
                    } | AgentEvent::OperationFailed { .. }
                );
                state.apply_runtime(&runtime_event);
                if terminal {
                    _active_root = None;
                }
                if let Ok(snapshot) = workspace_host.snapshot() {
                    state.refresh_sessions(snapshot);
                }
            }
        }
    }
}

pub(crate) async fn run_managed(
    mut prepared: PreparedTui,
    server: crate::managed::codex::CodexAppServer,
    models: ModelManager,
    config: ManagedChatConfig,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
) -> Result<ChatExit> {
    let connection = config.connection.clone();
    let workspace = config.workspace.clone();
    let artifact_store = config.artifact_store.clone();
    let owner = config.owner;
    let workspace_host = Arc::new(workspace_host);
    let mut driver = ManagedTuiDriver::start(
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
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut session_preferences = session::SessionPreferenceStore::load(frontend_dir, &workspace);
    state.set_rail_expanded(session_preferences.rail_expanded());
    state.refresh_sessions(workspace_host.snapshot()?);
    let mut events = EventStream::new();
    let mut pending_approval: Option<oneshot::Sender<ApprovalDecision>> = None;

    let exit = loop {
        if let Some(effect) = state.next_followup()
            && let Some(requested_exit) = dispatch_managed_effect(
                effect,
                &mut state,
                &driver,
                &workspace_host,
                &workspace,
                &artifact_store,
                owner,
                &prepared.preferences_path,
                &mut session_preferences,
                &mut pending_approval,
            )
            .await?
        {
            break requested_exit;
        }
        let terminal_area = prepared.terminal.terminal_mut().size()?;
        prepared
            .terminal
            .terminal_mut()
            .draw(|frame| view::render(frame, &state, prepared.profile))
            .context("could not draw Xana managed TUI")?;
        tokio::select! {
            terminal_event = events.next() => {
                let Some(terminal_event) = terminal_event else {
                    break ChatExit::Quit;
                };
                let terminal_event = terminal_event.context("terminal input failed")?;
                if let Some(action) = input_action(terminal_event, &state, terminal_area.into()) {
                    let effect = state.update_input(action);
                    if let Some(requested_exit) = dispatch_managed_effect(
                        effect,
                        &mut state,
                        &driver,
                        &workspace_host,
                        &workspace,
                        &artifact_store,
                        owner,
                        &prepared.preferences_path,
                        &mut session_preferences,
                        &mut pending_approval,
                    ).await? {
                        break requested_exit;
                    }
                }
            }
            managed_event = driver.next_event() => {
                let Some(managed_event) = managed_event else {
                    anyhow::bail!("Codex managed runtime stopped while the TUI was attached");
                };
                match managed_event {
                    ManagedTuiEvent::Notification(event) => state.apply_managed_event(&event),
                    ManagedTuiEvent::Approval { request, reply } => {
                        if let Some(stale) = pending_approval.replace(reply) {
                            let _ = stale.send(ApprovalDecision::Cancel);
                        }
                        state.open_managed_approval(request);
                    }
                    ManagedTuiEvent::ThreadOpened(thread_id) => {
                        state.set_managed_thread(&connection, thread_id);
                    }
                    ManagedTuiEvent::TurnFinished { operation_id, error } => {
                        state.finish_managed_turn(operation_id, error);
                        pending_approval = None;
                    }
                    ManagedTuiEvent::Cleared => state.managed_cleared(&connection),
                }
                if let Ok(snapshot) = workspace_host.snapshot() {
                    state.refresh_sessions(snapshot);
                }
            }
        }
    };
    if let Some(operation_id) = state.active_operation {
        driver.interrupt(operation_id);
    }
    if let Some(reply) = pending_approval {
        let _ = reply.send(ApprovalDecision::Cancel);
    }
    driver.shutdown().await?;
    Ok(exit)
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_managed_effect(
    effect: UpdateEffect,
    state: &mut TuiState,
    driver: &ManagedTuiDriver,
    workspace_host: &WorkspaceHost,
    workspace: &std::path::Path,
    artifact_store: &crate::artifact::ArtifactStore,
    owner: crate::identity::PrincipalId,
    preferences_path: &std::path::Path,
    session_preferences: &mut session::SessionPreferenceStore,
    pending_approval: &mut Option<oneshot::Sender<ApprovalDecision>>,
) -> Result<Option<ChatExit>> {
    match effect {
        UpdateEffect::None => {}
        UpdateEffect::Quit => return Ok(Some(ChatExit::Quit)),
        UpdateEffect::Doctor => return Ok(Some(ChatExit::Doctor(None))),
        UpdateEffect::Reset => return Ok(Some(ChatExit::Reset)),
        UpdateEffect::Setup(section) => return Ok(Some(ChatExit::Setup(section))),
        UpdateEffect::Submit {
            operation_id,
            input,
            images,
        } => {
            let model = driver.models.iter().find(|model| model.id == state.model);
            if !images.is_empty()
                && !model.is_some_and(|model| model.input_modalities.contains("image"))
            {
                state.restore_submission(
                    input,
                    images,
                    format!(
                        "{}/{} is not advertised as image-capable",
                        state.connection, state.model
                    ),
                );
            } else if let Err(reason) = driver
                .submit(operation_id, input.clone(), images.clone())
                .await
            {
                state.restore_submission(input, images, reason);
            } else {
                state.mark_submitted(operation_id, input);
            }
        }
        UpdateEffect::Interrupt { operation_id } => {
            if !driver.interrupt(operation_id) {
                state.set_status("No matching managed turn is active");
            }
        }
        UpdateEffect::Steer { input, .. } => {
            state.restore_submission(input, Vec::new(), "Codex app-server does not advertise same-turn steering; message retained as a draft".to_owned());
        }
        UpdateEffect::Attach(path) => {
            match ImageIngestor::new(artifact_store.clone(), ImageLimits::default())
                .ingest_path(workspace, &path, owner)
            {
                Ok(attachment) => state.stage_image(attachment),
                Err(error) => state.set_status(format!("could not attach {path}: {error}")),
            }
        }
        UpdateEffect::SelectModel(selection) => {
            let requested = selection
                .split_once('/')
                .map_or(selection.as_str(), |(_, model)| model);
            if !driver.models.iter().any(|model| model.id == requested) {
                state.set_status(format!("Codex does not advertise model {requested:?}"));
            } else {
                driver
                    .select_model(requested.to_owned())
                    .await
                    .map_err(anyhow::Error::msg)?;
                state.set_model(requested.to_owned());
            }
        }
        UpdateEffect::SetReasoning(effort) => {
            driver
                .set_reasoning((effort != "auto").then_some(effort))
                .await
                .map_err(anyhow::Error::msg)?;
            state
                .set_status("Reasoning updated for subsequent turns; managed context is unchanged");
        }
        UpdateEffect::PersistComposer(preset) => {
            if let Err(error) = PresentationPreferences::set_composer(preferences_path, preset) {
                state.set_status(format!("could not save composer preference: {error}"));
            }
        }
        UpdateEffect::ClearConversation => driver.clear().await.map_err(anyhow::Error::msg)?,
        UpdateEffect::OpenModelPicker => state.open_model_picker(
            driver
                .models
                .iter()
                .map(|model| format!("{}/{}", state.connection, model.id))
                .collect(),
        ),
        UpdateEffect::OpenReasoningPicker => {
            let choices = driver
                .models
                .iter()
                .find(|model| model.id == state.model)
                .map(|model| {
                    model
                        .reasoning_efforts
                        .iter()
                        .map(|effort| effort.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            state.open_reasoning_picker(choices);
        }
        UpdateEffect::OpenSessionPicker => state.open_session_picker(),
        UpdateEffect::ViewSession(conversation) => {
            match workspace_host.conversation_history_page(&conversation, None, 128) {
                Ok(page) => state.view_session_page(conversation, page),
                Err(error) => state.set_status(format!("could not inspect conversation: {error}")),
            }
        }
        UpdateEffect::LoadOlder(conversation) => {
            let Some(before) = state.history_before() else {
                return Ok(None);
            };
            match workspace_host.conversation_history_page(&conversation, Some(before), 128) {
                Ok(Some(page)) => state.prepend_history_page(page),
                Ok(None) => state.set_status("Managed history remains owned by its runtime"),
                Err(error) => state.set_status(format!("could not load older history: {error}")),
            }
        }
        UpdateEffect::PersistRail(expanded) => {
            if let Err(error) = session_preferences.set_rail_expanded(expanded) {
                state.set_status(format!("could not save session rail preference: {error}"));
            }
        }
        UpdateEffect::ArchiveConversation(conversation) => {
            let archived = match &conversation {
                ConversationRef::Managed {
                    connection,
                    thread_id,
                } if connection == &state.connection => driver
                    .archive(thread_id.clone())
                    .await
                    .map_err(anyhow::Error::msg)?,
                _ => workspace_host.archive_managed_conversation(&conversation)?,
            };
            if archived {
                state.archived_conversation(&conversation);
                state.refresh_sessions(workspace_host.snapshot()?);
            } else {
                state.set_status("Managed conversation was already absent from the local catalog");
            }
        }
        UpdateEffect::PersistActivity(activity) => {
            if let Err(error) = PresentationPreferences::set_activity(preferences_path, activity) {
                state.set_status(format!("could not save activity preference: {error}"));
            }
        }
        UpdateEffect::ArtifactAction { record, action } => {
            apply_artifact_action(state, artifact_store, record, action)?;
        }
        UpdateEffect::DecideManagedApproval(decision) => {
            let Some(reply) = pending_approval.take() else {
                state.set_status("Managed approval is no longer pending");
                return Ok(None);
            };
            if reply.send(decision).is_err() {
                state.set_status("Managed approval is no longer pending");
            }
        }
        UpdateEffect::DecideNativeApproval { .. } | UpdateEffect::DecideChildApproval { .. } => {
            state.set_status("Native approval cannot be sent to the managed runtime");
        }
    }
    Ok(None)
}

fn apply_artifact_action(
    state: &mut TuiState,
    store: &crate::artifact::ArtifactStore,
    record: crate::artifact::ArtifactRecord,
    action: ArtifactAction,
) -> Result<()> {
    const PREVIEW_BYTES: usize = 64 * 1024;
    match action {
        ArtifactAction::Preview => {
            let preview = if record.media_type.starts_with("text/")
                || matches!(
                    record.media_type.as_str(),
                    "application/json" | "application/toml"
                ) {
                let bytes = store
                    .read_bounded(&record, PREVIEW_BYTES)
                    .context("could not read artifact preview")?;
                String::from_utf8(bytes)
                    .unwrap_or_else(|_| "[artifact text is not valid UTF-8]".to_owned())
            } else {
                format!(
                    "[binary preview omitted: {} · {} bytes]",
                    record.media_type, record.byte_len
                )
            };
            state.show_artifact_preview(record, preview);
        }
        ArtifactAction::InsertReference => state.insert_artifact_reference(&record),
        ArtifactAction::Reveal | ArtifactAction::Open => {
            let path = store
                .verified_path(&record, crate::artifact::MAX_ARTIFACT_BYTES)
                .context("could not verify artifact before opening it")?;
            open_artifact_path(&path, action == ArtifactAction::Reveal)
                .context("could not start the OS artifact action")?;
            state.set_status(if action == ArtifactAction::Reveal {
                "Artifact revealed in the OS file manager"
            } else {
                "Artifact opened with the OS default application"
            });
        }
    }
    Ok(())
}

fn open_artifact_path(path: &std::path::Path, reveal: bool) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer.exe");
        if reveal {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        if reveal {
            command.arg("-R");
        }
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(if reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        });
        command
    };
    command.spawn().map(|_| ())
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_effect(
    effect: UpdateEffect,
    state: &mut TuiState,
    client: &EmbeddedClient,
    header: &ChatHeader,
    workspace_host: &WorkspaceHost,
    conversation: &ConversationRef,
    active_root: &mut Option<ActiveRootLease>,
    preferences_path: &std::path::Path,
    session_preferences: &mut session::SessionPreferenceStore,
) -> Result<Option<ChatExit>> {
    match effect {
        UpdateEffect::None => {}
        UpdateEffect::Quit => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Quit));
        }
        UpdateEffect::Doctor => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Doctor(Some(header.session_id))));
        }
        UpdateEffect::Reset => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Reset));
        }
        UpdateEffect::Setup(section) => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Setup(section)));
        }
        UpdateEffect::Submit {
            operation_id,
            input,
            images,
        } => {
            if !images.is_empty() {
                let descriptor = header
                    .models
                    .descriptor(&header.provider_name, &header.model)
                    .context("could not resolve selected model capabilities")?;
                if !descriptor.input_modalities.contains("image") {
                    state.restore_submission(
                        input,
                        images,
                        format!(
                            "{}/{} is not declared image-capable; refresh its catalog or add an explicit model override",
                            header.provider_name, header.model
                        ),
                    );
                    return Ok(None);
                }
            }
            let lease = match workspace_host.acquire_root(conversation.clone()) {
                Ok(lease) => lease,
                Err(error) => {
                    state.restore_submission(
                        input,
                        images,
                        format!("could not start turn: {error}"),
                    );
                    return Ok(None);
                }
            };
            let command = if images.is_empty() {
                RuntimeCommand::SubmitTurn {
                    operation_id,
                    input: input.clone(),
                }
            } else {
                RuntimeCommand::SubmitTurnWithImages {
                    operation_id,
                    input: input.clone(),
                    images: images.iter().map(|image| image.image.clone()).collect(),
                }
            };
            let result = client
                .send(command)
                .await
                .context("native TUI runtime stopped")?;
            if result.accepted {
                state.mark_submitted(operation_id, input);
                *active_root = Some(lease);
            } else {
                drop(lease);
                state.restore_submission(
                    input,
                    images,
                    result
                        .reason
                        .unwrap_or_else(|| "command rejected".to_owned()),
                );
            }
        }
        UpdateEffect::Interrupt { operation_id } => {
            let result = client
                .send(RuntimeCommand::InterruptOperation { operation_id })
                .await
                .context("native TUI runtime stopped during interrupt")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "interrupt was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::Steer {
            operation_id,
            input,
        } => {
            let result = client
                .send(RuntimeCommand::SteerOperation {
                    operation_id,
                    input,
                })
                .await
                .context("native TUI runtime stopped during steering")?;
            state.set_status(if result.accepted {
                "Steering update accepted".to_owned()
            } else {
                result
                    .reason
                    .unwrap_or_else(|| "steering update was rejected".to_owned())
            });
        }
        UpdateEffect::Attach(path) => {
            let descriptor = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .context("could not resolve selected model capabilities")?;
            if !descriptor.input_modalities.contains("image") {
                state.set_status(format!(
                    "{}/{} is not declared image-capable",
                    header.provider_name, header.model
                ));
            } else {
                match ImageIngestor::new(header.artifact_store.clone(), ImageLimits::default())
                    .ingest_path(&header.workspace_root, &path, header.owner)
                {
                    Ok(attachment) => state.stage_image(attachment),
                    Err(error) => state.set_status(format!("could not attach {path}: {error}")),
                }
            }
        }
        UpdateEffect::SelectModel(selection) => {
            let Some((connection, model)) = selection.split_once('/') else {
                state.set_status("Model selection must be CONNECTION/MODEL");
                return Ok(None);
            };
            match header.models.select(connection, model) {
                Ok(_) => {
                    client
                        .send(RuntimeCommand::Shutdown)
                        .await
                        .context("could not stop the old model runtime")?;
                    return Ok(Some(ChatExit::Restart));
                }
                Err(error) => state.set_status(format!("could not select model: {error}")),
            }
        }
        UpdateEffect::SetReasoning(effort) => {
            match header.models.update_reasoning_effort(Some(effort)) {
                Ok(_) => {
                    client
                        .send(RuntimeCommand::Shutdown)
                        .await
                        .context("could not stop the old reasoning runtime")?;
                    return Ok(Some(ChatExit::Restart));
                }
                Err(error) => state.set_status(format!("could not select reasoning: {error}")),
            }
        }
        UpdateEffect::PersistComposer(preset) => {
            if let Err(error) = PresentationPreferences::set_composer(preferences_path, preset) {
                state.set_status(format!("could not save composer preference: {error}"));
            }
        }
        UpdateEffect::ClearConversation => {
            let result = client
                .send(RuntimeCommand::ClearConversation)
                .await
                .context("native TUI runtime stopped while clearing")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "clear was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::OpenModelPicker => {
            let choices = header
                .models
                .summaries()
                .into_iter()
                .flat_map(|summary| {
                    summary
                        .models
                        .into_iter()
                        .map(move |model| format!("{}/{}", summary.id, model.id))
                })
                .collect();
            state.open_model_picker(choices);
        }
        UpdateEffect::OpenReasoningPicker => {
            let choices = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .map(|descriptor| {
                    descriptor
                        .reasoning_efforts
                        .into_iter()
                        .map(|effort| effort.id)
                        .collect()
                })
                .unwrap_or_default();
            state.open_reasoning_picker(choices);
        }
        UpdateEffect::OpenSessionPicker => state.open_session_picker(),
        UpdateEffect::ViewSession(conversation) => {
            match workspace_host.conversation_history_page(&conversation, None, 128) {
                Ok(page) => state.view_session_page(conversation, page),
                Err(error) => state.set_status(format!("could not inspect conversation: {error}")),
            }
        }
        UpdateEffect::LoadOlder(conversation) => {
            let Some(before) = state.history_before() else {
                return Ok(None);
            };
            match workspace_host.conversation_history_page(&conversation, Some(before), 128) {
                Ok(Some(page)) => state.prepend_history_page(page),
                Ok(None) => state.set_status("Managed history remains owned by its runtime"),
                Err(error) => state.set_status(format!("could not load older history: {error}")),
            }
        }
        UpdateEffect::PersistRail(expanded) => {
            if let Err(error) = session_preferences.set_rail_expanded(expanded) {
                state.set_status(format!("could not save session rail preference: {error}"));
            }
        }
        UpdateEffect::ArchiveConversation(conversation) => {
            if workspace_host.archive_managed_conversation(&conversation)? {
                state.archived_conversation(&conversation);
                state.refresh_sessions(workspace_host.snapshot()?);
            } else {
                state.set_status("Managed conversation was already absent from the local catalog");
            }
        }
        UpdateEffect::PersistActivity(activity) => {
            if let Err(error) = PresentationPreferences::set_activity(preferences_path, activity) {
                state.set_status(format!("could not save activity preference: {error}"));
            }
        }
        UpdateEffect::ArtifactAction { record, action } => {
            apply_artifact_action(state, &header.artifact_store, record, action)?;
        }
        UpdateEffect::DecideNativeApproval {
            operation_id,
            invocation_id,
            decision,
        } => {
            let result = client
                .send(RuntimeCommand::DecidePermission {
                    operation_id,
                    invocation_id,
                    decision,
                })
                .await
                .context("native TUI runtime stopped during approval")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "approval was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::DecideChildApproval {
            agent_id,
            operation_id,
            invocation_id,
            decision,
        } => {
            let result = client
                .send(RuntimeCommand::DecideChildPermission {
                    agent_id,
                    operation_id,
                    invocation_id,
                    decision,
                })
                .await
                .context("native child runtime stopped during approval")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "child approval was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::DecideManagedApproval(_) => {
            state.set_status("Managed approval cannot be sent through the native runtime");
        }
    }
    Ok(None)
}

fn input_action(
    event: Event,
    state: &TuiState,
    area: ratatui::layout::Rect,
) -> Option<InputAction> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(InputAction::Quit),
        Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(InputAction::Interrupt),
        Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(InputAction::OpenPalette),
        Event::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(match state.composer_preset {
            ComposerPreset::Submit => InputAction::Newline,
            ComposerPreset::Newline => InputAction::Submit,
        }),
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.intersects(KeyModifiers::CONTROL) => Some(InputAction::Submit),
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.intersects(KeyModifiers::SHIFT) => Some(InputAction::Newline),
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            kind: KeyEventKind::Press,
            ..
        }) => Some(if state.overlay.is_some() {
            InputAction::Confirm
        } else {
            match state.composer_preset {
                ComposerPreset::Submit => InputAction::Submit,
                ComposerPreset::Newline => InputAction::Newline,
            }
        }),
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            kind: KeyEventKind::Press,
            ..
        }) => Some(InputAction::Cancel),
        Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(if state.overlay.is_some() {
            InputAction::PaletteUp
        } else {
            InputAction::Move {
                direction: MoveDirection::Up,
                select: modifiers.contains(KeyModifiers::SHIFT),
            }
        }),
        Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(if state.overlay.is_some() {
            InputAction::PaletteDown
        } else {
            InputAction::Move {
                direction: MoveDirection::Down,
                select: modifiers.contains(KeyModifiers::SHIFT),
            }
        }),
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) if matches!(
            code,
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
        ) =>
        {
            let direction = match code {
                KeyCode::Left => MoveDirection::Left,
                KeyCode::Right => MoveDirection::Right,
                KeyCode::Home => MoveDirection::Home,
                KeyCode::End => MoveDirection::End,
                _ => unreachable!(),
            };
            Some(InputAction::Move {
                direction,
                select: modifiers.contains(KeyModifiers::SHIFT),
            })
        }
        Event::Key(KeyEvent {
            code: KeyCode::Backspace,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(InputAction::Backspace),
        Event::Key(KeyEvent {
            code: KeyCode::Delete,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(InputAction::Delete),
        Event::Key(KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            Some(InputAction::Insert(character.to_string()))
        }
        Event::Paste(text) => Some(InputAction::Paste(text)),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => Some(InputAction::Scroll(-3)),
            MouseEventKind::ScrollDown => Some(InputAction::Scroll(3)),
            MouseEventKind::Down(MouseButton::Left) => {
                view::pointer_action(mouse.column, mouse.row, false, state, area)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                view::pointer_action(mouse.column, mouse.row, true, state, area)
            }
            _ => None,
        },
        Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Key(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::Agent,
        context::ContextBudget,
        identity::StepId,
        message::{Message, Role},
        permission::{PermissionPolicy, PolicyDecision},
        prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
        provider::{ConversationalProvider, DeltaSink, ProviderError},
        tool::{ToolDefinition, ToolRegistry},
    };
    use futures::future::BoxFuture;
    use std::sync::Arc;
    use tokio::sync::Notify;

    struct ScriptedProvider;

    struct DelayedProvider {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

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
                deltas.text_delta(step_id, "from Xana");
                Ok(Message::text(Role::Assistant, "hello from Xana"))
            })
        }
    }

    impl ConversationalProvider for DelayedProvider {
        fn stream_message<'a>(
            &'a self,
            _messages: &'a [Message],
            _tools: &'a [&'a ToolDefinition],
            _step_id: StepId,
            _deltas: &'a dyn DeltaSink,
        ) -> BoxFuture<'a, Result<Message, ProviderError>> {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(Message::text(Role::Assistant, "released"))
            })
        }
    }

    fn scripted_client(provider: Box<dyn ConversationalProvider>) -> EmbeddedClient {
        let tools = ToolRegistry::new();
        let definitions = tools.definitions();
        let workspace = std::env::current_dir().unwrap();
        let environment = PromptEnvironment {
            operating_system: "test".to_owned(),
            working_directory: workspace.clone(),
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
        let agent = Agent::new(provider, tools, workspace.clone(), prompt, 2);
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).unwrap();
        let runtime = RuntimeHandle::spawn(agent, policy, true);
        EmbeddedClient::from_runtime(
            runtime,
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

    #[tokio::test]
    async fn scripted_turn_crosses_the_real_embedded_client_and_finishes_in_view_state() {
        let mut client = scripted_client(Box::new(ScriptedProvider));
        let mut state = TuiState::from_client(
            &client,
            ComposerPreset::Submit,
            model::ActivityVisibility::Auto,
            ConversationRef::NewNative,
        );
        state.update_input(InputAction::Insert("hi".to_owned()));
        let UpdateEffect::Submit {
            operation_id,
            input,
            images,
        } = state.update_input(InputAction::Submit)
        else {
            panic!("submit effect");
        };
        assert!(images.is_empty());
        state.mark_submitted(operation_id, input.clone());
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input,
            })
            .await
            .unwrap();

        while let Some(event) = client.next_event().await {
            let finished = matches!(
                event,
                AgentEvent::OperationStateChanged {
                    operation_id: actual,
                    state: OperationState::Finished(_),
                } if actual == operation_id
            );
            state.apply_runtime(&event);
            if finished {
                break;
            }
        }

        assert_eq!(state.messages.back().unwrap().text, "hello from Xana");
        assert!(!state.busy);
    }

    #[tokio::test]
    async fn delayed_provider_does_not_block_frontend_updates() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut client = scripted_client(Box::new(DelayedProvider {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }));
        let mut state = TuiState::from_client(
            &client,
            ComposerPreset::Submit,
            model::ActivityVisibility::Auto,
            ConversationRef::NewNative,
        );
        state.update_input(InputAction::Insert("wait".to_owned()));
        let UpdateEffect::Submit {
            operation_id,
            input,
            images,
        } = state.update_input(InputAction::Submit)
        else {
            panic!("submit effect");
        };
        assert!(images.is_empty());
        state.mark_submitted(operation_id, input.clone());
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input,
            })
            .await
            .unwrap();
        started.notified().await;

        state.update_input(InputAction::Insert("still responsive".to_owned()));
        assert_eq!(state.composer.text, "still responsive");
        assert!(state.busy);

        release.notify_one();
        while let Some(event) = client.next_event().await {
            let finished = matches!(
                event,
                AgentEvent::OperationStateChanged {
                    operation_id: actual,
                    state: OperationState::Finished(_),
                } if actual == operation_id
            );
            state.apply_runtime(&event);
            if finished {
                break;
            }
        }
        assert_eq!(state.messages.back().unwrap().text, "released");
    }

    #[test]
    fn artifact_preview_and_reference_actions_are_explicit_and_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::artifact::ArtifactStore::new(directory.path().to_owned());
        let (record, _) = store
            .put(
                b"bounded artifact preview",
                "text/plain",
                crate::identity::PrincipalId::new(),
            )
            .unwrap();
        let mut state = TuiState::starting(ComposerPreset::Submit);

        apply_artifact_action(&mut state, &store, record.clone(), ArtifactAction::Preview).unwrap();
        assert!(matches!(
            state.overlay,
            Some(model::Overlay::Artifact {
                preview: Some(ref preview),
                ..
            }) if preview == "bounded artifact preview"
        ));

        state.overlay = None;
        apply_artifact_action(
            &mut state,
            &store,
            record.clone(),
            ArtifactAction::InsertReference,
        )
        .unwrap();
        assert_eq!(
            state.composer.text,
            format!("artifact:{}", record.reference.id)
        );
    }

    #[test]
    fn terminal_events_map_paste_resize_and_cancellation_without_runtime_authority() {
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(
            input_action(
                Event::Paste("pasted text".to_owned()),
                &TuiState::starting(ComposerPreset::Submit),
                area,
            ),
            Some(InputAction::Paste("pasted text".to_owned()))
        );
        assert_eq!(
            input_action(
                Event::Resize(80, 24),
                &TuiState::starting(ComposerPreset::Submit),
                area,
            ),
            None
        );
        assert_eq!(
            input_action(
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                &TuiState::starting(ComposerPreset::Submit),
                area,
            ),
            Some(InputAction::Interrupt)
        );

        let submit = TuiState::starting(ComposerPreset::Submit);
        let newline = TuiState::starting(ComposerPreset::Newline);
        let alternate = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(
            input_action(alternate.clone(), &submit, area),
            Some(InputAction::Newline)
        );
        assert_eq!(
            input_action(alternate, &newline, area),
            Some(InputAction::Submit)
        );
        assert_eq!(
            input_action(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                &newline,
                area,
            ),
            Some(InputAction::Submit)
        );
    }
}
