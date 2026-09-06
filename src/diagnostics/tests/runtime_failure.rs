//! Runtime-to-retained-diagnostics regressions; no real provider or home.

use super::*;
use crate::{
    agent::Agent,
    context::ContextBudget,
    identity::{OperationId, StepId},
    message::Message,
    native_runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
    permission::{PermissionPolicy, PolicyDecision},
    prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
    provider::{ConversationalProvider, DeltaSink, ProviderError, ProviderErrorKind},
    tool::{ToolDefinition, ToolRegistry},
};
use crate::{
    failure::{FailureCategory, FailureDetails, FailureStage, MetadataDigest, TerminalDiagnostic},
    frontend::{ClientSnapshotSeed, EmbeddedClient, semantic::HostLocationV1},
    identity::SessionId,
    provider::{anthropic::AnthropicClient, openai_compat::OpenAiCompatClient},
};
use futures::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod hardening;

fn client(provider: Box<dyn ConversationalProvider>, workspace: &Path) -> EmbeddedClient {
    wrap(runtime(provider, workspace))
}

fn wrap(runtime: RuntimeHandle) -> EmbeddedClient {
    EmbeddedClient::from_runtime(
        runtime,
        ClientSnapshotSeed {
            session_id: SessionId::new(),
            connection: "fixture".into(),
            execution_owner: "native".into(),
            model: "fixture".into(),
            reasoning_effort: None,
            host_location: HostLocationV1::Embedded,
            approval_policy: "allow".into(),
            children: Vec::new(),
            resource_policy: Default::default(),
        },
    )
}

async fn finish(client: &mut EmbeddedClient, operation: OperationId) -> OperationOutcome {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .unwrap()
            .unwrap();
        if let AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(outcome),
        } = event
            && operation_id == operation
        {
            return outcome;
        }
    }
}

async fn stop(client: EmbeddedClient) {
    let (owner, mut observer) = client.into_parts();
    owner
        .send(crate::frontend::ClientCommand::new(
            RuntimeCommand::Shutdown,
        ))
        .await
        .unwrap();
    while tokio::time::timeout(Duration::from_secs(5), observer.next())
        .await
        .unwrap()
        .is_ok()
    {}
}

const OPENAI_HEALTHY: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"healthy\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
const ANTHROPIC_HEALTHY: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"healthy\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

async fn read_request(socket: &mut tokio::net::TcpStream) {
    let mut request = Vec::new();
    loop {
        assert!(request.len() < 64 * 1024, "fixture request bound");
        let mut chunk = [0; 2048];
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0);
        request.extend_from_slice(&chunk[..read]);
        if let Some(end) = request.windows(4).position(|value| value == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap();
            if request.len() >= end + 4 + length {
                return;
            }
        }
    }
}

async fn fixture_server(
    status: u16,
    body: &'static str,
    healthy: &'static str,
    idle: bool,
) -> (String, tokio::task::JoinHandle<usize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for index in 0..2 {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            read_request(&mut socket).await;
            let (status, payload) = if index == 0 {
                (status, body)
            } else {
                (200, healthy)
            };
            let length = if index == 0 && idle {
                payload.len() + 100
            } else {
                payload.len()
            };
            let response = format!(
                "HTTP/1.1 {status} Fixture\r\ncontent-type: text/event-stream\r\ncontent-length: {length}\r\nx-request-id: sk-secret-canary-request\r\nrequest-id: sk-secret-canary-request\r\nx-unknown-secret: canary-header-secret\r\nconnection: close\r\n\r\n{payload}"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            if index == 0 && idle {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        2
    });
    (format!("http://{address}"), task)
}

fn retained_terminals(paths: &XanaPaths) -> (String, Vec<TerminalDiagnostic>) {
    let records = list(paths)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == "log")
        .flat_map(|entry| read_records(paths, &entry.name, 1000).unwrap())
        .collect::<Vec<_>>();
    let diagnostics = records
        .iter()
        .filter_map(|record| {
            serde_json::from_str::<DiagnosticRecord>(record)
                .unwrap()
                .terminal
        })
        .collect();
    (records.join("\n"), diagnostics)
}

#[test]
fn real_http_failures_reach_typed_clients_survive_shutdown_and_allow_next_turn_without_replay() {
    let _guard = test_guard();
    let cases = [
        (
            401,
            "canary-private-body",
            FailureCategory::ProviderRejected,
            false,
        ),
        (
            403,
            "canary-private-body",
            FailureCategory::ProviderRejected,
            false,
        ),
        (
            429,
            "canary-private-body",
            FailureCategory::ProviderRateLimited,
            false,
        ),
        (
            503,
            "canary-private-body",
            FailureCategory::ProviderUnavailable,
            false,
        ),
        (
            200,
            "data: canary-malformed-json\n\n",
            FailureCategory::InvalidResponse,
            false,
        ),
        (
            200,
            "data: {\"choices\":[]}\n\n",
            FailureCategory::BrokenStream,
            false,
        ),
        (200, "", FailureCategory::ReadTimeout, true),
    ];
    for (status, body, expected, idle) in cases {
        let (directory, paths) = fixture();
        let diagnostics = DiagnosticRuntime::start(&paths).unwrap().unwrap();
        let failure = tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (url, fixture) = fixture_server(status, body, OPENAI_HEALTHY, idle).await;
            let provider = OpenAiCompatClient::new(url, "canary-model-label".into())
                .with_fixture_timeouts(Duration::from_secs(3), Duration::from_millis(100));
            let mut client = client(Box::new(provider), directory.path());
            let failed = OperationId::new();
            client
                .send(RuntimeCommand::SubmitTurn {
                    operation_id: failed,
                    input: "canary-private-prompt".into(),
                })
                .await
                .unwrap();
            assert_eq!(finish(&mut client, failed).await, OperationOutcome::Failed);
            let observed = client
                .snapshot()
                .terminal_diagnostics
                .iter()
                .find(|value| value.operation_id == Some(failed))
                .unwrap()
                .clone();
            assert_eq!(
                observed.failure.category, expected,
                "status {status}, idle {idle}"
            );
            assert_eq!(observed.failure.http_status, Some(status));
            assert_eq!(
                observed.failure.request_id_digest,
                MetadataDigest::request_id("sk-secret-canary-request")
            );
            let healthy = OperationId::new();
            client
                .send(RuntimeCommand::SubmitTurn {
                    operation_id: healthy,
                    input: "new independent request".into(),
                })
                .await
                .unwrap();
            assert_eq!(
                finish(&mut client, healthy).await,
                OperationOutcome::Completed
            );
            assert_eq!(
                fixture.await.unwrap(),
                2,
                "one request per explicitly submitted operation"
            );
            stop(client).await;
            observed
        });
        drop(diagnostics);
        let (text, retained) = retained_terminals(&paths);
        assert!(
            retained.contains(&failure),
            "fresh inspection must preserve exact consumer provenance"
        );
        assert!(!text.contains("canary"));
        assert!(!text.contains("127.0.0.1"));
    }
}

#[test]
fn anthropic_rejections_use_the_same_content_free_contract() {
    let _guard = test_guard();
    for status in [401, 403, 429, 503] {
        let (directory, paths) = fixture();
        let diagnostics = DiagnosticRuntime::start(&paths).unwrap().unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (url, fixture) = fixture_server(
                status,
                "canary-private-provider-body",
                ANTHROPIC_HEALTHY,
                false,
            )
            .await;
            let provider = AnthropicClient::new(
                url,
                crate::credential::SecretString::new("canary-api-key".into()).unwrap(),
                "fixture",
            );
            let mut client = client(Box::new(provider), directory.path());
            let operation = OperationId::new();
            client
                .send(RuntimeCommand::SubmitTurn {
                    operation_id: operation,
                    input: "canary-private-prompt".into(),
                })
                .await
                .unwrap();
            assert_eq!(
                finish(&mut client, operation).await,
                OperationOutcome::Failed
            );
            assert_eq!(
                client.snapshot().terminal_diagnostics[0].failure,
                FailureDetails::rejection(
                    status,
                    MetadataDigest::request_id("sk-secret-canary-request")
                )
            );
            let next = OperationId::new();
            client
                .send(RuntimeCommand::SubmitTurn {
                    operation_id: next,
                    input: "new request".into(),
                })
                .await
                .unwrap();
            assert_eq!(finish(&mut client, next).await, OperationOutcome::Completed);
            assert_eq!(fixture.await.unwrap(), 2);
            stop(client).await;
        });
        drop(diagnostics);
        assert!(!retained_terminals(&paths).0.contains("canary"));
    }
}

enum Reply {
    Failure(FailureDetails),
    Panic,
    Block(Arc<tokio::sync::Notify>),
    Tool,
    Healthy,
}

struct Scripted(Mutex<VecDeque<Reply>>);

impl ConversationalProvider for Scripted {
    fn stream_message<'a>(
        &'a self,
        _: &'a [Message],
        _: &'a [&'a ToolDefinition],
        _: StepId,
        _: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        let reply = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("one fixture step per request");
        Box::pin(async move {
            match reply {
                Reply::Failure(detail) => {
                    Err(ProviderError::new("canary-secret-error").with_failure(detail))
                }
                Reply::Panic => panic!("canary-private-panic"),
                Reply::Block(started) => {
                    started.notify_one();
                    std::future::pending().await
                }
                Reply::Tool => Ok(Message {
                    role: crate::message::Role::Assistant,
                    content: vec![crate::message::ContentBlock::ToolCall(
                        crate::message::ToolCall {
                            id: "test-tool".into(),
                            name: "not_registered".into(),
                            arguments: serde_json::json!({"private":"canary-tool-data"}),
                        },
                    )],
                }),
                Reply::Healthy => Ok(Message::text(crate::message::Role::Assistant, "healthy")),
            }
        })
    }
}

#[test]
fn typed_timeout_panic_and_cancellation_are_distinct_and_next_turn_recovers() {
    let _guard = test_guard();
    for category in [
        FailureCategory::ConnectTimeout,
        FailureCategory::HostPanic,
        FailureCategory::Cancelled,
    ] {
        let (directory, paths) = fixture();
        let diagnostics = DiagnosticRuntime::start(&paths).unwrap().unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let started = Arc::new(tokio::sync::Notify::new());
            let reply = match category {
                FailureCategory::ConnectTimeout => {
                    Reply::Failure(FailureDetails::new(category, FailureStage::ProviderConnect))
                }
                FailureCategory::HostPanic => Reply::Panic,
                _ => Reply::Block(Arc::clone(&started)),
            };
            let mut client = client(
                Box::new(Scripted(Mutex::new([reply, Reply::Healthy].into()))),
                directory.path(),
            );
            let failed = OperationId::new();
            client
                .send(RuntimeCommand::SubmitTurn {
                    operation_id: failed,
                    input: "canary-input".into(),
                })
                .await
                .unwrap();
            if category == FailureCategory::Cancelled {
                tokio::time::timeout(Duration::from_secs(5), started.notified())
                    .await
                    .unwrap();
                client
                    .send(RuntimeCommand::InterruptOperation {
                        operation_id: failed,
                    })
                    .await
                    .unwrap();
            }
            let expected = if category == FailureCategory::Cancelled {
                OperationOutcome::Interrupted
            } else {
                OperationOutcome::Failed
            };
            assert_eq!(finish(&mut client, failed).await, expected);
            assert_eq!(
                client.snapshot().terminal_diagnostics[0].failure.category,
                category
            );
            let next = OperationId::new();
            client
                .send(RuntimeCommand::SubmitTurn {
                    operation_id: next,
                    input: "independent new turn".into(),
                })
                .await
                .unwrap();
            assert_eq!(finish(&mut client, next).await, OperationOutcome::Completed);
            stop(client).await;
        });
        drop(diagnostics);
        let (text, terminal) = retained_terminals(&paths);
        assert!(
            terminal
                .iter()
                .any(|value| value.failure.category == category)
        );
        assert!(
            terminal
                .iter()
                .any(|value| value.outcome == crate::failure::TerminalOutcome::HostShutdown)
        );
        assert!(!text.contains("canary"));
    }
}

#[test]
fn suspended_and_declined_round_budget_diagnostics_do_not_become_completed() {
    let _guard = test_guard();
    let (directory, paths) = fixture();
    let diagnostics = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut client = client(
            Box::new(Scripted(Mutex::new(
                [Reply::Tool, Reply::Tool, Reply::Healthy].into(),
            ))),
            directory.path(),
        );
        let operation = OperationId::new();
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id: operation,
                input: "fixture".into(),
            })
            .await
            .unwrap();
        let suspension = loop {
            let event = tokio::time::timeout(Duration::from_secs(5), client.next_event())
                .await
                .unwrap()
                .unwrap();
            if let AgentEvent::RoundBudgetReached { suspension } = event {
                break suspension;
            }
        };
        client
            .send(RuntimeCommand::DecideRoundBudget {
                operation_id: operation,
                suspension_id: suspension.id,
                action: crate::native_runtime::RoundBudgetAction::Stop,
            })
            .await
            .unwrap();
        assert_eq!(
            finish(&mut client, operation).await,
            OperationOutcome::Declined
        );
        let outcomes = client
            .snapshot()
            .terminal_diagnostics
            .iter()
            .map(|value| value.outcome)
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes,
            vec![
                crate::failure::TerminalOutcome::Suspended,
                crate::failure::TerminalOutcome::Declined
            ]
        );
        let next = OperationId::new();
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id: next,
                input: "independent request".into(),
            })
            .await
            .unwrap();
        assert_eq!(finish(&mut client, next).await, OperationOutcome::Completed);
        stop(client).await;
    });
    drop(diagnostics);
    assert!(!retained_terminals(&paths).0.contains("canary"));
}

#[test]
fn malicious_metadata_is_absent_before_enqueue_and_in_support_export_at_every_level() {
    let _guard = test_guard();
    let (directory, paths) = fixture();
    let mut config: toml_edit::DocumentMut = fs::read_to_string(paths.config_file())
        .unwrap()
        .parse()
        .unwrap();
    config["diagnostics"]["level"] = toml_edit::value("trace");
    fs::write(paths.config_file(), config.to_string()).unwrap();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    for raw in [
        "sk-secret-canary",
        "aValidLookingSecretCanary123",
        "https://x.invalid/?token=canary",
        "C:/private/canary",
        "canary\r\nAuthorization: secret",
        &"canary".repeat(100),
    ] {
        let failure = FailureDetails::rejection(429, MetadataDigest::request_id(raw));
        let terminal = TerminalDiagnostic::new(
            Some(OperationId::new()),
            None,
            crate::failure::FailureOrigin::Native,
            crate::failure::TerminalOutcome::Failed,
            failure,
        )
        .route(Some(raw), Some(raw));
        assert!(!serde_json::to_string(&terminal).unwrap().contains("canary"));
        for level in [
            DiagnosticLevel::Error,
            DiagnosticLevel::Warn,
            DiagnosticLevel::Info,
            DiagnosticLevel::Debug,
            DiagnosticLevel::Trace,
        ] {
            let mut fact = DiagnosticFact::new(
                level,
                DiagnosticTarget::Runtime,
                EventKind::TerminalDiagnostic,
                EventOutcome::Failed,
            )
            .subject(raw)
            .correlation(raw);
            fact.terminal = Some(terminal.clone());
            emit(fact);
        }
        crate::telemetry::RuntimeTelemetry::record(
            &DiagnosticTelemetry,
            crate::telemetry::RuntimeTelemetryEvent {
                operation_id: OperationId::new(),
                kind: crate::telemetry::RuntimeTelemetryKind::ToolFailed,
                subject: raw.into(),
            },
        );
    }
    // Breadcrumbs are the typed record immediately before serialization/enqueue.
    let queued = serde_json::to_string(&*runtime.active.breadcrumbs.lock().unwrap()).unwrap();
    assert!(!queued.contains("canary"));
    for level in ["error", "warn", "info", "debug", "trace"] {
        assert!(
            queued.contains(&format!("\"level\":\"{level}\"")),
            "configured level not exercised: {level}"
        );
    }
    drop(runtime);
    let output = directory.path().join("support.json");
    export_support_bundle(&paths, &output).unwrap();
    assert!(!fs::read_to_string(output).unwrap().contains("canary"));
}

struct RejectedProvider;

impl ConversationalProvider for RejectedProvider {
    fn stream_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        _step: StepId,
        _deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async {
            Err(ProviderError::classified(
                ProviderErrorKind::Rejected,
                "https://fixture.invalid/private?api_key=canary-provider-body",
            ))
        })
    }
}

fn runtime(provider: Box<dyn ConversationalProvider>, workspace: &Path) -> RuntimeHandle {
    let agent = agent(provider, workspace);
    let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), workspace).unwrap();
    RuntimeHandle::spawn(agent, policy, true)
}

fn environment(workspace: &Path) -> PromptEnvironment {
    PromptEnvironment {
        connection: "test-connection".into(),
        model: "test-model".into(),
        operating_system: "test".into(),
        working_directory: workspace.to_owned(),
        configured_shell: "test".into(),
        surface: PromptSurface::Cli,
    }
}

fn agent(provider: Box<dyn ConversationalProvider>, workspace: &Path) -> Agent {
    let tools = ToolRegistry::new();
    let environment = environment(workspace);
    let prompt = assemble_snapshot(PromptInputs {
        tool_definitions: &tools.definitions(),
        environment: &environment,
        product_documentation: None,
        project_sources: &[],
        budget: ContextBudget {
            total_tokens: 16_384,
            conversation_reserve_tokens: 4_096,
        },
    })
    .unwrap();
    Agent::new(provider, tools, workspace.to_owned(), prompt, 2)
        .with_runtime_telemetry(runtime_telemetry())
}

#[test]
fn provider_failure_origin_survives_consumer_translation_and_immediate_shutdown() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    let diagnostics = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    let operation = OperationId::new();
    let observed = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut runtime = runtime(Box::new(RejectedProvider), _directory.path());
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: operation,
                input: "canary-private-input".into(),
            })
            .await
            .unwrap();
        let mut observed = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
                .await
                .unwrap()
                .unwrap();
            let finished = matches!(event, AgentEvent::OperationStateChanged {
                operation_id, state: OperationState::Finished(OperationOutcome::Failed)
            } if operation_id == operation);
            observed.push(serde_json::to_value(event).unwrap());
            if finished {
                break;
            }
        }
        assert!(runtime.shutdown_owned().await);
        observed
    });
    drop(diagnostics);
    let retained = list(&paths)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == "log")
        .flat_map(|entry| read_records(&paths, &entry.name, 100).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!retained.contains("canary-"));
    assert!(
        retained.contains("provider_rejected"),
        "origin category missing from retained diagnostics: {retained}"
    );
    assert!(
        observed
            .iter()
            .any(|event| event.get("TerminalDiagnostic").is_some()),
        "typed terminal provenance missing from consumer events"
    );
}
