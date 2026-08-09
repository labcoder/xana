//! Command-side client methods for the child-supervisor actor.

use super::*;

struct PlanResultBudget {
    empty_result_bytes: usize,
    retained_step_bytes: usize,
    retained_steps: usize,
}

impl PlanResultBudget {
    fn new(plan_id: OrchestrationPlanId, fingerprint: &str) -> Result<Self, SupervisorError> {
        let empty = OrchestrationPlanResult {
            version: crate::orchestration::ORCHESTRATION_PLAN_VERSION,
            plan_id,
            fingerprint: fingerprint.to_owned(),
            steps: Vec::new(),
        };
        let empty_result_bytes = encoded_len(&empty)
            .ok_or_else(|| SupervisorError::Plan("plan result cannot be encoded".to_owned()))?;
        Ok(Self {
            empty_result_bytes,
            retained_step_bytes: 0,
            retained_steps: 0,
        })
    }

    fn charge(&mut self, step: &OrchestrationPlanStepResult) -> Result<(), SupervisorError> {
        let step_bytes = encoded_len(step).ok_or_else(|| {
            SupervisorError::Plan("plan step result cannot be encoded".to_owned())
        })?;
        let separators = usize::from(self.retained_steps > 0);
        let retained_step_bytes = self
            .retained_step_bytes
            .checked_add(separators)
            .and_then(|bytes| bytes.checked_add(step_bytes))
            .ok_or_else(|| SupervisorError::Plan("plan result size overflowed".to_owned()))?;
        let total = self
            .empty_result_bytes
            .checked_add(retained_step_bytes)
            .ok_or_else(|| SupervisorError::Plan("plan result size overflowed".to_owned()))?;
        if total > MAX_PLAN_RESULT_BYTES {
            return Err(SupervisorError::Plan(format!(
                "plan result would contain {total} bytes, exceeding the \
                 {MAX_PLAN_RESULT_BYTES}-byte model-facing limit"
            )));
        }
        self.retained_step_bytes = retained_step_bytes;
        self.retained_steps += 1;
        Ok(())
    }

    fn retained_bytes(&self) -> usize {
        self.empty_result_bytes + self.retained_step_bytes
    }
}

impl ChildSupervisorHandle {
    #[cfg(test)]
    pub(crate) fn closed_for_test() -> Self {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        Self {
            commands,
            artifact_store: test_artifact_store(),
        }
    }

    pub(crate) async fn spawn_agent(
        &self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
    ) -> Result<AgentHandleSnapshot, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Spawn {
                parent_operation_id,
                request,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn spawn_many(
        &self,
        parent_operation_id: OperationId,
        requests: Vec<SpawnAgentRequest>,
    ) -> Result<Vec<AgentHandleSnapshot>, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::SpawnMany {
                parent_operation_id,
                requests,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn await_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<ChildReport, SupervisorError> {
        match self
            .await_agent_with(agent_id, AwaitAgentOptions::default())
            .await?
        {
            AwaitAgentOutcome::Report(report) => Ok(*report),
            AwaitAgentOutcome::TimedOut { .. } => {
                unreachable!("an unbounded await cannot time out")
            }
        }
    }

    pub(crate) async fn await_agent_with(
        &self,
        agent_id: AgentId,
        options: AwaitAgentOptions,
    ) -> Result<AwaitAgentOutcome, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Await { agent_id, reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        if let Some(timeout) = options.timeout {
            match time::timeout(timeout, response).await {
                Ok(report) => {
                    return report
                        .map_err(|_| SupervisorError::Unavailable)?
                        .map(|report| AwaitAgentOutcome::Report(Box::new(report)));
                }
                Err(_) => {
                    let cancellation_requested = if options.cancel_on_timeout {
                        self.cancel_agent(agent_id).await?.newly_requested
                    } else {
                        false
                    };
                    return Ok(AwaitAgentOutcome::TimedOut {
                        agent_id,
                        cancellation_requested,
                    });
                }
            }
        }
        response
            .await
            .map_err(|_| SupervisorError::Unavailable)?
            .map(|report| AwaitAgentOutcome::Report(Box::new(report)))
    }

    pub(crate) async fn list_agents(&self) -> Result<Vec<ChildInspection>, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::List { reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)
    }

    pub(crate) async fn inspect_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<ChildInspection, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Inspect { agent_id, reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    async fn snapshot_many(
        &self,
        agent_ids: Vec<AgentId>,
    ) -> Result<Vec<ChildInspection>, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::SnapshotMany { agent_ids, reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn collect_agents(
        &self,
        agent_ids: Vec<AgentId>,
        options: CollectAgentsOptions,
    ) -> Result<CollectionResult, SupervisorError> {
        if agent_ids.is_empty() {
            return Err(SupervisorError::InvalidRequest(
                "collect_agents requires at least one child handle".to_owned(),
            ));
        }
        if agent_ids.len() > MAX_COLLECTION_HANDLES {
            return Err(SupervisorError::TooManyCollectionHandles {
                requested: agent_ids.len(),
                maximum: MAX_COLLECTION_HANDLES,
            });
        }

        let mut inspections = self.snapshot_many(agent_ids.clone()).await?;
        let mut observations = BTreeMap::new();
        let mut pending = FuturesUnordered::new();
        let mut stop_after_failure = false;
        for inspection in &mut inspections {
            let agent_id = inspection.handle.admission.attribution.agent_id;
            if let Some(report) = inspection.report.take() {
                stop_after_failure |= options.failure_policy == CollectionFailurePolicy::FailFast
                    && report.status != crate::orchestration::ChildTerminalStatus::Completed;
                observations.insert(agent_id, CollectionObservation::Terminal(Box::new(report)));
            } else if !stop_after_failure {
                let handle = self.clone();
                pending.push(async move { (agent_id, handle.await_agent(agent_id).await) });
            }
        }

        let deadline = options
            .timeout
            .map(|timeout| time::Instant::now() + timeout);
        let mut timed_out = false;
        while !stop_after_failure && !pending.is_empty() {
            tokio::select! {
                biased;
                observation = pending.next() => {
                    let Some((agent_id, report)) = observation else { break };
                    match report {
                        Ok(report) => {
                            stop_after_failure = options.failure_policy
                                == CollectionFailurePolicy::FailFast
                                && report.status != crate::orchestration::ChildTerminalStatus::Completed;
                            observations.insert(
                                agent_id,
                                CollectionObservation::Terminal(Box::new(report)),
                            );
                        }
                        Err(error) => {
                            stop_after_failure = options.failure_policy
                                == CollectionFailurePolicy::FailFast;
                            observations.insert(agent_id, CollectionObservation::Error(error.to_string()));
                        }
                    }
                }
                _ = wait_for_deadline(deadline) => {
                    timed_out = true;
                    break;
                }
            }
        }

        for agent_id in agent_ids {
            if observations.contains_key(&agent_id) {
                continue;
            }
            let should_cancel = (timed_out && options.cancel_on_timeout)
                || (stop_after_failure && options.cancel_remaining);
            let cancellation_requested = if should_cancel {
                self.cancel_agent(agent_id)
                    .await
                    .map(|receipt| receipt.newly_requested)
                    .unwrap_or(false)
            } else {
                false
            };
            let observation = if timed_out {
                CollectionObservation::TimedOut {
                    cancellation_requested,
                }
            } else {
                CollectionObservation::SkippedAfterFailure {
                    cancellation_requested,
                }
            };
            observations.insert(agent_id, observation);
        }

        build_collection_result(
            inspections,
            &mut observations,
            options.failure_policy,
            &self.artifact_store,
        )
        .map_err(SupervisorError::Collection)
    }

    pub(crate) async fn cancel_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<ChildCancellationReceipt, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Cancel { agent_id, reply })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn delegate_agent(
        &self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
    ) -> Result<(AgentHandleSnapshot, ChildReport), SupervisorError> {
        let handle = self.spawn_agent(parent_operation_id, request).await?;
        let report = self
            .await_agent(handle.admission.attribution.agent_id)
            .await?;
        Ok((handle, report))
    }

    pub(crate) async fn validate_orchestration_plan(
        &self,
        parent_operation_id: OperationId,
        plan: OrchestrationPlan,
    ) -> Result<PlanValidationDiagnostic, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::ValidatePlan {
                parent_operation_id,
                plan,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn execute_orchestration_plan(
        &self,
        parent_operation_id: OperationId,
        plan: OrchestrationPlan,
    ) -> Result<OrchestrationPlanResult, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::StartPlan {
                parent_operation_id,
                plan,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        let StartedPlan { validated, handles } =
            response.await.map_err(|_| SupervisorError::Unavailable)??;
        let mut results = Vec::with_capacity(validated.plan.steps.len());
        let mut result_budget =
            PlanResultBudget::new(validated.plan.plan_id, &validated.fingerprint)?;
        for step in &validated.plan.steps {
            let result = match step {
                OrchestrationPlanStep::Spawn { id, .. } => {
                    let range = &validated.spawn_ranges[id];
                    OrchestrationPlanStepResult::Spawn {
                        id: id.clone(),
                        handles: handles[range.clone()].to_vec(),
                    }
                }
                OrchestrationPlanStep::Await {
                    id,
                    handle,
                    timeout_ms,
                    cancel_on_timeout,
                } => OrchestrationPlanStepResult::Await {
                    id: id.clone(),
                    outcome: self
                        .await_agent_with(
                            resolve_handle(handle, &validated.spawn_ranges, &handles),
                            await_options(*timeout_ms, *cancel_on_timeout),
                        )
                        .await?,
                },
                OrchestrationPlanStep::Collect {
                    id,
                    handles: references,
                    timeout_ms,
                    failure_policy,
                    cancel_remaining,
                    cancel_on_timeout,
                } => {
                    let agent_ids = references
                        .iter()
                        .map(|reference| {
                            resolve_handle(reference, &validated.spawn_ranges, &handles)
                        })
                        .collect();
                    OrchestrationPlanStepResult::Collect {
                        id: id.clone(),
                        result: self
                            .collect_agents(
                                agent_ids,
                                collect_options(
                                    *timeout_ms,
                                    *failure_policy,
                                    *cancel_remaining,
                                    *cancel_on_timeout,
                                ),
                            )
                            .await?,
                    }
                }
                OrchestrationPlanStep::Cancel { id, handle } => {
                    OrchestrationPlanStepResult::Cancel {
                        id: id.clone(),
                        receipt: Box::new(
                            self.cancel_agent(resolve_handle(
                                handle,
                                &validated.spawn_ranges,
                                &handles,
                            ))
                            .await?,
                        ),
                    }
                }
            };
            result_budget.charge(&result)?;
            results.push(result);
        }
        let result = OrchestrationPlanResult {
            version: crate::orchestration::ORCHESTRATION_PLAN_VERSION,
            plan_id: validated.plan.plan_id,
            fingerprint: validated.fingerprint,
            steps: results,
        };
        debug_assert_eq!(encoded_len(&result), Some(result_budget.retained_bytes()));
        Ok(result)
    }

    pub(crate) async fn decide_permission(
        &self,
        agent_id: AgentId,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    ) -> Result<(), SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::DecidePermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
                reply,
            })
            .await
            .map_err(|_| SupervisorError::Unavailable)?;
        response.await.map_err(|_| SupervisorError::Unavailable)?
    }

    pub(crate) async fn shutdown(&self) {
        let (reply, response) = oneshot::channel();
        if self
            .commands
            .send(SupervisorCommand::Shutdown { reply })
            .await
            .is_ok()
        {
            let _ = response.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{OrchestrationLimits, PermissionMode},
        identity::{AgentId, OperationId, SessionId, ThreadId},
        orchestration::{
            ChildAdmission, ChildAttribution, ChildResultSchema, ChildUsage, ExecutionOwner,
        },
    };

    fn large_report() -> ChildReport {
        let session_id = SessionId::new();
        ChildReport::completed(
            ChildAttribution {
                agent_id: AgentId::new(),
                parent_agent_id: AgentId::for_session(session_id),
                operation_id: OperationId::new(),
                parent_operation_id: OperationId::new(),
                thread_id: ThreadId::new(),
                route: "worker".to_owned(),
                profile: "worker".to_owned(),
                owner: ExecutionOwner::Native,
                connection: "local".to_owned(),
                model: "small".to_owned(),
            },
            "x".repeat(128 * 1024),
            ChildUsage::Unknown,
        )
    }

    #[test]
    fn plan_result_budget_rejects_repeated_large_awaits_before_retaining_them() {
        let step = OrchestrationPlanStepResult::Await {
            id: "repeat".to_owned(),
            outcome: AwaitAgentOutcome::Report(Box::new(large_report())),
        };
        let mut budget = PlanResultBudget::new(OrchestrationPlanId::new(), "fingerprint")
            .expect("result budget");

        budget.charge(&step).expect("first bounded await");
        assert!(matches!(
            budget.charge(&step),
            Err(SupervisorError::Plan(_))
        ));
        assert!(budget.retained_bytes() <= MAX_PLAN_RESULT_BYTES);
    }

    #[test]
    fn child_control_projection_omits_large_report_bodies() {
        let report = large_report();
        let mut snapshot = AgentHandleSnapshot::admitted(ChildAdmission {
            attribution: report.attribution.clone(),
            plan: None,
            task_preview: "task".to_owned(),
            task_hash: blake3::hash(b"task").to_hex().to_string(),
            result_schema: ChildResultSchema::Summary,
            capabilities: Vec::new(),
            permission_mode: PermissionMode::Deny,
            max_tool_rounds: 1,
            limits: OrchestrationLimits::default(),
            hard_token_limit: None,
            hard_spend_microusd: None,
        });
        snapshot.apply_report(&report);
        let child = ActiveChild {
            snapshot,
            report: Some(report.clone()),
            terminal_error: None,
            waiters: Vec::new(),
            prepared: None,
            permissions: None,
            cancellation: CancellationToken::new(),
            cancellation_requested: false,
            task: None,
            running_reserved: false,
            deadline: None,
        };

        let control = child_control_projection(&child);
        assert!(control.report.is_none());
        assert_eq!(control.handle.report, Some(report.reference.clone()));
        assert_eq!(child_collection_projection(&child).report, Some(report));
    }
}
