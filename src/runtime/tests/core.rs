//! Foreground runtime protocol, history, observation, and crash-boundary tests.

use super::*;

#[test]
fn commands_and_events_round_trip_through_json() {
    let operation_id = OperationId::new();
    let step_id = StepId::new();
    let invocation_id = ToolInvocationId::new();
    let result_id = crate::identity::ToolResultId::new();
    let message = Message::text(Role::Assistant, "hello");
    let child_attribution = ChildAttribution::new(
        crate::identity::AgentId::new(),
        crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
        operation_id,
        crate::identity::ThreadId::new(),
        &scripted_child_config(&SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: "task".to_owned(),
            result_schema: Default::default(),
            restrictions: Default::default(),
            handoff: Default::default(),
        }),
    );
    let commands = vec![
        RuntimeCommand::SubmitTurn {
            operation_id,
            input: "hello".to_owned(),
        },
        RuntimeCommand::ClearConversation,
        RuntimeCommand::ResumeOperation {
            session_id: crate::identity::SessionId::new(),
            operation_id,
        },
        RuntimeCommand::DecidePermission {
            operation_id,
            invocation_id,
            decision: ControllerDecision::AllowOnce,
        },
        RuntimeCommand::DecideChildPermission {
            agent_id: child_attribution.agent_id,
            operation_id: child_attribution.operation_id,
            invocation_id,
            decision: ControllerDecision::Deny,
        },
        RuntimeCommand::ListChildren,
        RuntimeCommand::InspectChild {
            agent_id: child_attribution.agent_id,
        },
        RuntimeCommand::CancelChild {
            agent_id: child_attribution.agent_id,
        },
        RuntimeCommand::Shutdown,
    ];
    let events = vec![
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        },
        AgentEvent::ChildLifecycleChanged {
            attribution: child_attribution.clone(),
            lifecycle: ChildLifecycle::Running,
        },
        AgentEvent::ChildReportCommitted {
            report: ChildReport::completed(
                child_attribution,
                "done".to_owned(),
                ChildUsage::Unknown,
            ),
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
        AgentEvent::PermissionRequested {
            request: PermissionRequest {
                operation_id,
                invocation_id,
                tool_name: "read_file".to_owned(),
                effect_class: crate::tool::EffectClass::Read,
                final_arguments: serde_json::json!({"path": "README.md"}),
                scope: PermissionScope::Unscoped,
            },
        },
        AgentEvent::PermissionAudited {
            fact: PermissionAuditFact {
                request: PermissionRequest {
                    operation_id,
                    invocation_id,
                    tool_name: "read_file".to_owned(),
                    effect_class: crate::tool::EffectClass::Read,
                    final_arguments: serde_json::json!({"path": "README.md"}),
                    scope: PermissionScope::Unscoped,
                },
                policy_evaluation: PolicyDecision::Ask,
                controller_decision: Some(ControllerDecision::AllowOnce),
                effective: PolicyDecision::Allow,
            },
        },
        AgentEvent::InvocationIntentCommitted {
            intent: crate::operation::InvocationIntent {
                operation_id,
                step_id,
                invocation_id,
                result_id,
                model_call_id: "call-1".to_owned(),
                target: crate::operation::InvocationTarget::Tool {
                    name: "read_file".to_owned(),
                    contract_version: 1,
                },
                final_arguments: serde_json::json!({"path": "README.md"}),
                permission: PermissionAuditFact {
                    request: PermissionRequest {
                        operation_id,
                        invocation_id,
                        tool_name: "read_file".to_owned(),
                        effect_class: crate::tool::EffectClass::Read,
                        final_arguments: serde_json::json!({"path": "README.md"}),
                        scope: PermissionScope::Unscoped,
                    },
                    policy_evaluation: PolicyDecision::Allow,
                    controller_decision: None,
                    effective: PolicyDecision::Allow,
                },
                saved_replay_safety: crate::tool::ReplaySafety::Safe,
            },
        },
        AgentEvent::InvocationResultCommitted {
            result: crate::operation::InvocationResultRecord {
                operation_id,
                invocation_id,
                result_id,
                outcome: crate::operation::InvocationOutcome::Completed {
                    output: crate::operation::DurableValueRef::InlineJson(serde_json::json!(
                        "contents"
                    )),
                },
            },
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
    let mut runtime = spawn_runtime(agent);
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
async fn persistent_runtime_commits_conversation_before_final_events() {
    let data = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(AtomicBool::new(false));
    let provider = QueueTransport {
        responses: Mutex::new(vec![Ok(Message::text(Role::Assistant, "durable answer"))].into()),
        requests: Arc::clone(&requests),
        completed,
        deltas: Vec::new(),
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), workspace_root.clone());
    let session = DurableSession::create(data.path(), workspace_root.clone())
        .expect("create durable session");
    let session_id = session.session_id();
    let session_path = session.path().to_owned();
    let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace_root)
        .expect("allow policy");
    let mut runtime = RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler)
        .expect("persistent runtime");
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "durable question".to_owned(),
        })
        .await
        .expect("submit durable turn");

    let mut saw_assistant = false;
    loop {
        let event = runtime.next_event().await.expect("runtime event");
        match event {
            AgentEvent::AssistantMessage { .. } => {
                let loaded = SessionStore::inspect(&session_path).expect("inspect live session");
                assert_eq!(loaded.records[0].session_id, session_id);
                assert!(matches!(
                    loaded.records[0].record,
                    crate::session::SessionRecord::SessionCreated { .. }
                ));
                let restored = reduce(&loaded.records).expect("reduce committed records");
                assert_eq!(
                    restored.conversation_path().expect("conversation path"),
                    vec![
                        Message::text(Role::User, "durable question"),
                        Message::text(Role::Assistant, "durable answer"),
                    ]
                );
                saw_assistant = true;
            }
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(OperationOutcome::Completed),
            } if actual == operation_id => break,
            _ => {}
        }
    }
    assert!(saw_assistant);
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
    let mut runtime = spawn_runtime(agent);
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
    let mut runtime = spawn_runtime(agent);
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
    let mut runtime = spawn_runtime(agent);
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
    let RuntimeHandle { commands, events } = spawn_runtime(agent);
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
    let mut runtime = spawn_runtime(agent);
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

struct RuntimeCrashObserver {
    target: CrashSite,
    path: std::path::PathBuf,
    snapshot: Mutex<Option<Vec<crate::session::SessionRecord>>>,
}

impl BoundaryObserver for RuntimeCrashObserver {
    fn reached(&self, site: CrashSite) -> anyhow::Result<()> {
        if site == self.target {
            let loaded = SessionStore::inspect(&self.path)?;
            *self
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(
                loaded
                    .records
                    .into_iter()
                    .map(|record| record.record)
                    .collect(),
            );
            anyhow::bail!("injected runtime crash at {site:?}");
        }
        Ok(())
    }
}

#[tokio::test]
async fn runtime_crash_sites_commit_acceptance_step_and_conversation_in_order() {
    for site in [
        CrashSite::AfterOperationAccepted,
        CrashSite::AfterStepStarted,
        CrashSite::AfterConversationResult,
    ] {
        let data = tempdir().expect("Xana data tempdir");
        let workspace = tempdir().expect("workspace tempdir");
        std::fs::write(workspace.path().join("note.txt"), "durable bytes")
            .expect("write readable fixture");
        let workspace_root = workspace.path().canonicalize().expect("workspace root");
        let session = DurableSession::create(data.path(), workspace_root.clone())
            .expect("create durable session");
        let path = session.path().to_owned();
        let observer = Arc::new(RuntimeCrashObserver {
            target: site,
            path,
            snapshot: Mutex::new(None),
        });
        let response = if site == CrashSite::AfterOperationAccepted {
            Message::text(Role::Assistant, "unreachable")
        } else {
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(crate::message::ToolCall {
                    id: "call-read".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "note.txt"}),
                })],
            }
        };
        let provider = QueueTransport {
            responses: Mutex::new(vec![Ok(response)].into()),
            requests: Arc::new(Mutex::new(Vec::new())),
            completed: Arc::new(AtomicBool::new(false)),
            deltas: Vec::new(),
        };
        let tools = ToolRegistry::builtins_for_tests().expect("builtin tools");
        let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
        let assembler = PromptAssembler::new(
            definitions,
            PromptEnvironment {
                operating_system: "test".to_owned(),
                working_directory: workspace_root.clone(),
                configured_shell: "test shell".to_owned(),
                surface: PromptSurface::Cli,
            },
            None,
            ContextBudget {
                total_tokens: 16_384,
                conversation_reserve_tokens: 4_096,
            },
        );
        let prompt = assembler.assemble(&[]).expect("base prompt");
        let agent = Agent::new(Box::new(provider), tools, workspace_root.clone(), prompt, 2)
            .with_boundary_observer(observer.clone());
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace_root)
            .expect("allow policy");
        let mut runtime = RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler)
            .expect("persistent runtime");
        let operation_id = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input: "exercise crash boundary".to_owned(),
            })
            .await
            .expect("submit turn");

        if site == CrashSite::AfterOperationAccepted {
            loop {
                if matches!(
                    runtime.next_event().await,
                    Some(AgentEvent::CommandRejected { .. })
                ) {
                    break;
                }
            }
        } else {
            assert_eq!(
                receive_finished(&mut runtime, operation_id).await,
                OperationOutcome::Failed
            );
        }

        let records = observer
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("runtime crash prefix");
        let has_accepted = records.iter().any(|record| {
            matches!(
                record,
                crate::session::SessionRecord::OperationAccepted {
                    operation_id: actual,
                    ..
                } if *actual == operation_id
            )
        });
        let has_step = records.iter().any(|record| {
            matches!(
                record,
                crate::session::SessionRecord::StepStarted {
                    operation_id: actual,
                    ..
                } if *actual == operation_id
            )
        });
        let has_result = records.iter().any(|record| {
            matches!(
                record,
                crate::session::SessionRecord::InvocationResultAppended { result }
                    if result.operation_id == operation_id
            )
        });
        assert!(has_accepted, "{site:?}");
        assert_eq!(
            has_step,
            site != CrashSite::AfterOperationAccepted,
            "{site:?}"
        );
        assert_eq!(
            has_result,
            site == CrashSite::AfterConversationResult,
            "{site:?}"
        );
    }
}
