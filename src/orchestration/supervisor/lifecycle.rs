//! Terminal lifecycle, deadlines, cancellation, and shutdown for supervised children.

use super::*;

impl ChildSupervisor {
    pub(super) async fn start_queued_children(
        &mut self,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        while self.ledger.can_start() {
            let Some(agent_id) = self.admission_queue.pop_front() else {
                break;
            };
            let eligible = self.children.get(&agent_id).is_some_and(|child| {
                child.snapshot.lifecycle == ChildLifecycle::Queued
                    && !child.cancellation_requested
                    && child.terminal_error.is_none()
                    && child.prepared.is_some()
            });
            if !eligible {
                continue;
            }
            let attribution = self.children[&agent_id]
                .snapshot
                .admission
                .attribution
                .clone();
            if let Err(error) = commits
                .append(SessionRecord::ChildLifecycleChanged {
                    agent_id,
                    lifecycle: ChildLifecycle::Running,
                })
                .await
            {
                self.record_transition_failure(agent_id, &error, commits, events)
                    .await;
                continue;
            }

            self.ledger.mark_started();
            let child = self
                .children
                .get_mut(&agent_id)
                .expect("queued child remains supervised");
            let prepared = child
                .prepared
                .take()
                .expect("eligible queued child has prepared execution");
            child.running_reserved = true;
            child.snapshot.apply_lifecycle(ChildLifecycle::Running);
            emit_lifecycle(events, &attribution, ChildLifecycle::Running);

            let (child_events, child_observations, child_controls, dropped_child_events) =
                AgentEventSender::child(CHILD_OBSERVATION_CHANNEL_CAPACITY);
            let (permissions, broker_task) =
                PermissionBroker::spawn(prepared.permission_policy, true, child_events.clone());
            let execution_context = ChildExecutionContext {
                attribution: attribution.clone(),
                operation_id: attribution.operation_id,
                permissions: permissions.clone(),
                events: child_events,
                cancellation: child.cancellation.clone(),
            };
            child.permissions = Some(permissions);
            child.task = Some(tokio::spawn(run_child_execution(RunningChild {
                agent_id,
                attribution,
                execution: prepared.execution,
                context: execution_context,
                child_observations,
                child_controls,
                dropped_child_events,
                broker_task,
                outer_events: events.clone(),
                completions: self.completion_sender.clone(),
                report_schema: child.snapshot.admission.result_schema,
                report_limits: child.snapshot.admission.limits.clone(),
                artifact_store: self.artifact_store.clone(),
                artifact_owner: self.artifact_owner,
            })));
        }
    }

    pub(super) async fn handle_completion(
        &mut self,
        completion: ChildCompletion,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get(&completion.agent_id) else {
            return;
        };
        if child.report.is_some() {
            return;
        }
        let MaterializedChildReport {
            report: materialized_report,
            artifact,
        } = completion.materialized;
        self.commit_terminal_report(
            completion.agent_id,
            materialized_report,
            artifact,
            commits,
            events,
        )
        .await;
    }

    pub(super) fn next_deadline(&self) -> Option<time::Instant> {
        self.children
            .values()
            .filter(|child| {
                !child.snapshot.lifecycle.is_terminal()
                    && child.terminal_error.is_none()
                    && !child.cancellation_requested
            })
            .filter_map(|child| child.deadline)
            .min()
    }

    pub(super) async fn expire_deadlines(
        &mut self,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let now = time::Instant::now();
        let expired = self
            .children
            .iter()
            .filter_map(|(agent_id, child)| {
                (!child.snapshot.lifecycle.is_terminal()
                    && child.terminal_error.is_none()
                    && !child.cancellation_requested
                    && child.deadline.is_some_and(|deadline| deadline <= now))
                .then_some(*agent_id)
            })
            .collect::<Vec<_>>();
        for agent_id in expired {
            let _ = self.request_cancellation(agent_id, commits, events).await;
        }
    }

    pub(super) fn release_child_capacity(&mut self, agent_id: AgentId) {
        let Some(child) = self.children.get_mut(&agent_id) else {
            return;
        };
        if child.running_reserved {
            self.ledger.mark_stopped();
            child.running_reserved = false;
        }
        child.prepared.take();
        child.deadline = None;
    }

    pub(super) async fn commit_terminal_report(
        &mut self,
        agent_id: AgentId,
        report: ChildReport,
        artifact: Option<crate::artifact::ArtifactRecord>,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get_mut(&agent_id) else {
            return;
        };
        if child.report.is_some() || child.terminal_error.is_some() {
            return;
        }
        if let Some(artifact) = artifact
            && let Err(error) = commits
                .append(SessionRecord::ArtifactRegistered { artifact })
                .await
        {
            child.snapshot.apply_lifecycle(ChildLifecycle::Interrupted);
            child.terminal_error = Some(error.clone());
            child
                .permissions
                .take()
                .inspect(|permissions| permissions.shutdown());
            child.task.take();
            for waiter in child.waiters.drain(..) {
                let _ = waiter.send(Err(error.clone()));
            }
            self.release_child_capacity(agent_id);
            return;
        }
        if let Err(error) = commits
            .append(SessionRecord::ChildReportCommitted {
                report: report.clone(),
            })
            .await
        {
            child.snapshot.apply_lifecycle(ChildLifecycle::Interrupted);
            child.terminal_error = Some(error.clone());
            child
                .permissions
                .take()
                .inspect(|permissions| permissions.shutdown());
            child.task.take();
            for waiter in child.waiters.drain(..) {
                let _ = waiter.send(Err(error.clone()));
            }
            self.release_child_capacity(agent_id);
            return;
        }
        child.snapshot.apply_report(&report);
        child.report = Some(report.clone());
        child
            .permissions
            .take()
            .inspect(|permissions| permissions.shutdown());
        child.task.take();
        emit_lifecycle(events, &report.attribution, report.lifecycle());
        let _ = events.send(AgentEvent::ChildReportCommitted {
            report: report.clone(),
        });
        for waiter in child.waiters.drain(..) {
            let _ = waiter.send(Ok(report.clone()));
        }
        self.release_child_capacity(agent_id);
    }

    pub(super) async fn record_transition_failure(
        &mut self,
        agent_id: AgentId,
        transition_error: &SupervisorError,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get(&agent_id) else {
            return;
        };
        let report = ChildReport::failed_with_schema(
            child.snapshot.admission.attribution.clone(),
            child.snapshot.admission.result_schema,
            format!("could not persist child lifecycle transition: {transition_error}"),
            child.snapshot.admission.limits.max_report_bytes,
        );
        self.commit_terminal_report(agent_id, report, None, commits, events)
            .await;
    }

    pub(super) async fn request_cancellation(
        &mut self,
        agent_id: AgentId,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<ChildCancellationReceipt, SupervisorError> {
        let (newly_requested, queued_report) = {
            let child = self
                .children
                .get_mut(&agent_id)
                .ok_or(SupervisorError::UnknownAgent(agent_id))?;
            let newly_requested = !child.snapshot.lifecycle.is_terminal()
                && child.terminal_error.is_none()
                && !child.cancellation_requested;
            let queued_report = if newly_requested {
                child.cancellation_requested = true;
                child.cancellation.cancel();
                if let Some(permissions) = &child.permissions {
                    permissions.shutdown();
                }
                (child.snapshot.lifecycle == ChildLifecycle::Queued).then(|| {
                    ChildReport::cancelled_with_schema(
                        child.snapshot.admission.attribution.clone(),
                        child.snapshot.admission.result_schema,
                        "queued child cancellation was requested and observed".to_owned(),
                        child.snapshot.admission.limits.max_report_bytes,
                    )
                })
            } else {
                None
            };
            (newly_requested, queued_report)
        };
        if let Some(report) = queued_report {
            self.commit_terminal_report(agent_id, report, None, commits, events)
                .await;
        }
        let handle = self
            .children
            .get(&agent_id)
            .ok_or(SupervisorError::UnknownAgent(agent_id))?
            .snapshot
            .clone();
        Ok(ChildCancellationReceipt {
            handle,
            newly_requested,
        })
    }

    pub(super) async fn shutdown_children(
        &mut self,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let active_ids = self
            .children
            .iter()
            .filter_map(|(agent_id, child)| {
                (!child.snapshot.lifecycle.is_terminal() && child.terminal_error.is_none())
                    .then_some(*agent_id)
            })
            .collect::<Vec<_>>();
        for agent_id in &active_ids {
            let _ = self.request_cancellation(*agent_id, commits, events).await;
        }

        let wait_for_terminals = async {
            while self.children.values().any(|child| {
                !child.snapshot.lifecycle.is_terminal() && child.terminal_error.is_none()
            }) {
                let Some(completion) = self.completions.recv().await else {
                    break;
                };
                self.handle_completion(completion, commits, events).await;
            }
        };
        if time::timeout(SHUTDOWN_GRACE, wait_for_terminals)
            .await
            .is_err()
        {
            for agent_id in active_ids {
                self.interrupt_unresponsive_child(agent_id, commits, events)
                    .await;
            }
        }
    }

    pub(super) async fn interrupt_unresponsive_child(
        &mut self,
        agent_id: AgentId,
        commits: &ChildCommitSender,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let Some(child) = self.children.get_mut(&agent_id) else {
            return;
        };
        if child.snapshot.lifecycle.is_terminal() || child.terminal_error.is_some() {
            return;
        }
        if let Some(task) = child.task.take() {
            task.abort();
            let _ = task.await;
        }
        let report = ChildReport::interrupted_with_schema(
            child.snapshot.admission.attribution.clone(),
            child.snapshot.admission.result_schema,
            "child did not stop within the runtime shutdown grace period".to_owned(),
            child.snapshot.admission.limits.max_report_bytes,
        );
        if let Err(error) = commits
            .append(SessionRecord::ChildReportCommitted {
                report: report.clone(),
            })
            .await
        {
            child.terminal_error = Some(error.clone());
            for waiter in child.waiters.drain(..) {
                let _ = waiter.send(Err(error.clone()));
            }
            self.release_child_capacity(agent_id);
            return;
        }
        child.snapshot.apply_report(&report);
        child.report = Some(report.clone());
        child.permissions.take().inspect(|broker| broker.shutdown());
        emit_lifecycle(events, &report.attribution, report.lifecycle());
        let _ = events.send(AgentEvent::ChildReportCommitted {
            report: report.clone(),
        });
        for waiter in child.waiters.drain(..) {
            let _ = waiter.send(Ok(report.clone()));
        }
        self.release_child_capacity(agent_id);
    }
}
