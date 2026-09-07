//! Versioned, bounded, repository-private frontend protocol values.
//!
//! Frontends send correlated commands and render bounded observations. This
//! module contains no terminal, network, provider-wire, or presentation
//! concerns. The embedded adapter is the reference transport; later transport
//! projections must preserve these semantics rather than invent another API.

use super::managed::ManagedClientEvent;
use super::semantic::{
    AttachmentPolicySnapshotV1, CompletionReceiptV1, CompletionStatusV1, ExecutionFactsV1,
    ExecutionOwnerV1, FactAuthorityV1, FactSourceV1, FreshnessV1, HostLocationV1, SemanticCodeV1,
    SemanticDeltaV1, SemanticEventEnvelopeV1, SemanticReplicaV1, SemanticSnapshotV1,
    UsageAggregateV1, UsageLedgerV1, UsageScopeV1, WorkspaceAuthorityV1, normalize_message,
};
use crate::{
    identity::{AgentId, ConversationId, OperationId, RoundBudgetId, SessionId, ToolInvocationId},
    message::Message,
    native_runtime::{AgentEvent, RoundBudgetAction, RuntimeCommand},
    orchestration::{ChildInspection, ChildLifecycle},
    permission::ControllerDecision,
    prompt::PromptPlanLedger,
    resource::ResourcePolicyV1,
    vision::ImageRef,
};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use uuid::Uuid;

pub(crate) const FRONTEND_PROTOCOL_VERSION: u16 = 14;
const MAX_SNAPSHOT_MESSAGES: usize = 512;
const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_OMISSION_LABEL_BYTES: usize = 160;
const MAX_PROJECTED_CONTENT_PARTS: usize = 256;
const MAX_PROJECTED_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_PROJECTED_FINAL_BYTES: usize = 512 * 1024;
const MAX_PROJECTED_USAGE_OBSERVATIONS: usize = 4_096;
const MAX_PROJECTED_PROMPT_PLANS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ClientCommandId(Uuid);

impl ClientCommandId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClientCommand {
    pub(crate) version: u16,
    pub(crate) id: ClientCommandId,
    pub(crate) semantic_id: String,
    pub(crate) value: ClientCommandValue,
}

impl ClientCommand {
    pub(crate) fn new(value: impl Into<ClientCommandValue>) -> Self {
        let value = value.into();
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            id: ClientCommandId::new(),
            semantic_id: value.semantic_id().to_owned(),
            value,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ClientCommandValue {
    SubmitDerivedTurn {
        operation_id: OperationId,
        input: String,
        owner_input: String,
    },
    SubmitCorrelatedTurn {
        binding: crate::operation::adapter::DesktopCommandKey,
        input: String,
        images: Vec<ImageRef>,
    },
    SubmitFiniteTurn {
        operation_id: OperationId,
        input: String,
        kind: crate::completion_evidence::WorkKind,
        contract: crate::completion_evidence::CompletionContract,
    },
    BrowserControl {
        action: crate::browser::BrowserControl,
    },
    SubmitTurn {
        operation_id: OperationId,
        input: String,
        images: Vec<ImageRef>,
    },
    ClearConversation,
    CompactConversation {
        operation_id: OperationId,
    },
    ResumeOperation {
        session_id: SessionId,
        operation_id: OperationId,
    },
    DecideRoundBudget {
        operation_id: OperationId,
        suspension_id: RoundBudgetId,
        action: RoundBudgetAction,
    },
    InterruptOperation {
        operation_id: OperationId,
    },
    SteerOperation {
        operation_id: OperationId,
        input: String,
    },
    DecidePermission {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    DecideChildPermission {
        agent_id: AgentId,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    ListChildren,
    InspectChild {
        agent_id: AgentId,
    },
    CancelChild {
        agent_id: AgentId,
    },
    Shutdown,
}

impl From<RuntimeCommand> for ClientCommandValue {
    fn from(command: RuntimeCommand) -> Self {
        match command {
            RuntimeCommand::SubmitDerivedTurn {
                operation_id,
                input,
                owner_input,
            } => Self::SubmitDerivedTurn {
                operation_id,
                input,
                owner_input,
            },
            RuntimeCommand::SubmitCorrelatedTurn {
                binding,
                input,
                images,
            } => Self::SubmitCorrelatedTurn {
                binding,
                input,
                images,
            },
            RuntimeCommand::SubmitFiniteTurn {
                operation_id,
                input,
                kind,
                contract,
            } => Self::SubmitFiniteTurn {
                operation_id,
                input,
                kind,
                contract,
            },
            RuntimeCommand::BrowserControl { action } => Self::BrowserControl { action },
            RuntimeCommand::SubmitTurn {
                operation_id,
                input,
            } => Self::SubmitTurn {
                operation_id,
                input,
                images: Vec::new(),
            },
            RuntimeCommand::SubmitTurnWithImages {
                operation_id,
                input,
                images,
            } => Self::SubmitTurn {
                operation_id,
                input,
                images,
            },
            RuntimeCommand::ClearConversation => Self::ClearConversation,
            RuntimeCommand::CompactConversation { operation_id } => {
                Self::CompactConversation { operation_id }
            }
            RuntimeCommand::ResumeOperation {
                session_id,
                operation_id,
            } => Self::ResumeOperation {
                session_id,
                operation_id,
            },
            RuntimeCommand::DecideRoundBudget {
                operation_id,
                suspension_id,
                action,
            } => Self::DecideRoundBudget {
                operation_id,
                suspension_id,
                action,
            },
            RuntimeCommand::InterruptOperation { operation_id } => {
                Self::InterruptOperation { operation_id }
            }
            RuntimeCommand::SteerOperation {
                operation_id,
                input,
            } => Self::SteerOperation {
                operation_id,
                input,
            },
            RuntimeCommand::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            } => Self::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            },
            RuntimeCommand::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            } => Self::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            },
            RuntimeCommand::ListChildren => Self::ListChildren,
            RuntimeCommand::InspectChild { agent_id } => Self::InspectChild { agent_id },
            RuntimeCommand::CancelChild { agent_id } => Self::CancelChild { agent_id },
            RuntimeCommand::Shutdown => Self::Shutdown,
        }
    }
}

impl From<ClientCommandValue> for RuntimeCommand {
    fn from(command: ClientCommandValue) -> Self {
        match command {
            ClientCommandValue::SubmitDerivedTurn {
                operation_id,
                input,
                owner_input,
            } => Self::SubmitDerivedTurn {
                operation_id,
                input,
                owner_input,
            },
            ClientCommandValue::SubmitCorrelatedTurn {
                binding,
                input,
                images,
            } => Self::SubmitCorrelatedTurn {
                binding,
                input,
                images,
            },
            ClientCommandValue::SubmitFiniteTurn {
                operation_id,
                input,
                kind,
                contract,
            } => Self::SubmitFiniteTurn {
                operation_id,
                input,
                kind,
                contract,
            },
            ClientCommandValue::BrowserControl { action } => Self::BrowserControl { action },
            ClientCommandValue::SubmitTurn {
                operation_id,
                input,
                images,
            } if images.is_empty() => Self::SubmitTurn {
                operation_id,
                input,
            },
            ClientCommandValue::SubmitTurn {
                operation_id,
                input,
                images,
            } => Self::SubmitTurnWithImages {
                operation_id,
                input,
                images,
            },
            ClientCommandValue::ClearConversation => Self::ClearConversation,
            ClientCommandValue::CompactConversation { operation_id } => {
                Self::CompactConversation { operation_id }
            }
            ClientCommandValue::ResumeOperation {
                session_id,
                operation_id,
            } => Self::ResumeOperation {
                session_id,
                operation_id,
            },
            ClientCommandValue::DecideRoundBudget {
                operation_id,
                suspension_id,
                action,
            } => Self::DecideRoundBudget {
                operation_id,
                suspension_id,
                action,
            },
            ClientCommandValue::InterruptOperation { operation_id } => {
                Self::InterruptOperation { operation_id }
            }
            ClientCommandValue::SteerOperation {
                operation_id,
                input,
            } => Self::SteerOperation {
                operation_id,
                input,
            },
            ClientCommandValue::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            } => Self::DecidePermission {
                operation_id,
                invocation_id,
                decision,
            },
            ClientCommandValue::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            } => Self::DecideChildPermission {
                agent_id,
                operation_id,
                invocation_id,
                decision,
            },
            ClientCommandValue::ListChildren => Self::ListChildren,
            ClientCommandValue::InspectChild { agent_id } => Self::InspectChild { agent_id },
            ClientCommandValue::CancelChild { agent_id } => Self::CancelChild { agent_id },
            ClientCommandValue::Shutdown => Self::Shutdown,
        }
    }
}

impl ClientCommandValue {
    pub(crate) fn semantic_id(&self) -> &'static str {
        match self {
            Self::BrowserControl { .. } => "browser.control.v1",
            Self::SubmitTurn { .. } => "turn.submit.v1",
            Self::SubmitDerivedTurn { .. } => "turn.submit.v1",
            Self::SubmitFiniteTurn { .. } => "turn.submit.v1",
            Self::SubmitCorrelatedTurn { .. } => "turn.submit.v1",
            Self::ClearConversation => "conversation.clear.v1",
            Self::CompactConversation { .. } => "conversation.compact.v1",
            Self::ResumeOperation { .. } => "run.resume.v1",
            Self::DecideRoundBudget {
                action: RoundBudgetAction::Continue,
                ..
            } => "run.continue.v1",
            Self::DecideRoundBudget {
                action: RoundBudgetAction::Stop,
                ..
            } => "run.stop.v1",
            Self::InterruptOperation { .. } => "run.interrupt.v1",
            Self::SteerOperation { .. } => "run.steer.v1",
            Self::DecidePermission { .. } | Self::DecideChildPermission { .. } => {
                "approval.decide.v1"
            }
            Self::ListChildren => "child.list.v1",
            Self::InspectChild { .. } => "child.inspect.v1",
            Self::CancelChild { .. } => "child.cancel.v1",
            Self::Shutdown => "application.shutdown.v1",
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("could not encode frontend command: {error}"))?
            .len();
        if bytes > MAX_COMMAND_BYTES {
            return Err(format!(
                "frontend command is {bytes} bytes; limit is {MAX_COMMAND_BYTES}"
            ));
        }
        if let Self::SubmitTurn { images, .. } = self
            && images.len() > 8
        {
            return Err("a frontend turn may contain at most 8 images".to_owned());
        }
        if let Self::SubmitFiniteTurn { contract, .. } = self {
            contract.validate().map_err(|error| error.to_string())?;
        }
        if let Self::SubmitDerivedTurn {
            input, owner_input, ..
        } = self
            && (input.trim().is_empty() || owner_input.trim().is_empty())
        {
            return Err("derived turns require the original authored input".into());
        }
        if let Self::SubmitCorrelatedTurn {
            binding, images, ..
        } = self
        {
            binding.validate().map_err(|error| error.to_string())?;
            if images.len() > 8 {
                return Err("a frontend turn may contain at most 8 images".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClientCommandResult {
    pub(crate) version: u16,
    pub(crate) command_id: ClientCommandId,
    pub(crate) accepted: bool,
    pub(crate) outcome: SemanticCodeV1,
    pub(crate) reason: Option<String>,
}

impl ClientCommandResult {
    pub(crate) fn accepted(command_id: ClientCommandId) -> Self {
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            command_id,
            accepted: true,
            outcome: SemanticCodeV1::new("command.accepted"),
            reason: None,
        }
    }

    pub(crate) fn rejected(command_id: ClientCommandId, reason: impl Into<String>) -> Self {
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            command_id,
            accepted: false,
            outcome: SemanticCodeV1::new("command.rejected"),
            reason: Some(bounded_text(reason.into(), MAX_OMISSION_LABEL_BYTES)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChildSnapshot {
    pub(crate) agent_id: AgentId,
    pub(crate) route: String,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) lifecycle: ChildLifecycle,
}

impl From<&ChildInspection> for ChildSnapshot {
    fn from(child: &ChildInspection) -> Self {
        Self {
            agent_id: child.handle.admission.attribution.agent_id,
            route: bounded_text(
                child.handle.admission.attribution.route.clone(),
                MAX_OMISSION_LABEL_BYTES,
            ),
            connection: bounded_text(
                child.handle.admission.attribution.connection.clone(),
                MAX_OMISSION_LABEL_BYTES,
            ),
            model: bounded_text(
                child.handle.admission.attribution.model.clone(),
                MAX_OMISSION_LABEL_BYTES,
            ),
            lifecycle: child.handle.lifecycle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClientSnapshot {
    #[serde(default)]
    pub(crate) terminal_diagnostics: Vec<crate::failure::TerminalDiagnostic>,
    pub(crate) version: u16,
    /// Last observation included in the snapshot. The initial embedded
    /// snapshot is captured before forwarding starts, so this is zero.
    pub(crate) sequence: u64,
    pub(crate) session_id: SessionId,
    pub(crate) connection: String,
    pub(crate) execution_owner: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) host_location: HostLocationV1,
    pub(crate) approval_policy: String,
    pub(crate) conversation: Vec<Message>,
    pub(crate) conversation_truncated: bool,
    /// Absolute active-path position; a bounded snapshot never masquerades as
    /// the whole durable transcript.
    #[serde(default)]
    pub(crate) conversation_start: usize,
    #[serde(default)]
    pub(crate) conversation_total: usize,
    pub(crate) active_operation: Option<OperationId>,
    pub(crate) children: Vec<ChildSnapshot>,
    pub(crate) pending_approval_count: usize,
    #[serde(default)]
    pub(crate) pending_approvals: Vec<PendingPermissionProjection>,
    pub(crate) activity_count: usize,
    pub(crate) artifact_count: usize,
    /// Frontend-neutral M4 semantics. Legacy provider-neutral messages remain
    /// available during the bounded migration to semantic projections.
    #[serde(default)]
    pub(crate) semantic: SemanticSnapshotV1,
    /// Bounded native prompt plans retained until the semantic protocol grows
    /// a dedicated context-budget event family.
    #[serde(default)]
    pub(crate) prompt_plans: Vec<(OperationId, PromptPlanLedger)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingPermissionProjection {
    pub(crate) operation_id: OperationId,
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) tool_name: String,
    pub(crate) effect_class: crate::tool::EffectClass,
    pub(crate) scope: crate::permission::PermissionScope,
    /// Exact bounded memory proposal needed for review after controller attach.
    /// Generic arguments and the original owner quotation remain excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) memory_proposal: Option<MemoryPermissionProposal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryPermissionAction {
    Remember,
    Correct,
    Forget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryPermissionProposal {
    pub(crate) action: MemoryPermissionAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) statement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<u64>,
}

impl MemoryPermissionProposal {
    pub(crate) fn from_request(request: &crate::permission::PermissionRequest) -> Option<Self> {
        if request.tool_name != "memory_update"
            || !matches!(
                request.scope,
                crate::permission::PermissionScope::PersonalMemory { .. }
            )
        {
            return None;
        }
        let args = &request.final_arguments;
        let action = match args.get("action")?.as_str()? {
            "remember" => MemoryPermissionAction::Remember,
            "correct" => MemoryPermissionAction::Correct,
            "forget" => MemoryPermissionAction::Forget,
            _ => return None,
        };
        let statement = if action == MemoryPermissionAction::Forget {
            None
        } else {
            let statement = args.get("statement")?.as_str()?;
            if statement.trim().is_empty()
                || statement.len() > 4096
                || statement
                    .chars()
                    .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
            {
                return None;
            }
            Some(statement.to_owned())
        };
        let (id, revision) = if action == MemoryPermissionAction::Remember {
            (None, None)
        } else {
            let id = args.get("id")?.as_str()?.parse::<Uuid>().ok()?;
            let revision = args.get("revision")?.as_u64()?;
            if id.is_nil() || revision == 0 || revision >= i64::MAX as u64 {
                return None;
            }
            (Some(id), Some(revision))
        };
        Some(Self {
            action,
            statement,
            id,
            revision,
        })
    }

    pub(crate) fn review_text(&self) -> String {
        let action = match self.action {
            MemoryPermissionAction::Remember => "Remember",
            MemoryPermissionAction::Correct => "Correct",
            MemoryPermissionAction::Forget => "Forget",
        };
        let mut text = action.to_owned();
        if let Some(id) = self.id {
            text.push_str(&format!(
                " memory {id} at revision {}",
                self.revision.unwrap_or(0)
            ));
        }
        if let Some(statement) = &self.statement {
            text.push_str(&format!("\nExact statement: {statement}"));
        }
        text
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ClientSnapshotSeed {
    pub(crate) session_id: SessionId,
    pub(crate) connection: String,
    pub(crate) execution_owner: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) host_location: HostLocationV1,
    pub(crate) approval_policy: String,
    pub(crate) children: Vec<ChildInspection>,
    pub(crate) resource_policy: ResourcePolicyV1,
}

impl ClientSnapshot {
    fn append_conversation_message(&mut self, message: &Message) {
        self.conversation_total = self.conversation_total.saturating_add(1);
        let mut conversation = std::mem::take(&mut self.conversation);
        conversation.push(message.clone());
        let (conversation, truncated) = bounded_history(conversation);
        self.conversation = conversation;
        self.conversation_start = self
            .conversation_total
            .saturating_sub(self.conversation.len());
        self.conversation_truncated |= truncated || self.conversation_start > 0;
        self.artifact_count = self
            .conversation
            .iter()
            .flat_map(Message::artifacts)
            .count();
    }
    pub(crate) fn initial_page(
        seed: ClientSnapshotSeed,
        page: crate::session::ConversationPage,
    ) -> Self {
        let supplied = page.messages.len();
        let mut snapshot = Self::initial(seed, page.messages);
        snapshot.conversation_start = page
            .start
            .saturating_add(supplied.saturating_sub(snapshot.conversation.len()));
        snapshot.conversation_total = page.total;
        snapshot.conversation_truncated |= page.has_older || snapshot.conversation_start > 0;
        snapshot
    }

    pub(crate) fn initial(seed: ClientSnapshotSeed, history: Vec<Message>) -> Self {
        let conversation_total = history.len();
        let (conversation, conversation_truncated) = bounded_history(history);
        let conversation_start = conversation_total.saturating_sub(conversation.len());
        let artifact_count = conversation
            .iter()
            .flat_map(crate::message::Message::artifacts)
            .count();
        let conversation_id = ConversationId::for_native(seed.session_id);
        Self {
            version: FRONTEND_PROTOCOL_VERSION,
            terminal_diagnostics: Vec::new(),
            sequence: 0,
            session_id: seed.session_id,
            connection: bounded_text(seed.connection, MAX_OMISSION_LABEL_BYTES),
            execution_owner: bounded_text(seed.execution_owner, MAX_OMISSION_LABEL_BYTES),
            model: bounded_text(seed.model, MAX_OMISSION_LABEL_BYTES),
            reasoning_effort: seed
                .reasoning_effort
                .map(|value| bounded_text(value, MAX_OMISSION_LABEL_BYTES)),
            host_location: seed.host_location,
            approval_policy: bounded_text(seed.approval_policy, MAX_OMISSION_LABEL_BYTES),
            conversation,
            conversation_truncated,
            conversation_start,
            conversation_total,
            active_operation: None,
            children: seed
                .children
                .iter()
                .take(64)
                .map(ChildSnapshot::from)
                .collect(),
            pending_approval_count: 0,
            pending_approvals: Vec::new(),
            activity_count: 0,
            artifact_count,
            semantic: SemanticSnapshotV1 {
                conversation_id: Some(conversation_id),
                attachment_policy: AttachmentPolicySnapshotV1 {
                    configured: seed.resource_policy,
                    ..AttachmentPolicySnapshotV1::default()
                },
                ..SemanticSnapshotV1::default()
            },
            prompt_plans: Vec::new(),
        }
    }

    pub(crate) fn apply(&mut self, event: &ClientEvent, sequence: u64) {
        self.sequence = sequence;
        if !matches!(event, ClientEvent::Semantic(_)) {
            self.semantic.sequence = sequence;
        }
        match event {
            ClientEvent::Runtime(event) => match event.as_ref() {
                AgentEvent::TerminalDiagnostic { diagnostic } => {
                    self.retain_terminal_diagnostic(diagnostic.clone());
                }
                AgentEvent::CompletionEvidenceRecorded {
                    operation_id,
                    evidence,
                } => {
                    if evidence.generation == *operation_id
                        && evidence.validate_outcome().is_ok()
                        && self
                            .semantic
                            .completion_receipts
                            .iter()
                            .find(|receipt| receipt.run_id == *operation_id)
                            .and_then(|receipt| receipt.evidence.as_ref())
                            .is_none_or(|previous| {
                                previous.revision < evidence.revision
                                    && previous.contract_digest == evidence.contract_digest
                                    && previous.kind == evidence.kind
                                    && previous.owner == evidence.owner
                            })
                    {
                        self.upsert_completion(
                            *operation_id,
                            if evidence.supported() {
                                crate::native_runtime::OperationOutcome::Completed
                            } else {
                                crate::native_runtime::OperationOutcome::Failed
                            },
                        );
                        if let Some(receipt) = self
                            .semantic
                            .completion_receipts
                            .iter_mut()
                            .find(|receipt| receipt.run_id == *operation_id)
                            && receipt
                                .evidence
                                .as_ref()
                                .is_none_or(|previous| previous.revision < evidence.revision)
                        {
                            receipt.evidence = Some(Box::new(evidence.clone()));
                        }
                        // Detail is durable in the Conversation journal. Do not
                        // let a long run list exhaust reconnect frame capacity.
                        while self.semantic.completion_receipts.len() > 1
                            && serde_json::to_vec(&self.semantic.completion_receipts)
                                .map_or(true, |bytes| bytes.len() > 256 * 1024)
                        {
                            self.semantic.completion_receipts.remove(0);
                        }
                    }
                }
                AgentEvent::UserMessageCommitted { message, .. } => {
                    append_semantic_content(&mut self.semantic, normalize_message(message));
                    self.append_conversation_message(message);
                }
                AgentEvent::OperationStateChanged {
                    operation_id,
                    state: crate::native_runtime::OperationState::Running,
                } => {
                    self.active_operation = Some(*operation_id);
                    self.upsert_execution_facts(*operation_id);
                }
                AgentEvent::OperationStateChanged {
                    operation_id,
                    state: crate::native_runtime::OperationState::Suspended,
                } => self.active_operation = Some(*operation_id),
                AgentEvent::OperationStateChanged {
                    operation_id,
                    state: crate::native_runtime::OperationState::Finished(outcome),
                } => {
                    self.upsert_completion(*operation_id, *outcome);
                    self.active_operation = None;
                }
                AgentEvent::OperationFailed { .. } => self.active_operation = None,
                AgentEvent::AssistantMessage {
                    operation_id,
                    message,
                } => {
                    let parts = normalize_message(message);
                    append_semantic_content(&mut self.semantic, parts.clone());
                    if !parts.is_empty() {
                        self.semantic
                            .authoritative_finals
                            .insert(*operation_id, parts);
                        bound_authoritative_finals(&mut self.semantic, *operation_id);
                    }
                    self.append_conversation_message(message);
                }
                AgentEvent::ConversationCleared => {
                    self.conversation.clear();
                    self.conversation_start = 0;
                    self.conversation_total = 0;
                    self.conversation_truncated = false;
                    self.active_operation = None;
                    self.artifact_count = 0;
                    self.pending_approval_count = 0;
                    self.pending_approvals.clear();
                    self.semantic.content.clear();
                    self.semantic.authoritative_finals.clear();
                    self.semantic.usage.clear();
                    self.semantic.execution_facts.clear();
                    self.semantic.completion_receipts.clear();
                    self.prompt_plans.clear();
                }
                AgentEvent::ToolFinished { result, .. } => {
                    append_semantic_content(&mut self.semantic, normalize_message(result));
                    self.append_conversation_message(result);
                }
                AgentEvent::PermissionRequested { request } => {
                    let projection = PendingPermissionProjection {
                        operation_id: request.operation_id,
                        invocation_id: request.invocation_id,
                        tool_name: bounded_text(
                            request.tool_name.clone(),
                            MAX_OMISSION_LABEL_BYTES,
                        ),
                        effect_class: request.effect_class,
                        scope: request.scope.clone(),
                        memory_proposal: MemoryPermissionProposal::from_request(request),
                    };
                    if let Some(existing) = self.pending_approvals.iter_mut().find(|candidate| {
                        candidate.operation_id == projection.operation_id
                            && candidate.invocation_id == projection.invocation_id
                    }) {
                        *existing = projection;
                    } else if self.pending_approvals.len() < 32 {
                        self.pending_approvals.push(projection);
                    }
                    self.pending_approval_count = self.pending_approvals.len();
                }
                AgentEvent::PermissionAudited { fact } => {
                    self.pending_approvals.retain(|candidate| {
                        candidate.operation_id != fact.request.operation_id
                            || candidate.invocation_id != fact.request.invocation_id
                    });
                    self.pending_approval_count = self.pending_approvals.len();
                }
                AgentEvent::ChildListSnapshot { children } => {
                    self.children = children.iter().take(64).map(ChildSnapshot::from).collect();
                }
                AgentEvent::UsageObserved {
                    operation_id,
                    usage,
                } => {
                    let context_capacity = self
                        .prompt_plans
                        .iter()
                        .rev()
                        .find(|(candidate, _)| candidate == operation_id)
                        .map(|(_, ledger)| ledger)
                        .map(|ledger| ledger.budget.context_window_tokens as u64);
                    let observation = crate::usage_observation::native_usage_observation(
                        *operation_id,
                        &self.session_id.to_string(),
                        usage,
                        context_capacity,
                        observed_at_unix_millis(),
                    );
                    append_semantic_usage(&mut self.semantic, observation);
                    self.activity_count = self.activity_count.saturating_add(1);
                }
                AgentEvent::PromptPlanUpdated {
                    operation_id,
                    ledger,
                } => {
                    if let Some(existing) = self
                        .prompt_plans
                        .iter_mut()
                        .find(|(candidate, _)| candidate == operation_id)
                    {
                        existing.1 = ledger.clone();
                    } else {
                        self.prompt_plans.push((*operation_id, ledger.clone()));
                    }
                    while self.prompt_plans.len() > MAX_PROJECTED_PROMPT_PLANS {
                        self.prompt_plans.remove(0);
                    }
                    self.activity_count = self.activity_count.saturating_add(1);
                }
                _ => self.activity_count = self.activity_count.saturating_add(1),
            },
            ClientEvent::Semantic(event) => {
                let original = self.semantic.clone();
                if let Ok(mut replica) = SemanticReplicaV1::from_snapshot(original.clone())
                    && replica
                        .apply(SemanticDeltaV1 {
                            sequence,
                            event: (**event).clone(),
                        })
                        .is_ok()
                {
                    self.semantic = replica.snapshot().clone();
                } else {
                    self.semantic = original;
                }
                self.activity_count = self.activity_count.saturating_add(1);
            }
            ClientEvent::PayloadOmitted { kind, .. } => {
                let role = match kind.as_str() {
                    "committed user message" => Some(crate::message::Role::User),
                    "assistant message" => Some(crate::message::Role::Assistant),
                    "tool result" => Some(crate::message::Role::Tool),
                    _ => None,
                };
                if let Some(role) = role {
                    self.append_conversation_message(&Message::text(
                        role,
                        "[Message omitted from this live view: payload exceeds the observation limit; inspect the saved Conversation.]",
                    ));
                    self.conversation_truncated = true;
                }
                self.activity_count = self.activity_count.saturating_add(1);
            }
            ClientEvent::Managed(event) => {
                if let ManagedClientEvent::TerminalDiagnostic(diagnostic) = event.as_ref() {
                    self.retain_terminal_diagnostic(diagnostic.clone());
                }
                self.activity_count = self.activity_count.saturating_add(1);
            }
        }
    }

    fn retain_terminal_diagnostic(&mut self, diagnostic: crate::failure::TerminalDiagnostic) {
        if self.terminal_diagnostics.len() >= 64 {
            self.terminal_diagnostics.remove(0);
        }
        self.terminal_diagnostics.push(diagnostic);
    }

    fn upsert_execution_facts(&mut self, run_id: OperationId) -> ExecutionFactsV1 {
        if let Some(facts) = self
            .semantic
            .execution_facts
            .iter()
            .find(|facts| facts.run_id == run_id)
        {
            return facts.clone();
        }
        let facts = ExecutionFactsV1 {
            conversation_id: ConversationId::for_native(self.session_id),
            run_id,
            owner: match self.execution_owner.as_str() {
                "managed" => ExecutionOwnerV1::Managed,
                "external_agent" => ExecutionOwnerV1::ExternalAgent,
                _ => ExecutionOwnerV1::Native,
            },
            host: self.host_location,
            // The native process is policy-gated but does not claim OS sandboxing.
            workspace_authority: WorkspaceAuthorityV1::UncontainedFullAccess,
            tool_authority: crate::tool::BUILTIN_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            connection: Some(self.connection.clone()),
            model: Some(self.model.clone()),
            capability_grants: Vec::new(),
            egress_policy: Some("outbound policy enforced".to_owned()),
            controller: Some("foreground controller".to_owned()),
            approval_policy: self.approval_policy.clone(),
            source: FactSourceV1::Runtime,
            freshness: FreshnessV1 {
                observed_at_unix_millis: observed_at_unix_millis(),
                max_age_millis: None,
            },
        };
        self.semantic.execution_facts.push(facts.clone());
        while self.semantic.execution_facts.len() > 128 {
            self.semantic.execution_facts.remove(0);
        }
        facts
    }

    fn upsert_completion(
        &mut self,
        run_id: OperationId,
        outcome: crate::native_runtime::OperationOutcome,
    ) {
        let execution = self.upsert_execution_facts(run_id);
        let scope = UsageScopeV1::Run { run_id };
        let period = self.session_id.to_string();
        let mut ledger = UsageLedgerV1::default();
        for observation in &self.semantic.usage {
            let _ = ledger.observe(observation.clone());
        }
        let usage = ledger
            .aggregate(&scope, &period)
            .unwrap_or_else(|_| UsageAggregateV1 {
                incomplete: true,
                ..UsageAggregateV1::default()
            });
        let status = match outcome {
            crate::native_runtime::OperationOutcome::Completed => CompletionStatusV1::Completed,
            crate::native_runtime::OperationOutcome::Failed => CompletionStatusV1::Failed,
            crate::native_runtime::OperationOutcome::Declined => CompletionStatusV1::Declined,
            crate::native_runtime::OperationOutcome::Interrupted => CompletionStatusV1::Interrupted,
        };
        let warning = match status {
            CompletionStatusV1::Completed => None,
            CompletionStatusV1::Failed => Some("run.failed"),
            CompletionStatusV1::Declined => Some("run.declined"),
            CompletionStatusV1::Interrupted => Some("run.interrupted"),
        };
        let id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("xana://completion/{}/{run_id}", execution.conversation_id).as_bytes(),
        );
        let receipt = CompletionReceiptV1 {
            evidence: self
                .semantic
                .completion_receipts
                .iter()
                .find(|receipt| receipt.run_id == run_id)
                .and_then(|receipt| receipt.evidence.clone()),
            id,
            conversation_id: execution.conversation_id,
            run_id,
            status,
            execution,
            artifacts: Vec::new(),
            checks: Vec::new(),
            usage,
            unresolved_warnings: warning.into_iter().map(SemanticCodeV1::new).collect(),
            source: FactSourceV1::Runtime,
            authority: FactAuthorityV1::Authoritative,
            freshness: FreshnessV1 {
                observed_at_unix_millis: observed_at_unix_millis(),
                max_age_millis: None,
            },
        };
        if let Some(existing) = self
            .semantic
            .completion_receipts
            .iter_mut()
            .find(|candidate| candidate.id == receipt.id)
        {
            *existing = receipt;
        } else {
            self.semantic.completion_receipts.push(receipt);
            while self.semantic.completion_receipts.len() > 128 {
                self.semantic.completion_receipts.remove(0);
            }
        }
    }
}

fn append_semantic_usage(
    semantic: &mut SemanticSnapshotV1,
    observation: super::semantic::UsageObservationV1,
) {
    if semantic
        .usage
        .iter()
        .any(|existing| existing.id == observation.id)
    {
        return;
    }
    if semantic.usage.len() == MAX_PROJECTED_USAGE_OBSERVATIONS {
        semantic.usage.remove(0);
    }
    semantic.usage.push(observation);
}

fn observed_at_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn bound_authoritative_finals(semantic: &mut SemanticSnapshotV1, retained: OperationId) {
    while !semantic.authoritative_finals.is_empty()
        && serde_json::to_vec(&semantic.authoritative_finals)
            .map_or(true, |encoded| encoded.len() > MAX_PROJECTED_FINAL_BYTES)
    {
        let remove = semantic
            .authoritative_finals
            .keys()
            .copied()
            .find(|operation_id| *operation_id != retained)
            .unwrap_or(retained);
        semantic.authoritative_finals.remove(&remove);
    }
}

fn append_semantic_content(
    semantic: &mut SemanticSnapshotV1,
    parts: impl IntoIterator<Item = super::semantic::ContentPartV1>,
) {
    semantic.content.extend(parts);
    if semantic.content.len() > MAX_PROJECTED_CONTENT_PARTS {
        let excess = semantic.content.len() - MAX_PROJECTED_CONTENT_PARTS;
        semantic.content.drain(..excess);
    }
    while !semantic.content.is_empty()
        && serde_json::to_vec(&semantic.content)
            .map_or(true, |encoded| encoded.len() > MAX_PROJECTED_CONTENT_BYTES)
    {
        semantic.content.remove(0);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ClientEvent {
    Runtime(Box<AgentEvent>),
    Managed(Box<ManagedClientEvent>),
    Semantic(Box<SemanticEventEnvelopeV1>),
    PayloadOmitted {
        kind: String,
        encoded_bytes: usize,
        limit: usize,
    },
}

impl ClientEvent {
    pub(crate) fn bounded(event: AgentEvent) -> Self {
        let kind = event_kind(&event);
        let redacted = serde_json::to_value(event).and_then(|mut value| {
            redact_sensitive_fields(&mut value);
            serde_json::from_value::<AgentEvent>(value)
        });
        match redacted.and_then(|event| serde_json::to_vec(&event).map(|encoded| (event, encoded)))
        {
            Ok((event, encoded)) if encoded.len() <= MAX_EVENT_BYTES => {
                Self::Runtime(Box::new(event))
            }
            Ok((_, encoded)) => Self::PayloadOmitted {
                kind: kind.to_owned(),
                encoded_bytes: encoded.len(),
                limit: MAX_EVENT_BYTES,
            },
            Err(_) => Self::PayloadOmitted {
                kind: kind.to_owned(),
                encoded_bytes: 0,
                limit: MAX_EVENT_BYTES,
            },
        }
    }
}

fn redact_sensitive_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
                if normalized.contains("authorization")
                    || normalized.contains("accesstoken")
                    || normalized.contains("refreshtoken")
                    || normalized.contains("apikey")
                    || normalized.contains("password")
                    || normalized == "secret"
                {
                    *value = serde_json::Value::String("[REDACTED]".to_owned());
                } else {
                    redact_sensitive_fields(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_sensitive_fields(value);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClientObservation {
    pub(crate) version: u16,
    pub(crate) sequence: u64,
    pub(crate) event: ClientEvent,
}

fn bounded_history(history: Vec<Message>) -> (Vec<Message>, bool) {
    let original_len = history.len();
    let mut retained = Vec::with_capacity(original_len.min(MAX_SNAPSHOT_MESSAGES));
    let mut encoded_bytes = 2_usize; // JSON array brackets.
    for message in history.into_iter().rev().take(MAX_SNAPSHOT_MESSAGES) {
        let Ok(encoded) = serde_json::to_vec(&message) else {
            break;
        };
        let separator = usize::from(!retained.is_empty());
        let Some(next_bytes) = encoded_bytes
            .checked_add(separator)
            .and_then(|value| value.checked_add(encoded.len()))
        else {
            break;
        };
        if next_bytes > MAX_SNAPSHOT_BYTES {
            break;
        }
        encoded_bytes = next_bytes;
        retained.push(message);
    }
    retained.reverse();
    let truncated = retained.len() < original_len;
    (retained, truncated)
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

fn event_kind(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::TerminalDiagnostic { .. } => "terminal diagnostic",
        AgentEvent::CompletionEvidenceRecorded { .. } => "completion evidence",
        AgentEvent::UserMessageCommitted { .. } => "committed user message",
        AgentEvent::BrowserStatus { .. } => "browser status",
        AgentEvent::OperationStateChanged { .. } => "operation state",
        AgentEvent::AssistantTextDelta { .. } => "assistant delta",
        AgentEvent::ProviderReasoningDelta { .. } => "provider reasoning delta",
        AgentEvent::PermissionRequested { .. } => "permission request",
        AgentEvent::PermissionAudited { .. } => "permission audit",
        AgentEvent::InvocationIntentCommitted { .. } => "invocation intent",
        AgentEvent::InvocationResultCommitted { .. } => "invocation result",
        AgentEvent::ToolFinished { .. } => "tool result",
        AgentEvent::AssistantMessage { .. } => "assistant message",
        AgentEvent::UsageObserved { .. } => "usage observation",
        AgentEvent::RoundBudgetReached { .. } => "round budget reached",
        AgentEvent::RoundBudgetDecisionCommitted { .. } => "round budget decision",
        AgentEvent::OperationFailed { .. } => "operation failure",
        AgentEvent::ConversationCleared => "conversation clear",
        AgentEvent::PromptPlanUpdated { .. } => "prompt plan",
        AgentEvent::CompactionStarted { .. } => "compaction started",
        AgentEvent::ConversationCompacted { .. } => "conversation compacted",
        AgentEvent::CompactionUnavailable { .. } => "compaction unavailable",
        AgentEvent::CommandRejected { .. } => "command rejection",
        AgentEvent::TurnStartUnavailable { .. } => "turn start unavailable",
        AgentEvent::ChildLifecycleChanged { .. } => "child lifecycle",
        AgentEvent::ChildActivity { .. } => "child activity",
        AgentEvent::ChildReportCommitted { .. } => "child report",
        AgentEvent::ChildListSnapshot { .. } => "child list",
        AgentEvent::ChildInspectionSnapshot { .. } => "child inspection",
        AgentEvent::ChildCancellationRequested { .. } => "child cancellation",
        AgentEvent::ExternalAgentActivity { .. } => "external-agent activity",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed::codex::ManagedNotification;
    use crate::message::{ContentBlock, Role};
    use crate::{permission::PermissionRequest, tool::EffectClass};

    #[test]
    fn completion_projection_is_byte_bounded_and_stale_evidence_cannot_downgrade_it() {
        use crate::completion_evidence::*;
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "fixture".into(),
                execution_owner: "native".into(),
                model: "fixture".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "deny".into(),
                children: vec![],
                resource_policy: ResourcePolicyV1::default(),
            },
            vec![],
        );
        let mut last = None;
        for index in 0..128 {
            let operation = OperationId::new();
            let contract = CompletionContract {
                conditions: vec![AcceptanceCondition::CommandSucceeded {
                    command: "c".repeat(4000),
                    cwd: ".".into(),
                }],
            };
            let mut evidence = CompletionEvidence::new(
                operation,
                WorkKind::Root,
                EvidenceOwner::Native,
                CompletionClaim::Completed,
                contract,
            )
            .unwrap();
            evidence.revision = 2;
            evidence.delivered(b"result");
            evidence.checks.push(CheckEvidence {
                invocation: crate::identity::ToolInvocationId::new(),
                generation: operation,
                command_digest: command_digest(&"c".repeat(4000), "."),
                work_revision: 0,
                outcome: CheckOutcome::Passed,
                exit_code: Some(0),
            });
            add_artifact(
                &mut evidence,
                crate::artifact::ArtifactRecord {
                    reference: crate::artifact::ArtifactRef {
                        id: crate::identity::ArtifactId::new(),
                        content_hash: crate::artifact::ContentHash::for_bytes(b"x"),
                    },
                    media_type: "text/plain".into(),
                    byte_len: 1,
                    owner: crate::identity::PrincipalId::new(),
                },
            );
            evidence.reserve_verifier(true, false).unwrap();
            snapshot.apply(
                &ClientEvent::bounded(AgentEvent::CompletionEvidenceRecorded {
                    operation_id: operation,
                    evidence: evidence.clone(),
                }),
                index + 1,
            );
            last = Some(evidence);
        }
        assert!(snapshot.semantic.completion_receipts.len() < 128);
        assert!(
            serde_json::to_vec(&snapshot.semantic.completion_receipts)
                .unwrap()
                .len()
                <= 256 * 1024
        );
        let last = last.unwrap();
        let mut stale = last.clone();
        stale.revision = 1;
        stale.claim = CompletionClaim::Interrupted;
        stale.evaluate();
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::CompletionEvidenceRecorded {
                operation_id: last.generation,
                evidence: stale,
            }),
            129,
        );
        assert_eq!(
            snapshot
                .semantic
                .completion_receipts
                .last()
                .unwrap()
                .evidence
                .as_deref(),
            Some(&last)
        );
        let mut newer = last.clone();
        newer.revision = 3;
        newer.verification = VerificationState::Passed;
        newer.artifacts[0].verified = true;
        newer.evaluate();
        assert!(valid_transition(Some(&last), &newer));
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::CompletionEvidenceRecorded {
                operation_id: newer.generation,
                evidence: newer.clone(),
            }),
            130,
        );
        let expected = snapshot
            .semantic
            .completion_receipts
            .last()
            .unwrap()
            .clone();
        assert!(expected.evidence.as_ref().unwrap().supported());
        for incoming in [last, newer.clone()] {
            snapshot.apply(
                &ClientEvent::bounded(AgentEvent::CompletionEvidenceRecorded {
                    operation_id: incoming.generation,
                    evidence: incoming,
                }),
                131,
            );
            assert_eq!(
                snapshot.semantic.completion_receipts.last(),
                Some(&expected)
            );
        }
        let mut incoherent = newer;
        incoherent.generation = OperationId::new();
        incoherent.claim = CompletionClaim::Failed;
        let expected_all = snapshot.semantic.completion_receipts.clone();
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::CompletionEvidenceRecorded {
                operation_id: incoherent.generation,
                evidence: incoherent,
            }),
            132,
        );
        assert_eq!(snapshot.semantic.completion_receipts, expected_all);
        let reopened: ClientSnapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert_eq!(
            reopened.semantic.completion_receipts,
            snapshot.semantic.completion_receipts
        );
    }

    #[test]
    fn ten_thousand_source_messages_reduce_to_the_recent_bounded_snapshot() {
        let history = (0..10_000)
            .map(|index| Message::text(Role::User, format!("message {index}")))
            .collect();
        let (bounded, truncated) = bounded_history(history);

        assert!(truncated);
        assert_eq!(bounded.len(), MAX_SNAPSHOT_MESSAGES);
        assert!(matches!(
            bounded.first().and_then(|message| message.content.first()),
            Some(ContentBlock::Text(text)) if text == "message 9488"
        ));
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= MAX_SNAPSHOT_BYTES);
    }

    #[test]
    fn byte_bounding_keeps_a_contiguous_recent_suffix_without_quadratic_removal() {
        let history = (0..MAX_SNAPSHOT_MESSAGES)
            .map(|index| Message::text(Role::User, format!("{index}:{}", "x".repeat(8 * 1024))))
            .collect::<Vec<_>>();

        let (bounded, truncated) = bounded_history(history);

        assert!(truncated);
        assert!(!bounded.is_empty());
        assert!(bounded.len() < MAX_SNAPSHOT_MESSAGES);
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= MAX_SNAPSHOT_BYTES);
        assert!(matches!(
            bounded.last().and_then(|message| message.content.first()),
            Some(ContentBlock::Text(text)) if text.starts_with("511:")
        ));
    }

    #[test]
    #[ignore = "manual release-profile M4 projection measurement; wall-clock thresholds do not belong in shared CI"]
    fn m4_reference_snapshot_projection_probe() {
        let fixture = (0..10_000)
            .map(|index| Message::text(Role::User, format!("message {index}: {}", "x".repeat(128))))
            .collect::<Vec<_>>();
        let mut samples = Vec::with_capacity(31);
        let mut retained = 0;
        let mut encoded = 0;
        for _ in 0..31 {
            let started = std::time::Instant::now();
            let (bounded, _) = bounded_history(fixture.clone());
            samples.push(started.elapsed());
            retained = bounded.len();
            encoded = serde_json::to_vec(&bounded).unwrap().len();
        }
        samples.sort_unstable();
        let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
        let p99 = samples[(samples.len() * 99 / 100).min(samples.len() - 1)];

        println!(
            concat!(
                "m4_metric {{\"name\":\"snapshot_projection\",",
                "\"source_messages\":10000,\"retained_messages\":{},",
                "\"encoded_bytes\":{},\"p95_us\":{},\"p99_us\":{}}}"
            ),
            retained,
            encoded,
            p95.as_micros(),
            p99.as_micros(),
        );
    }

    #[test]
    fn oversized_observation_becomes_bounded_omission() {
        let event = AgentEvent::CommandRejected {
            reason: "x".repeat(MAX_EVENT_BYTES + 1),
        };
        let bounded = ClientEvent::bounded(event);

        assert!(matches!(
            bounded,
            ClientEvent::PayloadOmitted {
                kind,
                encoded_bytes,
                limit: MAX_EVENT_BYTES,
            } if kind == "command rejection" && encoded_bytes > MAX_EVENT_BYTES
        ));
    }

    #[test]
    fn omitted_committed_messages_preserve_positions_without_retaining_payloads() {
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "fixture".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            Vec::new(),
        );
        let oversized = ClientEvent::bounded(AgentEvent::UserMessageCommitted {
            operation_id: OperationId::new(),
            message: Message::text(Role::User, "x".repeat(MAX_EVENT_BYTES + 1)),
        });
        assert!(matches!(oversized, ClientEvent::PayloadOmitted { .. }));
        snapshot.apply(&oversized, 1);
        assert_eq!(snapshot.conversation_total, 1);
        assert_eq!(snapshot.conversation_start, 0);
        assert_eq!(snapshot.conversation.len(), 1);
        assert!(snapshot.conversation_truncated);
        assert!(serde_json::to_vec(&snapshot.conversation).unwrap().len() < 512);
        snapshot.apply(
            &ClientEvent::PayloadOmitted {
                kind: "assistant delta".into(),
                encoded_bytes: MAX_EVENT_BYTES + 1,
                limit: MAX_EVENT_BYTES,
            },
            2,
        );
        assert_eq!(snapshot.conversation_total, 1);
    }

    #[test]
    fn interrupt_and_steer_commands_keep_exact_operation_correlation() {
        let operation_id = OperationId::new();
        for value in [
            ClientCommandValue::InterruptOperation { operation_id },
            ClientCommandValue::SteerOperation {
                operation_id,
                input: "focus on the failing test".to_owned(),
            },
        ] {
            let command = ClientCommand::new(value.clone());
            let encoded = serde_json::to_vec(&command).unwrap();
            let decoded: ClientCommand = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded.version, FRONTEND_PROTOCOL_VERSION);
            assert_eq!(decoded.semantic_id, value.semantic_id());
            assert!(crate::command_catalog::find(&decoded.semantic_id).is_some());
            assert_eq!(decoded.value, value);
            let runtime: RuntimeCommand = decoded.value.into();
            assert!(matches!(
                runtime,
                RuntimeCommand::InterruptOperation { operation_id: actual }
                    | RuntimeCommand::SteerOperation { operation_id: actual, .. }
                    if actual == operation_id
            ));
        }
    }

    #[test]
    fn every_frontend_command_semantic_id_is_registered() {
        for id in [
            "turn.submit.v1",
            "conversation.clear.v1",
            "conversation.compact.v1",
            "run.resume.v1",
            "run.continue.v1",
            "run.stop.v1",
            "run.interrupt.v1",
            "run.steer.v1",
            "approval.decide.v1",
            "child.list.v1",
            "child.inspect.v1",
            "child.cancel.v1",
            "browser.control.v1",
            "application.shutdown.v1",
        ] {
            assert!(crate::command_catalog::find(id).is_some(), "{id}");
        }
    }

    #[test]
    fn browser_controls_round_trip_without_entering_the_model_queue() {
        use crate::browser::{BrowserControl, BrowserResolution};
        for action in [
            BrowserControl::Status,
            BrowserControl::Takeover,
            BrowserControl::Close,
            BrowserControl::Resolve {
                receipt: uuid::Uuid::new_v4(),
                revision: 7,
                outcome: BrowserResolution::Applied,
            },
            BrowserControl::Resolve {
                receipt: uuid::Uuid::new_v4(),
                revision: 9,
                outcome: BrowserResolution::NotApplied,
            },
        ] {
            let value = ClientCommandValue::BrowserControl { action };
            let command = ClientCommand::new(value.clone());
            let decoded: ClientCommand =
                serde_json::from_slice(&serde_json::to_vec(&command).unwrap()).unwrap();
            assert_eq!(decoded.version, FRONTEND_PROTOCOL_VERSION);
            assert_eq!(decoded.semantic_id, "browser.control.v1");
            assert_eq!(decoded.value, value);
            assert_eq!(
                RuntimeCommand::from(decoded.value),
                RuntimeCommand::BrowserControl { action }
            );
        }
    }

    #[test]
    fn round_budget_command_keeps_exact_operation_and_suspension_correlation() {
        let operation_id = OperationId::new();
        let suspension_id = RoundBudgetId::new();
        let value = ClientCommandValue::DecideRoundBudget {
            operation_id,
            suspension_id,
            action: RoundBudgetAction::Continue,
        };
        let encoded = serde_json::to_vec(&ClientCommand::new(value.clone())).unwrap();
        let decoded: ClientCommand = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.version, FRONTEND_PROTOCOL_VERSION);
        assert_eq!(decoded.value, value);
        assert_eq!(
            RuntimeCommand::from(decoded.value),
            RuntimeCommand::DecideRoundBudget {
                operation_id,
                suspension_id,
                action: RoundBudgetAction::Continue,
            }
        );
    }

    #[test]
    fn runtime_observation_redacts_secret_shaped_fields_before_delivery() {
        let event = ClientEvent::bounded(AgentEvent::PermissionRequested {
            request: PermissionRequest {
                operation_id: OperationId::new(),
                invocation_id: ToolInvocationId::new(),
                tool_name: "remote_call".to_owned(),
                effect_class: EffectClass::Network,
                final_arguments: serde_json::json!({
                    "api_key": "top-secret",
                    "nested": {"accessToken": "also-secret"},
                    "path": "README.md"
                }),
                scope: crate::permission::PermissionScope::Unscoped,
                outbound_review: None,
            },
        });
        let json = serde_json::to_string(&event).unwrap();

        assert!(!json.contains("top-secret"));
        assert!(!json.contains("also-secret"));
        assert!(json.contains("[REDACTED]"));
        assert!(json.contains("README.md"));
    }

    #[test]
    fn snapshot_retains_only_safe_pending_approval_identity_and_scope() {
        let operation_id = OperationId::new();
        let invocation_id = ToolInvocationId::new();
        let request = PermissionRequest {
            operation_id,
            invocation_id,
            tool_name: "write_file".to_owned(),
            effect_class: EffectClass::Write,
            final_arguments: serde_json::json!({"secret": "must-not-enter-snapshot"}),
            scope: crate::permission::PermissionScope::WorkspacePath {
                canonical_path: std::path::PathBuf::from("workspace/README.md"),
            },
            outbound_review: None,
        };
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "test".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            Vec::new(),
        );

        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::PermissionRequested {
                request: request.clone(),
            }),
            1,
        );

        assert_eq!(snapshot.pending_approval_count, 1);
        assert_eq!(snapshot.pending_approvals[0].invocation_id, invocation_id);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("must-not-enter-snapshot"));
        assert!(encoded.contains("README.md"));

        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::PermissionAudited {
                fact: crate::permission::PermissionAuditFact {
                    request,
                    policy_evaluation: crate::permission::PolicyDecision::Ask,
                    controller_decision: Some(crate::permission::ControllerDecision::Deny),
                    effective: crate::permission::PolicyDecision::Deny,
                },
            }),
            2,
        );
        assert!(snapshot.pending_approvals.is_empty());
        assert_eq!(snapshot.pending_approval_count, 0);
    }

    #[test]
    fn memory_snapshot_retains_exact_proposal_without_owner_source_or_generic_arguments() {
        let id = Uuid::new_v4();
        let request = PermissionRequest {
            operation_id: OperationId::new(),
            invocation_id: ToolInvocationId::new(),
            tool_name: "memory_update".into(),
            effect_class: EffectClass::Write,
            final_arguments: serde_json::json!({
                "action":"correct", "statement":"My favorite color is blue",
                "id":id, "revision":7, "quote":"OWNER_SOURCE_MUST_NOT_ENTER_SNAPSHOT",
                "source_id":Uuid::new_v4(), "extra":"GENERIC_ARGUMENT_MUST_NOT_ENTER_SNAPSHOT"
            }),
            scope: crate::permission::PermissionScope::PersonalMemory {
                scope: "user".into(),
                review: true,
            },
            outbound_review: None,
        };
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "test".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: vec![],
                resource_policy: ResourcePolicyV1::default(),
            },
            vec![],
        );
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::PermissionRequested {
                request: request.clone(),
            }),
            1,
        );
        let proposal = snapshot.pending_approvals[0]
            .memory_proposal
            .as_ref()
            .unwrap();
        assert_eq!(proposal.action, MemoryPermissionAction::Correct);
        assert_eq!(proposal.id, Some(id));
        assert_eq!(proposal.revision, Some(7));
        assert!(proposal.review_text().contains("My favorite color is blue"));
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(encoded.contains("My favorite color is blue"));
        assert!(!encoded.contains("OWNER_SOURCE_MUST_NOT_ENTER_SNAPSHOT"));
        assert!(!encoded.contains("GENERIC_ARGUMENT_MUST_NOT_ENTER_SNAPSHOT"));
        assert!(!encoded.contains("final_arguments"));
        let mut bounded = request.clone();
        bounded.final_arguments["statement"] = serde_json::json!("é".repeat(2049));
        assert!(MemoryPermissionProposal::from_request(&bounded).is_none());
        bounded.final_arguments["statement"] = serde_json::json!("é".repeat(2048));
        assert_eq!(
            MemoryPermissionProposal::from_request(&bounded)
                .unwrap()
                .statement
                .unwrap()
                .len(),
            4096
        );
        bounded.tool_name = "write_file".into();
        assert!(MemoryPermissionProposal::from_request(&bounded).is_none());
        let mut old = serde_json::to_value(&snapshot.pending_approvals[0]).unwrap();
        old.as_object_mut().unwrap().remove("memory_proposal");
        let old: PendingPermissionProjection = serde_json::from_value(old).unwrap();
        assert!(
            old.memory_proposal.is_none(),
            "additive field preserves older snapshots"
        );
    }

    #[test]
    fn managed_notifications_cross_the_same_bounded_frontend_vocabulary() {
        let event = ManagedClientEvent::from_notification(ManagedNotification::AssistantDelta {
            item_id: Some("vendor-item-id".to_owned()),
            delta: "managed result".to_owned(),
        })
        .expect("conversation notification");
        let observation = ClientObservation {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: 4,
            event: ClientEvent::Managed(Box::new(event)),
        };
        let json = serde_json::to_string(&observation).unwrap();

        assert!(json.contains("managed result"));
        assert!(!json.contains("vendor-item-id"));
        assert!(!json.contains("jsonrpc"));
        assert!(
            ManagedClientEvent::from_notification(ManagedNotification::Other {
                method: "vendor/private/method".to_owned(),
            })
            .is_none()
        );
    }

    #[test]
    fn semantic_observation_advances_the_shared_snapshot_watermark() {
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "test".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            Vec::new(),
        );
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::CommandRejected {
                reason: "legacy event".into(),
            }),
            1,
        );
        let event = ClientEvent::Semantic(Box::new(SemanticEventEnvelopeV1 {
            version: crate::frontend::semantic::SEMANTIC_PROTOCOL_VERSION,
            kind: "content_appended".into(),
            payload: serde_json::json!({
                "kind": "content_appended",
                "parts": [{"kind": "text", "payload": {"text": "hello"}}],
                "origin": {"kind": "interactive"}
            }),
        }));

        snapshot.apply(&event, 2);

        assert_eq!(snapshot.semantic.sequence, 2);
        assert_eq!(snapshot.semantic.content.len(), 1);
    }

    #[test]
    fn initial_snapshot_freezes_the_configured_resource_policy() {
        let resource_policy = ResourcePolicyV1 {
            max_resources_per_turn: 9,
            ..ResourcePolicyV1::default()
        };

        let snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "test".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: resource_policy.clone(),
            },
            Vec::new(),
        );

        assert_eq!(
            snapshot.semantic.attachment_policy.configured,
            resource_policy
        );
        assert!(snapshot.semantic.attachment_policy.route_limit.is_none());
    }

    #[test]
    fn final_runtime_messages_gain_bounded_inert_semantic_projections() {
        let operation_id = OperationId::new();
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "test".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            Vec::new(),
        );
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::AssistantMessage {
                operation_id,
                message: Message::text(Role::Assistant, "```diff\n-old\n+new\n```"),
            }),
            1,
        );

        assert!(matches!(
            snapshot.semantic.content.as_slice(),
            [crate::frontend::semantic::ContentPartV1::Diff { .. }]
        ));
        assert!(matches!(
            snapshot.semantic.authoritative_finals[&operation_id].as_slice(),
            [crate::frontend::semantic::ContentPartV1::Diff { .. }]
        ));
        snapshot.semantic.validate().unwrap();

        snapshot.apply(&ClientEvent::bounded(AgentEvent::ConversationCleared), 2);
        assert!(snapshot.semantic.content.is_empty());
        assert!(snapshot.semantic.authoritative_finals.is_empty());
    }

    #[test]
    fn committed_messages_keep_absolute_tail_positions_for_late_attach_and_clear() {
        let mut snapshot = ClientSnapshot::initial_page(
            ClientSnapshotSeed {
                session_id: SessionId::new(),
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "fixture".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            crate::session::ConversationPage {
                messages: (1000..1128)
                    .map(|index| Message::text(Role::User, format!("old-{index}")))
                    .collect(),
                start: 1000,
                total: 1128,
                has_older: true,
            },
        );
        let operation_id = OperationId::new();
        let user = Message::text(Role::User, "current input");
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::UserMessageCommitted {
                operation_id,
                message: user.clone(),
            }),
            1,
        );
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::AssistantMessage {
                operation_id,
                message: Message::text(Role::Assistant, "response"),
            }),
            2,
        );
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::ToolFinished {
                operation_id,
                invocation_id: ToolInvocationId::new(),
                result: Message::text(Role::Tool, "evidence"),
            }),
            3,
        );
        assert_eq!(
            (snapshot.conversation_start, snapshot.conversation_total),
            (1000, 1131)
        );
        let attached: ClientSnapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert_eq!(
            attached
                .conversation
                .iter()
                .filter(|message| **message == user)
                .count(),
            1
        );
        assert!(!attached.semantic.authoritative_finals[&operation_id].is_empty());
        for index in 0..600 {
            snapshot.apply(
                &ClientEvent::bounded(AgentEvent::UserMessageCommitted {
                    operation_id: OperationId::new(),
                    message: Message::text(Role::User, format!("next-{index}")),
                }),
                index + 4,
            );
        }
        assert_eq!(snapshot.conversation_total, 1731);
        assert_eq!(
            snapshot.conversation_start + snapshot.conversation.len(),
            1731
        );
        assert!(snapshot.conversation.len() <= 512 && snapshot.conversation_truncated);
        snapshot.apply(&ClientEvent::bounded(AgentEvent::ConversationCleared), 605);
        assert_eq!(
            (snapshot.conversation_start, snapshot.conversation_total),
            (0, 0)
        );
        assert!(snapshot.conversation.is_empty() && !snapshot.conversation_truncated);
    }

    #[test]
    fn native_usage_is_projected_once_with_explicit_run_and_process_period() {
        let session_id = SessionId::new();
        let operation_id = OperationId::new();
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id,
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "test".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            Vec::new(),
        );
        let event = ClientEvent::bounded(AgentEvent::UsageObserved {
            operation_id,
            usage: crate::agent::AgentTurnUsage {
                input_tokens: Some(12),
                output_tokens: Some(4),
                total_tokens: Some(16),
                requests: 1,
                ..crate::agent::AgentTurnUsage::default()
            },
        });

        snapshot.apply(&event, 1);
        snapshot.apply(&event, 2);

        assert_eq!(
            snapshot.semantic.conversation_id,
            Some(ConversationId::for_native(session_id))
        );
        assert_eq!(snapshot.semantic.usage.len(), 1);
        let observation = &snapshot.semantic.usage[0];
        assert_eq!(
            observation.scope,
            crate::frontend::semantic::UsageScopeV1::Run {
                run_id: operation_id
            }
        );
        assert_eq!(observation.period, session_id.to_string());
        assert_eq!(observation.amounts.input_tokens, Some(12));
        assert_eq!(observation.amounts.output_tokens, Some(4));
    }

    #[test]
    fn native_runs_publish_deterministic_execution_facts_and_completion_receipts() {
        let session_id = SessionId::new();
        let operation_id = OperationId::new();
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id,
                connection: "local".into(),
                execution_owner: "native".into(),
                model: "test".into(),
                reasoning_effort: None,
                host_location: HostLocationV1::Embedded,
                approval_policy: "ask".into(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            Vec::new(),
        );

        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::OperationStateChanged {
                operation_id,
                state: crate::native_runtime::OperationState::Running,
            }),
            1,
        );
        snapshot.apply(
            &ClientEvent::bounded(AgentEvent::UsageObserved {
                operation_id,
                usage: crate::agent::AgentTurnUsage {
                    input_tokens: Some(12),
                    output_tokens: Some(4),
                    total_tokens: Some(16),
                    requests: 1,
                    ..crate::agent::AgentTurnUsage::default()
                },
            }),
            2,
        );
        let finished = ClientEvent::bounded(AgentEvent::OperationStateChanged {
            operation_id,
            state: crate::native_runtime::OperationState::Finished(
                crate::native_runtime::OperationOutcome::Completed,
            ),
        });
        snapshot.apply(&finished, 3);

        assert_eq!(snapshot.semantic.execution_facts.len(), 1);
        let facts = &snapshot.semantic.execution_facts[0];
        assert_eq!(
            facts.conversation_id,
            ConversationId::for_native(session_id)
        );
        assert_eq!(facts.run_id, operation_id);
        assert_eq!(facts.owner, ExecutionOwnerV1::Native);
        assert_eq!(facts.host, HostLocationV1::Embedded);
        assert_eq!(
            facts.workspace_authority,
            WorkspaceAuthorityV1::UncontainedFullAccess
        );
        assert_eq!(facts.connection.as_deref(), Some("local"));
        assert_eq!(facts.model.as_deref(), Some("test"));
        assert_eq!(facts.approval_policy, "ask");

        assert_eq!(snapshot.semantic.completion_receipts.len(), 1);
        let receipt = &snapshot.semantic.completion_receipts[0];
        assert_eq!(receipt.conversation_id, facts.conversation_id);
        assert_eq!(receipt.run_id, operation_id);
        assert_eq!(receipt.status, CompletionStatusV1::Completed);
        assert_eq!(receipt.usage.amounts.input_tokens, Some(12));
        assert_eq!(receipt.usage.amounts.output_tokens, Some(4));
        assert_eq!(receipt.usage.observation_count, 1);
        assert!(receipt.usage.incomplete);
        let receipt_id = receipt.id;
        snapshot.semantic.validate().unwrap();

        snapshot.apply(&finished, 4);

        assert_eq!(snapshot.semantic.execution_facts.len(), 1);
        assert_eq!(snapshot.semantic.completion_receipts.len(), 1);
        assert_eq!(snapshot.semantic.completion_receipts[0].id, receipt_id);
        snapshot.semantic.validate().unwrap();
    }
}
