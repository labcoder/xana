use super::{ExecutionOwner, ResolvedAgentConfig};
use crate::{
    config::{OrchestrationLimits, PermissionMode},
    identity::{AgentId, OperationId, StepId, ThreadId, ToolInvocationId},
    message::Message,
    permission::{PermissionAuditFact, PermissionRequest},
};
use serde::{Deserialize, Serialize};

pub(crate) const CHILD_REPORT_VERSION: u32 = 1;
pub(crate) const MAX_CHILD_TASK_BYTES: usize = 256 * 1024;
pub(crate) const MAX_CHILD_TASK_PREVIEW_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpawnAgentRequest {
    #[serde(default)]
    pub(crate) route: Option<String>,
    pub(crate) task: String,
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
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ChildReportReference {
    Inline { byte_len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildReport {
    pub(crate) version: u32,
    pub(crate) attribution: ChildAttribution,
    pub(crate) status: ChildTerminalStatus,
    pub(crate) output: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) usage: ChildUsage,
    pub(crate) reference: ChildReportReference,
}

impl ChildReport {
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
            output: Some(output),
            error: None,
            usage,
            reference: ChildReportReference::Inline { byte_len },
        }
    }

    pub(crate) fn failed(attribution: ChildAttribution, reason: String, max_bytes: usize) -> Self {
        let error = truncate_utf8(&reason, max_bytes);
        let byte_len = error.len();
        Self {
            version: CHILD_REPORT_VERSION,
            attribution,
            status: ChildTerminalStatus::Failed,
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
pub(crate) struct ChildAdmission {
    pub(crate) attribution: ChildAttribution,
    pub(crate) task_preview: String,
    pub(crate) task_hash: String,
    pub(crate) capabilities: Vec<String>,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) max_tool_rounds: usize,
    pub(crate) limits: OrchestrationLimits,
}

impl ChildAdmission {
    pub(crate) fn new(
        attribution: ChildAttribution,
        task: &str,
        resolved: &ResolvedAgentConfig,
    ) -> Self {
        Self {
            attribution,
            task_preview: truncate_utf8(task, MAX_CHILD_TASK_PREVIEW_BYTES),
            task_hash: blake3::hash(task.as_bytes()).to_hex().to_string(),
            capabilities: resolved
                .capabilities
                .capabilities()
                .iter()
                .map(ToString::to_string)
                .collect(),
            permission_mode: resolved.permission_mode,
            max_tool_rounds: resolved.max_tool_rounds,
            limits: resolved.orchestration.clone(),
        }
    }
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
            }),
            Err("child task must not be blank")
        );
        let text = "é".repeat(300);
        let preview = truncate_utf8(&text, MAX_CHILD_TASK_PREVIEW_BYTES);
        assert!(preview.len() <= MAX_CHILD_TASK_PREVIEW_BYTES);
        assert!(preview.is_char_boundary(preview.len()));
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
            task_preview: "task".to_owned(),
            task_hash: blake3::hash(b"task").to_hex().to_string(),
            capabilities: Vec::new(),
            permission_mode: PermissionMode::Deny,
            max_tool_rounds: 1,
            limits: OrchestrationLimits::default(),
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
}
