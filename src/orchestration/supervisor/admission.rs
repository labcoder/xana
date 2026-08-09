//! Exact preparation, atomic budget reservation, and durable child admission.

use super::*;

impl ChildSupervisor {
    pub(super) fn validate_plan(
        &mut self,
        parent_operation_id: OperationId,
        plan: OrchestrationPlan,
    ) -> Result<ValidatedOrchestrationPlan, SupervisorError> {
        let validated = validate_plan_structure(plan)
            .map_err(|error| SupervisorError::Plan(error.to_string()))?;
        let reservations = validated
            .spawn_requests
            .iter()
            .cloned()
            .map(|request| {
                self.prepare_candidate(parent_operation_id, request)
                    .map(|candidate| candidate.reservation)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let reserved = self
            .ledger
            .reserve_batch(&reservations)
            .map_err(SupervisorError::Budget)?;
        for reservation in reserved {
            self.ledger.rollback(reservation);
        }
        Ok(validated)
    }

    pub(super) async fn start_plan(
        &mut self,
        parent_operation_id: OperationId,
        plan: OrchestrationPlan,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<StartedPlan, SupervisorError> {
        if self.started_plans.contains(&plan.plan_id) {
            return Err(SupervisorError::DuplicatePlan(plan.plan_id));
        }
        let validated = validate_plan_structure(plan)
            .map_err(|error| SupervisorError::Plan(error.to_string()))?;
        let mut candidates = Vec::with_capacity(validated.spawn_requests.len());
        for step in &validated.plan.steps {
            let OrchestrationPlanStep::Spawn { id, requests } = step else {
                continue;
            };
            for (output_index, request) in requests.iter().cloned().enumerate() {
                let mut candidate = self.prepare_candidate(parent_operation_id, request)?;
                candidate.snapshot.admission.plan = Some(PlanChildAttribution {
                    plan_id: validated.plan.plan_id,
                    step_id: id.clone(),
                    output_index,
                });
                candidates.push(candidate);
            }
        }
        let reservation_requests = candidates
            .iter()
            .map(|candidate| candidate.reservation.clone())
            .collect::<Vec<_>>();
        let reservations = self
            .ledger
            .reserve_batch(&reservation_requests)
            .map_err(SupervisorError::Budget)?;
        for reservation in reservations {
            self.ledger.rollback(reservation);
        }
        let start = OrchestrationPlanStart {
            plan_id: validated.plan.plan_id,
            operation_id: parent_operation_id,
            fingerprint: validated.fingerprint.clone(),
            step_ids: validated
                .plan
                .steps
                .iter()
                .map(|step| step.id().to_owned())
                .collect(),
        };
        commits
            .append(SessionRecord::OrchestrationPlanStarted { start })
            .await?;
        self.started_plans.insert(validated.plan.plan_id);
        let handles = self.admit_candidates(candidates, commits, events).await?;
        Ok(StartedPlan { validated, handles })
    }

    pub(super) async fn spawn_child(
        &mut self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentHandleSnapshot, SupervisorError> {
        let candidate = self.prepare_candidate(parent_operation_id, request)?;
        let mut reservations = self
            .ledger
            .reserve_batch(std::slice::from_ref(&candidate.reservation))
            .map_err(SupervisorError::Budget)?;
        let reservation = reservations
            .pop()
            .expect("a one-child reservation returns one token");
        let mut snapshot = candidate.snapshot;
        let attribution = snapshot.admission.attribution.clone();

        if let Err(error) = commits
            .append(SessionRecord::ChildAdmitted {
                handle: snapshot.clone(),
            })
            .await
        {
            self.ledger.rollback(reservation);
            return Err(error);
        }
        emit_lifecycle(events, &attribution, ChildLifecycle::Admitted);

        let agent_id = attribution.agent_id;
        self.children.insert(
            agent_id,
            ActiveChild {
                snapshot: snapshot.clone(),
                report: None,
                terminal_error: None,
                waiters: Vec::new(),
                prepared: Some(candidate.prepared),
                permissions: None,
                cancellation: CancellationToken::new(),
                cancellation_requested: false,
                task: None,
                running_reserved: false,
                deadline: Some(candidate.deadline),
            },
        );

        // `ChildAdmitted` is already durable, so a queue-write failure must
        // leave its cumulative session reservation charged and its handle
        // owned by the live supervisor.
        if let Err(error) = commits
            .append(SessionRecord::ChildLifecycleChanged {
                agent_id,
                lifecycle: ChildLifecycle::Queued,
            })
            .await
        {
            self.record_transition_failure(agent_id, &error, commits, events)
                .await;
            return Err(error);
        }
        snapshot.apply_lifecycle(ChildLifecycle::Queued);
        emit_lifecycle(events, &attribution, ChildLifecycle::Queued);

        self.children
            .get_mut(&agent_id)
            .expect("durably admitted child remains supervised")
            .snapshot = snapshot.clone();
        self.admission_queue.push_back(agent_id);
        Ok(snapshot)
    }

    pub(super) async fn spawn_many_children(
        &mut self,
        parent_operation_id: OperationId,
        requests: Vec<SpawnAgentRequest>,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Vec<AgentHandleSnapshot>, SupervisorError> {
        if requests.is_empty() {
            return Err(SupervisorError::Budget(BudgetError::EmptyBatch));
        }
        let maximum_fan_out = self.ledger.budget().limits.max_fan_out;
        if requests.len() > maximum_fan_out {
            return Err(SupervisorError::Budget(BudgetError::FanOut {
                requested: requests.len(),
                maximum: maximum_fan_out,
            }));
        }
        let mut candidates = Vec::with_capacity(requests.len());
        for request in requests {
            candidates.push(self.prepare_candidate(parent_operation_id, request)?);
        }
        self.admit_candidates(candidates, commits, events).await
    }

    async fn admit_candidates(
        &mut self,
        candidates: Vec<AdmissionCandidate>,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Vec<AgentHandleSnapshot>, SupervisorError> {
        let reservation_requests = candidates
            .iter()
            .map(|candidate| candidate.reservation.clone())
            .collect::<Vec<_>>();
        let reservations = self
            .ledger
            .reserve_batch(&reservation_requests)
            .map_err(SupervisorError::Budget)?;

        let mut snapshots = candidates
            .iter()
            .map(|candidate| candidate.snapshot.clone())
            .collect::<Vec<_>>();
        for snapshot in &mut snapshots {
            snapshot.apply_lifecycle(ChildLifecycle::Queued);
        }
        if let Err(error) = commits
            .append(SessionRecord::ChildrenBatchAdmitted {
                handles: snapshots.clone(),
            })
            .await
        {
            for reservation in reservations {
                self.ledger.rollback(reservation);
            }
            return Err(error);
        }

        for (candidate, snapshot) in candidates.into_iter().zip(snapshots.iter()) {
            let attribution = snapshot.admission.attribution.clone();
            let agent_id = attribution.agent_id;
            emit_lifecycle(events, &attribution, ChildLifecycle::Admitted);
            emit_lifecycle(events, &attribution, ChildLifecycle::Queued);
            self.children.insert(
                agent_id,
                ActiveChild {
                    snapshot: snapshot.clone(),
                    report: None,
                    terminal_error: None,
                    waiters: Vec::new(),
                    prepared: Some(candidate.prepared),
                    permissions: None,
                    cancellation: CancellationToken::new(),
                    cancellation_requested: false,
                    task: None,
                    running_reserved: false,
                    deadline: Some(candidate.deadline),
                },
            );
            self.admission_queue.push_back(agent_id);
        }
        Ok(snapshots)
    }

    fn prepare_candidate(
        &self,
        parent_operation_id: OperationId,
        request: SpawnAgentRequest,
    ) -> Result<AdmissionCandidate, SupervisorError> {
        validate_spawn_request(&request)
            .map_err(|reason| SupervisorError::InvalidRequest(reason.to_owned()))?;
        let prepared = self
            .factory
            .prepare(&request)
            .map_err(SupervisorError::Admission)?;
        validate_child_ceiling(&prepared, self.ledger.budget())?;
        let attribution = ChildAttribution::new(
            AgentId::new(),
            self.parent.agent_id,
            parent_operation_id,
            self.parent.thread_id,
            &prepared.resolved,
        );
        let admission = ChildAdmission::new(
            attribution,
            &request.task,
            request.result_schema,
            &prepared.resolved,
        );
        let reservation = ReservationRequest {
            tool_rounds: prepared.resolved.max_tool_rounds,
            context_tokens: prepared.resolved.orchestration.max_context_tokens,
            report_bytes: prepared.resolved.orchestration.max_report_bytes,
            artifact_bytes: prepared.resolved.orchestration.max_artifact_bytes,
        };
        let deadline = time::Instant::now()
            + std::time::Duration::from_secs(prepared.resolved.orchestration.deadline_seconds);
        Ok(AdmissionCandidate {
            snapshot: AgentHandleSnapshot::admitted(admission),
            prepared,
            reservation,
            deadline,
        })
    }
}
