//! Presentation-safe Conversation facts for graphical clients.
//!
//! This module is deliberately a projection layer. It does not own execution,
//! authorization, persistence, or provider state, and it never exposes raw
//! protocol payloads to Desktop presentation code.

use crate::{
    a2a::ExternalAgentActivityKind,
    frontend::{
        ClientEvent, ClientSnapshot,
        semantic::{
            ActivityDisclosureV1, ActivityItemV1, ActivityOwnerV1, ActivityStateV1, AvailabilityV1,
            CompletionReceiptV1, CompletionStatusV1, DecodedSemanticEventV1, ExecutionFactsV1,
            ExecutionOwnerV1, FactAuthorityV1, FactSourceV1, FreshnessV1, HostLocationV1,
            SemanticCodeV1, SemanticEventV1, SemanticParamV1, UsageAccountingV1,
            UsageObservationV1, UsageScopeV1, WorkspaceAuthorityV1,
        },
    },
    managed::codex::ManagedNotification,
    native_runtime::AgentEvent,
    operation::{InvocationOutcome, InvocationTarget},
    orchestration::{ChildActivity, ChildLifecycle},
    prompt::PromptPlanLedger,
};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_PRESENTATION_DETAIL_BYTES: usize = 256 * 1024;

/// Origin of one projected fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopFactSource {
    Runtime,
    Surface,
    Connection,
    Model,
    Route,
    Adapter,
    Provider,
    ManagedRuntime,
    Mcp,
    A2a,
    Measured,
    Estimated,
    Cache,
}

/// Strength of one projected fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopFactAuthority {
    Authoritative,
    ProviderReported,
    Measured,
    Estimated,
}

/// Observation time and optional staleness window for one projected fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopFactFreshness {
    pub observed_at_unix_millis: u64,
    pub max_age_millis: Option<u64>,
}

/// Honest availability state for a capability or observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopAvailability {
    Available,
    Stale,
    Unsupported,
    Unavailable { code: String },
    PermissionRequired { code: String },
}

/// Execution owner for one nested Activity item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopActivityOwner {
    XanaRoot,
    NativeChild { agent_id: String },
    Managed { runtime: String },
    Mcp { server: String },
    A2a { agent: String },
}

/// Lifecycle of one nested Activity item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopActivityState {
    Queued,
    Working,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

/// Disclosure level of provider-visible Activity detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopActivityDisclosure {
    Summary,
    Detail,
    Hidden,
    Unavailable,
}

/// One bounded, owner-qualified Activity item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopActivityItem {
    pub id: String,
    pub parent_id: Option<String>,
    pub operation_id: Option<String>,
    pub owner: DesktopActivityOwner,
    pub state: DesktopActivityState,
    pub summary_code: String,
    pub summary_parameters: Vec<(String, String)>,
    pub disclosed_text: Option<String>,
    pub disclosure: DesktopActivityDisclosure,
    pub source: DesktopFactSource,
    pub freshness: DesktopFactFreshness,
    pub started_at_unix_millis: Option<u64>,
    pub finished_at_unix_millis: Option<u64>,
}

/// Runtime and authority facts for one Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopExecutionFact {
    pub operation_id: String,
    pub owner: String,
    pub host_location: String,
    pub workspace_authority: String,
    pub tool_authority: Vec<String>,
    pub connection: Option<String>,
    pub model: Option<String>,
    pub capability_grants: Vec<String>,
    pub egress_policy: Option<String>,
    pub controller: Option<String>,
    pub approval_policy: String,
    pub source: DesktopFactSource,
    pub freshness: DesktopFactFreshness,
}

/// One source-qualified usage observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopUsageFact {
    pub id: String,
    pub scope: String,
    pub period: String,
    pub accounting: String,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub request_count: Option<u64>,
    pub context_input_tokens: Option<u64>,
    pub context_capacity_tokens: Option<u64>,
    pub cost_microunits: Option<u64>,
    pub availability: DesktopAvailability,
    pub source: DesktopFactSource,
    pub authority: DesktopFactAuthority,
    pub freshness: DesktopFactFreshness,
}

/// One completion check preserved in a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopCompletionCheck {
    pub code: String,
    pub passed: bool,
}

/// Bounded durable evidence for a terminal Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopCompletionReceipt {
    pub id: String,
    pub operation_id: String,
    pub status: String,
    pub execution: DesktopExecutionFact,
    pub artifact_ids: Vec<String>,
    pub checks: Vec<DesktopCompletionCheck>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub request_count: Option<u64>,
    pub warnings: Vec<String>,
    pub source: DesktopFactSource,
    pub authority: DesktopFactAuthority,
    pub freshness: DesktopFactFreshness,
}

/// One actual capability fact. Availability and authorization remain separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRunCapability {
    pub id: String,
    pub availability: DesktopAvailability,
    pub selected: bool,
    pub authorized: bool,
    pub source: DesktopFactSource,
    pub freshness: DesktopFactFreshness,
}

/// Latest native prompt-budget ledger, or an exact reason one is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPromptLedger {
    pub operation_id: Option<String>,
    pub estimated_input_tokens: Option<usize>,
    pub input_budget_tokens: Option<usize>,
    pub context_window_tokens: Option<usize>,
    pub context_window_source: Option<String>,
    pub attachment_count: Option<usize>,
    pub attachment_bytes: Option<u64>,
    pub omitted_source_count: Option<usize>,
    pub unavailable_reason: Option<String>,
}

/// Complete bounded semantic state for the selected Conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConversationFacts {
    pub profile: Option<String>,
    pub activity: Vec<DesktopActivityItem>,
    pub execution: Vec<DesktopExecutionFact>,
    pub usage: Vec<DesktopUsageFact>,
    pub completions: Vec<DesktopCompletionReceipt>,
    pub capabilities: Vec<DesktopRunCapability>,
    pub prompt_ledger: DesktopPromptLedger,
}

pub(super) fn project_conversation_facts(
    snapshot: &ClientSnapshot,
    profile: Option<String>,
) -> DesktopConversationFacts {
    DesktopConversationFacts {
        profile,
        activity: snapshot
            .semantic
            .activity
            .iter()
            .map(project_activity)
            .collect(),
        execution: snapshot
            .semantic
            .execution_facts
            .iter()
            .map(project_execution)
            .collect(),
        usage: snapshot.semantic.usage.iter().map(project_usage).collect(),
        completions: snapshot
            .semantic
            .completion_receipts
            .iter()
            .map(project_completion)
            .collect(),
        capabilities: snapshot
            .semantic
            .capabilities
            .iter()
            .map(|capability| DesktopRunCapability {
                id: capability.id.clone(),
                availability: project_availability(&capability.availability),
                selected: capability.selected,
                authorized: capability.authorized,
                source: project_source(capability.source),
                freshness: project_freshness(&capability.freshness),
            })
            .collect(),
        prompt_ledger: snapshot
            .prompt_plans
            .last()
            .map(|(operation_id, ledger)| project_prompt_ledger(*operation_id, ledger))
            .unwrap_or_else(|| DesktopPromptLedger {
                operation_id: None,
                estimated_input_tokens: None,
                input_budget_tokens: None,
                context_window_tokens: None,
                context_window_source: None,
                attachment_count: None,
                attachment_bytes: None,
                omitted_source_count: None,
                unavailable_reason: Some(if snapshot.execution_owner == "native" {
                    "No native prompt plan has been produced for this Conversation yet".to_owned()
                } else {
                    "The managed runtime does not expose Xana's native prompt ledger".to_owned()
                }),
            }),
    }
}

pub(super) fn project_activity(item: &ActivityItemV1) -> DesktopActivityItem {
    DesktopActivityItem {
        id: item.id.to_string(),
        parent_id: item.parent_id.map(|id| id.to_string()),
        operation_id: item.run_id.map(|id| id.to_string()),
        owner: match &item.owner {
            ActivityOwnerV1::XanaRoot => DesktopActivityOwner::XanaRoot,
            ActivityOwnerV1::NativeChild { agent_id } => DesktopActivityOwner::NativeChild {
                agent_id: agent_id.to_string(),
            },
            ActivityOwnerV1::Managed { runtime } => DesktopActivityOwner::Managed {
                runtime: runtime.clone(),
            },
            ActivityOwnerV1::Mcp { server } => DesktopActivityOwner::Mcp {
                server: server.clone(),
            },
            ActivityOwnerV1::A2a { agent } => DesktopActivityOwner::A2a {
                agent: agent.clone(),
            },
        },
        state: match item.state {
            ActivityStateV1::Queued => DesktopActivityState::Queued,
            ActivityStateV1::Working => DesktopActivityState::Working,
            ActivityStateV1::Waiting => DesktopActivityState::Waiting,
            ActivityStateV1::Completed => DesktopActivityState::Completed,
            ActivityStateV1::Failed => DesktopActivityState::Failed,
            ActivityStateV1::Cancelled => DesktopActivityState::Cancelled,
        },
        summary_code: item.summary.code.clone(),
        summary_parameters: project_parameters(&item.summary),
        disclosed_text: item
            .disclosed_text
            .clone()
            .map(|text| bounded_detail(text, MAX_PRESENTATION_DETAIL_BYTES)),
        disclosure: match item.disclosure {
            ActivityDisclosureV1::Summary => DesktopActivityDisclosure::Summary,
            ActivityDisclosureV1::Detail => DesktopActivityDisclosure::Detail,
            ActivityDisclosureV1::Hidden => DesktopActivityDisclosure::Hidden,
            ActivityDisclosureV1::Unavailable => DesktopActivityDisclosure::Unavailable,
        },
        source: project_source(item.source),
        freshness: project_freshness(&item.freshness),
        started_at_unix_millis: item.started_at_unix_millis,
        finished_at_unix_millis: item.finished_at_unix_millis,
    }
}

/// Projects runtime-only events that do not already have a stronger terminal
/// or Conversation event representation. IDs are stable across intent/result
/// pairs so presentation code can replace rows rather than append raw logs.
pub(super) fn project_live_activity(event: &ClientEvent) -> Option<DesktopActivityItem> {
    let (id, parent_id, operation_id, owner, state, summary, detail, disclosure, source) =
        match event {
            ClientEvent::Runtime(event) => match event.as_ref() {
                AgentEvent::InvocationIntentCommitted { intent } => {
                    let target = match &intent.target {
                        InvocationTarget::Tool { name, .. } => name.clone(),
                        InvocationTarget::ContextTransform { operation } => operation.clone(),
                    };
                    (
                        format!("tool:{}", intent.invocation_id),
                        None,
                        Some(intent.operation_id.to_string()),
                        DesktopActivityOwner::XanaRoot,
                        DesktopActivityState::Working,
                        "tool.started".to_owned(),
                        Some(target),
                        DesktopActivityDisclosure::Summary,
                        DesktopFactSource::Runtime,
                    )
                }
                AgentEvent::InvocationResultCommitted { result } => {
                    let (state, detail) = match &result.outcome {
                        InvocationOutcome::Completed { .. } => (
                            DesktopActivityState::Completed,
                            Some("Completed".to_owned()),
                        ),
                        InvocationOutcome::Failed { error } => (
                            DesktopActivityState::Failed,
                            Some(bounded_detail(error.clone(), MAX_PRESENTATION_DETAIL_BYTES)),
                        ),
                        InvocationOutcome::Declined { reason } => (
                            DesktopActivityState::Cancelled,
                            Some(bounded_detail(
                                reason.clone(),
                                MAX_PRESENTATION_DETAIL_BYTES,
                            )),
                        ),
                        InvocationOutcome::Interrupted { reason } => (
                            DesktopActivityState::Cancelled,
                            Some(format!("Interrupted: {reason:?}").to_ascii_lowercase()),
                        ),
                    };
                    (
                        format!("tool:{}", result.invocation_id),
                        None,
                        Some(result.operation_id.to_string()),
                        DesktopActivityOwner::XanaRoot,
                        state,
                        "tool.finished".to_owned(),
                        detail,
                        DesktopActivityDisclosure::Summary,
                        DesktopFactSource::Runtime,
                    )
                }
                AgentEvent::ToolFinished {
                    operation_id,
                    invocation_id,
                    ..
                } => (
                    format!("tool:{invocation_id}"),
                    None,
                    Some(operation_id.to_string()),
                    DesktopActivityOwner::XanaRoot,
                    DesktopActivityState::Completed,
                    "tool.output_committed".to_owned(),
                    None,
                    DesktopActivityDisclosure::Summary,
                    DesktopFactSource::Runtime,
                ),
                AgentEvent::PromptPlanUpdated {
                    operation_id,
                    ledger,
                } => (
                    format!("prompt-plan:{operation_id}"),
                    None,
                    Some(operation_id.to_string()),
                    DesktopActivityOwner::XanaRoot,
                    DesktopActivityState::Completed,
                    "prompt.plan_updated".to_owned(),
                    Some(format!(
                        "{} / {} estimated input tokens",
                        ledger.estimated_input_tokens, ledger.budget.input_budget_tokens
                    )),
                    DesktopActivityDisclosure::Detail,
                    DesktopFactSource::Estimated,
                ),
                AgentEvent::CompactionStarted {
                    operation_id,
                    reason,
                } => (
                    format!("compaction:{operation_id}"),
                    None,
                    Some(operation_id.to_string()),
                    DesktopActivityOwner::XanaRoot,
                    DesktopActivityState::Working,
                    "conversation.compaction_started".to_owned(),
                    Some(format!("{reason:?}").to_ascii_lowercase()),
                    DesktopActivityDisclosure::Detail,
                    DesktopFactSource::Runtime,
                ),
                AgentEvent::ConversationCompacted { checkpoint } => (
                    format!("compaction:{}", checkpoint.operation_id),
                    None,
                    Some(checkpoint.operation_id.to_string()),
                    DesktopActivityOwner::XanaRoot,
                    DesktopActivityState::Completed,
                    "conversation.compacted".to_owned(),
                    Some(format!(
                        "Compacted {} canonical entries; raw history retained",
                        checkpoint.source_entry_count
                    )),
                    DesktopActivityDisclosure::Detail,
                    DesktopFactSource::Runtime,
                ),
                AgentEvent::CompactionUnavailable {
                    operation_id,
                    reason,
                } => (
                    format!("compaction:{operation_id}"),
                    None,
                    Some(operation_id.to_string()),
                    DesktopActivityOwner::XanaRoot,
                    DesktopActivityState::Failed,
                    "conversation.compaction_unavailable".to_owned(),
                    Some(bounded_detail(
                        reason.clone(),
                        MAX_PRESENTATION_DETAIL_BYTES,
                    )),
                    DesktopActivityDisclosure::Detail,
                    DesktopFactSource::Runtime,
                ),
                AgentEvent::ChildLifecycleChanged {
                    attribution,
                    lifecycle,
                } => (
                    format!("child:{}", attribution.agent_id),
                    None,
                    Some(attribution.parent_operation_id.to_string()),
                    DesktopActivityOwner::NativeChild {
                        agent_id: attribution.agent_id.to_string(),
                    },
                    project_child_state(*lifecycle),
                    "child.lifecycle".to_owned(),
                    Some(format!(
                        "{} · {} / {} · {lifecycle:?}",
                        attribution.route, attribution.connection, attribution.model
                    )),
                    DesktopActivityDisclosure::Detail,
                    DesktopFactSource::Runtime,
                ),
                AgentEvent::ChildActivity {
                    attribution,
                    activity,
                } => project_child_activity(attribution, activity),
                AgentEvent::ChildReportCommitted { report } => (
                    format!("child:{}", report.attribution.agent_id),
                    None,
                    Some(report.attribution.parent_operation_id.to_string()),
                    DesktopActivityOwner::NativeChild {
                        agent_id: report.attribution.agent_id.to_string(),
                    },
                    project_child_state(report.status.into()),
                    "child.report_committed".to_owned(),
                    report
                        .output
                        .clone()
                        .or_else(|| report.error.clone())
                        .map(|value| bounded_detail(value, MAX_PRESENTATION_DETAIL_BYTES)),
                    DesktopActivityDisclosure::Detail,
                    DesktopFactSource::Runtime,
                ),
                AgentEvent::ChildCancellationRequested { receipt } => (
                    format!("child:{}", receipt.handle.admission.attribution.agent_id),
                    None,
                    Some(
                        receipt
                            .handle
                            .admission
                            .attribution
                            .parent_operation_id
                            .to_string(),
                    ),
                    DesktopActivityOwner::NativeChild {
                        agent_id: receipt.handle.admission.attribution.agent_id.to_string(),
                    },
                    DesktopActivityState::Waiting,
                    "child.cancellation_requested".to_owned(),
                    None,
                    DesktopActivityDisclosure::Summary,
                    DesktopFactSource::Runtime,
                ),
                AgentEvent::ExternalAgentActivity {
                    operation_id,
                    activity,
                } => project_external_activity(*operation_id, activity),
                AgentEvent::ChildListSnapshot { .. }
                | AgentEvent::ChildInspectionSnapshot { .. } => return None,
                _ => return None,
            },
            ClientEvent::Semantic(envelope) => match envelope.decode().ok()? {
                DecodedSemanticEventV1::Known(event) => match event.as_ref() {
                    SemanticEventV1::ActivityUpserted { activity } => {
                        return Some(project_activity(activity));
                    }
                    SemanticEventV1::ProgressTextDelta { activity_id, delta } => (
                        activity_id.to_string(),
                        None,
                        None,
                        DesktopActivityOwner::XanaRoot,
                        DesktopActivityState::Working,
                        "activity.progress".to_owned(),
                        Some(bounded_detail(delta.clone(), MAX_PRESENTATION_DETAIL_BYTES)),
                        DesktopActivityDisclosure::Detail,
                        DesktopFactSource::Runtime,
                    ),
                    _ => return None,
                },
                DecodedSemanticEventV1::Unknown(_) => return None,
            },
            ClientEvent::Managed(_) | ClientEvent::PayloadOmitted { .. } => return None,
        };

    let now = observed_at_unix_millis();
    Some(DesktopActivityItem {
        id,
        parent_id,
        operation_id,
        owner,
        state,
        summary_code: summary,
        summary_parameters: Vec::new(),
        disclosed_text: detail,
        disclosure,
        source,
        freshness: DesktopFactFreshness {
            observed_at_unix_millis: now,
            max_age_millis: None,
        },
        started_at_unix_millis: Some(now),
        finished_at_unix_millis: matches!(
            state,
            DesktopActivityState::Completed
                | DesktopActivityState::Failed
                | DesktopActivityState::Cancelled
        )
        .then_some(now),
    })
}

#[allow(clippy::type_complexity)]
fn project_child_activity(
    attribution: &crate::orchestration::ChildAttribution,
    activity: &ChildActivity,
) -> (
    String,
    Option<String>,
    Option<String>,
    DesktopActivityOwner,
    DesktopActivityState,
    String,
    Option<String>,
    DesktopActivityDisclosure,
    DesktopFactSource,
) {
    let owner = DesktopActivityOwner::NativeChild {
        agent_id: attribution.agent_id.to_string(),
    };
    let parent_id = Some(format!("child:{}", attribution.agent_id));
    let operation_id = Some(attribution.parent_operation_id.to_string());
    match activity {
        ChildActivity::AssistantTextDelta { step_id, text } => (
            format!("child:{}:assistant:{step_id}", attribution.agent_id),
            parent_id,
            operation_id,
            owner,
            DesktopActivityState::Working,
            "child.assistant_progress".to_owned(),
            Some(bounded_detail(text.clone(), MAX_PRESENTATION_DETAIL_BYTES)),
            DesktopActivityDisclosure::Detail,
            DesktopFactSource::Runtime,
        ),
        ChildActivity::ProviderReasoningDelta { step_id, text } => (
            format!("child:{}:reasoning:{step_id}", attribution.agent_id),
            parent_id,
            operation_id,
            owner,
            DesktopActivityState::Working,
            "child.reasoning_summary".to_owned(),
            Some(bounded_detail(text.clone(), MAX_PRESENTATION_DETAIL_BYTES)),
            DesktopActivityDisclosure::Summary,
            DesktopFactSource::Provider,
        ),
        ChildActivity::PermissionRequested { request } => (
            format!(
                "child:{}:permission:{}",
                attribution.agent_id, request.invocation_id
            ),
            parent_id,
            operation_id,
            owner,
            DesktopActivityState::Waiting,
            "child.permission_required".to_owned(),
            Some(request.tool_name.clone()),
            DesktopActivityDisclosure::Summary,
            DesktopFactSource::Runtime,
        ),
        ChildActivity::PermissionAudited { fact } => (
            format!(
                "child:{}:permission:{}",
                attribution.agent_id, fact.request.invocation_id
            ),
            parent_id,
            operation_id,
            owner,
            DesktopActivityState::Completed,
            "child.permission_resolved".to_owned(),
            Some(format!("{:?}", fact.effective).to_ascii_lowercase()),
            DesktopActivityDisclosure::Summary,
            DesktopFactSource::Runtime,
        ),
        ChildActivity::ToolFinished { invocation_id, .. } => (
            format!("child:{}:tool:{invocation_id}", attribution.agent_id),
            parent_id,
            operation_id,
            owner,
            DesktopActivityState::Completed,
            "child.tool_finished".to_owned(),
            None,
            DesktopActivityDisclosure::Summary,
            DesktopFactSource::Runtime,
        ),
        ChildActivity::Warning { message } => (
            format!("child:{}:warning", attribution.agent_id),
            parent_id,
            operation_id,
            owner,
            DesktopActivityState::Failed,
            "child.warning".to_owned(),
            Some(bounded_detail(
                message.clone(),
                MAX_PRESENTATION_DETAIL_BYTES,
            )),
            DesktopActivityDisclosure::Detail,
            DesktopFactSource::Runtime,
        ),
        ChildActivity::ManagedRuntime { notification } => {
            project_managed_child_activity(attribution, notification)
        }
        ChildActivity::ExternalAgent { activity } => {
            let mut projected =
                project_external_activity(attribution.parent_operation_id, activity);
            projected.1 = parent_id;
            projected
        }
        ChildActivity::Suspended => (
            format!("child:{}", attribution.agent_id),
            None,
            operation_id,
            owner,
            DesktopActivityState::Waiting,
            "child.suspended".to_owned(),
            None,
            DesktopActivityDisclosure::Summary,
            DesktopFactSource::Runtime,
        ),
    }
}

#[allow(clippy::type_complexity)]
fn project_managed_child_activity(
    attribution: &crate::orchestration::ChildAttribution,
    notification: &ManagedNotification,
) -> (
    String,
    Option<String>,
    Option<String>,
    DesktopActivityOwner,
    DesktopActivityState,
    String,
    Option<String>,
    DesktopActivityDisclosure,
    DesktopFactSource,
) {
    let child = attribution.agent_id.to_string();
    let owner = DesktopActivityOwner::Managed {
        runtime: attribution.connection.clone(),
    };
    let parent = Some(format!("child:{child}"));
    let operation = Some(attribution.parent_operation_id.to_string());
    let (suffix, state, summary, detail, disclosure) = match notification {
        ManagedNotification::ReasoningSummaryDelta { item_id, delta, .. }
        | ManagedNotification::ReasoningDelta { item_id, delta } => (
            format!("reasoning:{}", item_id.as_deref().unwrap_or("current")),
            DesktopActivityState::Working,
            "managed.reasoning_summary",
            Some(delta.clone()),
            DesktopActivityDisclosure::Summary,
        ),
        ManagedNotification::PlanDelta { item_id, delta } => (
            format!("plan:{}", item_id.as_deref().unwrap_or("current")),
            DesktopActivityState::Working,
            "managed.plan",
            Some(delta.clone()),
            DesktopActivityDisclosure::Detail,
        ),
        ManagedNotification::CommandOutputDelta { item_id, delta } => (
            format!("command:{}", item_id.as_deref().unwrap_or("current")),
            DesktopActivityState::Working,
            "managed.command_output",
            Some(delta.clone()),
            DesktopActivityDisclosure::Detail,
        ),
        ManagedNotification::ItemStarted(item) => (
            format!("item:{}", item.id),
            DesktopActivityState::Working,
            "managed.item_started",
            Some(format!("{}\n{}", item.label, item.details)),
            DesktopActivityDisclosure::Detail,
        ),
        ManagedNotification::ItemCompleted(item) => (
            format!("item:{}", item.id),
            DesktopActivityState::Completed,
            "managed.item_completed",
            Some(format!("{}\n{}", item.label, item.details)),
            DesktopActivityDisclosure::Detail,
        ),
        ManagedNotification::TurnCompleted { status, error, .. } => (
            "turn".to_owned(),
            if error.is_some() {
                DesktopActivityState::Failed
            } else {
                DesktopActivityState::Completed
            },
            "managed.turn_completed",
            Some(error.clone().unwrap_or_else(|| status.clone())),
            DesktopActivityDisclosure::Summary,
        ),
        ManagedNotification::Warning(message) => (
            "warning".to_owned(),
            DesktopActivityState::Failed,
            "managed.warning",
            Some(message.clone()),
            DesktopActivityDisclosure::Detail,
        ),
        ManagedNotification::AssistantDelta { item_id, delta } => (
            format!("assistant:{}", item_id.as_deref().unwrap_or("current")),
            DesktopActivityState::Working,
            "managed.assistant_progress",
            Some(delta.clone()),
            DesktopActivityDisclosure::Detail,
        ),
        other => (
            "state".to_owned(),
            DesktopActivityState::Working,
            "managed.activity",
            Some(format!("{other:?}")),
            DesktopActivityDisclosure::Detail,
        ),
    };
    (
        format!("child:{child}:managed:{suffix}"),
        parent,
        operation,
        owner,
        state,
        summary.to_owned(),
        detail.map(|value| bounded_detail(value, MAX_PRESENTATION_DETAIL_BYTES)),
        disclosure,
        DesktopFactSource::ManagedRuntime,
    )
}

#[allow(clippy::type_complexity)]
fn project_external_activity(
    operation_id: crate::identity::OperationId,
    activity: &crate::a2a::ExternalAgentActivity,
) -> (
    String,
    Option<String>,
    Option<String>,
    DesktopActivityOwner,
    DesktopActivityState,
    String,
    Option<String>,
    DesktopActivityDisclosure,
    DesktopFactSource,
) {
    let (state, summary, detail) = match &activity.activity {
        ExternalAgentActivityKind::Sending {
            classes,
            total_bytes,
        } => (
            DesktopActivityState::Working,
            "a2a.sending",
            Some(format!("{} bytes · {classes:?}", total_bytes)),
        ),
        ExternalAgentActivityKind::TaskIdentified { task_id, .. } => (
            DesktopActivityState::Working,
            "a2a.task_identified",
            Some(task_id.clone()),
        ),
        ExternalAgentActivityKind::Status { state, message } => (
            if matches!(state.as_str(), "completed" | "done") {
                DesktopActivityState::Completed
            } else if matches!(state.as_str(), "failed" | "error") {
                DesktopActivityState::Failed
            } else {
                DesktopActivityState::Working
            },
            "a2a.status",
            message.clone().or_else(|| Some(state.clone())),
        ),
        ExternalAgentActivityKind::Message { text } => (
            DesktopActivityState::Working,
            "a2a.message",
            Some(text.clone()),
        ),
        ExternalAgentActivityKind::Artifact {
            name,
            media_type,
            byte_len,
        } => (
            DesktopActivityState::Completed,
            "a2a.artifact",
            Some(format!("{name} · {media_type} · {byte_len} bytes")),
        ),
        ExternalAgentActivityKind::CancellationRequested { task_id } => (
            DesktopActivityState::Waiting,
            "a2a.cancellation_requested",
            Some(task_id.clone()),
        ),
        ExternalAgentActivityKind::Detached { task_id } => (
            DesktopActivityState::Cancelled,
            "a2a.detached",
            Some(task_id.clone()),
        ),
    };
    (
        format!("a2a:{operation_id}:{}", activity.agent_name),
        None,
        Some(operation_id.to_string()),
        DesktopActivityOwner::A2a {
            agent: activity.agent_name.clone(),
        },
        state,
        summary.to_owned(),
        detail.map(|value| bounded_detail(value, MAX_PRESENTATION_DETAIL_BYTES)),
        DesktopActivityDisclosure::Detail,
        DesktopFactSource::A2a,
    )
}

fn project_child_state(state: ChildLifecycle) -> DesktopActivityState {
    match state {
        ChildLifecycle::Admitted | ChildLifecycle::Queued => DesktopActivityState::Queued,
        ChildLifecycle::Running => DesktopActivityState::Working,
        ChildLifecycle::Suspended => DesktopActivityState::Waiting,
        ChildLifecycle::Completed => DesktopActivityState::Completed,
        ChildLifecycle::Failed => DesktopActivityState::Failed,
        ChildLifecycle::Cancelled | ChildLifecycle::Interrupted => DesktopActivityState::Cancelled,
    }
}

fn observed_at_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn project_parameters(code: &SemanticCodeV1) -> Vec<(String, String)> {
    code.parameters
        .iter()
        .map(|(key, value)| {
            let value = match value {
                SemanticParamV1::Bool(value) => value.to_string(),
                SemanticParamV1::Integer(value) => value.to_string(),
                SemanticParamV1::Unsigned(value) => value.to_string(),
                SemanticParamV1::Text(value) => value.clone(),
            };
            (key.clone(), value)
        })
        .collect()
}

fn project_execution(facts: &ExecutionFactsV1) -> DesktopExecutionFact {
    DesktopExecutionFact {
        operation_id: facts.run_id.to_string(),
        owner: match facts.owner {
            ExecutionOwnerV1::Native => "native",
            ExecutionOwnerV1::Managed => "managed",
            ExecutionOwnerV1::ExternalAgent => "external_agent",
        }
        .to_owned(),
        host_location: match facts.host {
            HostLocationV1::Embedded => "embedded",
            HostLocationV1::Attached => "attached",
            HostLocationV1::Loopback => "loopback",
        }
        .to_owned(),
        workspace_authority: match facts.workspace_authority {
            WorkspaceAuthorityV1::None => "none",
            WorkspaceAuthorityV1::ReadOnly => "read_only",
            WorkspaceAuthorityV1::WorkspaceWrite => "workspace_write",
            WorkspaceAuthorityV1::UncontainedFullAccess => "uncontained_full_access",
        }
        .to_owned(),
        tool_authority: facts.tool_authority.clone(),
        connection: facts.connection.clone(),
        model: facts.model.clone(),
        capability_grants: facts.capability_grants.clone(),
        egress_policy: facts.egress_policy.clone(),
        controller: facts.controller.clone(),
        approval_policy: facts.approval_policy.clone(),
        source: project_source(facts.source),
        freshness: project_freshness(&facts.freshness),
    }
}

fn project_usage(usage: &UsageObservationV1) -> DesktopUsageFact {
    DesktopUsageFact {
        id: usage.id.to_string(),
        scope: project_usage_scope(&usage.scope),
        period: usage.period.clone(),
        accounting: match usage.accounting {
            UsageAccountingV1::Delta => "delta".to_owned(),
            UsageAccountingV1::CumulativeSnapshot { sequence } => {
                format!("cumulative:{sequence}")
            }
        },
        input_tokens: usage.amounts.input_tokens,
        cached_input_tokens: usage.amounts.cached_input_tokens,
        output_tokens: usage.amounts.output_tokens,
        reasoning_tokens: usage.amounts.reasoning_tokens,
        request_count: usage.amounts.request_count,
        context_input_tokens: usage.context.as_ref().map(|value| value.input_tokens),
        context_capacity_tokens: usage
            .context
            .as_ref()
            .and_then(|value| value.capacity_tokens),
        cost_microunits: usage.amounts.cost_microunits,
        availability: project_availability(&usage.availability),
        source: project_source(usage.source),
        authority: project_authority(usage.authority),
        freshness: project_freshness(&usage.freshness),
    }
}

fn project_usage_scope(scope: &UsageScopeV1) -> String {
    match scope {
        UsageScopeV1::Request { run_id } => format!("request:{run_id}"),
        UsageScopeV1::Run { run_id } => format!("run:{run_id}"),
        UsageScopeV1::Conversation { conversation_id } => {
            format!("conversation:{conversation_id}")
        }
        UsageScopeV1::Connection { connection } => format!("connection:{connection}"),
        UsageScopeV1::Model { connection, model } => format!("model:{connection}/{model}"),
        UsageScopeV1::Account {
            connection,
            account,
        } => format!("account:{connection}/{account}"),
        UsageScopeV1::RateLimitBucket { connection, bucket } => {
            format!("rate_limit:{connection}/{bucket}")
        }
    }
}

fn project_completion(receipt: &CompletionReceiptV1) -> DesktopCompletionReceipt {
    DesktopCompletionReceipt {
        id: receipt.id.to_string(),
        operation_id: receipt.run_id.to_string(),
        status: match receipt.status {
            CompletionStatusV1::Completed => "completed",
            CompletionStatusV1::Failed => "failed",
            CompletionStatusV1::Declined => "declined",
            CompletionStatusV1::Interrupted => "interrupted",
        }
        .to_owned(),
        execution: project_execution(&receipt.execution),
        artifact_ids: receipt.artifacts.iter().map(ToString::to_string).collect(),
        checks: receipt
            .checks
            .iter()
            .map(|check| DesktopCompletionCheck {
                code: check.code.clone(),
                passed: check.passed,
            })
            .collect(),
        input_tokens: receipt.usage.amounts.input_tokens,
        output_tokens: receipt.usage.amounts.output_tokens,
        request_count: receipt.usage.amounts.request_count,
        warnings: receipt
            .unresolved_warnings
            .iter()
            .map(|warning| warning.code.clone())
            .collect(),
        source: project_source(receipt.source),
        authority: project_authority(receipt.authority),
        freshness: project_freshness(&receipt.freshness),
    }
}

fn project_prompt_ledger(
    operation_id: crate::identity::OperationId,
    ledger: &PromptPlanLedger,
) -> DesktopPromptLedger {
    DesktopPromptLedger {
        operation_id: Some(operation_id.to_string()),
        estimated_input_tokens: Some(ledger.estimated_input_tokens),
        input_budget_tokens: Some(ledger.budget.input_budget_tokens),
        context_window_tokens: Some(ledger.budget.context_window_tokens),
        context_window_source: Some(
            format!("{:?}", ledger.budget.context_window_source).to_ascii_lowercase(),
        ),
        attachment_count: Some(ledger.attachment_count),
        attachment_bytes: Some(ledger.attachment_bytes),
        omitted_source_count: Some(ledger.omitted_source_ids.len()),
        unavailable_reason: None,
    }
}

fn project_source(source: FactSourceV1) -> DesktopFactSource {
    match source {
        FactSourceV1::Runtime => DesktopFactSource::Runtime,
        FactSourceV1::Surface => DesktopFactSource::Surface,
        FactSourceV1::Connection => DesktopFactSource::Connection,
        FactSourceV1::Model => DesktopFactSource::Model,
        FactSourceV1::Route => DesktopFactSource::Route,
        FactSourceV1::Adapter => DesktopFactSource::Adapter,
        FactSourceV1::Provider => DesktopFactSource::Provider,
        FactSourceV1::ManagedRuntime => DesktopFactSource::ManagedRuntime,
        FactSourceV1::Mcp => DesktopFactSource::Mcp,
        FactSourceV1::A2a => DesktopFactSource::A2a,
        FactSourceV1::Measured => DesktopFactSource::Measured,
        FactSourceV1::Estimated => DesktopFactSource::Estimated,
        FactSourceV1::Cache => DesktopFactSource::Cache,
    }
}

fn project_authority(authority: FactAuthorityV1) -> DesktopFactAuthority {
    match authority {
        FactAuthorityV1::Authoritative => DesktopFactAuthority::Authoritative,
        FactAuthorityV1::ProviderReported => DesktopFactAuthority::ProviderReported,
        FactAuthorityV1::Measured => DesktopFactAuthority::Measured,
        FactAuthorityV1::Estimated => DesktopFactAuthority::Estimated,
    }
}

fn project_freshness(freshness: &FreshnessV1) -> DesktopFactFreshness {
    DesktopFactFreshness {
        observed_at_unix_millis: freshness.observed_at_unix_millis,
        max_age_millis: freshness.max_age_millis,
    }
}

fn project_availability(availability: &AvailabilityV1) -> DesktopAvailability {
    match availability {
        AvailabilityV1::Available => DesktopAvailability::Available,
        AvailabilityV1::Stale => DesktopAvailability::Stale,
        AvailabilityV1::Unsupported => DesktopAvailability::Unsupported,
        AvailabilityV1::Unavailable { code } => {
            DesktopAvailability::Unavailable { code: code.clone() }
        }
        AvailabilityV1::PermissionRequired { code } => {
            DesktopAvailability::PermissionRequired { code: code.clone() }
        }
    }
}

fn bounded_detail(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = max_bytes.saturating_sub(3);
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    text.truncate(boundary);
    text.push_str("...");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frontend::semantic::{ActivityDisclosureV1, ActivityStateV1},
        identity::{ConversationId, OperationId},
    };
    use std::collections::BTreeMap;
    use uuid::Uuid;

    #[test]
    fn activity_projection_retains_owner_provenance_and_disclosure() {
        let item = ActivityItemV1 {
            id: Uuid::new_v4(),
            parent_id: None,
            conversation_id: ConversationId::new(),
            run_id: Some(OperationId::new()),
            owner: ActivityOwnerV1::Managed {
                runtime: "codex".to_owned(),
            },
            state: ActivityStateV1::Working,
            summary: SemanticCodeV1 {
                code: "managed.reasoning_summary".to_owned(),
                parameters: BTreeMap::new(),
            },
            disclosed_text: Some("Reviewing the workspace".to_owned()),
            disclosure: ActivityDisclosureV1::Summary,
            source: FactSourceV1::ManagedRuntime,
            freshness: FreshnessV1 {
                observed_at_unix_millis: 42,
                max_age_millis: None,
            },
            started_at_unix_millis: Some(41),
            finished_at_unix_millis: None,
        };

        let projected = project_activity(&item);
        assert_eq!(
            projected.owner,
            DesktopActivityOwner::Managed {
                runtime: "codex".to_owned()
            }
        );
        assert_eq!(projected.disclosure, DesktopActivityDisclosure::Summary);
        assert_eq!(
            projected.disclosed_text.as_deref(),
            Some("Reviewing the workspace")
        );
        assert_eq!(projected.source, DesktopFactSource::ManagedRuntime);
    }

    #[test]
    fn execution_and_nested_activity_owners_remain_explicit() {
        let conversation_id = ConversationId::new();
        let run_id = OperationId::new();
        for (owner, expected) in [
            (ExecutionOwnerV1::Native, "native"),
            (ExecutionOwnerV1::Managed, "managed"),
            (ExecutionOwnerV1::ExternalAgent, "external_agent"),
        ] {
            let facts = ExecutionFactsV1 {
                conversation_id,
                run_id,
                owner,
                host: HostLocationV1::Embedded,
                workspace_authority: WorkspaceAuthorityV1::ReadOnly,
                tool_authority: vec!["workspace.read".to_owned()],
                connection: Some("fixture".to_owned()),
                model: Some("fixture-model".to_owned()),
                capability_grants: Vec::new(),
                egress_policy: Some("deny".to_owned()),
                controller: Some("desktop".to_owned()),
                approval_policy: "ask".to_owned(),
                source: FactSourceV1::Runtime,
                freshness: FreshnessV1 {
                    observed_at_unix_millis: 42,
                    max_age_millis: None,
                },
            };
            facts.validate().unwrap();
            assert_eq!(project_execution(&facts).owner, expected);
        }

        for (owner, expected) in [
            (
                ActivityOwnerV1::Mcp {
                    server: "docs".to_owned(),
                },
                DesktopActivityOwner::Mcp {
                    server: "docs".to_owned(),
                },
            ),
            (
                ActivityOwnerV1::A2a {
                    agent: "research".to_owned(),
                },
                DesktopActivityOwner::A2a {
                    agent: "research".to_owned(),
                },
            ),
        ] {
            let item = ActivityItemV1 {
                id: Uuid::new_v4(),
                parent_id: None,
                conversation_id,
                run_id: Some(run_id),
                owner,
                state: ActivityStateV1::Working,
                summary: SemanticCodeV1::new("activity.working"),
                disclosed_text: None,
                disclosure: ActivityDisclosureV1::Summary,
                source: FactSourceV1::Runtime,
                freshness: FreshnessV1 {
                    observed_at_unix_millis: 42,
                    max_age_millis: None,
                },
                started_at_unix_millis: Some(42),
                finished_at_unix_millis: None,
            };
            assert_eq!(project_activity(&item).owner, expected);
        }
    }

    #[test]
    fn bounded_detail_preserves_utf8_boundaries() {
        let text = "水".repeat(100_000);
        let projected = bounded_detail(text, 1024);
        assert!(projected.len() <= 1024);
        assert!(projected.ends_with("..."));
    }
}
