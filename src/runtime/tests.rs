use super::*;
use crate::{
    agent::{ChatError, ChatTransport, DeltaSink},
    context::ContextBudget,
    identity::{StepId, ToolInvocationId},
    message::{ContentBlock, Role, ToolResult},
    prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
    tool::{ToolDefinition, ToolRegistry},
};
use futures::future::BoxFuture;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Notify, mpsc};

type CapturedRequests = Arc<Mutex<Vec<Vec<Message>>>>;
type CompletionFlag = Arc<AtomicBool>;

struct QueueTransport {
    responses: Mutex<VecDeque<Result<Message, String>>>,
    requests: CapturedRequests,
    completed: CompletionFlag,
    deltas: Vec<String>,
}

impl ChatTransport for QueueTransport {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ChatError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(messages.to_vec());
            for text in &self.deltas {
                deltas.text_delta(step_id, text);
            }
            let result = self
                .responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .unwrap_or_else(|| Err("script exhausted".to_owned()))
                .map_err(ChatError::new);
            self.completed.store(true, Ordering::SeqCst);
            result
        })
    }
}

struct BlockingTransport {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl ChatTransport for BlockingTransport {
    fn stream_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        _step_id: StepId,
        _deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ChatError>> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Message::text(Role::Assistant, "released"))
        })
    }
}

fn make_agent(provider: Box<dyn ChatTransport>) -> Agent {
    let tools = ToolRegistry::new();
    let definitions = tools.definitions();
    let workspace = std::env::current_dir().expect("current directory");
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
    .expect("test prompt");
    Agent::new(provider, tools, workspace, prompt, 2)
}

fn queue_agent(
    responses: Vec<Result<Message, String>>,
    deltas: Vec<String>,
) -> (Agent, CapturedRequests, CompletionFlag) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(AtomicBool::new(false));
    let transport = QueueTransport {
        responses: Mutex::new(responses.into()),
        requests: Arc::clone(&requests),
        completed: Arc::clone(&completed),
        deltas,
    };
    (make_agent(Box::new(transport)), requests, completed)
}

async fn receive_finished(
    handle: &mut RuntimeHandle,
    operation_id: OperationId,
) -> OperationOutcome {
    loop {
        match handle.next_event().await.expect("runtime event") {
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(outcome),
            } if actual == operation_id => return outcome,
            _ => {}
        }
    }
}

#[test]
fn commands_and_events_round_trip_through_json() {
    let operation_id = OperationId::new();
    let step_id = StepId::new();
    let invocation_id = ToolInvocationId::new();
    let message = Message::text(Role::Assistant, "hello");
    let commands = vec![
        RuntimeCommand::SubmitTurn {
            operation_id,
            input: "hello".to_owned(),
        },
        RuntimeCommand::ClearConversation,
        RuntimeCommand::DecideProvisionalApproval {
            operation_id,
            invocation_id,
            approved: true,
        },
        RuntimeCommand::Shutdown,
    ];
    let events = vec![
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        },
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Suspended,
        },
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(OperationOutcome::Completed),
        },
        AgentEvent::AssistantTextDelta {
            operation_id,
            step_id,
            text: "hel".to_owned(),
        },
        AgentEvent::ProvisionalApprovalRequested {
            operation_id,
            invocation_id,
            tool_name: "run_command".to_owned(),
            action: "cargo test".to_owned(),
        },
        AgentEvent::ToolFinished {
            operation_id,
            invocation_id,
            result: Message::tool_result(ToolResult::success("call-1", "ok")),
        },
        AgentEvent::AssistantMessage {
            operation_id,
            message,
        },
        AgentEvent::OperationFailed {
            operation_id,
            reason: "provider unavailable".to_owned(),
        },
        AgentEvent::ConversationCleared,
        AgentEvent::CommandRejected {
            reason: "busy".to_owned(),
        },
    ];

    for command in commands {
        let encoded = serde_json::to_string(&command).expect("command JSON");
        assert_eq!(
            serde_json::from_str::<RuntimeCommand>(&encoded).expect("decoded command"),
            command
        );
    }
    for event in events {
        let encoded = serde_json::to_string(&event).expect("event JSON");
        assert_eq!(
            serde_json::from_str::<AgentEvent>(&encoded).expect("decoded event"),
            event
        );
    }
}

#[tokio::test]
async fn runtime_owns_history_across_turns() {
    let (agent, requests, _) = queue_agent(
        vec![
            Ok(Message::text(Role::Assistant, "first answer")),
            Ok(Message::text(Role::Assistant, "second answer")),
        ],
        Vec::new(),
    );
    let mut runtime = RuntimeHandle::spawn(agent);
    let first = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: first,
            input: "first question".to_owned(),
        })
        .await
        .expect("first command");
    assert_eq!(
        receive_finished(&mut runtime, first).await,
        OperationOutcome::Completed
    );

    let second = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: second,
            input: "second question".to_owned(),
        })
        .await
        .expect("second command");
    assert_eq!(
        receive_finished(&mut runtime, second).await,
        OperationOutcome::Completed
    );

    let requests = requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1][1], Message::text(Role::User, "first question"));
    assert_eq!(
        requests[1][2],
        Message::text(Role::Assistant, "first answer")
    );
    assert_eq!(requests[1][3], Message::text(Role::User, "second question"));
}

#[tokio::test]
async fn clear_resets_runtime_history() {
    let (agent, requests, _) = queue_agent(
        vec![
            Ok(Message::text(Role::Assistant, "first answer")),
            Ok(Message::text(Role::Assistant, "fresh answer")),
        ],
        Vec::new(),
    );
    let mut runtime = RuntimeHandle::spawn(agent);
    let first = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: first,
            input: "remember me".to_owned(),
        })
        .await
        .expect("first turn");
    receive_finished(&mut runtime, first).await;
    runtime
        .send(RuntimeCommand::ClearConversation)
        .await
        .expect("clear command");
    assert_eq!(
        runtime.next_event().await,
        Some(AgentEvent::ConversationCleared)
    );

    let second = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: second,
            input: "fresh start".to_owned(),
        })
        .await
        .expect("second turn");
    receive_finished(&mut runtime, second).await;

    let requests = requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(requests[1].len(), 2);
    assert_eq!(requests[1][1], Message::text(Role::User, "fresh start"));
}

#[tokio::test]
async fn active_root_turn_rejects_a_second_submission_and_shutdown_interrupts() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let agent = make_agent(Box::new(BlockingTransport {
        started: Arc::clone(&started),
        release,
    }));
    let mut runtime = RuntimeHandle::spawn(agent);
    let active = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: active,
            input: "wait".to_owned(),
        })
        .await
        .expect("active turn");
    assert!(matches!(
        runtime.next_event().await,
        Some(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        }) if operation_id == active
    ));
    started.notified().await;

    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: OperationId::new(),
            input: "too soon".to_owned(),
        })
        .await
        .expect("second command transport");
    assert!(matches!(
        runtime.next_event().await,
        Some(AgentEvent::CommandRejected { reason }) if reason.contains("already active")
    ));
    runtime
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("shutdown command");
    assert_eq!(
        runtime.next_event().await,
        Some(AgentEvent::OperationStateChanged {
            operation_id: active,
            state: OperationState::Finished(OperationOutcome::Interrupted),
        })
    );
}

#[tokio::test]
async fn deltas_keep_operation_and_step_identity() {
    let (agent, _, _) = queue_agent(
        vec![Ok(Message::text(Role::Assistant, "hello"))],
        vec!["hel".to_owned(), "lo".to_owned()],
    );
    let mut runtime = RuntimeHandle::spawn(agent);
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "hello".to_owned(),
        })
        .await
        .expect("turn");
    let running = runtime.next_event().await.expect("running");
    let first = runtime.next_event().await.expect("first delta");
    let second = runtime.next_event().await.expect("second delta");

    assert!(
        matches!(running, AgentEvent::OperationStateChanged { operation_id: actual, state: OperationState::Running } if actual == operation_id)
    );
    let (first_step, second_step) = match (first, second) {
        (
            AgentEvent::AssistantTextDelta {
                operation_id: first_operation,
                step_id: first_step,
                text: first_text,
            },
            AgentEvent::AssistantTextDelta {
                operation_id: second_operation,
                step_id: second_step,
                text: second_text,
            },
        ) => {
            assert_eq!(first_operation, operation_id);
            assert_eq!(second_operation, operation_id);
            assert_eq!(first_text, "hel");
            assert_eq!(second_text, "lo");
            (first_step, second_step)
        }
        events => panic!("unexpected delta events: {events:?}"),
    };
    assert_eq!(first_step, second_step);
    assert_eq!(
        receive_finished(&mut runtime, operation_id).await,
        OperationOutcome::Completed
    );
}

#[tokio::test]
async fn dropped_event_receiver_does_not_fail_operation() {
    let (agent, _, completed) = queue_agent(
        vec![Ok(Message::text(Role::Assistant, "still completes"))],
        vec!["still ".to_owned(), "completes".to_owned()],
    );
    let RuntimeHandle { commands, events } = RuntimeHandle::spawn(agent);
    drop(events);
    commands
        .send(RuntimeCommand::SubmitTurn {
            operation_id: OperationId::new(),
            input: "continue without observer".to_owned(),
        })
        .await
        .expect("runtime accepts command");
    while !completed.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    commands
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("runtime remains available after passive event loss");
}

#[tokio::test]
async fn failures_always_end_with_a_terminal_failed_state() {
    let (agent, _, _) = queue_agent(vec![Err("provider failed".to_owned())], Vec::new());
    let mut runtime = RuntimeHandle::spawn(agent);
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "fail".to_owned(),
        })
        .await
        .expect("turn");
    assert_eq!(
        receive_finished(&mut runtime, operation_id).await,
        OperationOutcome::Failed
    );
}

fn requested_action() -> crate::approval::RequestedAction {
    crate::approval::RequestedAction {
        tool_name: "run_command",
        shell: "test shell",
        command: "cargo test".to_owned(),
        argv: "shell cargo test".to_owned(),
        cwd: Path::new("workspace").to_owned(),
    }
}

#[tokio::test]
async fn approvals_are_correlated_reject_duplicates_and_clean_up() {
    let (events, mut receiver) = mpsc::unbounded_channel();
    let coordinator = Arc::new(ProvisionalApprovalCoordinator::new(events));
    let operation_id = OperationId::new();
    let invocation_id = ToolInvocationId::new();
    let waiter = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move {
            coordinator
                .request(operation_id, invocation_id, &requested_action())
                .await
        })
    };
    assert!(matches!(
        receiver.recv().await,
        Some(AgentEvent::OperationStateChanged {
            state: OperationState::Suspended,
            ..
        })
    ));
    assert!(matches!(
        receiver.recv().await,
        Some(AgentEvent::ProvisionalApprovalRequested { operation_id: actual_operation, invocation_id: actual_invocation, .. })
            if actual_operation == operation_id && actual_invocation == invocation_id
    ));
    assert!(matches!(
        coordinator
            .request(operation_id, invocation_id, &requested_action())
            .await,
        Err(crate::approval::ApprovalError::DuplicatePending { .. })
    ));
    coordinator
        .decide(operation_id, invocation_id, true)
        .expect("matching decision");
    assert!(waiter.await.expect("approval task").expect("decision"));
    assert_eq!(coordinator.pending_count(), 0);
    assert!(
        coordinator
            .decide(operation_id, invocation_id, false)
            .is_err()
    );
}

#[tokio::test]
async fn approvals_fail_closed_without_a_controller() {
    let (events, receiver) = mpsc::unbounded_channel();
    drop(receiver);
    let coordinator = ProvisionalApprovalCoordinator::new(events);

    assert!(matches!(
        coordinator
            .request(
                OperationId::new(),
                ToolInvocationId::new(),
                &requested_action()
            )
            .await,
        Err(crate::approval::ApprovalError::ControllerUnavailable)
    ));
    assert_eq!(coordinator.pending_count(), 0);
}

#[test]
fn events_are_passive_observations_not_commands() {
    fn observe(_event: AgentEvent) {}
    fn command(_command: RuntimeCommand) {}

    observe(AgentEvent::CommandRejected {
        reason: "observation only".to_owned(),
    });
    command(RuntimeCommand::ClearConversation);

    let result = Message::tool_result(ToolResult::error("call", "failed"));
    assert!(matches!(
        result.content.as_slice(),
        [ContentBlock::ToolResult(_)]
    ));
}
