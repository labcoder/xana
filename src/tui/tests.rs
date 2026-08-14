use super::state::{ArtifactAction, UpdateEffect};
use super::*;
use crate::{
    agent::Agent,
    context::ContextBudget,
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    identity::StepId,
    message::{Message, Role},
    native_runtime::{AgentEvent, OperationState, RuntimeCommand, RuntimeHandle},
    permission::{PermissionPolicy, PolicyDecision},
    prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
    provider::{ConversationalProvider, DeltaSink, ProviderError},
    tool::{ToolDefinition, ToolRegistry},
    workspace_host::ConversationRef,
};
use anyhow::Result;
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
        state::ActivityVisibility::Auto,
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

    loop {
        let event = client.next_event().await.unwrap();
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
        state::ActivityVisibility::Auto,
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
    loop {
        let event = client.next_event().await.unwrap();
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

    effects::apply_artifact_action(&mut state, &store, record.clone(), ArtifactAction::Preview)
        .unwrap();
    assert!(matches!(
        state.overlay,
        Some(state::Overlay::Artifact {
            preview: Some(ref preview),
            ..
        }) if preview == "bounded artifact preview"
    ));

    state.overlay = None;
    effects::apply_artifact_action(
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
        Some(InputAction::CopyOrInterrupt)
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
