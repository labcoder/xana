use super::*;
use crate::identity::SessionId;
use crate::native_runtime::configuration::{ExecutionRefresh, PreparedExecution};
use crate::profile::execution::ExecutionConfiguration;
use std::path::Path;

fn configuration_fixture() -> (tempfile::TempDir, ExecutionConfiguration) {
    let directory = tempdir().unwrap();
    let paths = crate::paths::XanaPaths::resolve(Some(directory.path().into())).unwrap();
    std::fs::write(
        paths.config_file(),
        crate::config::XanaConfig::render_initial(crate::config::InitialConfig {
            connection: crate::config::InitialConnection::Ollama {
                name: "test".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
            },
            model: "test-model".into(),
            max_tool_rounds: 8,
            shell: Default::default(),
            permission_mode: PermissionMode::Deny,
            reasoning_effort: None,
        })
        .unwrap(),
    )
    .unwrap();
    crate::private_state::ensure_interoperable_records(&paths).unwrap();
    let configuration = crate::profile::execution::resolve_current(&paths, None).unwrap();
    (directory, configuration)
}

fn prepared(
    configuration: ExecutionConfiguration,
    workspace: &Path,
    requests: CapturedRequests,
) -> PreparedExecution {
    let provider = QueueTransport {
        responses: Mutex::new(vec![Ok(Message::text(Role::Assistant, "fixture answer")); 3].into()),
        requests,
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, prompt_assembler) = persistent_agent(Box::new(provider), workspace.to_owned());
    PreparedExecution {
        agent,
        prompt_assembler,
        configuration,
        child_supervisor: None,
        memory: None,
        policy: PermissionPolicy::new(PolicyDecision::Deny, vec![], workspace).unwrap(),
    }
}

struct ScriptedRefresh(Mutex<VecDeque<anyhow::Result<Option<PreparedExecution>>>>);
impl ExecutionRefresh for ScriptedRefresh {
    fn prepare<'a>(
        &'a self,
        _: &'a DurableSession,
        _: &'a ExecutionConfiguration,
    ) -> BoxFuture<'a, anyhow::Result<Option<PreparedExecution>>> {
        Box::pin(async { self.0.lock().unwrap().pop_front().unwrap_or(Ok(None)) })
    }
}

#[tokio::test]
async fn configuration_revisions_preserve_history_and_repair_in_place_in_both_storage_lanes() {
    for protected in [false, true] {
        let (_fixture, first) = configuration_fixture();
        let data = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let workspace = workspace.path().canonicalize().unwrap();
        let home = protected.then(|| {
            crate::storage::ProtectedStore::initialize(
                data.path(),
                &crate::storage::RecoveryIdentity::generate(),
                &crate::storage::TestCustody::default(),
            )
            .unwrap()
        });
        let mut session = match &home {
            Some(home) => {
                DurableSession::create_protected(home.clone(), workspace.clone(), SessionId::new())
                    .unwrap()
            }
            None => DurableSession::create(data.path(), workspace.clone()).unwrap(),
        };
        let id = session.session_id();
        session.configure_execution(first.clone()).unwrap();
        let mut second = first.clone();
        second.profile.max_tool_rounds.value += 1;
        let requests = Arc::new(Mutex::new(vec![]));
        let refresh = ScriptedRefresh(Mutex::new(VecDeque::from([
            Ok(None),
            Err(anyhow::anyhow!("invalid owner settings fixture")),
            Ok(Some(prepared(second.clone(), &workspace, requests.clone()))),
            Ok(None),
        ])));
        let mut runtime = RuntimeHandle::spawn_configurable(
            prepared(first.clone(), &workspace, requests.clone()),
            session,
            Arc::new(refresh),
            None,
        )
        .unwrap();
        let operation = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: operation,
                input: "first question".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_finished(&mut runtime, operation)
            )
            .await
            .unwrap(),
            OperationOutcome::Completed
        );

        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: OperationId::new(),
                input: "not admitted".into(),
            })
            .await
            .unwrap();
        loop {
            if let AgentEvent::CommandRejected { reason } =
                tokio::time::timeout(Duration::from_secs(5), runtime.next_event())
                    .await
                    .unwrap()
                    .unwrap()
            {
                assert!(reason.contains("invalid owner settings fixture"));
                break;
            }
        }
        let second_operation = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: second_operation,
                input: "second question".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_finished(&mut runtime, second_operation)
            )
            .await
            .unwrap(),
            OperationOutcome::Completed
        );
        runtime.send(RuntimeCommand::Shutdown).await.unwrap();
        runtime.exit.changed().await.unwrap();

        let restored = match &home {
            Some(home) => DurableSession::inspect_protected(home, id).unwrap().1,
            None => DurableSession::inspect_restored(data.path(), id).unwrap().1,
        };
        assert_eq!(restored.execution_configuration, Some(second.clone()));
        assert_eq!(
            restored.operation_details[&operation]
                .configuration_digest
                .as_deref(),
            Some(first.digest().as_str())
        );
        assert_eq!(
            restored.operation_details[&second_operation]
                .configuration_digest
                .as_deref(),
            Some(second.digest().as_str())
        );
        let messages = restored.conversation_path().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0], Message::text(Role::User, "first question"));
        assert_eq!(messages[2], Message::text(Role::User, "second question"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].contains(&messages[0]));
        assert!(requests[1].contains(&messages[1]));
    }
}

struct BlockingRefresh(Arc<Notify>);

#[tokio::test]
async fn changed_restart_blocks_old_continuation_but_stop_allows_the_next_turn_here() {
    let (_fixture, configuration) = configuration_fixture();
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let mut session = DurableSession::create(data.path(), workspace.clone()).unwrap();
    let id = session.session_id();
    session.configure_execution(configuration.clone()).unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let provider = QueueTransport {
        responses: Mutex::new(VecDeque::from([Ok(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "fixture".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path":"missing.txt"}),
            })],
        })])),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, prompt_assembler) =
        core::persistent_tool_agent(Box::new(provider), workspace.clone(), 1);
    let mut first = prepared(configuration.clone(), &workspace, requests.clone());
    first.agent = agent;
    first.prompt_assembler = prompt_assembler;
    let refresh = || Arc::new(ScriptedRefresh(Mutex::new(VecDeque::new())));
    let mut runtime = RuntimeHandle::spawn_configurable(first, session, refresh(), None).unwrap();
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "read fixture".into(),
        })
        .await
        .unwrap();
    let suspension = loop {
        if let AgentEvent::RoundBudgetReached { suspension } =
            tokio::time::timeout(Duration::from_secs(5), runtime.next_event())
                .await
                .unwrap()
                .unwrap()
        {
            break suspension;
        }
    };
    runtime.send(RuntimeCommand::Shutdown).await.unwrap();
    runtime.exit.changed().await.unwrap();
    drop(runtime);
    let (session, _) = DurableSession::resume(data.path(), id).unwrap();
    let mut runtime = RuntimeHandle::spawn_configurable(
        prepared(configuration, &workspace, requests.clone()),
        session,
        refresh(),
        Some("fixture inputs changed: Stop first".into()),
    )
    .unwrap();
    runtime
        .send(RuntimeCommand::DecideRoundBudget {
            operation_id,
            suspension_id: suspension.id,
            action: RoundBudgetAction::Continue,
        })
        .await
        .unwrap();
    loop {
        if let AgentEvent::CommandRejected { reason } =
            tokio::time::timeout(Duration::from_secs(5), runtime.next_event())
                .await
                .unwrap()
                .unwrap()
        {
            assert!(reason.contains("Stop first"));
            break;
        }
    }
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "Continue must not call a provider"
    );
    runtime
        .send(RuntimeCommand::DecideRoundBudget {
            operation_id,
            suspension_id: suspension.id,
            action: RoundBudgetAction::Stop,
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            receive_finished(&mut runtime, operation_id)
        )
        .await
        .unwrap(),
        OperationOutcome::Declined
    );
    let next = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: next,
            input: "new question here".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), receive_finished(&mut runtime, next))
            .await
            .unwrap(),
        OperationOutcome::Completed
    );
    runtime.send(RuntimeCommand::Shutdown).await.unwrap();
    runtime.exit.changed().await.unwrap();
    let state = DurableSession::inspect_restored(data.path(), id).unwrap().1;
    assert_eq!(state.operation_details.len(), 2);
    assert_eq!(
        state.operation_details[&operation_id].finished,
        Some(OperationOutcome::Declined)
    );
    assert_eq!(
        state.operation_details[&next].finished,
        Some(OperationOutcome::Completed)
    );
}

impl ExecutionRefresh for BlockingRefresh {
    fn prepare<'a>(
        &'a self,
        _: &'a DurableSession,
        _: &'a ExecutionConfiguration,
    ) -> BoxFuture<'a, anyhow::Result<Option<PreparedExecution>>> {
        Box::pin(async {
            self.0.notify_one();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn shutdown_cancels_configuration_preparation_without_admitting_or_losing_history() {
    let (_fixture, configuration) = configuration_fixture();
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let mut session = DurableSession::create(data.path(), workspace.clone()).unwrap();
    session.configure_execution(configuration.clone()).unwrap();
    let id = session.session_id();
    let started = Arc::new(Notify::new());
    let requests = Arc::new(Mutex::new(vec![]));
    let mut runtime = RuntimeHandle::spawn_configurable(
        prepared(configuration, &workspace, requests.clone()),
        session,
        Arc::new(BlockingRefresh(started.clone())),
        None,
    )
    .unwrap();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: OperationId::new(),
            input: "draft".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    runtime.send(RuntimeCommand::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), runtime.exit.changed())
        .await
        .unwrap()
        .unwrap();
    let state = DurableSession::inspect_restored(data.path(), id).unwrap().1;
    assert!(state.operation_details.is_empty());
    assert!(state.conversation_path().unwrap().is_empty());
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn execution_configuration_cannot_change_or_rebind_after_admission() {
    let (_fixture, first) = configuration_fixture();
    let directory = tempdir().unwrap();
    let mut session =
        DurableSession::create(directory.path(), directory.path().canonicalize().unwrap()).unwrap();
    session.configure_execution(first.clone()).unwrap();
    let entry = session
        .append_message(Message::text(Role::User, "fixture"))
        .unwrap();
    let operation = OperationId::new();
    session
        .append_record(SessionRecord::OperationAccepted {
            operation_id: operation,
            thread_id: session.thread_id(),
            input_entry_id: entry,
        })
        .unwrap();
    let binding = SessionRecord::OperationConfigurationBound {
        operation_id: operation,
        configuration_digest: first.digest(),
    };
    session.append_record(binding.clone()).unwrap();
    assert!(session.append_record(binding).is_err());
    let mut second = first;
    second.profile.max_tool_rounds.value += 1;
    assert!(session.configure_execution(second.clone()).is_err());
    session
        .append_record(SessionRecord::OperationFinished {
            operation_id: operation,
            outcome: OperationOutcome::Interrupted,
        })
        .unwrap();
    session.configure_execution(second).unwrap();
}
