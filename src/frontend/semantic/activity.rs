use super::{
    FactAuthorityV1, FactSourceV1, FreshnessV1, MAX_SAFE_TEXT_BYTES, SemanticCodeV1, SemanticError,
    UsageAggregateV1, validate_code, validate_text,
};
use crate::identity::{AgentId, ArtifactId, ConversationId, OperationId, ToolInvocationId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const MAX_ACTIVITY_ITEMS: usize = 256;
const MAX_ACTIVITY_DEPTH: usize = 16;
const MAX_RECEIPT_ARTIFACTS: usize = 128;
const MAX_RECEIPT_CHECKS: usize = 128;
const MAX_RECEIPT_WARNINGS: usize = 64;
const MAX_CAPABILITY_CODES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ActivityOwnerV1 {
    XanaRoot,
    NativeChild { agent_id: AgentId },
    Managed { runtime: String },
    Mcp { server: String },
    A2a { agent: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityStateV1 {
    Queued,
    Working,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityDisclosureV1 {
    Summary,
    Detail,
    Hidden,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityItemV1 {
    pub(crate) id: Uuid,
    pub(crate) parent_id: Option<Uuid>,
    pub(crate) conversation_id: ConversationId,
    pub(crate) run_id: Option<OperationId>,
    pub(crate) owner: ActivityOwnerV1,
    pub(crate) state: ActivityStateV1,
    pub(crate) summary: SemanticCodeV1,
    /// Provider-visible reasoning summary or ordinary activity detail. Hidden
    /// chain-of-thought is never represented by this protocol.
    pub(crate) disclosed_text: Option<String>,
    pub(crate) disclosure: ActivityDisclosureV1,
    pub(crate) source: FactSourceV1,
    pub(crate) freshness: FreshnessV1,
    pub(crate) started_at_unix_millis: Option<u64>,
    pub(crate) finished_at_unix_millis: Option<u64>,
}

impl ActivityItemV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        self.summary.validate()?;
        if let Some(text) = self.disclosed_text.as_deref() {
            validate_text("activity disclosure", text, MAX_SAFE_TEXT_BYTES)?;
        }
        match &self.owner {
            ActivityOwnerV1::Managed { runtime } => validate_code("managed runtime", runtime, 96)?,
            ActivityOwnerV1::Mcp { server } => validate_code("MCP server", server, 96)?,
            ActivityOwnerV1::A2a { agent } => validate_code("A2A agent", agent, 96)?,
            ActivityOwnerV1::XanaRoot | ActivityOwnerV1::NativeChild { .. } => {}
        }
        if self.finished_at_unix_millis.is_some_and(|finished| {
            self.started_at_unix_millis
                .is_some_and(|started| finished < started)
        }) {
            return Err(SemanticError::InvalidStructure {
                field: "activity time",
                reason: "finish must not precede start",
            });
        }
        Ok(())
    }
}

pub(super) fn validate_activity_tree(items: &[ActivityItemV1]) -> Result<(), SemanticError> {
    if items.len() > MAX_ACTIVITY_ITEMS {
        return Err(SemanticError::TooManyValues {
            field: "activity items",
            actual: items.len(),
            limit: MAX_ACTIVITY_ITEMS,
        });
    }
    let by_id = items
        .iter()
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    if by_id.len() != items.len() {
        return Err(SemanticError::InvalidStructure {
            field: "activity items",
            reason: "must have unique IDs",
        });
    }
    for item in items {
        item.validate()?;
        let mut cursor = item.parent_id;
        let mut visited = BTreeSet::new();
        for _ in 0..MAX_ACTIVITY_DEPTH {
            let Some(id) = cursor else { break };
            if !visited.insert(id) {
                return Err(SemanticError::InvalidStructure {
                    field: "activity tree",
                    reason: "must not contain a cycle",
                });
            }
            let parent = by_id.get(&id).ok_or(SemanticError::InvalidStructure {
                field: "activity parent",
                reason: "must refer to an item in the same snapshot",
            })?;
            if parent.conversation_id != item.conversation_id {
                return Err(SemanticError::InvalidStructure {
                    field: "activity parent",
                    reason: "must belong to the same Conversation",
                });
            }
            cursor = parent.parent_id;
        }
        if cursor.is_some() {
            return Err(SemanticError::InvalidStructure {
                field: "activity tree",
                reason: "exceeds the maximum nesting depth",
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttentionKindV1 {
    Working,
    NeedsYou,
    Blocked,
    Failed,
    Completed,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttentionItemV1 {
    pub(crate) id: Uuid,
    pub(crate) conversation_id: ConversationId,
    pub(crate) run_id: Option<OperationId>,
    pub(crate) kind: AttentionKindV1,
    pub(crate) message: SemanticCodeV1,
    pub(crate) created_at_unix_millis: u64,
    pub(crate) acknowledged_at_unix_millis: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct AttentionStateV1(BTreeMap<Uuid, AttentionItemV1>);

impl AttentionStateV1 {
    pub(crate) fn upsert(&mut self, item: AttentionItemV1) -> Result<(), SemanticError> {
        item.message.validate()?;
        self.0.insert(item.id, item);
        Ok(())
    }

    pub(crate) fn acknowledge(&mut self, id: Uuid, at_unix_millis: u64) -> bool {
        let Some(item) = self.0.get_mut(&id) else {
            return false;
        };
        item.acknowledged_at_unix_millis = Some(at_unix_millis);
        true
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &AttentionItemV1> {
        self.0.values()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApprovalStateV1 {
    Pending,
    ApprovedOnce,
    ApprovedForSession,
    Denied,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalV1 {
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) conversation_id: ConversationId,
    pub(crate) run_id: OperationId,
    pub(crate) capability: String,
    pub(crate) request: SemanticCodeV1,
    pub(crate) state: ApprovalStateV1,
}

impl ApprovalV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        validate_code("approval capability", &self.capability, 128)?;
        self.request.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionOwnerV1 {
    Native,
    Managed,
    ExternalAgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostLocationV1 {
    Embedded,
    Attached,
    Loopback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceAuthorityV1 {
    None,
    ReadOnly,
    WorkspaceWrite,
    UncontainedFullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionFactsV1 {
    pub(crate) conversation_id: ConversationId,
    pub(crate) run_id: OperationId,
    pub(crate) owner: ExecutionOwnerV1,
    pub(crate) host: HostLocationV1,
    pub(crate) workspace_authority: WorkspaceAuthorityV1,
    pub(crate) tool_authority: Vec<String>,
    pub(crate) connection: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) capability_grants: Vec<String>,
    pub(crate) egress_policy: Option<String>,
    pub(crate) controller: Option<String>,
    pub(crate) approval_policy: String,
    pub(crate) source: FactSourceV1,
    pub(crate) freshness: FreshnessV1,
}

impl ExecutionFactsV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        if self.tool_authority.len() > MAX_CAPABILITY_CODES
            || self.capability_grants.len() > MAX_CAPABILITY_CODES
        {
            return Err(SemanticError::TooManyValues {
                field: "execution capabilities",
                actual: self.tool_authority.len().max(self.capability_grants.len()),
                limit: MAX_CAPABILITY_CODES,
            });
        }
        for code in self.tool_authority.iter().chain(&self.capability_grants) {
            validate_code("execution capability", code, 128)?;
        }
        for (field, value) in [
            ("connection", self.connection.as_deref()),
            ("model", self.model.as_deref()),
            ("egress policy", self.egress_policy.as_deref()),
            ("controller", self.controller.as_deref()),
            ("approval policy", Some(self.approval_policy.as_str())),
        ] {
            if let Some(value) = value {
                validate_text(field, value, 256)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompletionStatusV1 {
    Completed,
    Failed,
    Declined,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckReceiptV1 {
    pub(crate) code: String,
    pub(crate) passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompletionReceiptV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence: Option<Box<crate::completion_evidence::CompletionEvidence>>,
    pub(crate) id: Uuid,
    pub(crate) conversation_id: ConversationId,
    pub(crate) run_id: OperationId,
    pub(crate) status: CompletionStatusV1,
    pub(crate) execution: ExecutionFactsV1,
    pub(crate) artifacts: Vec<ArtifactId>,
    pub(crate) checks: Vec<CheckReceiptV1>,
    pub(crate) usage: UsageAggregateV1,
    pub(crate) unresolved_warnings: Vec<SemanticCodeV1>,
    pub(crate) source: FactSourceV1,
    pub(crate) authority: FactAuthorityV1,
    pub(crate) freshness: FreshnessV1,
}

impl CompletionReceiptV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        if let Some(evidence) = &self.evidence
            && (evidence.generation != self.run_id || evidence.validate_outcome().is_err())
        {
            return Err(SemanticError::InvalidStructure {
                field: "completion evidence",
                reason: "must be bounded and belong to this exact run",
            });
        }
        if self.execution.conversation_id != self.conversation_id
            || self.execution.run_id != self.run_id
        {
            return Err(SemanticError::InvalidStructure {
                field: "completion receipt identity",
                reason: "must match its execution facts",
            });
        }
        self.execution.validate()?;
        for (field, actual, limit) in [
            (
                "receipt artifacts",
                self.artifacts.len(),
                MAX_RECEIPT_ARTIFACTS,
            ),
            ("receipt checks", self.checks.len(), MAX_RECEIPT_CHECKS),
            (
                "receipt warnings",
                self.unresolved_warnings.len(),
                MAX_RECEIPT_WARNINGS,
            ),
        ] {
            if actual > limit {
                return Err(SemanticError::TooManyValues {
                    field,
                    actual,
                    limit,
                });
            }
        }
        for check in &self.checks {
            validate_code("check code", &check.code, 128)?;
        }
        for warning in &self.unresolved_warnings {
            warning.validate()?;
        }
        Ok(())
    }
}
