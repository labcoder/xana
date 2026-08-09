//! Root-to-child delegation and runtime commit coordination.

use super::*;

#[tokio::test]
async fn supervisor_wait_services_its_pending_durable_child_commit() {
    let (commits, mut child_commits) = ChildCommitSender::channel();
    let operation_id = OperationId::new();
    let response = commits.append(SessionRecord::OperationStateChanged {
        operation_id,
        state: OperationState::Running,
    });
    let mut session = None;

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        await_supervisor_response(&mut child_commits, &mut session, response),
    )
    .await
    .expect("supervisor response must not deadlock on its runtime-owned commit");

    assert!(result.is_ok());
}

#[tokio::test]
async fn root_tool_delegates_one_durable_child_without_an_intermediate_model_turn() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let session =
        DurableSession::create(directory.path(), workspace.clone()).expect("durable session");
    let session_id = session.session_id();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (supervisor_handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: session.agent_id(),
            thread_id: session.thread_id(),
        },
        Arc::new(ImmediateChildFactory {
            workspace: workspace.clone(),
            requests: Arc::clone(&requests),
        }),
    );
    let root_responses = vec![
        Ok(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "delegate-1".to_owned(),
                name: "delegate_agent".to_owned(),
                arguments: serde_json::json!({
                    "route": "worker",
                    "task": "inspect the bounded seam"
                }),
            })],
        }),
        Ok(Message::text(Role::Assistant, "root collected report")),
    ];
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(root_responses.into()),
        requests: Arc::clone(&captured),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: Vec::new(),
    };
    let mut tools = ToolRegistry::new();
    tools
        .enable_child_delegation(supervisor_handle.clone())
        .expect("delegation tool");
    let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
    let assembler = PromptAssembler::new(
        definitions,
        PromptEnvironment {
            operating_system: "test".to_owned(),
            working_directory: workspace.clone(),
            configured_shell: "test shell".to_owned(),
            surface: PromptSurface::Cli,
        },
        None,
        ContextBudget {
            total_tokens: 16_384,
            conversation_reserve_tokens: 4_096,
        },
    );
    let prompt = assembler.assemble(&[]).expect("root prompt");
    let agent = Agent::new(Box::new(provider), tools, workspace.clone(), prompt, 2);
    let policy =
        PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).expect("allow policy");
    let mut runtime = RuntimeHandle::spawn_persistent_with_supervisor(
        agent,
        policy,
        true,
        session,
        assembler,
        supervisor_handle,
        supervisor,
    )
    .expect("persistent runtime with child supervisor");
    let operation_id = OperationId::new();

    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "delegate once".to_owned(),
        })
        .await
        .expect("submit root turn");

    let mut lifecycle = Vec::new();
    let mut terminal_report = None;
    loop {
        let event = runtime.next_event().await.expect("runtime event");
        match event {
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle: state,
            } => {
                assert_eq!(attribution.parent_operation_id, operation_id);
                assert_eq!(attribution.route, "worker");
                assert_eq!(attribution.connection, "scripted");
                assert_eq!(attribution.model, "child-model");
                lifecycle.push(state);
            }
            AgentEvent::ChildReportCommitted { report } => {
                terminal_report = Some(report);
            }
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(OperationOutcome::Completed),
            } if actual == operation_id => break,
            _ => {}
        }
    }

    assert_eq!(
        lifecycle,
        vec![
            ChildLifecycle::Admitted,
            ChildLifecycle::Queued,
            ChildLifecycle::Running,
            ChildLifecycle::Completed,
        ]
    );
    let report = terminal_report.expect("terminal child report");
    assert_eq!(report.output.as_deref(), Some("child result"));
    assert_eq!(
        requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_slice(),
        &[SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: "inspect the bounded seam".to_owned(),
            result_schema: Default::default(),
            restrictions: Default::default(),
            handoff: Default::default(),
        }]
    );
    {
        let provider_requests = captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(provider_requests.len(), 2);
        assert!(matches!(
            provider_requests[1].last(),
            Some(Message {
                role: Role::Tool,
                content,
            }) if matches!(content.as_slice(), [ContentBlock::ToolResult(result)] if result.output.contains("child result"))
        ));
    }

    runtime
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("shutdown runtime");
    let (_, restored) =
        DurableSession::inspect_restored(directory.path(), session_id).expect("inspect session");
    assert_eq!(restored.children.len(), 1);
    let child = restored.children.values().next().expect("restored child");
    assert_eq!(child.handle.lifecycle, ChildLifecycle::Completed);
    assert_eq!(
        child
            .report
            .as_ref()
            .and_then(|report| report.output.as_deref()),
        Some("child result")
    );
}
