//! Closed orchestration-plan validation, admission, execution, and collection.

use super::*;

#[tokio::test]
async fn durable_plan_start_admits_the_exact_prepared_child_without_reresolution() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let calls = Arc::new(AtomicUsize::new(0));
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(SinglePrepareChildFactory {
            workspace,
            calls: Arc::clone(&calls),
        }),
    );
    let (commits, mut commit_receiver) = ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let plan = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![OrchestrationPlanStep::Spawn {
            id: "only".to_owned(),
            requests: vec![SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "prepare exactly once".to_owned(),
                result_schema: ChildResultSchema::Summary,
                restrictions: Default::default(),
                handoff: Default::default(),
            }],
        }],
    };

    let result = handle
        .execute_orchestration_plan(OperationId::new(), plan)
        .await
        .expect("plan uses its pre-commit preparation");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.steps.len(), 1);
    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn validated_plan_uses_the_supervisor_once_and_preserves_collection_order() {
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
    let spawn_request = |task: &str| SpawnAgentRequest {
        route: Some("worker".to_owned()),
        task: task.to_owned(),
        result_schema: ChildResultSchema::Summary,
        restrictions: Default::default(),
        handoff: Default::default(),
    };
    let plan = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![
            OrchestrationPlanStep::Spawn {
                id: "parallel".to_owned(),
                requests: vec![
                    spawn_request("delay:30:first"),
                    spawn_request("delay:10:second"),
                    spawn_request("delay:20:third"),
                ],
            },
            OrchestrationPlanStep::Collect {
                id: "ordered".to_owned(),
                handles: vec![
                    PlanHandleRef {
                        step: "parallel".to_owned(),
                        index: 2,
                    },
                    PlanHandleRef {
                        step: "parallel".to_owned(),
                        index: 0,
                    },
                    PlanHandleRef {
                        step: "parallel".to_owned(),
                        index: 1,
                    },
                ],
                timeout_ms: None,
                failure_policy: CollectionFailurePolicy::ContinueOnError,
                cancel_remaining: false,
                cancel_on_timeout: false,
            },
        ],
    };
    let operation_id = OperationId::new();
    let first_validation = handle
        .validate_orchestration_plan(operation_id, plan.clone())
        .await
        .expect("first pure validation");
    let second_validation = handle
        .validate_orchestration_plan(operation_id, plan.clone())
        .await
        .expect("repeat pure validation");
    assert_eq!(first_validation, second_validation);
    let mut invalid_route = plan.clone();
    invalid_route.plan_id = OrchestrationPlanId::new();
    let OrchestrationPlanStep::Spawn { requests, .. } = &mut invalid_route.steps[0] else {
        unreachable!("fixture begins with spawn");
    };
    requests[0].route = Some("missing".to_owned());
    assert!(
        handle
            .validate_orchestration_plan(operation_id, invalid_route)
            .await
            .is_err()
    );
    let over_budget = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![OrchestrationPlanStep::Spawn {
            id: "too_wide".to_owned(),
            requests: (0..9)
                .map(|index| spawn_request(&format!("complete:{index}")))
                .collect(),
        }],
    };
    assert!(matches!(
        handle
            .validate_orchestration_plan(operation_id, over_budget)
            .await,
        Err(crate::orchestration::SupervisorError::Budget(_))
    ));
    assert!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty(),
        "validation must commit no records"
    );
    assert!(handle.list_agents().await.expect("children").is_empty());

    let result = handle
        .execute_orchestration_plan(operation_id, plan.clone())
        .await
        .expect("execute plan");
    let OrchestrationPlanStepResult::Spawn { handles, .. } = &result.steps[0] else {
        panic!("first result is spawn");
    };
    assert!(handles.iter().enumerate().all(|(index, handle)| {
        handle.admission.plan.as_ref().is_some_and(|attribution| {
            attribution.plan_id == plan.plan_id
                && attribution.step_id == "parallel"
                && attribution.output_index == index
        })
    }));
    let expected = [
        handles[2].admission.attribution.agent_id,
        handles[0].admission.attribution.agent_id,
        handles[1].admission.attribution.agent_id,
    ];
    let OrchestrationPlanStepResult::Collect { result, .. } = &result.steps[1] else {
        panic!("second result is collection");
    };
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.attribution.agent_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(result.entries.iter().all(|entry| {
        entry.status == Some(ChildTerminalStatus::Completed) && entry.usage == ChildUsage::Unknown
    }));
    assert!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .any(|record| matches!(record, SessionRecord::OrchestrationPlanStarted { .. }))
    );
    assert!(matches!(
        handle.execute_orchestration_plan(operation_id, plan).await,
        Err(crate::orchestration::SupervisorError::DuplicatePlan(_))
    ));

    let cancel_plan = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![
            OrchestrationPlanStep::Spawn {
                id: "pending".to_owned(),
                requests: vec![spawn_request("pending:forever")],
            },
            OrchestrationPlanStep::Cancel {
                id: "stop".to_owned(),
                handle: PlanHandleRef {
                    step: "pending".to_owned(),
                    index: 0,
                },
            },
            OrchestrationPlanStep::Await {
                id: "stopped".to_owned(),
                handle: PlanHandleRef {
                    step: "pending".to_owned(),
                    index: 0,
                },
                timeout_ms: None,
                cancel_on_timeout: false,
            },
        ],
    };
    let cancellation = handle
        .execute_orchestration_plan(OperationId::new(), cancel_plan)
        .await
        .expect("cancel plan");
    let OrchestrationPlanStepResult::Await { outcome, .. } = &cancellation.steps[2] else {
        panic!("final cancellation evidence is await");
    };
    assert!(matches!(
        outcome,
        AwaitAgentOutcome::Report(report)
            if report.status == ChildTerminalStatus::Cancelled
    ));

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn plan_start_persistence_failure_admits_no_child() {
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
        if let Some(command) = commit_receiver.recv().await {
            assert!(matches!(
                command.record,
                SessionRecord::OrchestrationPlanStarted { .. }
            ));
            let _ = command
                .acknowledged
                .send(Err("injected plan start failure".to_owned()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let plan = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![OrchestrationPlanStep::Spawn {
            id: "never".to_owned(),
            requests: vec![SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "complete:never admitted".to_owned(),
                result_schema: ChildResultSchema::Summary,
                restrictions: Default::default(),
                handoff: Default::default(),
            }],
        }],
    };
    assert!(matches!(
        handle
            .execute_orchestration_plan(OperationId::new(), plan)
            .await,
        Err(crate::orchestration::SupervisorError::Durability(_))
    ));
    assert!(handle.list_agents().await.expect("children").is_empty());

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}
