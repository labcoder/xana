use super::*;
use crate::{
    context::ContextBudget,
    message::{Role, ToolResultStatus},
    permission::{PermissionBroker, PermissionPolicy, PolicyDecision},
    prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
    provider::{ConversationalProvider, DeltaSink, ProviderError, ProviderUsage},
    shell::{Shell, ShellConfig},
    tool::ToolDefinition,
};
use futures::future::BoxFuture;
use std::{
    collections::VecDeque,
    fs,
    sync::{Arc, Mutex},
};
use tempfile::tempdir;

#[derive(Clone)]
struct ScriptedResponse {
    deltas: Vec<String>,
    message: Message,
    usage: Option<ProviderUsage>,
}

struct ScriptedChatTransport {
    responses: Mutex<VecDeque<ScriptedResponse>>,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl ScriptedChatTransport {
    fn new(responses: Vec<ScriptedResponse>) -> (Self, Arc<Mutex<Vec<Vec<Message>>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                responses: Mutex::new(responses.into()),
                requests: Arc::clone(&requests),
            },
            requests,
        )
    }
}

impl ConversationalProvider for ScriptedChatTransport {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(messages.to_vec());
            let response = self
                .responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .ok_or_else(|| ProviderError::new("script exhausted"))?;
            for text in &response.deltas {
                deltas.text_delta(step_id, text);
            }
            if let Some(usage) = response.usage {
                deltas.usage(usage);
            }
            Ok(response.message)
        })
    }
}

fn make_agent(
    provider: ScriptedChatTransport,
    workspace: &std::path::Path,
    max_tool_rounds: usize,
) -> Agent {
    let shell = Shell::resolve(ShellConfig::default()).expect("platform shell");
    let tools = ToolRegistry::builtins(shell).expect("built-in registry");
    let definitions = tools.definitions();
    let environment = PromptEnvironment {
        operating_system: "test".to_owned(),
        working_directory: workspace.to_owned(),
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

    Agent::new(
        Box::new(provider),
        tools,
        workspace.to_owned(),
        prompt,
        max_tool_rounds,
    )
}

fn operation_services() -> (
    OperationId,
    crate::permission::PermissionBrokerHandle,
    mpsc::UnboundedSender<AgentEvent>,
    mpsc::UnboundedReceiver<AgentEvent>,
) {
    let operation_id = OperationId::new();
    let (events, receiver) = mpsc::unbounded_channel();
    let policy = PermissionPolicy::new(
        PolicyDecision::Allow,
        Vec::new(),
        &std::env::current_dir().expect("current directory"),
    )
    .expect("allow policy");
    let (permissions, _broker) = PermissionBroker::spawn(policy, false, events.clone());
    (operation_id, permissions, events, receiver)
}

#[test]
fn requested_tools_preserve_model_order_and_values() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text("I'll inspect both.".to_owned()),
            ContentBlock::ToolCall(ToolCall {
                id: "call-a".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "a.txt"}),
            }),
            ContentBlock::ToolCall(ToolCall {
                id: "call-b".to_owned(),
                name: "list_files".to_owned(),
                arguments: serde_json::json!({"path": "src"}),
            }),
        ],
    };

    let calls = requested_tools(&message);

    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call-a");
    assert_eq!(calls[1].id, "call-b");
}

#[test]
fn requested_tools_are_empty_for_final_text() {
    assert!(requested_tools(&Message::text(Role::Assistant, "Finished.")).is_empty());
}

#[tokio::test]
async fn scripted_text_turn_emits_deltas_then_final_message() {
    let workspace = tempdir().expect("temporary workspace");
    let response = ScriptedResponse {
        deltas: vec!["hel".to_owned(), "lo".to_owned()],
        message: Message::text(Role::Assistant, "hello"),
        usage: None,
    };
    let (provider, _) = ScriptedChatTransport::new(vec![response]);
    let agent = make_agent(provider, workspace.path(), 2);
    let (operation_id, permissions, events, mut receiver) = operation_services();
    let mut history = vec![Message::text(Role::User, "say hello")];

    let result = agent
        .run_turn(operation_id, &mut history, permissions, events)
        .await
        .expect("scripted turn");
    let first = receiver.recv().await.expect("first delta");
    let second = receiver.recv().await.expect("second delta");

    assert_eq!(result, Message::text(Role::Assistant, "hello"));
    let (first_step, second_step) = match (first, second) {
        (
            AgentEvent::AssistantTextDelta {
                step_id: first_step,
                text: first_text,
                ..
            },
            AgentEvent::AssistantTextDelta {
                step_id: second_step,
                text: second_text,
                ..
            },
        ) => {
            assert_eq!(first_text, "hel");
            assert_eq!(second_text, "lo");
            (first_step, second_step)
        }
        other => panic!("unexpected events: {other:?}"),
    };
    assert_eq!(first_step, second_step);
}

#[tokio::test]
async fn scripted_tool_turn_preserves_step_and_invocation_identity() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("note.txt"), "contents").expect("fixture");
    let tool_response = ScriptedResponse {
        deltas: Vec::new(),
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "provider-call".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "note.txt"}),
            })],
        },
        usage: None,
    };
    let final_response = ScriptedResponse {
        deltas: vec!["done".to_owned()],
        message: Message::text(Role::Assistant, "done"),
        usage: None,
    };
    let (provider, _) = ScriptedChatTransport::new(vec![tool_response, final_response]);
    let agent = make_agent(provider, workspace.path(), 3);
    let (operation_id, permissions, events, mut receiver) = operation_services();
    let mut history = vec![Message::text(Role::User, "read note")];

    let result = agent
        .run_turn(operation_id, &mut history, permissions, events)
        .await
        .expect("scripted tool turn");
    let audit_event = receiver.recv().await.expect("permission audit");
    let tool_event = receiver.recv().await.expect("tool completion");
    let delta_event = receiver.recv().await.expect("final delta");

    assert_eq!(result, Message::text(Role::Assistant, "done"));
    assert!(matches!(audit_event, AgentEvent::PermissionAudited { .. }));
    match tool_event {
        AgentEvent::ToolFinished {
            operation_id: actual_operation,
            invocation_id,
            result,
        } => {
            assert_eq!(actual_operation, operation_id);
            assert!(!invocation_id.to_string().is_empty());
            assert!(matches!(
                result.content.as_slice(),
                [ContentBlock::ToolResult(tool_result)]
                    if tool_result.call_id == "provider-call"
                        && tool_result.status == ToolResultStatus::Success
            ));
        }
        other => panic!("unexpected tool event: {other:?}"),
    }
    assert!(matches!(
        delta_event,
        AgentEvent::AssistantTextDelta { operation_id: actual, text, .. }
            if actual == operation_id && text == "done"
    ));
}

#[tokio::test]
async fn tool_round_limit_finishes_failed() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("note.txt"), "contents").expect("fixture");
    let response = ScriptedResponse {
        deltas: Vec::new(),
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "call".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "note.txt"}),
            })],
        },
        usage: None,
    };
    let (provider, _) = ScriptedChatTransport::new(vec![response]);
    let agent = make_agent(provider, workspace.path(), 1);
    let (operation_id, permissions, events, _receiver) = operation_services();
    let mut history = vec![Message::text(Role::User, "loop")];

    let error = agent
        .run_turn(operation_id, &mut history, permissions, events)
        .await
        .expect_err("round limit");

    assert!(error.to_string().contains("1-round tool limit"));
}

#[tokio::test]
async fn subscriber_drop_does_not_change_returned_message() {
    let workspace = tempdir().expect("temporary workspace");
    let response = ScriptedResponse {
        deltas: vec!["still works".to_owned()],
        message: Message::text(Role::Assistant, "still works"),
        usage: None,
    };
    let (provider, _) = ScriptedChatTransport::new(vec![response]);
    let agent = make_agent(provider, workspace.path(), 2);
    let (operation_id, permissions, events, receiver) = operation_services();
    drop(receiver);
    let mut history = vec![Message::text(Role::User, "continue")];

    let result = agent
        .run_turn(operation_id, &mut history, permissions, events)
        .await
        .expect("observer-independent result");

    assert_eq!(result, Message::text(Role::Assistant, "still works"));
}

#[tokio::test]
async fn prompt_prefix_remains_stable_across_streamed_tool_rounds() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("note.txt"), "contents").expect("fixture");
    let responses = vec![
        ScriptedResponse {
            deltas: Vec::new(),
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(ToolCall {
                    id: "call".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "note.txt"}),
                })],
            },
            usage: None,
        },
        ScriptedResponse {
            deltas: vec!["complete".to_owned()],
            message: Message::text(Role::Assistant, "complete"),
            usage: None,
        },
    ];
    let (provider, requests) = ScriptedChatTransport::new(responses);
    let agent = make_agent(provider, workspace.path(), 3);
    let (operation_id, permissions, events, _receiver) = operation_services();
    let mut history = vec![Message::text(Role::User, "read")];

    agent
        .run_turn(operation_id, &mut history, permissions, events)
        .await
        .expect("tool turn");
    let requests = requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0][0], requests[1][0]);
    assert_eq!(requests[0][0].role, Role::System);
    assert!(requests[1].len() > requests[0].len());
}

#[tokio::test]
async fn usage_aggregates_across_tool_rounds_without_filling_missing_fields() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("note.txt"), "contents").expect("fixture");
    let responses = vec![
        ScriptedResponse {
            deltas: Vec::new(),
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(ToolCall {
                    id: "call".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "note.txt"}),
                })],
            },
            usage: Some(ProviderUsage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                total_tokens: Some(12),
            }),
        },
        ScriptedResponse {
            deltas: Vec::new(),
            message: Message::text(Role::Assistant, "complete"),
            usage: Some(ProviderUsage {
                input_tokens: Some(20),
                output_tokens: None,
                total_tokens: None,
            }),
        },
    ];
    let (provider, _) = ScriptedChatTransport::new(responses);
    let agent = make_agent(provider, workspace.path(), 3);
    let (operation_id, permissions, events, _receiver) = operation_services();
    let mut history = vec![Message::text(Role::User, "read")];

    let result = agent
        .run_turn_with_usage(operation_id, &mut history, permissions, events)
        .await
        .expect("tool turn with usage");

    assert_eq!(result.usage.requests, 2);
    assert_eq!(result.usage.input_tokens, Some(30));
    assert_eq!(result.usage.output_tokens, None);
    assert_eq!(result.usage.total_tokens, None);
}

#[tokio::test]
async fn zero_round_limit_fails_before_history_changes() {
    let workspace = tempdir().expect("temporary workspace");
    let (provider, _) = ScriptedChatTransport::new(Vec::new());
    let agent = make_agent(provider, workspace.path(), 0);
    let (operation_id, permissions, events, _receiver) = operation_services();
    let mut history = Vec::new();

    let result = agent
        .run_turn(operation_id, &mut history, permissions, events)
        .await;

    assert!(result.is_err());
    assert!(history.is_empty());
}
