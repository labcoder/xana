use super::{ExecutionOwner, ResolvedAgentConfig};
use crate::{
    artifact::ArtifactRef,
    config::{OrchestrationLimits, PermissionMode},
    identity::{AgentId, OperationId, StepId, ThreadId, ToolInvocationId},
    message::Message,
    permission::{PermissionAuditFact, PermissionRequest},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(crate) const CHILD_REPORT_VERSION: u32 = 1;
pub(crate) const MAX_CHILD_TASK_BYTES: usize = 256 * 1024;
pub(crate) const MAX_CHILD_TASK_PREVIEW_BYTES: usize = 512;
pub(crate) const MAX_CHILD_CONTEXT_PREVIEWS: usize = 8;
pub(crate) const MAX_CHILD_CONTEXT_PREVIEW_BYTES: usize = 16 * 1024;
pub(crate) const MAX_CHILD_CONTEXT_HANDOFF_BYTES: usize = 64 * 1024;
pub(crate) const MAX_CHILD_ARTIFACT_REFS: usize = 16;
pub(crate) const COLLECTION_RESULT_VERSION: u32 = 1;
pub(crate) const MAX_COLLECTION_HANDLES: usize = 64;
pub(crate) const MAX_COLLECTION_RESULT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_COLLECTION_PREVIEW_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpawnAgentRequest {
    #[serde(default)]
    pub(crate) route: Option<String>,
    pub(crate) task: String,
    #[serde(default)]
    pub(crate) result_schema: ChildResultSchema,
    #[serde(default)]
    pub(crate) restrictions: ChildRestrictions,
    #[serde(default)]
    pub(crate) handoff: ChildContextHandoff,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildContextHandoff {
    #[serde(default)]
    pub(crate) previews: Vec<ChildContextPreview>,
    #[serde(default)]
    pub(crate) artifacts: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildContextPreview {
    pub(crate) label: String,
    pub(crate) content: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildResultSchema {
    #[default]
    Summary,
    Json,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildRestrictions {
    #[serde(default)]
    pub(crate) permission_mode: Option<PermissionMode>,
    #[serde(default)]
    pub(crate) max_tool_rounds: Option<usize>,
    #[serde(default)]
    pub(crate) deadline_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) max_context_tokens: Option<usize>,
    #[serde(default)]
    pub(crate) max_report_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) max_artifact_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) hard_token_limit: Option<u64>,
    #[serde(default)]
    pub(crate) hard_spend_microusd: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ChildActivity {
    AssistantTextDelta {
        step_id: StepId,
        text: String,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    PermissionAudited {
        fact: PermissionAuditFact,
    },
    ToolFinished {
        invocation_id: ToolInvocationId,
        result: Message,
    },
    Warning {
        message: String,
    },
    Suspended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildAttribution {
    pub(crate) agent_id: AgentId,
    pub(crate) parent_agent_id: AgentId,
    pub(crate) operation_id: OperationId,
    pub(crate) parent_operation_id: OperationId,
    pub(crate) thread_id: ThreadId,
    pub(crate) route: String,
    pub(crate) profile: String,
    pub(crate) owner: ExecutionOwner,
    pub(crate) connection: String,
    pub(crate) model: String,
}

impl ChildAttribution {
    pub(crate) fn new(
        agent_id: AgentId,
        parent_agent_id: AgentId,
        parent_operation_id: OperationId,
        thread_id: ThreadId,
        resolved: &ResolvedAgentConfig,
    ) -> Self {
        Self {
            agent_id,
            parent_agent_id,
            operation_id: OperationId::new(),
            parent_operation_id,
            thread_id,
            route: resolved.route.clone(),
            profile: resolved.profile.clone(),
            owner: resolved.owner,
            connection: resolved.connection.clone(),
            model: resolved.model.id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildLifecycle {
    Admitted,
    Queued,
    Running,
    Suspended,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl ChildLifecycle {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildTerminalStatus {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl From<ChildTerminalStatus> for ChildLifecycle {
    fn from(value: ChildTerminalStatus) -> Self {
        match value {
            ChildTerminalStatus::Completed => Self::Completed,
            ChildTerminalStatus::Failed => Self::Failed,
            ChildTerminalStatus::Cancelled => Self::Cancelled,
            ChildTerminalStatus::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum ChildUsage {
    #[default]
    Unknown,
    Measured {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        requests: u64,
        #[serde(default)]
        spend_microusd: Option<u64>,
    },
    Estimated {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        requests: Option<u64>,
        spend_microusd: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ChildReportReference {
    Inline {
        byte_len: usize,
    },
    Artifact {
        artifact: ArtifactRef,
        byte_len: u64,
        preview_byte_len: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildReport {
    pub(crate) version: u32,
    pub(crate) attribution: ChildAttribution,
    pub(crate) status: ChildTerminalStatus,
    #[serde(default)]
    pub(crate) schema: ChildResultSchema,
    pub(crate) output: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) usage: ChildUsage,
    pub(crate) reference: ChildReportReference,
}

impl ChildReport {
    #[cfg(test)]
    pub(crate) fn completed(
        attribution: ChildAttribution,
        output: String,
        usage: ChildUsage,
    ) -> Self {
        let byte_len = output.len();
        Self {
            version: CHILD_REPORT_VERSION,
            attribution,
            status: ChildTerminalStatus::Completed,
            schema: ChildResultSchema::Summary,
            output: Some(output),
            error: None,
            usage,
            reference: ChildReportReference::Inline { byte_len },
        }
    }

    pub(crate) fn completed_with_reference(
        attribution: ChildAttribution,
        schema: ChildResultSchema,
        output: String,
        usage: ChildUsage,
        reference: ChildReportReference,
    ) -> Self {
        Self {
            version: CHILD_REPORT_VERSION,
            attribution,
            status: ChildTerminalStatus::Completed,
            schema,
            output: Some(output),
            error: None,
            usage,
            reference,
        }
    }

    pub(crate) fn failed_with_schema(
        attribution: ChildAttribution,
        schema: ChildResultSchema,
        reason: String,
        max_bytes: usize,
    ) -> Self {
        Self::terminal_error(
            attribution,
            ChildTerminalStatus::Failed,
            schema,
            reason,
            max_bytes,
        )
    }

    pub(crate) fn cancelled_with_schema(
        attribution: ChildAttribution,
        schema: ChildResultSchema,
        reason: String,
        max_bytes: usize,
    ) -> Self {
        Self::terminal_error(
            attribution,
            ChildTerminalStatus::Cancelled,
            schema,
            reason,
            max_bytes,
        )
    }

    pub(crate) fn interrupted_with_schema(
        attribution: ChildAttribution,
        schema: ChildResultSchema,
        reason: String,
        max_bytes: usize,
    ) -> Self {
        Self::terminal_error(
            attribution,
            ChildTerminalStatus::Interrupted,
            schema,
            reason,
            max_bytes,
        )
    }

    fn terminal_error(
        attribution: ChildAttribution,
        status: ChildTerminalStatus,
        schema: ChildResultSchema,
        reason: String,
        max_bytes: usize,
    ) -> Self {
        let error = truncate_utf8(&reason, max_bytes);
        let byte_len = error.len();
        Self {
            version: CHILD_REPORT_VERSION,
            attribution,
            status,
            schema,
            output: None,
            error: Some(error),
            usage: ChildUsage::Unknown,
            reference: ChildReportReference::Inline { byte_len },
        }
    }

    pub(crate) fn lifecycle(&self) -> ChildLifecycle {
        self.status.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildInspection {
    pub(crate) handle: AgentHandleSnapshot,
    pub(crate) report: Option<ChildReport>,
    /// True only for a read-only restart projection. Producing this view never
    /// appends a terminal record.
    pub(crate) projected_interruption: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AwaitAgentOptions {
    pub(crate) timeout: Option<Duration>,
    pub(crate) cancel_on_timeout: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum AwaitAgentOutcome {
    Report(Box<ChildReport>),
    TimedOut {
        agent_id: AgentId,
        cancellation_requested: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildCancellationReceipt {
    pub(crate) handle: AgentHandleSnapshot,
    pub(crate) newly_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildAdmission {
    pub(crate) attribution: ChildAttribution,
    #[serde(default)]
    pub(crate) plan: Option<PlanChildAttribution>,
    pub(crate) task_preview: String,
    pub(crate) task_hash: String,
    #[serde(default)]
    pub(crate) result_schema: ChildResultSchema,
    pub(crate) capabilities: Vec<String>,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) max_tool_rounds: usize,
    pub(crate) limits: OrchestrationLimits,
    #[serde(default)]
    pub(crate) hard_token_limit: Option<u64>,
    #[serde(default)]
    pub(crate) hard_spend_microusd: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanChildAttribution {
    pub(crate) plan_id: crate::identity::OrchestrationPlanId,
    pub(crate) step_id: String,
    pub(crate) output_index: usize,
}

impl ChildAdmission {
    pub(crate) fn new(
        attribution: ChildAttribution,
        task: &str,
        result_schema: ChildResultSchema,
        resolved: &ResolvedAgentConfig,
    ) -> Self {
        Self {
            attribution,
            plan: None,
            task_preview: truncate_utf8(task, MAX_CHILD_TASK_PREVIEW_BYTES),
            task_hash: blake3::hash(task.as_bytes()).to_hex().to_string(),
            result_schema,
            capabilities: resolved
                .capabilities
                .capabilities()
                .iter()
                .map(ToString::to_string)
                .collect(),
            permission_mode: resolved.permission_mode,
            max_tool_rounds: resolved.max_tool_rounds,
            limits: resolved.orchestration.clone(),
            hard_token_limit: resolved.hard_token_limit,
            hard_spend_microusd: resolved.hard_spend_microusd,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CollectionFailurePolicy {
    #[default]
    ContinueOnError,
    FailFast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CollectAgentsOptions {
    pub(crate) timeout: Option<Duration>,
    pub(crate) failure_policy: CollectionFailurePolicy,
    pub(crate) cancel_remaining: bool,
    pub(crate) cancel_on_timeout: bool,
}

impl Default for CollectAgentsOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            failure_policy: CollectionFailurePolicy::ContinueOnError,
            cancel_remaining: false,
            cancel_on_timeout: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CollectedValue {
    Summary {
        text: String,
    },
    Json {
        value: serde_json::Value,
    },
    Preview {
        schema: ChildResultSchema,
        text: String,
        truncated: bool,
    },
    ArtifactPreview {
        schema: ChildResultSchema,
        preview: String,
        artifact: ArtifactRef,
        byte_len: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CollectionEntryState {
    Terminal,
    TimedOut,
    SkippedAfterFailure,
    ArtifactUnavailable,
    CollectionError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectedChildResult {
    pub(crate) attribution: ChildAttribution,
    pub(crate) state: CollectionEntryState,
    pub(crate) status: Option<ChildTerminalStatus>,
    pub(crate) usage: ChildUsage,
    pub(crate) reference: Option<ChildReportReference>,
    pub(crate) value: Option<CollectedValue>,
    pub(crate) error: Option<String>,
    pub(crate) cancellation_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectionResult {
    pub(crate) version: u32,
    pub(crate) failure_policy: CollectionFailurePolicy,
    pub(crate) entries: Vec<CollectedChildResult>,
    pub(crate) complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentHandleSnapshot {
    pub(crate) admission: ChildAdmission,
    pub(crate) lifecycle: ChildLifecycle,
    pub(crate) usage: ChildUsage,
    pub(crate) report: Option<ChildReportReference>,
}

impl AgentHandleSnapshot {
    pub(crate) fn admitted(admission: ChildAdmission) -> Self {
        Self {
            admission,
            lifecycle: ChildLifecycle::Admitted,
            usage: ChildUsage::Unknown,
            report: None,
        }
    }

    pub(crate) fn apply_lifecycle(&mut self, lifecycle: ChildLifecycle) {
        self.lifecycle = lifecycle;
    }

    pub(crate) fn apply_report(&mut self, report: &ChildReport) {
        self.lifecycle = report.lifecycle();
        self.usage = report.usage.clone();
        self.report = Some(report.reference.clone());
    }
}

pub(crate) fn validate_spawn_request(request: &SpawnAgentRequest) -> Result<(), &'static str> {
    if request.task.trim().is_empty() {
        return Err("child task must not be blank");
    }
    if request.task.len() > MAX_CHILD_TASK_BYTES {
        return Err("child task exceeds the maximum encoded size");
    }
    if request
        .route
        .as_ref()
        .is_some_and(|route| route.trim().is_empty())
    {
        return Err("child route must not be blank when supplied");
    }
    if request.handoff.previews.len() > MAX_CHILD_CONTEXT_PREVIEWS {
        return Err("child context handoff has too many previews");
    }
    if request.handoff.artifacts.len() > MAX_CHILD_ARTIFACT_REFS {
        return Err("child context handoff has too many artifact references");
    }
    let mut context_bytes = 0_usize;
    let mut labels = std::collections::HashSet::new();
    for preview in &request.handoff.previews {
        if preview.label.trim().is_empty() || preview.label.len() > 128 {
            return Err("child context preview label must be 1..=128 bytes");
        }
        if preview.content.trim().is_empty() {
            return Err("child context preview must not be blank");
        }
        if preview.content.len() > MAX_CHILD_CONTEXT_PREVIEW_BYTES {
            return Err("child context preview exceeds the maximum encoded size");
        }
        context_bytes = context_bytes
            .checked_add(preview.content.len())
            .ok_or("child context handoff exceeds the aggregate encoded size")?;
        if context_bytes > MAX_CHILD_CONTEXT_HANDOFF_BYTES {
            return Err("child context handoff exceeds the aggregate encoded size");
        }
        if !labels.insert(preview.label.trim()) {
            return Err("child context preview labels must be unique");
        }
    }
    let mut artifacts = std::collections::HashSet::new();
    for artifact in &request.handoff.artifacts {
        if !artifacts.insert(artifact.clone()) {
            return Err("child artifact references must be unique");
        }
    }
    let restrictions = &request.restrictions;
    if restrictions.max_tool_rounds == Some(0)
        || restrictions.deadline_seconds == Some(0)
        || restrictions.max_context_tokens == Some(0)
        || restrictions.max_report_bytes == Some(0)
        || restrictions.max_artifact_bytes == Some(0)
        || restrictions.hard_token_limit == Some(0)
        || restrictions.hard_spend_microusd == Some(0)
    {
        return Err("child restriction bounds must be greater than zero");
    }
    Ok(())
}

pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_validation_and_preview_bounds_are_utf8_safe() {
        assert_eq!(
            validate_spawn_request(&SpawnAgentRequest {
                route: None,
                task: "  ".to_owned(),
                result_schema: ChildResultSchema::Summary,
                restrictions: ChildRestrictions::default(),
                handoff: ChildContextHandoff::default(),
            }),
            Err("child task must not be blank")
        );
        let text = "é".repeat(300);
        let preview = truncate_utf8(&text, MAX_CHILD_TASK_PREVIEW_BYTES);
        assert!(preview.len() <= MAX_CHILD_TASK_PREVIEW_BYTES);
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn context_handoff_rejects_duplicates_and_oversized_selected_content() {
        let request = |previews| SpawnAgentRequest {
            route: None,
            task: "inspect".to_owned(),
            result_schema: ChildResultSchema::Summary,
            restrictions: ChildRestrictions::default(),
            handoff: ChildContextHandoff {
                previews,
                artifacts: Vec::new(),
            },
        };
        assert_eq!(
            validate_spawn_request(&request(vec![
                ChildContextPreview {
                    label: "same".to_owned(),
                    content: "one".to_owned(),
                },
                ChildContextPreview {
                    label: "same".to_owned(),
                    content: "two".to_owned(),
                },
            ])),
            Err("child context preview labels must be unique")
        );
        assert_eq!(
            validate_spawn_request(&request(vec![ChildContextPreview {
                label: "large".to_owned(),
                content: "x".repeat(MAX_CHILD_CONTEXT_PREVIEW_BYTES + 1),
            }])),
            Err("child context preview exceeds the maximum encoded size")
        );
    }

    #[test]
    fn terminal_report_updates_handle_without_losing_attribution() {
        let session = crate::identity::SessionId::new();
        let attribution = ChildAttribution {
            agent_id: AgentId::new(),
            parent_agent_id: AgentId::for_session(session),
            operation_id: OperationId::new(),
            parent_operation_id: OperationId::new(),
            thread_id: ThreadId::new(),
            route: "worker".to_owned(),
            profile: "worker".to_owned(),
            owner: ExecutionOwner::Native,
            connection: "local".to_owned(),
            model: "small".to_owned(),
        };
        let admission = ChildAdmission {
            attribution: attribution.clone(),
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
        };
        let mut handle = AgentHandleSnapshot::admitted(admission);
        let report =
            ChildReport::completed(attribution.clone(), "done".to_owned(), ChildUsage::Unknown);

        handle.apply_report(&report);

        assert_eq!(handle.admission.attribution, attribution);
        assert_eq!(handle.lifecycle, ChildLifecycle::Completed);
        assert_eq!(
            handle.report,
            Some(ChildReportReference::Inline { byte_len: 4 })
        );
    }

    #[test]
    fn usage_distinguishes_measured_estimated_and_unknown_values() {
        let measured = ChildUsage::Measured {
            input_tokens: Some(10),
            output_tokens: Some(5),
            total_tokens: Some(15),
            requests: 1,
            spend_microusd: Some(250),
        };
        let estimated = ChildUsage::Estimated {
            input_tokens: Some(8),
            output_tokens: None,
            total_tokens: None,
            requests: Some(1),
            spend_microusd: None,
        };
        for usage in [measured, estimated, ChildUsage::Unknown] {
            let encoded = serde_json::to_vec(&usage).expect("encode usage");
            let decoded: ChildUsage = serde_json::from_slice(&encoded).expect("decode usage");
            assert_eq!(decoded, usage);
        }

        let prior_measured: ChildUsage = serde_json::from_value(serde_json::json!({
            "state":"measured",
            "input_tokens":10,
            "output_tokens":5,
            "total_tokens":15,
            "requests":1
        }))
        .expect("decode pre-spend measured usage");
        assert!(matches!(
            prior_measured,
            ChildUsage::Measured {
                spend_microusd: None,
                ..
            }
        ));
    }

    #[test]
    fn unknown_result_schemas_are_rejected_during_request_decoding() {
        let request = serde_json::json!({
            "task": "bounded task",
            "result_schema": "xml"
        });
        assert!(serde_json::from_value::<SpawnAgentRequest>(request).is_err());
    }
}
