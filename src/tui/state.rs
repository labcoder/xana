//! Terminal-independent state and update policy for the full-screen client.

mod commands;
mod projection;
mod window;

pub(super) use super::composer::MoveDirection;
use super::composer::{Composer, MAX_INPUT_BYTES, sanitize_input};
use super::espejo::{EspejoScope, EspejoViewState};
use super::session::{self, SessionRow};
use super::{
    activity::{self, ActivityCard, ActivityKind, ActivityState, ApprovalPrompt, ApprovalTarget},
    command::{self, CommandId, CommandSpec, ParsedCommand},
    rich_text::{ArtifactView, RichDocument},
};
use crate::{
    agent::SessionUsage,
    frontend::{
        ClientSnapshot, EmbeddedClient, ManagedClientEvent,
        semantic::{
            AttachmentV1, CompletionReceiptV1, CompletionStatusV1, ExecutionFactsV1,
            ExecutionOwnerV1, FactAuthorityV1, FactSourceV1, FreshnessV1, HostLocationV1,
            SemanticCodeV1, SemanticSnapshotV1, UsageAggregateV1, UsageLedgerV1, UsageScopeV1,
            WorkspaceAuthorityV1, normalize_message,
        },
    },
    identity::{AgentId, OperationId, ToolInvocationId},
    managed::codex::ManagedTokenUsage,
    message::{ContentBlock, Message, Role},
    native_runtime::{AgentEvent, OperationState, RoundBudgetAction, RoundBudgetSuspension},
    permission::ControllerDecision,
    presentation::{ActivityPaneChoice, ComposerPreset},
    prompt::PromptPlanLedger,
    vision::{ImageAttachment, MAX_IMAGE_BYTES_PER_TURN, MAX_IMAGES_PER_TURN, image_paths_in_text},
    workspace_host::{ConversationRef, WorkspaceSnapshot},
};
use std::{
    collections::{BTreeMap, VecDeque},
    ops::Range,
};

const MAX_VISIBLE_MESSAGES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_ACTIVITY: usize = 256;
const MAX_ACTIVITY_BYTES: usize = 16 * 1024;
const MAX_FOLLOWUPS: usize = 32;
const MAX_FOLLOWUP_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutClass {
    Wide,
    Medium,
    Narrow,
}

impl LayoutClass {
    pub(super) fn for_width(width: u16) -> Self {
        if width >= 110 {
            Self::Wide
        } else if width >= 72 {
            Self::Medium
        } else {
            Self::Narrow
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageKind {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VisibleMessage {
    pub(super) kind: MessageKind,
    pub(super) text: String,
    pub(super) document: RichDocument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScreenPoint {
    pub(super) column: u16,
    pub(super) row: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConversationSelection {
    pub(super) start: ScreenPoint,
    pub(super) end: ScreenPoint,
    pub(super) dragged: bool,
    pub(super) text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OwnerCapabilities {
    pub(super) interrupt: bool,
    pub(super) steer: bool,
    pub(super) model: bool,
    pub(super) reasoning: bool,
    pub(super) compact: bool,
}

impl OwnerCapabilities {
    pub(super) const fn native() -> Self {
        Self {
            interrupt: true,
            steer: false,
            model: true,
            reasoning: false,
            compact: true,
        }
    }

    const fn managed() -> Self {
        Self {
            interrupt: true,
            steer: false,
            model: true,
            reasoning: true,
            compact: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivityVisibility {
    Auto,
    Open,
    Hidden,
}

impl From<ActivityPaneChoice> for ActivityVisibility {
    fn from(value: ActivityPaneChoice) -> Self {
        match value {
            ActivityPaneChoice::Auto => Self::Auto,
            ActivityPaneChoice::Open => Self::Open,
            ActivityPaneChoice::Hidden => Self::Hidden,
        }
    }
}

impl From<ActivityVisibility> for ActivityPaneChoice {
    fn from(value: ActivityVisibility) -> Self {
        match value {
            ActivityVisibility::Auto => Self::Auto,
            ActivityVisibility::Open => Self::Open,
            ActivityVisibility::Hidden => Self::Hidden,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InputAction {
    Insert(String),
    Paste(String),
    Move {
        direction: MoveDirection,
        select: bool,
    },
    Backspace,
    Delete,
    Submit,
    Newline,
    HistoryPrevious,
    HistoryNext,
    CompleteFile,
    OpenPalette,
    PaletteUp,
    PaletteDown,
    PreviewSelected,
    SelectEspejo(usize),
    Confirm,
    Cancel,
    CopyOrInterrupt,
    Scroll(i16),
    BeginConversationSelection(ScreenPoint),
    ExtendConversationSelection(ScreenPoint),
    FinishConversationSelection {
        end: ScreenPoint,
        text: Option<String>,
    },
    ClearConversationSelection,
    BeginActivitySelection(ScreenPoint),
    ExtendActivitySelection(ScreenPoint),
    FinishActivitySelection {
        end: ScreenPoint,
        text: Option<String>,
    },
    ClearActivitySelection,
    PlaceCursor {
        line: usize,
        column: usize,
        width: u16,
        scroll: u16,
        select: bool,
    },
    ChooseOverlay(usize),
    ViewSession(ConversationRef),
    ToggleActivity(usize),
    OpenActivityDetail(usize),
    ToggleSessionsView,
    ToggleHeader,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum UpdateEffect {
    None,
    Doctor,
    Reset,
    Setup(String),
    Settings(String),
    ControlCommand {
        family: String,
        arguments: String,
    },
    CompleteFile {
        query: String,
        replacement: Range<usize>,
    },
    Submit {
        operation_id: OperationId,
        input: String,
        images: Vec<ImageAttachment>,
        vision_route: Option<String>,
    },
    PrepareVision {
        operation_id: OperationId,
        input: String,
        images: Vec<ImageAttachment>,
        plan: Box<crate::app::vision::VisionPlan>,
        decision: crate::outbound::OutboundApprovalDecision,
    },
    Interrupt {
        operation_id: OperationId,
    },
    Steer {
        operation_id: OperationId,
        input: String,
    },
    Attach(String),
    AttachDropped(String),
    AttachApproved(String),
    AttachAndSubmit {
        operation_id: OperationId,
        input: String,
        paths: Vec<String>,
        approved_external: bool,
    },
    AttachClipboard,
    SelectModel(String),
    SetReasoning(String),
    PersistComposer(ComposerPreset),
    ClearConversation,
    CompactConversation {
        operation_id: OperationId,
    },
    DecideRoundBudget {
        suspension: RoundBudgetSuspension,
        action: RoundBudgetAction,
    },
    NewConversation,
    OpenModelPicker,
    OpenReasoningPicker,
    OpenSessionPicker,
    ViewSession(ConversationRef),
    SwitchConversation(ConversationRef),
    LoadOlder(ConversationRef),
    LoadNewer(ConversationRef),
    PersistRail(bool),
    ArchiveConversation(ConversationRef),
    PersistActivity(ActivityPaneChoice),
    CopyText(String),
    ArtifactAction {
        record: crate::artifact::ArtifactRecord,
        action: ArtifactAction,
    },
    DecideNativeApproval {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    DecideChildApproval {
        agent_id: AgentId,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
        decision: ControllerDecision,
    },
    DecideManagedApproval(crate::managed::codex::ApprovalDecision),
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArtifactAction {
    Preview,
    CopyReference,
    Save,
    InsertReference,
    Reveal,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueuedTurn {
    pub(super) input: String,
    images: Vec<ImageAttachment>,
    vision_route: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversationDraft {
    composer: Composer,
    images: Vec<ImageAttachment>,
    resources: Vec<AttachmentV1>,
    vision_route: Option<String>,
}

impl Default for ConversationDraft {
    fn default() -> Self {
        Self {
            composer: Composer::new(),
            images: Vec::new(),
            resources: Vec::new(),
            vision_route: None,
        }
    }
}

/// Frontend-local state carried across an in-process TUI Conversation switch.
///
/// This is deliberately not canonical Conversation data. It keeps unsent input
/// associated with its exact Conversation while the execution owner is rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TuiContinuation {
    drafts: BTreeMap<ConversationRef, ConversationDraft>,
    queued: BTreeMap<ConversationRef, VecDeque<QueuedTurn>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Overlay {
    Palette {
        query: String,
        selected: usize,
    },
    PastePreview {
        text: String,
    },
    ExternalImageApproval {
        operation_id: OperationId,
        input: String,
        paths: Vec<String>,
        external_paths: Vec<String>,
        selected: usize,
    },
    VisionApproval {
        operation_id: OperationId,
        input: String,
        images: Vec<ImageAttachment>,
        plan: Box<crate::app::vision::VisionPlan>,
        selected: usize,
    },
    Help,
    Queue,
    ModelPicker {
        choices: Vec<String>,
        selected: usize,
    },
    ReasoningPicker {
        choices: Vec<String>,
        selected: usize,
    },
    SessionPicker {
        query: String,
        choices: Vec<SessionRow>,
        selected: usize,
    },
    Approval {
        prompt: Box<ApprovalPrompt>,
        selected: usize,
    },
    ExternalResourceApproval {
        path: String,
        selected: usize,
    },
    Artifact {
        artifact: Box<ArtifactView>,
        selected: usize,
        preview: Option<String>,
    },
    ActivityDetail {
        card: Box<ActivityCard>,
        scroll: u16,
        selection: Option<ConversationSelection>,
    },
    CommandResult {
        title: String,
        content: String,
        scroll: u16,
    },
    FileCompletion {
        query: String,
        replacement: Range<usize>,
        choices: Vec<String>,
        selected: usize,
    },
    ProfileCreate {
        fields: [String; 3],
        selected: usize,
        error: Option<String>,
    },
}

pub(super) struct TuiState {
    pub(super) connection: String,
    pub(super) model: String,
    pub(super) session: String,
    pub(super) status: String,
    pub(super) composer: Composer,
    composer_history: VecDeque<String>,
    composer_history_cursor: Option<usize>,
    composer_history_scratch: String,
    pending_history_entries: VecDeque<String>,
    pub(super) messages: VecDeque<VisibleMessage>,
    pub(super) activity: VecDeque<ActivityCard>,
    pub(super) busy: bool,
    pub(super) work_indicator_frame: u8,
    pub(super) active_operation: Option<OperationId>,
    pub(super) pending_round_budget: Option<RoundBudgetSuspension>,
    pub(super) followups: VecDeque<QueuedTurn>,
    pub(super) overlay: Option<Overlay>,
    pub(super) activity_visibility: ActivityVisibility,
    pub(super) auto_activity_open: bool,
    pub(super) composer_preset: ComposerPreset,
    pub(super) scroll: u16,
    pub(super) sessions: Vec<SessionRow>,
    pub(super) rail_expanded: bool,
    pub(super) header_expanded: bool,
    pub(super) espejo: Option<EspejoViewState>,
    pub(super) conversation_selection: Option<ConversationSelection>,
    pub(super) runtime_conversation: ConversationRef,
    pub(super) viewed_conversation: ConversationRef,
    pub(super) native_usage: SessionUsage,
    pub(super) managed_usage: Option<(u64, u64, u64)>,
    pub(super) managed_usage_sequence: u64,
    pub(super) semantic: SemanticSnapshotV1,
    pub(super) prompt_plans: Vec<(OperationId, PromptPlanLedger)>,
    pub(super) inline_image_capability: String,
    pub(super) capabilities: OwnerCapabilities,
    execution_owner: ExecutionOwnerV1,
    pending_images: Vec<ImageAttachment>,
    pending_resources: Vec<AttachmentV1>,
    pending_vision_route: Option<String>,
    drafts: BTreeMap<ConversationRef, ConversationDraft>,
    queued: BTreeMap<ConversationRef, VecDeque<QueuedTurn>>,
    background_messages: Option<VecDeque<VisibleMessage>>,
    history_start: usize,
    history_end: usize,
    history_preview: bool,
    history_has_older: bool,
    pub(super) workspace: std::path::PathBuf,
    pub(super) workspace_id: String,
    pub(super) active_host_root: Option<(ConversationRef, u32)>,
}

impl TuiState {
    pub(super) fn session_picker_open(&self) -> bool {
        matches!(self.overlay, Some(Overlay::SessionPicker { .. }))
    }

    pub(super) fn show_command_result(&mut self, title: String, content: String) {
        self.status = format!("{title} completed");
        self.overlay = Some(Overlay::CommandResult {
            title,
            content,
            scroll: 0,
        });
    }

    pub(super) fn show_command_error(&mut self, title: String, error: String) {
        self.status = format!("{title} failed");
        self.overlay = Some(Overlay::CommandResult {
            title,
            content: format!("Error: {error}"),
            scroll: 0,
        });
    }

    pub(super) fn install_composer_history(
        &mut self,
        entries: impl IntoIterator<Item = String>,
        warning: Option<String>,
    ) {
        self.composer_history.clear();
        for entry in entries {
            crate::terminal_productivity::append_history_entry(&mut self.composer_history, &entry);
        }
        self.composer_history_cursor = None;
        self.composer_history_scratch.clear();
        if let Some(warning) = warning {
            self.push_activity(warning);
        }
    }

    pub(super) fn take_pending_history_entry(&mut self) -> Option<String> {
        self.pending_history_entries.pop_front()
    }

    pub(super) fn composer_history_active(&self) -> bool {
        self.composer_history_cursor.is_some()
    }

    fn remember_composer_submission(&mut self, input: &str) {
        if let Some(entry) =
            crate::terminal_productivity::append_history_entry(&mut self.composer_history, input)
        {
            self.pending_history_entries.push_back(entry);
        }
        self.reset_composer_history_navigation();
    }

    fn recall_composer_history(&mut self, previous: bool) {
        if self.composer_history.is_empty() {
            self.status = "No composer history is retained for this workspace".to_owned();
            return;
        }
        let index = if previous {
            match self.composer_history_cursor {
                Some(index) => index.saturating_sub(1),
                None => {
                    self.composer_history_scratch = self.composer.text.clone();
                    self.composer_history.len() - 1
                }
            }
        } else {
            let Some(index) = self.composer_history_cursor else {
                return;
            };
            if index + 1 == self.composer_history.len() {
                self.composer_history_cursor = None;
                self.composer
                    .replace(std::mem::take(&mut self.composer_history_scratch));
                self.status = "Restored current draft".to_owned();
                return;
            }
            index + 1
        };
        self.composer_history_cursor = Some(index);
        self.composer.replace(self.composer_history[index].clone());
        self.status = format!(
            "Composer history {} of {}",
            index + 1,
            self.composer_history.len()
        );
    }

    fn reset_composer_history_navigation(&mut self) {
        self.composer_history_cursor = None;
        self.composer_history_scratch.clear();
    }

    pub(super) fn show_file_completions(
        &mut self,
        query: String,
        replacement: Range<usize>,
        choices: Vec<String>,
    ) {
        let current = self
            .composer
            .text
            .get(replacement.clone())
            .unwrap_or_default();
        if self.composer.cursor != replacement.end || current != format!("@{query}") {
            return;
        }
        if choices.is_empty() {
            self.status = format!("No workspace files match @{query}");
            return;
        }
        self.overlay = Some(Overlay::FileCompletion {
            query,
            replacement,
            choices,
            selected: 0,
        });
    }

    pub(super) fn fail_file_completion(&mut self, reason: String) {
        self.status = format!("Workspace file completion failed: {reason}");
    }

    pub(super) fn starting(composer_preset: ComposerPreset) -> Self {
        Self {
            connection: "loading".to_owned(),
            model: "resolving configuration".to_owned(),
            session: "not opened".to_owned(),
            status: "Starting Xana locally…".to_owned(),
            composer: Composer::new(),
            composer_history: VecDeque::new(),
            composer_history_cursor: None,
            composer_history_scratch: String::new(),
            pending_history_entries: VecDeque::new(),
            messages: VecDeque::new(),
            activity: VecDeque::from([ActivityCard::new(
                "Xana",
                "frontend",
                ActivityKind::Status,
                ActivityState::Complete,
                "local frontend ready",
                "",
            )]),
            busy: true,
            work_indicator_frame: 0,
            active_operation: None,
            pending_round_budget: None,
            followups: VecDeque::new(),
            overlay: None,
            activity_visibility: ActivityVisibility::Auto,
            auto_activity_open: false,
            composer_preset,
            scroll: 0,
            sessions: Vec::new(),
            rail_expanded: true,
            header_expanded: true,
            espejo: None,
            conversation_selection: None,
            runtime_conversation: ConversationRef::NewNative,
            viewed_conversation: ConversationRef::NewNative,
            native_usage: SessionUsage::default(),
            managed_usage: None,
            managed_usage_sequence: 0,
            semantic: SemanticSnapshotV1::default(),
            prompt_plans: Vec::new(),
            inline_image_capability: "terminal image capability has not been observed".to_owned(),
            capabilities: OwnerCapabilities::native(),
            execution_owner: ExecutionOwnerV1::Native,
            pending_images: Vec::new(),
            pending_resources: Vec::new(),
            pending_vision_route: None,
            drafts: BTreeMap::new(),
            queued: BTreeMap::new(),
            background_messages: None,
            history_start: 0,
            history_end: 0,
            history_preview: false,
            history_has_older: false,
            workspace: std::path::PathBuf::new(),
            workspace_id: String::new(),
            active_host_root: None,
        }
    }

    pub(super) fn from_client(
        client: &EmbeddedClient,
        composer_preset: ComposerPreset,
        activity_visibility: ActivityVisibility,
        conversation: ConversationRef,
    ) -> Self {
        let snapshot = client.snapshot();
        let mut messages = snapshot
            .conversation
            .iter()
            .map(message_projection)
            .collect::<VecDeque<_>>();
        window::bound(&mut messages, false);
        let mut state = Self {
            connection: snapshot.connection.clone(),
            model: snapshot.model.clone(),
            session: snapshot.session_id.to_string(),
            status: "Ready".to_owned(),
            composer: Composer::new(),
            composer_history: VecDeque::new(),
            composer_history_cursor: None,
            composer_history_scratch: String::new(),
            pending_history_entries: VecDeque::new(),
            messages,
            activity: VecDeque::new(),
            busy: snapshot.active_operation.is_some(),
            work_indicator_frame: 0,
            active_operation: snapshot.active_operation,
            pending_round_budget: None,
            followups: VecDeque::new(),
            overlay: None,
            activity_visibility,
            auto_activity_open: false,
            composer_preset,
            scroll: 0,
            sessions: Vec::new(),
            rail_expanded: true,
            header_expanded: true,
            espejo: None,
            conversation_selection: None,
            runtime_conversation: conversation.clone(),
            viewed_conversation: conversation,
            native_usage: SessionUsage::default(),
            managed_usage: None,
            managed_usage_sequence: 0,
            semantic: snapshot.semantic.clone(),
            prompt_plans: snapshot.prompt_plans.clone(),
            inline_image_capability: "terminal image capability has not been observed".to_owned(),
            capabilities: OwnerCapabilities::native(),
            execution_owner: ExecutionOwnerV1::Native,
            pending_images: Vec::new(),
            pending_resources: Vec::new(),
            pending_vision_route: None,
            drafts: BTreeMap::new(),
            queued: BTreeMap::new(),
            background_messages: None,
            history_start: 0,
            history_end: 0,
            history_preview: false,
            history_has_older: false,
            workspace: std::path::PathBuf::new(),
            workspace_id: String::new(),
            active_host_root: None,
        };
        if snapshot.conversation_truncated {
            state.push_activity("older conversation content is outside the bounded snapshot");
        }
        state
    }

    pub(super) fn from_managed(
        connection: String,
        model: String,
        session: String,
        composer_preset: ComposerPreset,
        activity_visibility: ActivityVisibility,
        conversation: ConversationRef,
    ) -> Self {
        let conversation_id = conversation.conversation_id();
        Self {
            connection,
            model,
            session,
            status: "Ready".to_owned(),
            composer: Composer::new(),
            composer_history: VecDeque::new(),
            composer_history_cursor: None,
            composer_history_scratch: String::new(),
            pending_history_entries: VecDeque::new(),
            messages: VecDeque::new(),
            activity: VecDeque::new(),
            busy: false,
            work_indicator_frame: 0,
            active_operation: None,
            pending_round_budget: None,
            followups: VecDeque::new(),
            overlay: None,
            activity_visibility,
            auto_activity_open: false,
            composer_preset,
            scroll: 0,
            sessions: Vec::new(),
            rail_expanded: true,
            header_expanded: true,
            espejo: None,
            conversation_selection: None,
            runtime_conversation: conversation.clone(),
            viewed_conversation: conversation,
            native_usage: SessionUsage::default(),
            managed_usage: None,
            managed_usage_sequence: 0,
            semantic: SemanticSnapshotV1 {
                conversation_id,
                ..SemanticSnapshotV1::default()
            },
            prompt_plans: Vec::new(),
            inline_image_capability: "terminal image capability has not been observed".to_owned(),
            capabilities: OwnerCapabilities::managed(),
            execution_owner: ExecutionOwnerV1::Managed,
            pending_images: Vec::new(),
            pending_resources: Vec::new(),
            pending_vision_route: None,
            drafts: BTreeMap::new(),
            queued: BTreeMap::new(),
            background_messages: None,
            history_start: 0,
            history_end: 0,
            history_preview: false,
            history_has_older: false,
            workspace: std::path::PathBuf::new(),
            workspace_id: String::new(),
            active_host_root: None,
        }
    }

    #[cfg(test)]
    fn with_capabilities(mut self, capabilities: OwnerCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    fn submit_text(&mut self, input: String) -> UpdateEffect {
        if input.trim().is_empty() {
            self.status = "Message cannot be blank".to_owned();
            return UpdateEffect::None;
        }
        if !self.pending_resources.is_empty() {
            self.composer.replace(input);
            self.status = format!(
                "{} typed resource(s) remain staged; this route does not advertise provider input for them. Use /artifact ID to inspect or /attach clear before sending.",
                self.pending_resources.len()
            );
            return UpdateEffect::None;
        }
        let images = self.take_pending_images();
        let vision_route = (!images.is_empty())
            .then(|| self.pending_vision_route.take())
            .flatten();
        if self.viewed_conversation != self.runtime_conversation {
            self.composer.replace(input);
            self.restore_images(images);
            self.pending_vision_route = vision_route;
            self.status = "Draft retained; return to the runtime conversation or use exact resume before submitting".to_owned();
            return UpdateEffect::None;
        }
        if let Some(operation_id) = self
            .pending_round_budget
            .as_ref()
            .map(|suspension| suspension.operation_id)
        {
            self.composer.replace(input);
            self.restore_images(images);
            self.pending_vision_route = vision_route;
            self.status = format!("Turn {} is awaiting /continue or /stop", operation_id);
            return UpdateEffect::None;
        }
        self.remember_composer_submission(&input);
        if self.busy {
            if self.followups.len() >= MAX_FOLLOWUPS
                || self
                    .queued_bytes()
                    .saturating_add(input.len())
                    .saturating_add(image_bytes(&images))
                    > MAX_FOLLOWUP_BYTES
            {
                self.composer.replace(input);
                self.restore_images(images);
                self.status = "Follow-up queue reached its 32-item/2 MiB bound".to_owned();
                return UpdateEffect::None;
            }
            self.followups.push_back(QueuedTurn {
                input,
                images,
                vision_route,
            });
            self.status = format!("Queued follow-up {}", self.followups.len());
            return UpdateEffect::None;
        }
        UpdateEffect::Submit {
            operation_id: OperationId::new(),
            input,
            images,
            vision_route,
        }
    }

    pub(super) fn mark_submitted(&mut self, operation_id: OperationId, input: String) {
        self.restore_live_tail();
        self.push_message(MessageKind::User, input);
        self.busy = true;
        self.work_indicator_frame = 0;
        self.active_operation = Some(operation_id);
        if self.execution_owner == ExecutionOwnerV1::Managed {
            self.upsert_managed_execution_facts(operation_id);
        }
        self.status = "Working…".to_owned();
        if self.activity_visibility == ActivityVisibility::Auto {
            self.auto_activity_open = false;
        }
    }

    pub(super) fn set_inline_image_capability(&mut self, capability: String) {
        self.inline_image_capability = capability;
    }

    pub(super) fn restore_submission(
        &mut self,
        input: String,
        images: Vec<ImageAttachment>,
        vision_route: Option<String>,
        reason: String,
    ) {
        if self.composer.text.is_empty() {
            self.composer.replace(input);
            self.restore_images(images);
            self.pending_vision_route = vision_route;
        } else if self.followups.len() < MAX_FOLLOWUPS {
            self.followups.push_front(QueuedTurn {
                input,
                images,
                vision_route,
            });
        }
        self.status = reason;
        self.busy = false;
        self.work_indicator_frame = 0;
        self.active_operation = None;
    }

    pub(super) fn next_followup(&mut self) -> Option<UpdateEffect> {
        if self.busy || self.pending_round_budget.is_some() {
            return None;
        }
        let turn = self.followups.pop_front()?;
        Some(UpdateEffect::Submit {
            operation_id: OperationId::new(),
            input: turn.input,
            images: turn.images,
            vision_route: turn.vision_route,
        })
    }

    pub(super) fn advance_work_indicator(&mut self) -> bool {
        if !self.busy || self.active_operation.is_none() {
            return false;
        }
        self.work_indicator_frame = (self.work_indicator_frame + 1) % 4;
        true
    }

    pub(super) fn stage_image(&mut self, attachment: ImageAttachment) {
        let _ = self.try_stage_image(attachment);
    }

    pub(super) fn stage_resource(&mut self, attachment: AttachmentV1) {
        let configured_limit = self
            .semantic
            .attachment_policy
            .configured
            .max_resources_per_turn;
        if self
            .pending_images
            .len()
            .saturating_add(self.pending_resources.len())
            >= usize::from(configured_limit)
        {
            self.status =
                format!("At most {configured_limit} resources may be staged for one turn");
            return;
        }
        if self.pending_resources.iter().any(|existing| {
            existing.resource.artifact.reference.content_hash
                == attachment.resource.artifact.reference.content_hash
        }) {
            self.status = "That resource is already staged".to_owned();
            return;
        }
        let kind = attachment.resource.kind.code().to_owned();
        let id = attachment.resource.artifact.reference.id;
        self.pending_resources.push(attachment);
        self.status =
            format!("Staged {kind} artifact {id}; provider input is unavailable on this route");
    }

    pub(super) fn request_external_resource_approval(&mut self, path: String) {
        self.overlay = Some(Overlay::ExternalResourceApproval { path, selected: 0 });
        self.status = "Approve reading this resource outside the launch workspace?".to_owned();
    }

    fn try_stage_image(&mut self, attachment: ImageAttachment) -> bool {
        if self.pending_images.len() >= MAX_IMAGES_PER_TURN {
            self.status = "At most 8 images may be staged for one turn".to_owned();
            return false;
        }
        let total = self
            .pending_images
            .iter()
            .map(|attachment| attachment.image.byte_len)
            .sum::<u64>()
            .saturating_add(attachment.image.byte_len);
        if total > MAX_IMAGE_BYTES_PER_TURN {
            self.status = "Image attachments exceed the 20 MiB per-turn budget".to_owned();
            return false;
        }
        let source = attachment.source_path.clone();
        self.pending_images.push(attachment);
        self.status = format!(
            "Staged image {source} ({} pending)",
            self.pending_images.len()
        );
        true
    }

    pub(super) fn attach_and_submit(
        &mut self,
        input: String,
        attachments: Vec<ImageAttachment>,
    ) -> UpdateEffect {
        let mut unique = Vec::new();
        for attachment in attachments {
            let content_hash = &attachment.image.artifact.reference.content_hash;
            if self
                .pending_images
                .iter()
                .chain(unique.iter())
                .any(|existing: &ImageAttachment| {
                    &existing.image.artifact.reference.content_hash == content_hash
                })
            {
                continue;
            }
            unique.push(attachment);
        }
        if self.pending_images.len().saturating_add(unique.len()) > MAX_IMAGES_PER_TURN {
            self.status = "At most 8 images may be staged for one turn".to_owned();
            self.composer.replace(input);
            return UpdateEffect::None;
        }
        let total = self
            .pending_images
            .iter()
            .chain(unique.iter())
            .map(|attachment| attachment.image.byte_len)
            .sum::<u64>();
        if total > MAX_IMAGE_BYTES_PER_TURN {
            self.status = "Image attachments exceed the 20 MiB per-turn budget".to_owned();
            self.composer.replace(input);
            return UpdateEffect::None;
        }
        self.pending_images.extend(unique);
        self.submit_text(input)
    }

    pub(super) fn submit_without_auto_attachment(&mut self, input: String) -> UpdateEffect {
        self.submit_text(input)
    }

    pub(super) fn restore_auto_attachment_draft(&mut self, input: String, reason: String) {
        self.composer.replace(input);
        self.status = reason;
    }

    pub(super) fn request_external_image_approval(
        &mut self,
        operation_id: OperationId,
        input: String,
        paths: Vec<String>,
        external_paths: Vec<String>,
    ) {
        self.overlay = Some(Overlay::ExternalImageApproval {
            operation_id,
            input,
            paths,
            external_paths: external_paths.clone(),
            selected: 0,
        });
        self.status = format!(
            "Approve reading {} external image(s)?",
            external_paths.len()
        );
    }

    pub(super) fn request_vision_approval(
        &mut self,
        operation_id: OperationId,
        input: String,
        images: Vec<ImageAttachment>,
        plan: crate::app::vision::VisionPlan,
    ) {
        self.status = plan.preview(images.len());
        self.overlay = Some(Overlay::VisionApproval {
            operation_id,
            input,
            images,
            plan: Box::new(plan),
            selected: 0,
        });
    }

    pub(super) fn set_vision_route(&mut self, route: Option<String>) {
        self.pending_vision_route = route.clone();
        self.status = match route {
            Some(route) => format!("Vision specialist {route:?} selected for the next image turn"),
            None => {
                "Native vision preferred; a default specialist is used only when needed".to_owned()
            }
        };
    }

    pub(super) fn finish_vision_preparation(
        &mut self,
        receipt: &crate::app::vision::VisionReceipt,
    ) {
        self.push_activity(format!(
            "vision derivative ready via {}/{} from {} source artifact(s); usage {}; cost {}",
            receipt.connection,
            receipt.model,
            receipt.source_artifact_ids.len(),
            if receipt.usage_available {
                "reported"
            } else {
                "unavailable"
            },
            if receipt.cost_available {
                "reported"
            } else {
                "unavailable"
            }
        ));
        self.status = format!(
            "Vision derivative ready via route {}; continuing the same turn",
            receipt.route
        );
    }

    pub(super) fn fail_vision_preparation(&mut self, reason: String) {
        self.busy = false;
        self.work_indicator_frame = 0;
        self.active_operation = None;
        self.status = reason;
    }

    pub(super) fn pending_image_count(&self) -> usize {
        self.pending_images.len()
    }

    pub(super) fn pending_resource_count(&self) -> usize {
        self.pending_resources.len()
    }

    pub(super) fn clear_pending_attachments(&mut self) -> usize {
        let count = self.pending_images.len() + self.pending_resources.len();
        self.pending_images.clear();
        self.pending_resources.clear();
        self.pending_vision_route = None;
        count
    }

    pub(super) fn pending_attachment_summary(&self) -> String {
        let mut lines = self
            .pending_images
            .iter()
            .map(|attachment| {
                format!(
                    "{} · image · {} · {} bytes · provider input pending validation",
                    attachment.image.artifact.reference.id,
                    attachment.image.media_type,
                    attachment.image.byte_len
                )
            })
            .collect::<Vec<_>>();
        lines.extend(self.pending_resources.iter().map(|attachment| {
            let detected = attachment
                .resource
                .media_type
                .detected
                .as_deref()
                .unwrap_or("unknown");
            format!(
                "{} · {} · {} · {} bytes · {:?}",
                attachment.resource.artifact.reference.id,
                attachment.resource.kind.code(),
                detected,
                attachment.resource.artifact.byte_len,
                attachment.resource.validation,
            )
        }));
        if lines.is_empty() {
            "No resources are staged for the next turn.".to_owned()
        } else {
            lines.join("\n")
        }
    }

    pub(super) fn open_model_picker(&mut self, choices: Vec<String>) {
        if choices.is_empty() {
            self.status = "No models are available in configured/cached catalogs".to_owned();
        } else {
            let selected_model = format!("{}/{}", self.connection, self.model);
            let selected = choices
                .iter()
                .position(|choice| choice.split_whitespace().next() == Some(&selected_model))
                .unwrap_or(0);
            self.overlay = Some(Overlay::ModelPicker { choices, selected });
        }
    }

    pub(super) fn open_reasoning_picker(&mut self, choices: Vec<String>) {
        if choices.is_empty() {
            self.status = "The selected model advertises no reasoning choices".to_owned();
        } else {
            self.overlay = Some(Overlay::ReasoningPicker {
                choices,
                selected: 0,
            });
        }
    }

    pub(super) fn refresh_sessions(&mut self, snapshot: WorkspaceSnapshot) {
        self.workspace = snapshot.workspace.clone();
        self.workspace_id.clone_from(&snapshot.workspace_id);
        self.active_host_root = snapshot
            .active
            .as_ref()
            .map(|active| (active.conversation.clone(), active.process_id()));
        let previous = std::mem::take(&mut self.sessions);
        self.sessions = session::project(
            snapshot,
            &self.runtime_conversation,
            &self.connection,
            &self.model,
        );
        for row in &mut self.sessions {
            if let Some(old) = previous
                .iter()
                .find(|old| old.conversation == row.conversation)
            {
                row.unread = old.unread;
                row.error = old.error;
            }
        }
    }

    pub(super) fn open_session_picker(&mut self) {
        if self.sessions.is_empty() {
            self.status = "No retained conversations are available".to_owned();
        } else {
            self.overlay = Some(Overlay::SessionPicker {
                query: String::new(),
                choices: self.sessions.clone(),
                selected: 0,
            });
        }
    }

    #[cfg(test)]
    pub(super) fn view_session(
        &mut self,
        conversation: ConversationRef,
        history: Option<Vec<Message>>,
    ) {
        let page = history.map(|messages| crate::session::ConversationPage {
            start: 0,
            total: messages.len(),
            has_older: false,
            messages,
        });
        self.view_session_page(conversation, page);
    }

    pub(super) fn view_session_page(
        &mut self,
        conversation: ConversationRef,
        page: Option<crate::session::ConversationPage>,
    ) {
        if conversation != self.viewed_conversation {
            self.save_viewed_draft();
            self.restore_draft(&conversation);
        }
        let history = page.as_ref().map(|page| page.messages.as_slice());
        if conversation == self.runtime_conversation {
            if let Some(messages) = self.background_messages.take() {
                self.messages = messages;
            }
            self.history_preview = false;
            self.viewed_conversation = conversation.clone();
            self.status = "Viewing the runtime conversation".to_owned();
        } else {
            if self.viewed_conversation == self.runtime_conversation
                && self.background_messages.is_none()
            {
                self.background_messages = Some(std::mem::take(&mut self.messages));
            }
            self.messages = history.map_or_else(
                || {
                    VecDeque::from([VisibleMessage {
                        kind: MessageKind::System,
                        text: "Managed transcript remains owned by its runtime and is unavailable to this local history viewer".to_owned(),
                        document: RichDocument::plain(
                            "Managed transcript remains owned by its runtime and is unavailable to this local history viewer",
                        ),
                    }])
                },
                |history| {
                    history
                        .iter()
                        .map(message_projection)
                        .collect::<VecDeque<_>>()
                },
            );
            self.viewed_conversation = conversation.clone();
            self.history_preview = page.is_some();
            self.status = if self.busy {
                "Inspecting another conversation; the active root remains controlled in its original conversation".to_owned()
            } else {
                "Inspecting retained history; use exact resume to continue it".to_owned()
            };
        }
        if conversation == self.runtime_conversation {
            self.seed_saved_history_cursor(
                page.as_ref().map_or(self.messages.len(), |page| page.total),
            );
        } else {
            self.history_start = page.as_ref().map_or(0, |page| page.start);
            self.history_has_older = page.as_ref().is_some_and(|page| page.has_older);
            self.history_end = self.history_start.saturating_add(self.messages.len());
        }
        self.bound_tail_window();
        self.scroll = 0;
        if let Some(row) = self
            .sessions
            .iter_mut()
            .find(|row| row.conversation == conversation)
        {
            row.unread = false;
            row.error = false;
            row.title = session::preview_title(history.unwrap_or_default(), &row.title);
        }
    }

    pub(super) fn prepend_history_page(&mut self, page: crate::session::ConversationPage) {
        if self.viewed_conversation == self.runtime_conversation && !self.history_preview {
            self.background_messages = Some(self.messages.clone());
        }
        self.history_preview = true;
        self.history_end = self
            .history_end
            .max(self.history_start.saturating_add(self.messages.len()));
        let added = page.messages.len();
        let mut older = page
            .messages
            .iter()
            .map(message_projection)
            .collect::<VecDeque<_>>();
        older.append(&mut self.messages);
        // Retain the requested older page, not the newest tail it replaces.
        window::bound(&mut older, true);
        self.messages = older;
        self.conversation_selection = None;
        self.scroll = u16::MAX;
        self.history_start = page.start;
        self.history_has_older = page.has_older;
        self.status = format!(
            "Loaded {} older message(s); {} remain outside the viewport",
            added, self.history_start
        );
    }

    pub(super) fn archived_conversation(&mut self, conversation: &ConversationRef) {
        self.sessions
            .retain(|row| &row.conversation != conversation);
        if &self.viewed_conversation == conversation {
            self.view_session_page(self.runtime_conversation.clone(), None);
        }
        self.status =
            "Managed conversation handle archived locally; the vendor thread was not deleted"
                .to_owned();
    }

    pub(super) fn seed_saved_history_cursor(&mut self, total: usize) {
        self.history_start = total.saturating_sub(self.messages.len());
        self.history_end = total;
        self.history_has_older = self.history_start > 0;
    }

    pub(super) fn history_before(&self) -> Option<usize> {
        self.history_has_older.then_some(self.history_start)
    }

    pub(super) fn history_newer_start(&self) -> Option<usize> {
        let end = self.history_start.saturating_add(self.messages.len());
        (self.history_preview && end < self.history_end).then_some(end)
    }

    pub(super) fn replace_newer_page(&mut self, page: crate::session::ConversationPage) {
        self.messages = page.messages.iter().map(message_projection).collect();
        self.history_start = page.start;
        self.history_end = page.total;
        self.history_has_older = page.has_older;
        window::bound(&mut self.messages, true);
        self.conversation_selection = None;
        self.scroll = u16::MAX;
        self.status = "Loaded newer saved history".to_owned();
    }

    pub(super) fn attach_conversation(&mut self, conversation: ConversationRef) -> UpdateEffect {
        if conversation == self.runtime_conversation {
            if conversation == self.viewed_conversation {
                self.status = "This TUI is already attached to that Conversation".to_owned();
                return UpdateEffect::None;
            }
            return UpdateEffect::ViewSession(conversation);
        }
        if self.busy || self.active_operation.is_some() || !self.followups.is_empty() {
            self.status = "Finish or interrupt the attached Run and drain its queued input before switching Conversations".to_owned();
            return UpdateEffect::None;
        }
        let Some(row) = self
            .sessions
            .iter()
            .find(|row| row.conversation == conversation)
        else {
            self.status = "That Conversation is no longer available in this workspace".to_owned();
            return UpdateEffect::None;
        };
        match row.state {
            crate::workspace_host::ConversationState::Inactive => {
                self.status = format!("Attaching to {}…", row.title);
                UpdateEffect::SwitchConversation(conversation)
            }
            crate::workspace_host::ConversationState::Active
            | crate::workspace_host::ConversationState::Controlled
            | crate::workspace_host::ConversationState::Observable => {
                self.status = format!(
                    "{} is controlled by another active root; preview remains available until that Run stops",
                    row.title
                );
                UpdateEffect::None
            }
            crate::workspace_host::ConversationState::Unavailable => {
                self.status = format!(
                    "{} is unavailable; refresh the Conversation list or inspect Diagnostics",
                    row.title
                );
                UpdateEffect::None
            }
        }
    }

    fn conversation_row_estimate(&self) -> usize {
        self.messages
            .iter()
            .fold(0usize, |total, message| {
                total.saturating_add(message_row_estimate(message))
            })
            .saturating_add(if self.busy && self.active_operation.is_some() {
                2
            } else {
                0
            })
    }

    pub(super) fn set_rail_expanded(&mut self, expanded: bool) {
        self.rail_expanded = expanded;
    }

    pub(super) fn set_status(&mut self, status: impl Into<String>) {
        self.status = bounded(status.into(), MAX_ACTIVITY_BYTES);
    }

    pub(super) fn palette_entries(&self) -> Vec<CommandSpec> {
        match &self.overlay {
            Some(Overlay::Palette { query, .. }) => command::search(query),
            _ => Vec::new(),
        }
    }

    fn take_pending_images(&mut self) -> Vec<ImageAttachment> {
        self.pending_images.drain(..).collect()
    }

    fn restore_images(&mut self, images: Vec<ImageAttachment>) {
        self.pending_images.extend(images);
    }

    fn queued_bytes(&self) -> usize {
        self.followups
            .iter()
            .map(|turn| turn.input.len().saturating_add(image_bytes(&turn.images)))
            .sum()
    }

    fn save_viewed_draft(&mut self) {
        let draft = ConversationDraft {
            composer: std::mem::replace(&mut self.composer, Composer::new()),
            images: std::mem::take(&mut self.pending_images),
            resources: std::mem::take(&mut self.pending_resources),
            vision_route: self.pending_vision_route.take(),
        };
        self.drafts.insert(self.viewed_conversation.clone(), draft);
    }

    fn restore_draft(&mut self, conversation: &ConversationRef) {
        let draft = self.drafts.remove(conversation).unwrap_or_default();
        self.composer = draft.composer;
        self.pending_images = draft.images;
        self.pending_resources = draft.resources;
        self.pending_vision_route = draft.vision_route;
    }

    pub(crate) fn restore_continuation(&mut self, continuation: TuiContinuation) {
        self.drafts = continuation.drafts;
        self.queued = continuation.queued;
        let viewed = self.viewed_conversation.clone();
        self.restore_draft(&viewed);
        self.followups = self
            .queued
            .remove(&self.runtime_conversation)
            .unwrap_or_default();
    }

    pub(crate) fn into_continuation(mut self) -> TuiContinuation {
        self.save_viewed_draft();
        if !self.followups.is_empty() {
            self.queued
                .insert(self.runtime_conversation.clone(), self.followups);
        }
        TuiContinuation {
            drafts: self.drafts,
            queued: self.queued,
        }
    }
}

fn message_row_estimate(message: &VisibleMessage) -> usize {
    2usize
        .saturating_add(message.document.lines.len())
        .saturating_add(message.document.links.len())
        .saturating_add(message.document.artifacts.len())
        .saturating_add(usize::from(message.document.truncated))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalChoice {
    Once,
    Session,
    SaveAllow,
    SaveDeny,
    Deny,
}

fn approval_choices(prompt: &ApprovalPrompt) -> Vec<ApprovalChoice> {
    let mut choices = Vec::with_capacity(3);
    if prompt.allow_once {
        choices.push(ApprovalChoice::Once);
    }
    if prompt.allow_session {
        choices.push(ApprovalChoice::Session);
    }
    if prompt.save_allow {
        choices.push(ApprovalChoice::SaveAllow);
    }
    if prompt.save_deny {
        choices.push(ApprovalChoice::SaveDeny);
    }
    if prompt.deny {
        choices.push(ApprovalChoice::Deny);
    }
    choices
}

fn approval_choice_count(prompt: &ApprovalPrompt) -> usize {
    approval_choices(prompt).len()
}

fn controller_decision(
    choice: ApprovalChoice,
    scope: crate::permission::PermissionScope,
) -> ControllerDecision {
    match choice {
        ApprovalChoice::Once => ControllerDecision::AllowOnce,
        ApprovalChoice::Session => ControllerDecision::AllowSession { scope },
        ApprovalChoice::SaveAllow => ControllerDecision::SaveOutboundAllow,
        ApprovalChoice::SaveDeny => ControllerDecision::SaveOutboundDeny,
        ApprovalChoice::Deny => ControllerDecision::Deny,
    }
}

fn session_matches(row: &SessionRow, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty()
        || row.title.to_ascii_lowercase().contains(&query)
        || row.connection.to_ascii_lowercase().contains(&query)
        || row.model.to_ascii_lowercase().contains(&query)
        || row.execution_owner.contains(&query)
        || row.state.to_string().contains(&query)
}

fn image_bytes(images: &[ImageAttachment]) -> usize {
    images.iter().fold(0_usize, |total, image| {
        total.saturating_add(usize::try_from(image.image.byte_len).unwrap_or(usize::MAX))
    })
}

fn message_projection(message: &Message) -> VisibleMessage {
    let kind = match message.role {
        Role::User => MessageKind::User,
        Role::Assistant => MessageKind::Assistant,
        Role::Tool => MessageKind::Tool,
        Role::System => MessageKind::System,
    };
    let mut text = String::new();
    let mut artifacts = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text(value) => append_bounded(&mut text, value, MAX_MESSAGE_BYTES),
            ContentBlock::Image(image) => append_bounded(
                &mut text,
                &format!(
                    "[image artifact: {} · {} · {} bytes]",
                    image.artifact.reference.id, image.media_type, image.byte_len
                ),
                MAX_MESSAGE_BYTES,
            ),
            ContentBlock::ToolCall(call) => append_bounded(
                &mut text,
                &format!("[tool call: {}]", call.name),
                MAX_MESSAGE_BYTES,
            ),
            ContentBlock::ToolResult(result) => {
                append_bounded(&mut text, &result.output, MAX_MESSAGE_BYTES)
            }
        }
    }
    for block in &message.content {
        if let ContentBlock::Image(image) = block {
            artifacts.push(ArtifactView {
                record: image.artifact.clone(),
                label: format!("image · {} · {} bytes", image.media_type, image.byte_len),
                details: Vec::new(),
            });
        }
    }
    let parts = normalize_message(message);
    let document = if parts.is_empty() {
        RichDocument::parse(&text, artifacts)
    } else {
        RichDocument::from_parts(&parts)
    };
    VisibleMessage {
        kind,
        text,
        document,
    }
}

fn bounded(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

pub(super) fn append_bounded(target: &mut String, value: &str, limit: usize) {
    if target.len() >= limit {
        return;
    }
    let remaining = limit - target.len();
    if value.len() <= remaining {
        target.push_str(value);
        return;
    }
    let mut boundary = remaining.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    target.push_str(&value[..boundary]);
    target.push_str(&"..."[..remaining.min(3)]);
}

fn trim_front<T>(values: &mut VecDeque<T>, limit: usize) {
    while values.len() > limit {
        values.pop_front();
    }
}

#[cfg(test)]
mod tests;
