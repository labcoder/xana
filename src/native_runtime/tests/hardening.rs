//! Phase 4 resource, persistence-failure, recovery, and shutdown adversarial tests.

use super::*;

#[tokio::test]
async fn report_overflow_is_registered_before_commit_and_missing_bytes_are_attributed() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let artifact_root = directory.path().join("artifacts");
    let artifacts = crate::artifact::ArtifactStore::new(artifact_root.clone());
    let (handle, supervisor) = ChildSupervisor::new_with_budget_and_artifacts(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ImmediateChildFactory {
            workspace,
            requests: Arc::new(Mutex::new(Vec::new())),
        }),
        OrchestrationBudget::new(OrchestrationLimits::default(), 8),
        artifacts,
        crate::identity::PrincipalId::new(),
    );
    let records = Arc::new(Mutex::new(Vec::new()));
    let committed = Arc::clone(&records);
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            committed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(command.record);
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let child = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "overflow report".to_owned(),
                result_schema: ChildResultSchema::Summary,
                restrictions: crate::orchestration::ChildRestrictions {
                    max_report_bytes: Some("child result".len() - 1),
                    ..Default::default()
                },
                handoff: Default::default(),
            },
        )
        .await
        .expect("overflow child");
    let child_id = child.admission.attribution.agent_id;
    let report = handle.await_agent(child_id).await.expect("artifact report");
    let crate::orchestration::ChildReportReference::Artifact { artifact, .. } = &report.reference
    else {
        panic!("one byte over the inline limit must use an artifact");
    };
    let artifact_path = {
        let records = records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let artifact_index = records
            .iter()
            .position(|record| matches!(record, SessionRecord::ArtifactRegistered { .. }))
            .expect("artifact registration");
        let report_index = records
            .iter()
            .position(|record| matches!(record, SessionRecord::ChildReportCommitted { .. }))
            .expect("report commit");
        assert!(artifact_index < report_index);
        artifact_root.join(artifact.content_hash.as_str())
    };
    std::fs::remove_file(artifact_path).expect("remove artifact bytes");

    let collected = handle
        .collect_agents(vec![child_id], CollectAgentsOptions::default())
        .await
        .expect("attributed collection result");
    assert_eq!(
        collected.entries[0].state,
        CollectionEntryState::ArtifactUnavailable
    );
    assert_eq!(collected.entries[0].attribution.agent_id, child_id);

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn invalid_or_oversized_batch_creates_no_child_record_or_event() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let requests_seen = Arc::new(Mutex::new(Vec::new()));
    let limits = OrchestrationLimits {
        max_fan_out: 2,
        max_descendants: 2,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ImmediateChildFactory {
            workspace,
            requests: Arc::clone(&requests_seen),
        }),
        OrchestrationBudget::for_tests(limits, 2),
    );
    let records = Arc::new(Mutex::new(Vec::new()));
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let committed = Arc::clone(&records);
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            committed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(command.record);
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));

    let oversized = (0..3)
        .map(|index| SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: format!("too many {index}"),
            result_schema: Default::default(),
            restrictions: Default::default(),
            handoff: Default::default(),
        })
        .collect();
    assert!(matches!(
        handle.spawn_many(OperationId::new(), oversized).await,
        Err(crate::orchestration::SupervisorError::Budget(
            crate::orchestration::BudgetError::FanOut { .. }
        ))
    ));
    assert!(
        handle
            .spawn_many(
                OperationId::new(),
                vec![
                    SpawnAgentRequest {
                        route: Some("worker".to_owned()),
                        task: "valid first member".to_owned(),
                        result_schema: Default::default(),
                        restrictions: Default::default(),
                        handoff: Default::default(),
                    },
                    SpawnAgentRequest {
                        route: Some("worker".to_owned()),
                        task: "  ".to_owned(),
                        result_schema: Default::default(),
                        restrictions: Default::default(),
                        handoff: Default::default(),
                    },
                ],
            )
            .await
            .is_err()
    );
    tokio::task::yield_now().await;
    assert!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    );
    assert!(event_receiver.try_recv().is_err());
    assert!(
        handle
            .list_agents()
            .await
            .expect("list children")
            .is_empty()
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn competing_batch_callers_share_one_atomic_descendant_ledger() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let running = Arc::new(AtomicUsize::new(0));
    let maximum_running = Arc::new(AtomicUsize::new(0));
    let limits = OrchestrationLimits {
        max_fan_out: 2,
        max_descendants: 2,
        max_concurrency: 1,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(CountingBarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            running,
            maximum_running,
        }),
        OrchestrationBudget::for_tests(limits, 2),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let batch = |label: &str| {
        (0..2)
            .map(|index| SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: format!("{label} {index}"),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            })
            .collect::<Vec<_>>()
    };
    let first_handle = handle.clone();
    let second_handle = handle.clone();
    let first = tokio::spawn(async move {
        first_handle
            .spawn_many(OperationId::new(), batch("first"))
            .await
    });
    let second = tokio::spawn(async move {
        second_handle
            .spawn_many(OperationId::new(), batch("second"))
            .await
    });
    let first = first.await.expect("first caller task");
    let second = second.await.expect("second caller task");
    let winner = match (first, second) {
        (Ok(handles), Err(crate::orchestration::SupervisorError::Budget(_)))
        | (Err(crate::orchestration::SupervisorError::Budget(_)), Ok(handles)) => handles,
        outcomes => panic!("exactly one caller must reserve the shared ledger: {outcomes:?}"),
    };
    assert_eq!(winner.len(), 2);
    let first_start = started.acquire().await.expect("one running child");
    first_start.forget();
    release.add_permits(2);
    let second_start = started.acquire().await.expect("queued child starts fairly");
    second_start.forget();
    for child in winner {
        assert_eq!(
            handle
                .await_agent(child.admission.attribution.agent_id)
                .await
                .expect("winner report")
                .status,
            ChildTerminalStatus::Completed
        );
    }

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn admission_deadline_cancels_running_work_and_releases_its_running_slot() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            dropped: None,
        }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let timed = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "observe the admission deadline".to_owned(),
                result_schema: Default::default(),
                restrictions: crate::orchestration::ChildRestrictions {
                    deadline_seconds: Some(1),
                    ..Default::default()
                },
                handoff: Default::default(),
            },
        )
        .await
        .expect("admit timed child");
    started.notified().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        handle
            .await_agent(timed.admission.attribution.agent_id)
            .await
            .expect("deadline report")
            .status,
        ChildTerminalStatus::Cancelled
    );

    let replacement = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "reuse released reservation".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("replacement admission");
    started.notified().await;
    release.notify_one();
    assert_eq!(
        handle
            .await_agent(replacement.admission.attribution.agent_id)
            .await
            .expect("replacement report")
            .status,
        ChildTerminalStatus::Completed
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn await_timeout_is_not_cancellation_unless_explicitly_requested() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            dropped: None,
        }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let first = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "time out without cancelling".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("first child");
    started.notified().await;
    let first_id = first.admission.attribution.agent_id;
    let collected_timeout = handle
        .collect_agents(
            vec![first_id],
            CollectAgentsOptions {
                timeout: Some(Duration::ZERO),
                ..Default::default()
            },
        )
        .await
        .expect("collection timeout");
    assert_eq!(
        collected_timeout.entries[0].state,
        CollectionEntryState::TimedOut
    );
    assert!(!collected_timeout.entries[0].cancellation_requested);
    assert_eq!(
        handle
            .await_agent_with(
                first_id,
                AwaitAgentOptions {
                    timeout: Some(Duration::ZERO),
                    cancel_on_timeout: false,
                },
            )
            .await
            .expect("timeout outcome"),
        AwaitAgentOutcome::TimedOut {
            agent_id: first_id,
            cancellation_requested: false,
        }
    );
    assert_eq!(
        handle
            .inspect_agent(first_id)
            .await
            .expect("still supervised")
            .handle
            .lifecycle,
        ChildLifecycle::Running
    );
    release.notify_one();
    assert_eq!(
        handle
            .await_agent(first_id)
            .await
            .expect("later collection")
            .status,
        ChildTerminalStatus::Completed
    );

    let second = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "time out and cancel".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("second child");
    started.notified().await;
    let second_id = second.admission.attribution.agent_id;
    assert_eq!(
        handle
            .await_agent_with(
                second_id,
                AwaitAgentOptions {
                    timeout: Some(Duration::ZERO),
                    cancel_on_timeout: true,
                },
            )
            .await
            .expect("cancel-on-timeout outcome"),
        AwaitAgentOutcome::TimedOut {
            agent_id: second_id,
            cancellation_requested: true,
        }
    );
    assert_eq!(
        handle
            .await_agent(second_id)
            .await
            .expect("cancel terminal")
            .status,
        ChildTerminalStatus::Cancelled
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn cancelling_a_child_closes_pending_permission_before_any_effect() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let effect_ran = Arc::new(AtomicBool::new(false));
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(PermissionChildFactory {
            workspace,
            effect_ran: Arc::clone(&effect_ran),
        }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let admitted = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "request one effect".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("admit permission child");
    let agent_id = admitted.admission.attribution.agent_id;
    let request = loop {
        if let AgentEvent::ChildActivity {
            activity: crate::orchestration::ChildActivity::PermissionRequested { request },
            ..
        } = event_receiver.recv().await.expect("child permission event")
        {
            break request;
        }
    };

    assert!(
        handle
            .cancel_agent(agent_id)
            .await
            .expect("cancel permission child")
            .newly_requested
    );
    assert!(
        handle
            .decide_permission(
                agent_id,
                request.operation_id,
                request.invocation_id,
                ControllerDecision::AllowOnce,
            )
            .await
            .is_err(),
        "a cancellation-closed permission request cannot later authorize an effect"
    );
    assert_eq!(
        handle
            .await_agent(agent_id)
            .await
            .expect("cancel report")
            .status,
        ChildTerminalStatus::Cancelled
    );
    assert!(!effect_ran.load(Ordering::SeqCst));

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn runtime_shutdown_observes_child_terminal_commit_before_stopping() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let mut session =
        DurableSession::create(directory.path(), workspace.clone()).expect("durable session");
    let session_id = session.session_id();
    let parent_operation_id = OperationId::new();
    let input_entry_id = session
        .append_message(Message::text(Role::User, "own the shutdown child"))
        .expect("durable parent input");
    session
        .append_record(SessionRecord::OperationAccepted {
            operation_id: parent_operation_id,
            thread_id: session.thread_id(),
            input_entry_id,
        })
        .expect("durable parent operation");
    let started = Arc::new(Notify::new());
    let (supervisor_handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: session.agent_id(),
            thread_id: session.thread_id(),
        },
        Arc::new(BarrierChildFactory {
            workspace: workspace.clone(),
            started: Arc::clone(&started),
            release: Arc::new(Notify::new()),
            dropped: None,
        }),
    );
    let provider = QueueTransport {
        responses: Mutex::new(VecDeque::new()),
        requests: Arc::new(Mutex::new(Vec::new())),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: Vec::new(),
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), workspace.clone());
    let policy =
        PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).expect("allow policy");
    let mut runtime = RuntimeHandle::spawn_persistent_with_supervisor(
        agent,
        policy,
        true,
        session,
        assembler,
        supervisor_handle.clone(),
        supervisor,
    )
    .expect("persistent runtime");
    let admitted = supervisor_handle
        .spawn_agent(
            parent_operation_id,
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "stop with the runtime".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("admit child");
    started.notified().await;

    runtime
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("request shutdown");
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.next_event().await.is_some() {}
    })
    .await
    .expect("runtime shutdown is bounded");

    let (_, restored) =
        DurableSession::inspect_restored(directory.path(), session_id).expect("inspect session");
    let child = restored
        .children
        .get(&admitted.admission.attribution.agent_id)
        .expect("durable child");
    assert_eq!(child.handle.lifecycle, ChildLifecycle::Cancelled);
    assert_eq!(
        child.report.as_ref().map(|report| report.status),
        Some(ChildTerminalStatus::Cancelled)
    );
}

#[tokio::test]
async fn child_events_never_claim_a_transition_whose_commit_failed() {
    for fail_at in 1..=4 {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory
            .path()
            .canonicalize()
            .expect("canonical workspace");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (handle, supervisor) = ChildSupervisor::new(
            ParentExecution {
                agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
                thread_id: crate::identity::ThreadId::new(),
            },
            Arc::new(ImmediateChildFactory {
                workspace,
                requests,
            }),
        );
        let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
        let commit_task = tokio::spawn(async move {
            let mut count = 0;
            while let Some(command) = commit_receiver.recv().await {
                count += 1;
                let result = if count == fail_at {
                    Err(format!("injected commit failure {fail_at}"))
                } else {
                    Ok(())
                };
                let _ = command.acknowledged.send(result);
            }
        });
        let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let supervisor_task = tokio::spawn(supervisor.run(commits, events));

        let spawn = handle
            .spawn_agent(
                OperationId::new(),
                SpawnAgentRequest {
                    route: Some("worker".to_owned()),
                    task: "commit ordering".to_owned(),
                    result_schema: Default::default(),
                    restrictions: Default::default(),
                    handoff: Default::default(),
                },
            )
            .await;
        match fail_at {
            1 => {
                assert!(spawn.is_err(), "admission commit should fail");
                assert!(handle.list_agents().await.expect("children").is_empty());
            }
            2 => {
                assert!(spawn.is_err(), "queued transition should fail");
                let children = handle.list_agents().await.expect("owned child");
                assert_eq!(children.len(), 1);
                let child = &children[0];
                assert_eq!(child.handle.lifecycle, ChildLifecycle::Failed);
                assert!(child.report.is_none());
                let report = handle
                    .await_agent(child.handle.admission.attribution.agent_id)
                    .await
                    .expect("recovered failed report");
                assert_eq!(report.status, ChildTerminalStatus::Failed);
            }
            3 => {
                let child_id = spawn
                    .expect("running transition happens after admission")
                    .admission
                    .attribution
                    .agent_id;
                let report = handle
                    .await_agent(child_id)
                    .await
                    .expect("recovered failed report");
                assert_eq!(report.status, ChildTerminalStatus::Failed);
            }
            4 => {
                let child_id = spawn
                    .expect("report commit happens after admission")
                    .admission
                    .attribution
                    .agent_id;
                assert!(handle.await_agent(child_id).await.is_err());
            }
            _ => unreachable!(),
        }
        tokio::task::yield_now().await;
        let emitted = std::iter::from_fn(|| event_receiver.try_recv().ok()).collect::<Vec<_>>();
        let lifecycle_count = emitted
            .iter()
            .filter(|event| matches!(event, AgentEvent::ChildLifecycleChanged { .. }))
            .count();
        let expected_lifecycle_count = [0, 0, 2, 3, 3][fail_at];
        assert_eq!(
            lifecycle_count, expected_lifecycle_count,
            "commit {fail_at}"
        );
        let terminal_was_emitted = emitted.iter().any(|event| {
            matches!(
                event,
                AgentEvent::ChildReportCommitted { .. }
                    | AgentEvent::ChildLifecycleChanged {
                        lifecycle: ChildLifecycle::Completed
                            | ChildLifecycle::Failed
                            | ChildLifecycle::Cancelled
                            | ChildLifecycle::Interrupted,
                        ..
                    }
            )
        });
        assert_eq!(terminal_was_emitted, matches!(fail_at, 2 | 3));
        if fail_at == 2 {
            assert!(!emitted.iter().any(|event| matches!(
                event,
                AgentEvent::ChildLifecycleChanged {
                    lifecycle: ChildLifecycle::Running,
                    ..
                }
            )));
        }

        handle.shutdown().await;
        supervisor_task.await.expect("supervisor task");
        commit_task.await.expect("commit task");
    }
}
