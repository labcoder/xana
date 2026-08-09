//! Native full-screen terminal frontend over Xana's embedded client contract.
//!
//! Crossterm/Ratatui types stop at this module. The state/update layer consumes
//! provider-neutral snapshots and runtime events and emits runtime commands.

mod lifecycle;
mod model;
mod view;

use crate::{
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    permission::ControllerDecision,
    presentation::ResolvedPresentation,
    runtime::{AgentEvent, OperationState, RuntimeCommand, RuntimeHandle},
    terminal::{ChatExit, ChatHeader},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use lifecycle::TerminalSession;
use model::{InputAction, TuiState, UpdateEffect};
use std::io;

pub(crate) struct PreparedTui {
    terminal: TerminalSession,
    profile: ResolvedPresentation,
}

impl PreparedTui {
    pub(crate) const fn profile(&self) -> ResolvedPresentation {
        self.profile
    }
}

pub(crate) fn prepare(profile: ResolvedPresentation) -> io::Result<PreparedTui> {
    let mut terminal = TerminalSession::enter()?;
    let state = TuiState::starting();
    terminal
        .terminal_mut()
        .draw(|frame| view::render(frame, &state, profile))?;
    Ok(PreparedTui { terminal, profile })
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
    let mut state = TuiState::from_client(&client);
    let mut events = EventStream::new();
    let mut _active_root: Option<ActiveRootLease> = None;

    loop {
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
                if let Some(action) = input_action(terminal_event) {
                    match state.update_input(action) {
                        UpdateEffect::None => {}
                        UpdateEffect::Quit => {
                            let _ = client.send(RuntimeCommand::Shutdown).await;
                            return Ok(ChatExit::Quit);
                        }
                        UpdateEffect::Submit { operation_id, input } => {
                            let lease = match workspace_host.acquire_root(conversation.clone()) {
                                Ok(lease) => lease,
                                Err(error) => {
                                    state.apply_runtime(&AgentEvent::CommandRejected {
                                        reason: format!("could not start turn: {error}"),
                                    });
                                    continue;
                                }
                            };
                            let result = client
                                .send(RuntimeCommand::SubmitTurn { operation_id, input })
                                .await
                                .context("native TUI runtime stopped")?;
                            if result.accepted {
                                _active_root = Some(lease);
                            } else {
                                drop(lease);
                                state.apply_runtime(&AgentEvent::CommandRejected {
                                    reason: result.reason.unwrap_or_else(|| "command rejected".to_owned()),
                                });
                            }
                        }
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
                        state: OperationState::Finished(_),
                        ..
                    } | AgentEvent::OperationFailed { .. } | AgentEvent::CommandRejected { .. }
                );
                state.apply_runtime(&runtime_event);
                handle_fail_closed_approval(&client, &runtime_event).await?;
                if terminal {
                    _active_root = None;
                }
            }
        }
    }
}

fn input_action(event: Event) -> Option<InputAction> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char('c' | 'q'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL) => Some(InputAction::Quit),
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            kind: KeyEventKind::Press,
            ..
        }) => Some(InputAction::Submit),
        Event::Key(KeyEvent {
            code: KeyCode::Backspace,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) => Some(InputAction::Backspace),
        Event::Key(KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            Some(InputAction::Insert(character.to_string()))
        }
        Event::Paste(text) => Some(InputAction::Insert(text)),
        Event::Resize(_, _)
        | Event::FocusGained
        | Event::FocusLost
        | Event::Mouse(_)
        | Event::Key(_) => None,
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
        let mut state = TuiState::from_client(&client);
        state.update_input(InputAction::Insert("hi".to_owned()));
        let UpdateEffect::Submit {
            operation_id,
            input,
        } = state.update_input(InputAction::Submit)
        else {
            panic!("submit effect");
        };
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
        let mut state = TuiState::from_client(&client);
        state.update_input(InputAction::Insert("wait".to_owned()));
        let UpdateEffect::Submit {
            operation_id,
            input,
        } = state.update_input(InputAction::Submit)
        else {
            panic!("submit effect");
        };
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input,
            })
            .await
            .unwrap();
        started.notified().await;

        state.update_input(InputAction::Insert("still responsive".to_owned()));
        assert_eq!(state.input, "still responsive");
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
            input_action(Event::Paste("pasted text".to_owned())),
            Some(InputAction::Insert("pasted text".to_owned()))
        );
        assert_eq!(input_action(Event::Resize(80, 24)), None);
        assert_eq!(
            input_action(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            ))),
            Some(InputAction::Quit)
        );
    }
}
