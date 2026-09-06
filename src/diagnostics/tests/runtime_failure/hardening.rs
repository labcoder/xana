use super::*;
use crate::failure::{FailureOrigin, TerminalOutcome};
use crate::telemetry::{RuntimeTelemetry, RuntimeTelemetryEvent};

struct ObservedFailureBarrier {
    observed: Arc<tokio::sync::Notify>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl RuntimeTelemetry for ObservedFailureBarrier {
    fn record(&self, event: RuntimeTelemetryEvent) {
        DiagnosticTelemetry.record(event);
    }

    fn terminal(&self, diagnostic: TerminalDiagnostic) {
        DiagnosticTelemetry.terminal(diagnostic);
    }

    fn provider_failure(&self, operation: OperationId, failure: FailureDetails) {
        DiagnosticTelemetry.provider_failure(operation, failure);
        // Deterministically queue Shutdown after the Agent observed the error,
        // but before its completion is available to the biased owner loop.
        self.observed.notify_one();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    }
}

#[test]
fn queued_shutdown_cannot_erase_an_already_observed_provider_failure() {
    let _guard = test_guard();
    let (directory, paths) = fixture();
    let diagnostics = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    let operation = OperationId::new();
    let observed = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let ready = Arc::new(tokio::sync::Notify::new());
        let (release, receiver) = std::sync::mpsc::channel();
        let agent = agent(Box::new(RejectedProvider), directory.path()).with_runtime_telemetry(
            Arc::new(ObservedFailureBarrier {
                observed: Arc::clone(&ready),
                release: Mutex::new(receiver),
            }),
        );
        let policy =
            PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), directory.path()).unwrap();
        let client = wrap(RuntimeHandle::spawn(agent, policy, true));
        let (owner, mut observer) = client.into_parts();
        owner
            .send(crate::frontend::ClientCommand::new(
                RuntimeCommand::SubmitTurn {
                    operation_id: operation,
                    input: "canary-shutdown-race".into(),
                },
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), ready.notified())
            .await
            .unwrap();
        owner
            .send(crate::frontend::ClientCommand::new(
                RuntimeCommand::Shutdown,
            ))
            .await
            .unwrap();
        release.send(()).unwrap();
        while let Ok(event) = tokio::time::timeout(Duration::from_secs(5), observer.next())
            .await
            .unwrap()
        {
            assert!(
                !matches!(event.event, crate::frontend::ClientEvent::Runtime(ref value)
                if matches!(value.as_ref(), AgentEvent::OperationStateChanged {
                    operation_id, state: OperationState::Finished(OperationOutcome::Completed)
                } if *operation_id == operation))
            );
        }
        observer.snapshot().terminal_diagnostics.clone()
    });
    let origin = observed
        .iter()
        .find(|record| {
            record.operation_id == Some(operation)
                && record.failure.category == FailureCategory::ProviderRejected
        })
        .expect("origin before shutdown");
    assert_eq!(origin.outcome, TerminalOutcome::Failed);
    assert!(
        observed
            .iter()
            .any(|record| record.origin == FailureOrigin::Host
                && record.outcome == TerminalOutcome::HostShutdown)
    );
    drop(diagnostics);
    let (text, retained) = retained_terminals(&paths);
    assert!(retained.contains(origin));
    assert!(!text.contains("canary"));
}

struct PersistenceFaultProvider {
    store: crate::storage::ProtectedStore,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl ConversationalProvider for PersistenceFaultProvider {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        _: &'a [&'a ToolDefinition],
        _: StepId,
        _: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        let index = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if index == 0 {
                self.store.reject_diagnostic_fixture_record().unwrap();
                Ok(Message {
                    role: crate::message::Role::Assistant,
                    content: vec![
                        crate::message::ContentBlock::Text("canary-persistence-message".into()),
                        crate::message::ContentBlock::ToolCall(crate::message::ToolCall {
                            id: "fixture-never-dispatched".into(),
                            name: "not_registered".into(),
                            arguments: serde_json::json!({"path":"C:/private/canary"}),
                        }),
                    ],
                })
            } else {
                assert_eq!(index, 1, "no automatic request replay");
                assert!(!messages.iter().flat_map(|message| &message.content).any(|block|
                    matches!(block, crate::message::ContentBlock::ToolCall(_))
                    || matches!(block, crate::message::ContentBlock::Text(text) if text == "canary-persistence-message")),
                    "a rejected durable write cannot contaminate the next provider request");
                Ok(Message::text(crate::message::Role::Assistant, "healthy"))
            }
        })
    }
}

#[test]
fn real_protected_write_rejection_reaches_client_and_retained_diagnostics_then_recovers() {
    let _guard = test_guard();
    let (directory, paths) = fixture();
    let diagnostics = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    let store = crate::storage::ProtectedStore::initialize(
        &directory.path().join("isolated-protected-store"),
        &crate::storage::RecoveryIdentity::generate(),
        &crate::storage::TestCustody::default(),
    )
    .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failed = OperationId::new();
    let observed = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let workspace = directory.path().canonicalize().unwrap();
        let provider = PersistenceFaultProvider {
            store: store.clone(),
            calls: Arc::clone(&calls),
        };
        let session_id = SessionId::new();
        let session = crate::session::DurableSession::create_protected(
            store.clone(),
            workspace.clone(),
            session_id,
        )
        .unwrap();
        let prompt = crate::prompt::PromptAssembler::new(
            Vec::new(),
            environment(&workspace),
            None,
            ContextBudget {
                total_tokens: 16_384,
                conversation_reserve_tokens: 4_096,
            },
        );
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).unwrap();
        let runtime = RuntimeHandle::spawn_persistent(
            agent(Box::new(provider), &workspace),
            policy,
            true,
            session,
            prompt,
            None,
        )
        .unwrap();
        let mut client = wrap(runtime);
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id: failed,
                input: "canary-first-input".into(),
            })
            .await
            .unwrap();
        // A rejected SQLite append poisons this writer. It cannot honestly
        // acknowledge OperationFinished, even though the failure is observable.
        loop {
            match tokio::time::timeout(Duration::from_secs(5), client.next_event())
                .await
                .expect("the failed writer must report its terminal-commit failure")
                .unwrap()
            {
                AgentEvent::OperationFailed {
                    operation_id,
                    reason,
                } if operation_id == failed => {
                    assert!(reason.starts_with("could not commit operation finish:"));
                    break;
                }
                AgentEvent::OperationStateChanged {
                    operation_id,
                    state: OperationState::Finished(_),
                } if operation_id == failed => panic!("poisoned writer fabricated a finish"),
                _ => {}
            }
        }
        let failure = client
            .snapshot()
            .terminal_diagnostics
            .iter()
            .find(|record| {
                record.operation_id == Some(failed)
                    && record.failure.category == FailureCategory::Storage
            })
            .expect("actual failed persistence acknowledgment")
            .clone();
        assert_eq!(failure.failure.stage, FailureStage::Persistence);
        assert!(
            !client
                .snapshot()
                .conversation
                .iter()
                .flat_map(|message| &message.content)
                .any(|block| matches!(block, crate::message::ContentBlock::ToolResult(_)))
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id: OperationId::new(),
                input: "must not use the poisoned writer".into(),
            })
            .await
            .unwrap();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), client.next_event())
                .await
                .expect("poisoned writer must refuse new input")
                .unwrap()
            {
                AgentEvent::CommandRejected { reason } => {
                    assert!(reason.starts_with("could not commit user conversation entry:"));
                    break;
                }
                AgentEvent::OperationStateChanged {
                    state: OperationState::Running,
                    ..
                } => panic!("poisoned writer admitted another turn"),
                _ => {}
            }
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        stop(client).await;
        // Explicitly reopen the same durable Conversation. This is a new
        // request, not recovery/replay of the old accepted operation, which
        // remains inspectable and must not be relabelled safely finished.
        let (_, old) =
            crate::session::DurableSession::inspect_protected(&store, session_id).unwrap();
        assert_eq!(old.operations[&failed], OperationState::Running);
        assert!(old.operation_details[&failed].invocation_order.is_empty());
        let (session, _) =
            crate::session::DurableSession::resume_protected(store.clone(), session_id).unwrap();
        let prompt = crate::prompt::PromptAssembler::new(
            Vec::new(),
            environment(&workspace),
            None,
            ContextBudget {
                total_tokens: 16_384,
                conversation_reserve_tokens: 4_096,
            },
        );
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).unwrap();
        let runtime = RuntimeHandle::spawn_persistent(
            agent(
                Box::new(PersistenceFaultProvider {
                    store: store.clone(),
                    calls: Arc::clone(&calls),
                }),
                &workspace,
            ),
            policy,
            true,
            session,
            prompt,
            None,
        )
        .unwrap();
        let mut client = wrap(runtime);
        let healthy = OperationId::new();
        client
            .send(RuntimeCommand::SubmitTurn {
                operation_id: healthy,
                input: "new healthy request".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            finish(&mut client, healthy).await,
            OperationOutcome::Completed
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        stop(client).await;
        let (_, restored) =
            crate::session::DurableSession::inspect_protected(&store, session_id).unwrap();
        assert_eq!(restored.operations[&failed], OperationState::Running);
        assert_eq!(
            restored.operations[&healthy],
            OperationState::Finished(OperationOutcome::Completed)
        );
        failure
    });
    drop(diagnostics);
    let (text, retained) = retained_terminals(&paths);
    assert!(retained.contains(&observed));
    assert!(!text.contains("canary"));
    let output = directory.path().join("support-persistence.json");
    export_support_bundle(&paths, &output).unwrap();
    assert!(!fs::read_to_string(output).unwrap().contains("canary"));
}

#[test]
fn legacy_identifier_redaction_covers_read_and_export_without_rewriting_evidence() {
    let _guard = test_guard();
    let (directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    let (log_dir, _) = resolve_directories(&paths, &runtime.active.settings).unwrap();
    let crash_dir = runtime.active.crash_dir.clone();
    emit(DiagnosticFact::new(
        DiagnosticLevel::Info,
        DiagnosticTarget::Provider,
        EventKind::ProviderFailed,
        EventOutcome::Failed,
    ));
    drop(runtime);
    let entry = list(&paths)
        .unwrap()
        .into_iter()
        .find(|entry| entry.kind == "log")
        .unwrap();
    let log = log_dir.join(&entry.name);
    let mut record: DiagnosticRecord =
        serde_json::from_str(fs::read_to_string(&log).unwrap().lines().next().unwrap()).unwrap();
    record.subject = Some("C:/private/canary".into());
    record.correlation_id = Some("private-mcp-canary-label".into());
    let original = serde_json::to_vec(&record).unwrap();
    fs::write(&log, &original).unwrap();
    let report = CrashReport {
        version: CRASH_VERSION,
        timestamp_ms: now_ms(),
        xana_version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        pid: std::process::id(),
        kind: EventKind::TaskPanicked,
        thread: "private-canary-thread".into(),
        location_hash: None,
        line: None,
        column: None,
        backtrace_hash: hash_label("fixture"),
        backtrace_lines: 1,
        breadcrumbs: vec![record],
    };
    let crash = crash_dir.join("crash-legacy.json");
    let crash_bytes = serde_json::to_vec(&report).unwrap();
    create_private_file(&crash)
        .unwrap()
        .write_all(&crash_bytes)
        .unwrap();
    assert!(
        !read_records(&paths, &entry.name, 20)
            .unwrap()
            .join("\n")
            .contains("canary")
    );
    assert!(
        !read_records(&paths, "crash-legacy.json", 20)
            .unwrap()
            .join("\n")
            .contains("canary")
    );
    let output = directory.path().join("legacy-support.json");
    export_support_bundle(&paths, &output).unwrap();
    assert!(!fs::read_to_string(output).unwrap().contains("canary"));
    assert_eq!(fs::read(log).unwrap(), original);
    assert_eq!(fs::read(crash).unwrap(), crash_bytes);
}
