//! Pure lifecycle, attention, notification, and startup-recovery contracts.
//!
//! Platform adapters may display these facts, but they do not author them.
//! Payloads deliberately contain stable metadata rather than prompts, model
//! output, reasoning, file names, tool arguments, or credentials.

#[cfg(test)]
mod tests;

use crate::{
    artifact::{ArtifactError, ArtifactRecoveryReport, ArtifactStore},
    identity::OperationId,
    paths::XanaPaths,
    workspace_host::ConversationRef,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};

const MAX_DEDUPLICATION_KEYS: usize = 256;
const MAX_NOTICE_CODE_BYTES: usize = 96;

/// Privacy-safe notification policy shared by configuration and frontends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationPolicy {
    pub enabled: bool,
    pub approvals: bool,
    pub questions: bool,
    pub completions: bool,
    pub failures: bool,
    pub controller_lost: bool,
    pub host_failures: bool,
}

impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            approvals: true,
            questions: true,
            completions: true,
            failures: true,
            controller_lost: true,
            host_failures: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostLifecycleState {
    Running,
    Draining,
    Persisting,
    Closing,
    Stopped,
}

impl HostLifecycleState {
    pub(crate) fn accepts_work(self) -> bool {
        self == Self::Running
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalNoticeKind {
    HostFailure,
    ControllerLost,
    RecoveryAction,
    ResourcePressure,
    StorageFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalNotice {
    pub(crate) kind: GlobalNoticeKind,
    pub(crate) code: String,
    pub(crate) conversation: Option<ConversationRef>,
    pub(crate) operation_id: Option<OperationId>,
}

impl GlobalNotice {
    pub fn new(kind: GlobalNoticeKind, code: impl Into<String>) -> Self {
        Self {
            kind,
            code: bounded_code(code.into()),
            conversation: None,
            operation_id: None,
        }
    }

    pub(crate) fn conversation(mut self, conversation: ConversationRef) -> Self {
        self.conversation = Some(conversation);
        self
    }

    pub(crate) fn operation(mut self, operation_id: OperationId) -> Self {
        self.operation_id = Some(operation_id);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientFocus {
    Focused,
    Unfocused,
    Minimized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    Approval,
    Question,
    Completed,
    Failed,
    ControllerLost,
    HostFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionSignal {
    pub kind: AttentionKind,
    pub(crate) conversation: Option<ConversationRef>,
    pub(crate) operation_id: Option<OperationId>,
}

impl AttentionSignal {
    pub fn new(kind: AttentionKind) -> Self {
        Self {
            kind,
            conversation: None,
            operation_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationDestination {
    Conversation,
    Activity,
    Diagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationCandidate {
    pub title: &'static str,
    pub body: &'static str,
    pub destination: NotificationDestination,
    pub(crate) conversation: Option<ConversationRef>,
    pub(crate) operation_id: Option<OperationId>,
}

/// Focus-aware, bounded deduplication policy shared by graphical adapters.
pub struct NotificationPlanner {
    keys: BTreeSet<String>,
    order: VecDeque<String>,
}

impl NotificationPlanner {
    pub fn new() -> Self {
        Self {
            keys: BTreeSet::new(),
            order: VecDeque::new(),
        }
    }

    pub fn plan(
        &mut self,
        settings: &NotificationPolicy,
        focus: ClientFocus,
        signal: &AttentionSignal,
    ) -> Option<NotificationCandidate> {
        if !settings.enabled || focus == ClientFocus::Focused || !enabled(settings, signal.kind) {
            return None;
        }
        let key = deduplication_key(signal);
        if !self.keys.insert(key.clone()) {
            return None;
        }
        self.order.push_back(key);
        if self.order.len() > MAX_DEDUPLICATION_KEYS
            && let Some(expired) = self.order.pop_front()
        {
            self.keys.remove(&expired);
        }
        Some(candidate(signal))
    }
}

impl Default for NotificationPlanner {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastWindowChoice {
    KeepXanaOpen,
    CancelAndQuit,
    Return,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastWindowEffect {
    KeepOpen,
    RequestShutdown,
    NoChange,
}

pub fn last_window_effect(choice: LastWindowChoice) -> LastWindowEffect {
    match choice {
        LastWindowChoice::KeepXanaOpen => LastWindowEffect::KeepOpen,
        LastWindowChoice::CancelAndQuit => LastWindowEffect::RequestShutdown,
        LastWindowChoice::Return => LastWindowEffect::NoChange,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnedExecutionCleanup {
    Clean,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShutdownPlan {
    pub(crate) active_runs: Vec<ShutdownRun>,
    pub(crate) pending_approval_conversations: Vec<ConversationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShutdownRun {
    pub(crate) conversation: ConversationRef,
    pub(crate) operation_id: OperationId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShutdownProof {
    pub(crate) durable_state_flushed: bool,
    pub(crate) owned_execution: OwnedExecutionCleanup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ShutdownReceipt {
    pub(crate) interrupted_runs: Vec<ShutdownRunReceipt>,
    pub(crate) durable_state_flushed: bool,
    pub(crate) owned_execution: OwnedExecutionCleanup,
    pub(crate) unresolved_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ShutdownRunReceipt {
    pub(crate) conversation: ConversationRef,
    pub(crate) operation_id: OperationId,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StartupRecoveryReport {
    pub(crate) stale_exit_markers: usize,
    pub(crate) artifacts: ArtifactRecoveryReport,
}

/// Reconciles only state whose ownership and partial-file grammar are proven.
/// It does not resume, replay, or declare any interrupted operation complete.
pub(crate) fn recover_startup(
    paths: &XanaPaths,
    stale_exit_markers: usize,
) -> Result<StartupRecoveryReport, ArtifactError> {
    let artifacts = ArtifactStore::open(paths.data_dir())?;
    Ok(StartupRecoveryReport {
        stale_exit_markers,
        artifacts: artifacts.reconcile_partials()?,
    })
}

fn enabled(settings: &NotificationPolicy, kind: AttentionKind) -> bool {
    match kind {
        AttentionKind::Approval => settings.approvals,
        AttentionKind::Question => settings.questions,
        AttentionKind::Completed => settings.completions,
        AttentionKind::Failed => settings.failures,
        AttentionKind::ControllerLost => settings.controller_lost,
        AttentionKind::HostFailure => settings.host_failures,
    }
}

fn candidate(signal: &AttentionSignal) -> NotificationCandidate {
    let (title, body, destination) = match signal.kind {
        AttentionKind::Approval => (
            "Xana needs approval",
            "A Conversation is waiting for your decision.",
            NotificationDestination::Conversation,
        ),
        AttentionKind::Question => (
            "Xana has a question",
            "A Conversation is waiting for your response.",
            NotificationDestination::Conversation,
        ),
        AttentionKind::Completed => (
            "Xana finished",
            "A Conversation completed.",
            NotificationDestination::Conversation,
        ),
        AttentionKind::Failed => (
            "Xana needs attention",
            "A Conversation failed. Review Activity for details.",
            NotificationDestination::Activity,
        ),
        AttentionKind::ControllerLost => (
            "Xana lost its controller",
            "A Conversation needs a controller before work can continue.",
            NotificationDestination::Activity,
        ),
        AttentionKind::HostFailure => (
            "Xana host needs attention",
            "Open Diagnostics to review a local host failure.",
            NotificationDestination::Diagnostics,
        ),
    };
    NotificationCandidate {
        title,
        body,
        destination,
        conversation: signal.conversation.clone(),
        operation_id: signal.operation_id,
    }
}

fn deduplication_key(signal: &AttentionSignal) -> String {
    format!(
        "{:?}:{}:{}",
        signal.kind,
        signal
            .conversation
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        signal
            .operation_id
            .map_or_else(String::new, |id| id.to_string())
    )
}

fn bounded_code(mut code: String) -> String {
    code.retain(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
    if code.is_empty() {
        return "unspecified".to_owned();
    }
    code.truncate(code.len().min(MAX_NOTICE_CODE_BYTES));
    code
}
