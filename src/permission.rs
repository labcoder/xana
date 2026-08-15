//! Provider-neutral permission policy, scope, broker, and audit model.

mod broker;
mod policy;
mod scope;

pub(crate) use broker::{Authorization, PermissionBroker, PermissionBrokerHandle};
pub(crate) use policy::{Evaluation, PermissionPolicy, PolicyError};
pub(crate) use scope::scope_contains;

use crate::{
    identity::{OperationId, ToolInvocationId},
    outbound::OutboundApprovalRequest,
    tool::EffectClass,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PolicyDecision {
    Deny,
    Ask,
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PermissionScope {
    WorkspacePath {
        canonical_path: PathBuf,
    },
    ExternalPath {
        canonical_path: PathBuf,
    },
    Command {
        shell: String,
        canonical_cwd: PathBuf,
        command: String,
    },
    External {
        recipient_identity_digest: String,
        operation: String,
    },
    Unscoped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PermissionRequest {
    pub(crate) operation_id: OperationId,
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) tool_name: String,
    pub(crate) effect_class: EffectClass,
    pub(crate) final_arguments: Value,
    pub(crate) scope: PermissionScope,
    #[serde(default)]
    pub(crate) outbound_review: Option<OutboundApprovalRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ControllerDecision {
    Deny,
    AllowOnce,
    AllowSession { scope: PermissionScope },
    SaveOutboundAllow,
    SaveOutboundDeny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PermissionAuditFact {
    pub(crate) request: PermissionRequest,
    pub(crate) policy_evaluation: PolicyDecision,
    pub(crate) controller_decision: Option<ControllerDecision>,
    pub(crate) effective: PolicyDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PermissionRule {
    pub(crate) id: String,
    pub(crate) decision: PolicyDecision,
    #[serde(default)]
    pub(crate) tool: Option<String>,
    #[serde(default)]
    pub(crate) effect: Option<EffectClass>,
    #[serde(default)]
    pub(crate) workspace: Option<PathBuf>,
    #[serde(default)]
    pub(crate) command: Option<String>,
}

#[cfg(test)]
mod tests;
