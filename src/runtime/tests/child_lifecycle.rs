//! Child lifecycle, cancellation, native/managed execution, and activity projection.

use super::*;

#[tokio::test]
async fn dropping_one_await_does_not_detach_the_supervised_child() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let limits = OrchestrationLimits {
        max_concurrency: 1,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
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
        OrchestrationBudget::for_tests(limits, 8),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let admitted = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "wait at the barrier".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("admit child");
    started.notified().await;
    let child_id = admitted.admission.attribution.agent_id;
    let abandoned = tokio::spawn({
        let handle = handle.clone();
        async move { handle.await_agent(child_id).await }
    });
    tokio::task::yield_now().await;
    abandoned.abort();
    let _ = abandoned.await;

    release.notify_one();
    let report = handle
        .await_agent(child_id)
        .await
        .expect("later await succeeds");
    assert_eq!(report.output.as_deref(), Some("released child"));
    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn completed_children_do_not_replenish_the_total_descendant_budget() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let limits = OrchestrationLimits {
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
            requests,
        }),
        OrchestrationBudget::for_tests(limits, 8),
    );
    let (commits, mut commit_receiver) = ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let request = || SpawnAgentRequest {
        route: Some("worker".to_owned()),
        task: "bounded descendant".to_owned(),
        result_schema: ChildResultSchema::Summary,
        restrictions: Default::default(),
        handoff: Default::default(),
    };

    for _ in 0..2 {
        let child = handle
            .spawn_agent(OperationId::new(), request())
            .await
            .expect("descendant within total bound");
        assert_eq!(
            handle
                .await_agent(child.admission.attribution.agent_id)
                .await
                .expect("completed child")
                .status,
            ChildTerminalStatus::Completed
        );
    }
    assert!(matches!(
        handle.spawn_agent(OperationId::new(), request()).await,
        Err(crate::orchestration::SupervisorError::Budget(
            crate::orchestration::BudgetError::Descendants { remaining: 0, .. }
        ))
    ));
    assert_eq!(handle.list_agents().await.expect("bounded list").len(), 2);

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn running_cancellation_is_observed_once_and_repeated_reads_are_idempotent() {
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
            release,
            dropped: None,
        }),
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
    let admitted = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "cancel at the barrier".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("admit child");
    started.notified().await;
    let agent_id = admitted.admission.attribution.agent_id;

    let first = handle.cancel_agent(agent_id).await.expect("cancel child");
    let repeated = handle
        .cancel_agent(agent_id)
        .await
        .expect("repeat cancellation");
    assert!(first.newly_requested);
    assert!(!repeated.newly_requested);

    let report = handle.await_agent(agent_id).await.expect("cancel report");
    assert_eq!(report.status, ChildTerminalStatus::Cancelled);
    assert_eq!(
        handle.await_agent(agent_id).await.expect("repeat report"),
        report
    );
    let inspection = handle.inspect_agent(agent_id).await.expect("inspect child");
    assert_eq!(inspection.handle.lifecycle, ChildLifecycle::Cancelled);
    assert!(inspection.report.is_none());
    assert_eq!(inspection.handle.report, Some(report.reference));

    tokio::task::yield_now().await;
    let terminal_events = std::iter::from_fn(|| event_receiver.try_recv().ok())
        .filter(|event| {
            matches!(
                event,
                AgentEvent::ChildReportCommitted { .. }
                    | AgentEvent::ChildLifecycleChanged {
                        lifecycle: ChildLifecycle::Cancelled,
                        ..
                    }
            )
        })
        .count();
    assert_eq!(terminal_events, 2, "one lifecycle and one report event");
    assert_eq!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|record| matches!(record, SessionRecord::ChildReportCommitted { .. }))
            .count(),
        1
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn queued_cancellation_never_starts_the_child_and_releases_its_slot_once() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let limits = OrchestrationLimits {
        max_concurrency: 1,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            dropped: Some(Arc::clone(&dropped)),
        }),
        OrchestrationBudget::for_tests(limits, 8),
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
                task: "occupy the only running slot".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("first child");
    started.notified().await;
    let second = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "remain queued".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("second child");
    let second_id = second.admission.attribution.agent_id;
    assert_eq!(second.lifecycle, ChildLifecycle::Queued);

    let receipt = handle
        .cancel_agent(second_id)
        .await
        .expect("cancel queued child");
    assert!(receipt.newly_requested);
    assert_eq!(receipt.handle.lifecycle, ChildLifecycle::Cancelled);
    assert_eq!(
        handle
            .await_agent(second_id)
            .await
            .expect("queued report")
            .status,
        ChildTerminalStatus::Cancelled
    );
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "a terminal queued child must release its prepared execution"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), started.notified())
            .await
            .is_err(),
        "the cancelled queued execution must never start"
    );

    release.notify_one();
    assert_eq!(
        handle
            .await_agent(first.admission.attribution.agent_id)
            .await
            .expect("first report")
            .status,
        ChildTerminalStatus::Completed
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn atomic_batch_runs_in_input_order_without_exceeding_parallel_capacity() {
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
        max_fan_out: 4,
        max_descendants: 4,
        max_concurrency: 2,
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
            running: Arc::clone(&running),
            maximum_running: Arc::clone(&maximum_running),
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
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let operation_id = OperationId::new();
    let requests = (0..4)
        .map(|index| SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: format!("parallel child {index}"),
            result_schema: Default::default(),
            restrictions: Default::default(),
            handoff: Default::default(),
        })
        .collect::<Vec<_>>();

    let handles = handle
        .spawn_many(operation_id, requests)
        .await
        .expect("admit full batch");
    assert_eq!(handles.len(), 4);
    assert!(
        handles
            .iter()
            .all(|handle| handle.lifecycle == ChildLifecycle::Queued)
    );
    let first_starts = started.acquire_many(2).await.expect("two starts");
    first_starts.forget();
    assert_eq!(running.load(Ordering::SeqCst), 2);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), started.acquire())
            .await
            .is_err(),
        "a third child must remain queued"
    );

    release.add_permits(4);
    let remaining_starts = started.acquire_many(2).await.expect("remaining starts");
    remaining_starts.forget();
    for child in &handles {
        let report = handle
            .await_agent(child.admission.attribution.agent_id)
            .await
            .expect("collect child");
        assert_eq!(report.status, ChildTerminalStatus::Completed);
    }
    assert_eq!(maximum_running.load(Ordering::SeqCst), 2);
    assert_eq!(running.load(Ordering::SeqCst), 0);
    assert_eq!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|record| matches!(record, SessionRecord::ChildrenBatchAdmitted { .. }))
            .count(),
        1
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn collection_preserves_input_order_mixed_failures_and_fail_fast_evidence() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ScriptedOutcomeChildFactory { workspace }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let request = |task: &str| SpawnAgentRequest {
        route: Some("worker".to_owned()),
        task: task.to_owned(),
        result_schema: ChildResultSchema::Summary,
        restrictions: Default::default(),
        handoff: Default::default(),
    };
    let children = handle
        .spawn_many(
            OperationId::new(),
            vec![
                request("delay:30:first"),
                request("fail:second failed"),
                request("delay:10:third"),
            ],
        )
        .await
        .expect("mixed batch");
    let ids = children
        .iter()
        .map(|child| child.admission.attribution.agent_id)
        .collect::<Vec<_>>();
    let result = handle
        .collect_agents(ids.clone(), CollectAgentsOptions::default())
        .await
        .expect("continue-on-error collection");
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.attribution.agent_id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.status)
            .collect::<Vec<_>>(),
        vec![
            Some(ChildTerminalStatus::Completed),
            Some(ChildTerminalStatus::Failed),
            Some(ChildTerminalStatus::Completed),
        ]
    );
    assert!(result.complete);
    assert!(
        result
            .entries
            .iter()
            .all(|entry| entry.usage == ChildUsage::Unknown)
    );
    assert_eq!(
        handle
            .collect_agents(ids, CollectAgentsOptions::default())
            .await
            .expect("idempotent repeat"),
        result
    );

    let fail_fast = handle
        .spawn_many(
            OperationId::new(),
            vec![
                request("fail:stop now"),
                request("pending:one"),
                request("pending:two"),
            ],
        )
        .await
        .expect("fail-fast batch");
    let failed_id = fail_fast[0].admission.attribution.agent_id;
    let _ = handle
        .await_agent(failed_id)
        .await
        .expect("failed child report");
    let fail_fast_ids = fail_fast
        .iter()
        .map(|child| child.admission.attribution.agent_id)
        .collect::<Vec<_>>();
    let result = handle
        .collect_agents(
            fail_fast_ids,
            CollectAgentsOptions {
                failure_policy: CollectionFailurePolicy::FailFast,
                cancel_remaining: true,
                ..Default::default()
            },
        )
        .await
        .expect("fail-fast collection");
    assert_eq!(result.entries[0].status, Some(ChildTerminalStatus::Failed));
    assert!(result.entries[1..].iter().all(|entry| {
        entry.state == CollectionEntryState::SkippedAfterFailure && entry.cancellation_requested
    }));
    assert!(!result.complete);

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn mixed_native_routes_preserve_exact_owner_connection_and_model_attribution() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ScriptedOutcomeChildFactory { workspace }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let request = |route: &str, task: &str| SpawnAgentRequest {
        route: Some(route.to_owned()),
        task: task.to_owned(),
        result_schema: ChildResultSchema::Summary,
        restrictions: Default::default(),
        handoff: Default::default(),
    };

    let children = handle
        .spawn_many(
            OperationId::new(),
            vec![
                request("alpha", "delay:20:first"),
                request("beta", "fail:isolated failure"),
                request("alpha", "delay:1:third"),
            ],
        )
        .await
        .expect("mixed native admission");
    let ids = children
        .iter()
        .map(|child| child.admission.attribution.agent_id)
        .collect::<Vec<_>>();
    let result = handle
        .collect_agents(ids, CollectAgentsOptions::default())
        .await
        .expect("mixed native collection");

    let labels = result
        .entries
        .iter()
        .map(|entry| {
            (
                entry.attribution.route.as_str(),
                entry.attribution.owner,
                entry.attribution.connection.as_str(),
                entry.attribution.model.as_str(),
                entry.status,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![
            (
                "alpha",
                ExecutionOwner::Native,
                "native-a",
                "model-a",
                Some(ChildTerminalStatus::Completed),
            ),
            (
                "beta",
                ExecutionOwner::Native,
                "native-b",
                "model-b",
                Some(ChildTerminalStatus::Failed),
            ),
            (
                "alpha",
                ExecutionOwner::Native,
                "native-a",
                "model-a",
                Some(ChildTerminalStatus::Completed),
            ),
        ]
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn managed_child_uses_public_supervision_for_activity_usage_and_interruption() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let runner = Arc::new(SupervisorManagedRunner::default());
    let factory = SupervisorManagedFactory {
        workspace,
        runner: runner.clone(),
    };
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(factory),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let request = |task: &str| SpawnAgentRequest {
        route: Some("codex".to_owned()),
        task: task.to_owned(),
        result_schema: ChildResultSchema::Summary,
        restrictions: Default::default(),
        handoff: Default::default(),
    };

    let completed = handle
        .spawn_agent(OperationId::new(), request("complete"))
        .await
        .expect("managed admission");
    let report = handle
        .await_agent(completed.admission.attribution.agent_id)
        .await
        .expect("managed report");
    runner.started.notified().await;
    assert_eq!(report.status, ChildTerminalStatus::Completed);
    assert_eq!(
        report.usage,
        ChildUsage::Measured {
            input_tokens: Some(20),
            output_tokens: Some(5),
            total_tokens: Some(25),
            requests: 1,
            spend_microusd: None,
        }
    );
    let mut saw_usage = false;
    while let Ok(event) = event_receiver.try_recv() {
        saw_usage |= matches!(
            event,
            AgentEvent::ChildActivity {
                attribution,
                activity: crate::orchestration::ChildActivity::ManagedRuntime {
                    notification: crate::managed::codex::ManagedNotification::TokenUsageUpdated { .. }
                },
            } if attribution.owner == ExecutionOwner::Codex
                && attribution.connection == "codex-test"
                && attribution.model == "gpt-managed"
        );
    }
    assert!(saw_usage);

    let cancelled = handle
        .spawn_agent(OperationId::new(), request("cancel"))
        .await
        .expect("managed cancellation admission");
    runner.started.notified().await;
    handle
        .cancel_agent(cancelled.admission.attribution.agent_id)
        .await
        .expect("request managed cancellation");
    let report = handle
        .await_agent(cancelled.admission.attribution.agent_id)
        .await
        .expect("managed cancellation report");
    assert_eq!(report.status, ChildTerminalStatus::Cancelled);
    assert!(
        report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("interrupt acknowledged"))
    );
    assert_eq!(runner.interruptions.load(Ordering::SeqCst), 1);

    let rejected = handle
        .spawn_agent(OperationId::new(), request("interrupt-rejected"))
        .await
        .expect("managed rejection admission");
    runner.started.notified().await;
    handle
        .cancel_agent(rejected.admission.attribution.agent_id)
        .await
        .expect("request rejected interruption");
    let rejected_report = handle
        .await_agent(rejected.admission.attribution.agent_id)
        .await
        .expect("managed rejection report");
    assert_eq!(rejected_report.status, ChildTerminalStatus::Failed);
    assert!(
        rejected_report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("-32601"))
    );

    let completed_race = handle
        .spawn_agent(OperationId::new(), request("completion-wins"))
        .await
        .expect("managed completion race admission");
    runner.started.notified().await;
    handle
        .cancel_agent(completed_race.admission.attribution.agent_id)
        .await
        .expect("request racing interruption");
    let completed_report = handle
        .await_agent(completed_race.admission.attribution.agent_id)
        .await
        .expect("managed completion race report");
    assert_eq!(completed_report.status, ChildTerminalStatus::Completed);
    assert_eq!(
        completed_report.output.as_deref(),
        Some("Codex completed before interruption took effect")
    );
    assert_eq!(runner.interruptions.load(Ordering::SeqCst), 3);

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn child_activity_flood_is_bounded_and_reports_observation_truncation() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ActivityFloodChildFactory { workspace }),
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
                task: "flood activity".to_owned(),
                result_schema: ChildResultSchema::Summary,
                restrictions: Default::default(),
                handoff: Default::default(),
            },
        )
        .await
        .expect("admit activity child");
    let report = handle
        .await_agent(admitted.admission.attribution.agent_id)
        .await
        .expect("activity child report");
    assert_eq!(report.status, ChildTerminalStatus::Completed);

    let activities = std::iter::from_fn(|| event_receiver.try_recv().ok())
        .filter_map(|event| match event {
            AgentEvent::ChildActivity { activity, .. } => Some(activity),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(activities.len() < 5_000);
    assert!(activities.iter().any(|activity| {
        matches!(
            activity,
            crate::orchestration::ChildActivity::Warning { message }
                if message.contains("observation budget")
        )
    }));
    assert!(activities.iter().any(|activity| {
        matches!(
            activity,
            crate::orchestration::ChildActivity::PermissionRequested { request }
                if request.tool_name == "critical_after_flood"
        )
    }));

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}
