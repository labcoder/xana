//! Surface-neutral chat launch and exit values.

use crate::{
    artifact::ArtifactStore,
    config::PermissionMode,
    host_lifecycle::NotificationPolicy,
    identity::{OperationId, PrincipalId, SessionId},
    model_catalog::ModelManager,
    native_runtime::{OperationState, RoundBudgetSuspension},
    orchestration::ChildInspection,
    presentation::ResolvedPresentation,
    resource::ResourcePolicyV1,
    workspace_host::ConversationRef,
};
use std::path::PathBuf;

pub(crate) struct ChatHeader {
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) profile_name: String,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) endpoint: String,
    pub(crate) context_report: String,
    pub(crate) session_id: SessionId,
    pub(crate) session_path: PathBuf,
    pub(crate) resumed: bool,
    pub(crate) repair_truncate_to: Option<u64>,
    pub(crate) unfinished: Vec<(OperationId, OperationState)>,
    pub(crate) round_budget_suspension: Option<RoundBudgetSuspension>,
    pub(crate) children: Vec<ChildInspection>,
    pub(crate) workspace_root: PathBuf,
    pub(crate) artifact_store: ArtifactStore,
    pub(crate) owner: PrincipalId,
    pub(crate) models: ModelManager,
    pub(crate) presentation: ResolvedPresentation,
    pub(crate) resource_policy: ResourcePolicyV1,
    pub(crate) notification_policy: NotificationPolicy,
    pub(crate) vision: super::vision::VisionTurnService,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatExit {
    Quit,
    Restart,
    NewConversation,
    SwitchConversation(ConversationRef),
    DesktopNewConversation {
        workspace: PathBuf,
    },
    DesktopSwitchConversation {
        workspace: PathBuf,
        conversation: ConversationRef,
    },
    Doctor(Option<SessionId>),
    Reset,
    Setup(String),
    Settings(String),
    ControlCommand {
        family: String,
        arguments: String,
    },
}
