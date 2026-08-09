//! Runs one admitted child and projects its bounded observations into the supervisor stream.

use super::*;
use crate::orchestration::{ChildExecution, ChildResultSchema};

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
    let materialized = match outcome {
        ChildExecutionOutcome::Completed(output) => {
            materialize_completed_report(
                attribution.clone(),
                report_schema,
                output.text,
                output.usage,
                report_limits.clone(),
                artifact_store,
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
    let _ = completions.send(ChildCompletion {
        agent_id,
        materialized,
    });
}

fn child_activity(event: AgentEvent) -> Option<ChildActivity> {
    match event {
        AgentEvent::AssistantTextDelta { step_id, text, .. } => {
            Some(ChildActivity::AssistantTextDelta { step_id, text })
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
        AgentEvent::OperationStateChanged { .. }
        | AgentEvent::InvocationIntentCommitted { .. }
        | AgentEvent::InvocationResultCommitted { .. }
        | AgentEvent::AssistantMessage { .. }
        | AgentEvent::ConversationCleared
        | AgentEvent::CommandRejected { .. }
        | AgentEvent::ChildLifecycleChanged { .. }
        | AgentEvent::ChildReportCommitted { .. }
        | AgentEvent::ChildListSnapshot { .. }
        | AgentEvent::ChildInspectionSnapshot { .. }
        | AgentEvent::ChildCancellationRequested { .. } => None,
    }
}
