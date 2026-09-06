//! Runs one admitted child and projects its bounded observations into the supervisor stream.

use super::*;
use crate::orchestration::{ChildExecution, ChildResultSchema};
use crate::orchestration::{ChildTerminalStatus, ChildUsage, ExecutionOwner};

const MAX_CHILD_ACTIVITY_EVENTS: usize = 4_096;
const MAX_CHILD_ACTIVITY_BYTES: usize = 4 * 1024 * 1024;
pub(super) const CHILD_OBSERVATION_CHANNEL_CAPACITY: usize = 256;

pub(super) struct RunningChild {
    pub(super) agent_id: AgentId,
    pub(super) attribution: ChildAttribution,
    pub(super) execution: Box<dyn ChildExecution>,
    pub(super) context: ChildExecutionContext,
    pub(super) child_observations: mpsc::Receiver<AgentEvent>,
    pub(super) child_controls: mpsc::UnboundedReceiver<AgentEvent>,
    pub(super) dropped_child_events: DroppedAgentEvents,
    pub(super) broker_task: JoinHandle<()>,
    pub(super) outer_events: mpsc::UnboundedSender<AgentEvent>,
    pub(super) completions: mpsc::UnboundedSender<ChildCompletion>,
    pub(super) report_schema: ChildResultSchema,
    pub(super) report_limits: crate::config::OrchestrationLimits,
    pub(super) artifact_store: ArtifactStore,
    pub(super) artifact_owner: crate::identity::PrincipalId,
    pub(super) commits: ChildCommitSender,
    pub(super) contract: crate::completion_evidence::CompletionContract,
    pub(super) hard_token_limit: Option<u64>,
}

struct ChildActivityForwarder {
    attribution: ChildAttribution,
    outer_events: mpsc::UnboundedSender<AgentEvent>,
    attribution_bytes: usize,
    forwarded_events: usize,
    forwarded_bytes: usize,
    truncated: bool,
}

impl ChildActivityForwarder {
    fn new(attribution: ChildAttribution, outer_events: mpsc::UnboundedSender<AgentEvent>) -> Self {
        let attribution_bytes = encoded_len(&attribution).unwrap_or(usize::MAX);
        Self {
            attribution,
            outer_events,
            attribution_bytes,
            forwarded_events: 0,
            forwarded_bytes: 0,
            truncated: false,
        }
    }

    fn forward(&mut self, activity: ChildActivity) {
        if matches!(activity, ChildActivity::PermissionRequested { .. }) {
            let _ = self.outer_events.send(AgentEvent::ChildActivity {
                attribution: self.attribution.clone(),
                activity,
            });
            return;
        }
        let event_bytes = encoded_len(&activity)
            .and_then(|activity_bytes| activity_bytes.checked_add(self.attribution_bytes));
        let within_budget = self.forwarded_events < MAX_CHILD_ACTIVITY_EVENTS
            && event_bytes.is_some_and(|event_bytes| {
                self.forwarded_bytes
                    .checked_add(event_bytes)
                    .is_some_and(|total| total <= MAX_CHILD_ACTIVITY_BYTES)
            });
        if !within_budget {
            self.truncated = true;
            return;
        }
        self.forwarded_events += 1;
        self.forwarded_bytes += event_bytes.expect("checked activity size exists within budget");
        let _ = self.outer_events.send(AgentEvent::ChildActivity {
            attribution: self.attribution.clone(),
            activity,
        });
    }

    fn finish(self, dropped_before_forwarding: usize) {
        if self.truncated || dropped_before_forwarding > 0 {
            let _ = self.outer_events.send(AgentEvent::ChildActivity {
                attribution: self.attribution,
                activity: ChildActivity::Warning {
                    message: format!(
                        "child live activity exceeded the {MAX_CHILD_ACTIVITY_EVENTS}-event or \
                         {MAX_CHILD_ACTIVITY_BYTES}-byte observation budget or its bounded \
                         producer queue; {dropped_before_forwarding} queued event(s) and further \
                         non-control activity beyond the observation budget were dropped"
                    ),
                },
            });
        }
    }
}

pub(super) async fn run_child_execution(child: RunningChild) {
    let RunningChild {
        agent_id,
        attribution,
        execution,
        context,
        mut child_observations,
        mut child_controls,
        dropped_child_events,
        broker_task,
        outer_events,
        completions,
        report_schema,
        report_limits,
        artifact_store,
        artifact_owner,
        commits,
        contract,
        hard_token_limit,
    } = child;
    let cancellation = context.cancellation.clone();
    let handles_cancellation = execution.handles_cancellation();
    let mut activity_forwarder = ChildActivityForwarder::new(attribution.clone(), outer_events);
    let run = execution.run(context);
    tokio::pin!(run);
    let outcome = loop {
        tokio::select! {
            biased;
            result = &mut run => break result,
            _ = cancellation.cancelled(), if !handles_cancellation => {
                break ChildExecutionOutcome::Cancelled(
                    "child execution observed cancellation".to_owned()
                );
            }
            event = child_controls.recv() => {
                let Some(event) = event else { continue };
                if let Some(activity) = child_activity(event) {
                    activity_forwarder.forward(activity);
                }
            }
            event = child_observations.recv() => {
                let Some(event) = event else { continue };
                if let Some(activity) = child_activity(event) {
                    activity_forwarder.forward(activity);
                }
            }
        }
    };
    while let Ok(event) = child_controls.try_recv() {
        if let Some(activity) = child_activity(event) {
            activity_forwarder.forward(activity);
        }
    }
    while let Ok(event) = child_observations.try_recv() {
        if let Some(activity) = child_activity(event) {
            activity_forwarder.forward(activity);
        }
    }
    activity_forwarder.finish(dropped_child_events.count());
    broker_task.abort();
    let _ = broker_task.await;
    let mut evidence = match &outcome {
        ChildExecutionOutcome::Completed(output) => output.evidence.as_deref().cloned(),
        _ => None,
    }
    .unwrap_or_else(|| {
        crate::completion_evidence::CompletionEvidence::new(
            attribution.operation_id,
            crate::completion_evidence::WorkKind::Child,
            if attribution.owner == ExecutionOwner::Native {
                crate::completion_evidence::EvidenceOwner::Native
            } else {
                crate::completion_evidence::EvidenceOwner::Managed
            },
            match &outcome {
                ChildExecutionOutcome::Completed(_) => {
                    crate::completion_evidence::CompletionClaim::Completed
                }
                ChildExecutionOutcome::Failed(_) => {
                    crate::completion_evidence::CompletionClaim::Failed
                }
                ChildExecutionOutcome::Cancelled(_) => {
                    crate::completion_evidence::CompletionClaim::Cancelled
                }
            },
            contract.clone(),
        )
        .expect("validated child contract")
    });
    evidence.contract = contract;
    evidence.contract_digest = evidence.contract.digest();
    if let ChildExecutionOutcome::Completed(output) = &outcome {
        evidence.delivered(output.text.as_bytes());
        if let Some(limit) = hard_token_limit {
            let used = match output.usage {
                ChildUsage::Measured { total_tokens, .. } => total_tokens,
                _ => None,
            };
            evidence.budget.remaining_tokens = used.map(|used| limit.saturating_sub(used));
            evidence.budget.exhausted |= used.is_none_or(|used| used >= limit);
            evidence.budget.exceeded |= used.is_some_and(|used| used > limit);
            evidence.omitted_observations |= used.is_none();
        }
    }
    let mut materialized = match outcome {
        ChildExecutionOutcome::Completed(output) => {
            materialize_completed_report(
                attribution.clone(),
                report_schema,
                output.text,
                output.usage,
                report_limits.clone(),
                artifact_store.clone(),
                artifact_owner,
            )
            .await
        }
        ChildExecutionOutcome::Failed(reason) => MaterializedChildReport {
            report: ChildReport::failed_with_schema(
                attribution.clone(),
                report_schema,
                reason,
                report_limits.max_report_bytes,
            ),
            artifact: None,
        },
        ChildExecutionOutcome::Cancelled(reason) => MaterializedChildReport {
            report: ChildReport::cancelled_with_schema(
                attribution,
                report_schema,
                reason,
                report_limits.max_report_bytes,
            ),
            artifact: None,
        },
    };
    if let Some(artifact) = &materialized.artifact {
        // Registration precedes any referenced verification receipt.
        if let Err(error) = commits
            .append(SessionRecord::ArtifactRegistered {
                artifact: artifact.clone(),
            })
            .await
        {
            materialized.report = ChildReport::failed_with_schema(
                materialized.report.attribution.clone(),
                report_schema,
                format!("completion artifact registration failed: {error}"),
                report_limits.max_report_bytes,
            );
            materialized.artifact = None;
            evidence.claim = crate::completion_evidence::CompletionClaim::Failed;
        } else {
            crate::completion_evidence::add_artifact(&mut evidence, artifact.clone());
            materialized.artifact = None; // already registered, do not duplicate at report commit
        }
    }
    evidence.revision = 2;
    if materialized.report.status != ChildTerminalStatus::Completed {
        evidence.claim = crate::completion_evidence::CompletionClaim::Failed;
    }
    let reserved = evidence
        .reserve_verifier(!cancellation.is_cancelled(), cancellation.is_cancelled())
        .unwrap_or(false);
    if let Err(error) = commits
        .append(SessionRecord::CompletionEvidenceRecorded {
            evidence: evidence.clone(),
        })
        .await
    {
        materialized.report = ChildReport::failed_with_schema(
            materialized.report.attribution.clone(),
            report_schema,
            format!("completion evidence commit failed: {error}"),
            report_limits.max_report_bytes,
        );
    } else if reserved {
        let reservation = evidence.clone();
        let cancelled = cancellation.clone();
        if let Ok(Ok(verified)) = tokio::task::spawn_blocking(move || {
            crate::completion_evidence::verify_artifacts(reservation, &artifact_store, &cancelled)
        })
        .await
            && commits
                .append(SessionRecord::CompletionEvidenceRecorded {
                    evidence: verified.clone(),
                })
                .await
                .is_ok()
        {
            evidence = verified;
        }
    }
    materialized
        .report
        .apply_completion_evidence(evidence, report_limits.max_report_bytes);
    let _ = completions.send(ChildCompletion {
        agent_id,
        materialized,
    });
}

fn child_activity(event: AgentEvent) -> Option<ChildActivity> {
    match event {
        AgentEvent::PromptPlanUpdated { ledger, .. } => Some(ChildActivity::PromptPlan {
            ledger: Box::new(ledger),
        }),
        AgentEvent::AssistantTextDelta { step_id, text, .. } => {
            Some(ChildActivity::AssistantTextDelta { step_id, text })
        }
        AgentEvent::ProviderReasoningDelta { step_id, text, .. } => {
            Some(ChildActivity::ProviderReasoningDelta { step_id, text })
        }
        AgentEvent::PermissionRequested { request } => {
            Some(ChildActivity::PermissionRequested { request })
        }
        AgentEvent::PermissionAudited { fact } => Some(ChildActivity::PermissionAudited { fact }),
        AgentEvent::ToolFinished {
            invocation_id,
            result,
            ..
        } => Some(ChildActivity::ToolFinished {
            invocation_id,
            result,
        }),
        AgentEvent::OperationFailed { reason, .. } => Some(ChildActivity::Warning {
            message: truncate_utf8(&reason, 4096),
        }),
        AgentEvent::OperationStateChanged {
            state: OperationState::Suspended,
            ..
        } => Some(ChildActivity::Suspended),
        AgentEvent::ChildActivity { activity, .. } => Some(activity),
        AgentEvent::ExternalAgentActivity { activity, .. } => {
            Some(ChildActivity::ExternalAgent { activity })
        }
        AgentEvent::OperationStateChanged { .. }
        | AgentEvent::CompletionEvidenceRecorded { .. }
        | AgentEvent::TerminalDiagnostic { .. }
        | AgentEvent::BrowserStatus { .. }
        | AgentEvent::InvocationIntentCommitted { .. }
        | AgentEvent::InvocationResultCommitted { .. }
        | AgentEvent::AssistantMessage { .. }
        | AgentEvent::UserMessageCommitted { .. }
        | AgentEvent::UsageObserved { .. }
        | AgentEvent::ConversationCleared
        | AgentEvent::CompactionStarted { .. }
        | AgentEvent::ConversationCompacted { .. }
        | AgentEvent::CompactionUnavailable { .. }
        | AgentEvent::CommandRejected { .. }
        | AgentEvent::ChildLifecycleChanged { .. }
        | AgentEvent::ChildReportCommitted { .. }
        | AgentEvent::ChildListSnapshot { .. }
        | AgentEvent::ChildInspectionSnapshot { .. }
        | AgentEvent::ChildCancellationRequested { .. }
        | AgentEvent::RoundBudgetReached { .. }
        | AgentEvent::RoundBudgetDecisionCommitted { .. } => None,
    }
}
