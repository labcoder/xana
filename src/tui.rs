//! Native full-screen terminal frontend over Xana's embedded client contract.
//!
//! Crossterm/Ratatui types stop at this module. The state/update layer consumes
//! provider-neutral snapshots and runtime events and emits runtime commands.

mod command;
mod lifecycle;
mod model;
mod session;
mod view;

use crate::{
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    permission::ControllerDecision,
    presentation::{ComposerPreset, PresentationPreferences, ResolvedPresentation},
    runtime::{AgentEvent, OperationState, RuntimeCommand, RuntimeHandle},
    terminal::{ChatExit, ChatHeader},
    vision::{ImageIngestor, ImageLimits},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::StreamExt;
use lifecycle::TerminalSession;
use model::{InputAction, MoveDirection, TuiState, UpdateEffect};
use std::{io, path::PathBuf};

pub(crate) struct PreparedTui {
    terminal: TerminalSession,
    profile: ResolvedPresentation,
    composer_preset: ComposerPreset,
    preferences_path: PathBuf,
}

impl PreparedTui {
    pub(crate) const fn profile(&self) -> ResolvedPresentation {
        self.profile
    }
}

pub(crate) fn prepare(
    profile: ResolvedPresentation,
    composer_preset: ComposerPreset,
    preferences_path: PathBuf,
) -> io::Result<PreparedTui> {
    let mut terminal = TerminalSession::enter()?;
    let state = TuiState::starting(composer_preset);
    terminal
        .terminal_mut()
        .draw(|frame| view::render(frame, &state, profile))?;
    Ok(PreparedTui {
        terminal,
        profile,
        composer_preset,
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
    let mut state = TuiState::from_client(&client, prepared.composer_preset, conversation.clone());
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
                if let Some(action) = input_action(terminal_event, &state) {
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
                handle_fail_closed_approval(&client, &runtime_event).await?;
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
                    images: images.clone(),
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
            match workspace_host.conversation_history(&conversation) {
                Ok(history) => state.view_session(conversation, history),
                Err(error) => state.set_status(format!("could not inspect conversation: {error}")),
            }
        }
        UpdateEffect::PersistRail(expanded) => {
            if let Err(error) = session_preferences.set_rail_expanded(expanded) {
                state.set_status(format!("could not save session rail preference: {error}"));
            }
        }
    }
    Ok(None)
}

fn input_action(event: Event, state: &TuiState) -> Option<InputAction> {
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
            _ => None,
        },
        Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Key(_) => None,
    }
}

async fn handle_fail_closed_approval(client: &EmbeddedClient, event: &AgentEvent) -> Result<()> {
    match event {
        AgentEvent::PermissionRequested { request } => {
            client
                .send(RuntimeCommand::DecidePermission {
                    operation_id: request.operation_id,
                    invocation_id: request.invocation_id,
                    decision: ControllerDecision::Deny,
                })
                .await
                .context("could not deny unsupported TUI approval")?;
        }
        AgentEvent::ChildActivity {
            attribution,
            activity: crate::orchestration::ChildActivity::PermissionRequested { request },
        } => {
            client
                .send(RuntimeCommand::DecideChildPermission {
                    agent_id: attribution.agent_id,
                    operation_id: request.operation_id,
                    invocation_id: request.invocation_id,
                    decision: ControllerDecision::Deny,
                })
                .await
                .context("could not deny unsupported child TUI approval")?;
        }
        _ => {}
    }
    Ok(())
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
        let mut state =
            TuiState::from_client(&client, ComposerPreset::Submit, ConversationRef::NewNative);
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
        let mut state =
            TuiState::from_client(&client, ComposerPreset::Submit, ConversationRef::NewNative);
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
    fn terminal_events_map_paste_resize_and_cancellation_without_runtime_authority() {
        assert_eq!(
            input_action(
                Event::Paste("pasted text".to_owned()),
                &TuiState::starting(ComposerPreset::Submit)
            ),
            Some(InputAction::Paste("pasted text".to_owned()))
        );
        assert_eq!(
            input_action(
                Event::Resize(80, 24),
                &TuiState::starting(ComposerPreset::Submit)
            ),
            None
        );
        assert_eq!(
            input_action(
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                &TuiState::starting(ComposerPreset::Submit)
            ),
            Some(InputAction::Interrupt)
        );

        let submit = TuiState::starting(ComposerPreset::Submit);
        let newline = TuiState::starting(ComposerPreset::Newline);
        let alternate = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(
            input_action(alternate.clone(), &submit),
            Some(InputAction::Newline)
        );
        assert_eq!(input_action(alternate, &newline), Some(InputAction::Submit));
        assert_eq!(
            input_action(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                &newline,
            ),
            Some(InputAction::Submit)
        );
    }
}
