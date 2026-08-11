//! Terminal-independent state and update policy for the full-screen client.

mod commands;
mod projection;

pub(super) use super::composer::MoveDirection;
use super::composer::{Composer, MAX_INPUT_BYTES, sanitize_input};
use super::session::{self, SessionRow};
use super::{
    activity::{self, ActivityCard, ActivityKind, ActivityState, ApprovalPrompt, ApprovalTarget},
    command::{self, CommandId, CommandSpec, ParsedCommand},
    rich_text::{ArtifactView, RichDocument},
};
use crate::{
    frontend::{EmbeddedClient, ManagedClientEvent},
    identity::{AgentId, OperationId, ToolInvocationId},
    message::{ContentBlock, Message, Role},
    native_runtime::{AgentEvent, OperationState},
    permission::ControllerDecision,
    presentation::{ActivityPaneChoice, ComposerPreset},
    vision::ImageAttachment,
    workspace_host::{ConversationRef, WorkspaceSnapshot},
};
use std::collections::VecDeque;

const MAX_VISIBLE_MESSAGES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_ACTIVITY: usize = 256;
const MAX_ACTIVITY_BYTES: usize = 16 * 1024;
const MAX_FOLLOWUPS: usize = 32;
const MAX_FOLLOWUP_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGES: usize = 8;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

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
}

impl OwnerCapabilities {
    pub(super) const fn native() -> Self {
        Self {
            interrupt: true,
            steer: false,
            model: true,
            reasoning: false,
        }
    }

    const fn managed() -> Self {
        Self {
            interrupt: true,
            steer: false,
            model: true,
            reasoning: true,
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
    OpenPalette,
    PaletteUp,
    PaletteDown,
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
    Submit {
        operation_id: OperationId,
        input: String,
        images: Vec<ImageAttachment>,
    },
    Interrupt {
        operation_id: OperationId,
    },
    Steer {
        operation_id: OperationId,
        input: String,
    },
    Attach(String),
    SelectModel(String),
    SetReasoning(String),
    PersistComposer(ComposerPreset),
    ClearConversation,
    NewConversation,
    OpenModelPicker,
    OpenReasoningPicker,
    OpenSessionPicker,
    ViewSession(ConversationRef),
    LoadOlder(ConversationRef),
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
    InsertReference,
    Reveal,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueuedTurn {
    pub(super) input: String,
    images: Vec<ImageAttachment>,
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
    Artifact {
        artifact: Box<ArtifactView>,
        selected: usize,
        preview: Option<String>,
    },
}

pub(super) struct TuiState {
    pub(super) connection: String,
    pub(super) model: String,
    pub(super) session: String,
    pub(super) status: String,
    pub(super) composer: Composer,
    pub(super) messages: VecDeque<VisibleMessage>,
    pub(super) activity: VecDeque<ActivityCard>,
    pub(super) busy: bool,
    pub(super) active_operation: Option<OperationId>,
    pub(super) followups: VecDeque<QueuedTurn>,
    pub(super) overlay: Option<Overlay>,
    pub(super) activity_visibility: ActivityVisibility,
    pub(super) auto_activity_open: bool,
    pub(super) composer_preset: ComposerPreset,
    pub(super) scroll: u16,
    pub(super) sessions: Vec<SessionRow>,
    pub(super) rail_expanded: bool,
    pub(super) header_expanded: bool,
    pub(super) conversation_selection: Option<ConversationSelection>,
    pub(super) runtime_conversation: ConversationRef,
    pub(super) viewed_conversation: ConversationRef,
    capabilities: OwnerCapabilities,
    pending_images: Vec<ImageAttachment>,
    background_messages: Option<VecDeque<VisibleMessage>>,
    history_start: usize,
    history_has_older: bool,
}

impl TuiState {
    pub(super) fn starting(composer_preset: ComposerPreset) -> Self {
        Self {
            connection: "loading".to_owned(),
            model: "resolving configuration".to_owned(),
            session: "not opened".to_owned(),
            status: "Starting Xana locally…".to_owned(),
            composer: Composer::new(),
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
            active_operation: None,
            followups: VecDeque::new(),
            overlay: None,
            activity_visibility: ActivityVisibility::Auto,
            auto_activity_open: false,
            composer_preset,
            scroll: 0,
            sessions: Vec::new(),
            rail_expanded: true,
            header_expanded: true,
            conversation_selection: None,
            runtime_conversation: ConversationRef::NewNative,
            viewed_conversation: ConversationRef::NewNative,
            capabilities: OwnerCapabilities::native(),
            pending_images: Vec::new(),
            background_messages: None,
            history_start: 0,
            history_has_older: false,
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
        trim_front(&mut messages, MAX_VISIBLE_MESSAGES);
        let mut state = Self {
            connection: snapshot.connection.clone(),
            model: snapshot.model.clone(),
            session: snapshot.session_id.to_string(),
            status: "Ready".to_owned(),
            composer: Composer::new(),
            messages,
            activity: VecDeque::new(),
            busy: snapshot.active_operation.is_some(),
            active_operation: snapshot.active_operation,
            followups: VecDeque::new(),
            overlay: None,
            activity_visibility,
            auto_activity_open: false,
            composer_preset,
            scroll: 0,
            sessions: Vec::new(),
            rail_expanded: true,
            header_expanded: true,
            conversation_selection: None,
            runtime_conversation: conversation.clone(),
            viewed_conversation: conversation,
            capabilities: OwnerCapabilities::native(),
            pending_images: Vec::new(),
            background_messages: None,
            history_start: 0,
            history_has_older: false,
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
        Self {
            connection,
            model,
            session,
            status: "Ready".to_owned(),
            composer: Composer::new(),
            messages: VecDeque::new(),
            activity: VecDeque::new(),
            busy: false,
            active_operation: None,
            followups: VecDeque::new(),
            overlay: None,
            activity_visibility,
            auto_activity_open: false,
            composer_preset,
            scroll: 0,
            sessions: Vec::new(),
            rail_expanded: true,
            header_expanded: true,
            conversation_selection: None,
            runtime_conversation: conversation.clone(),
            viewed_conversation: conversation,
            capabilities: OwnerCapabilities::managed(),
            pending_images: Vec::new(),
            background_messages: None,
            history_start: 0,
            history_has_older: false,
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
        let images = self.take_pending_images();
        if self.viewed_conversation != self.runtime_conversation {
            self.composer.replace(input);
            self.restore_images(images);
            self.status = "Draft retained; return to the runtime conversation or use exact resume before submitting".to_owned();
            return UpdateEffect::None;
        }
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
            self.followups.push_back(QueuedTurn { input, images });
            self.status = format!("Queued follow-up {}", self.followups.len());
            return UpdateEffect::None;
        }
        UpdateEffect::Submit {
            operation_id: OperationId::new(),
            input,
            images,
        }
    }

    pub(super) fn mark_submitted(&mut self, operation_id: OperationId, input: String) {
        self.push_message(MessageKind::User, input);
        self.busy = true;
        self.active_operation = Some(operation_id);
        self.status = "Working…".to_owned();
        if self.activity_visibility == ActivityVisibility::Auto {
            self.auto_activity_open = false;
        }
    }

    pub(super) fn restore_submission(
        &mut self,
        input: String,
        images: Vec<ImageAttachment>,
        reason: String,
    ) {
        if self.composer.text.is_empty() {
            self.composer.replace(input);
            self.restore_images(images);
        } else if self.followups.len() < MAX_FOLLOWUPS {
            self.followups.push_front(QueuedTurn { input, images });
        }
        self.status = reason;
        self.busy = false;
        self.active_operation = None;
    }

    pub(super) fn next_followup(&mut self) -> Option<UpdateEffect> {
        if self.busy {
            return None;
        }
        let turn = self.followups.pop_front()?;
        Some(UpdateEffect::Submit {
            operation_id: OperationId::new(),
            input: turn.input,
            images: turn.images,
        })
    }

    pub(super) fn stage_image(&mut self, attachment: ImageAttachment) {
        if self.pending_images.len() >= MAX_IMAGES {
            self.status = "At most 8 images may be staged for one turn".to_owned();
            return;
        }
        let total = self
            .pending_images
            .iter()
            .map(|attachment| attachment.image.byte_len)
            .sum::<u64>()
            .saturating_add(attachment.image.byte_len);
        if total > MAX_IMAGE_BYTES {
            self.status = "Image attachments exceed the 20 MiB per-turn budget".to_owned();
            return;
        }
        let source = attachment.source_path.clone();
        self.pending_images.push(attachment);
        self.status = format!(
            "Staged image {source} ({} pending)",
            self.pending_images.len()
        );
    }

    pub(super) fn pending_image_count(&self) -> usize {
        self.pending_images.len()
    }

    pub(super) fn open_model_picker(&mut self, choices: Vec<String>) {
        if choices.is_empty() {
            self.status = "No models are available in configured/cached catalogs".to_owned();
        } else {
            self.overlay = Some(Overlay::ModelPicker {
                choices,
                selected: 0,
            });
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
        let history = page.as_ref().map(|page| page.messages.as_slice());
        if conversation == self.runtime_conversation {
            if self.viewed_conversation != self.runtime_conversation
                && let Some(messages) = self.background_messages.take()
            {
                self.messages = messages;
            }
            self.viewed_conversation = conversation.clone();
            self.status = "Viewing the runtime conversation".to_owned();
        } else {
            if self.viewed_conversation == self.runtime_conversation {
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
                    let mut messages = history
                        .iter()
                        .map(message_projection)
                        .collect::<VecDeque<_>>();
                    trim_front(&mut messages, MAX_VISIBLE_MESSAGES);
                    messages
                },
            );
            self.viewed_conversation = conversation.clone();
            self.status = if self.busy {
                "Inspecting another conversation; the active root remains controlled in its original conversation".to_owned()
            } else {
                "Inspecting retained history; use exact resume to continue it".to_owned()
            };
        }
        self.history_start = page.as_ref().map_or(0, |page| page.start);
        self.history_has_older = page.as_ref().is_some_and(|page| page.has_older);
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
        let added = page.messages.len();
        let mut older = page
            .messages
            .iter()
            .map(message_projection)
            .collect::<VecDeque<_>>();
        older.append(&mut self.messages);
        trim_front(&mut older, MAX_VISIBLE_MESSAGES);
        self.messages = older;
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

    pub(super) fn history_before(&self) -> Option<usize> {
        self.history_has_older.then_some(self.history_start)
    }

    fn conversation_row_estimate(&self) -> usize {
        self.messages.iter().fold(0usize, |total, message| {
            total.saturating_add(message_row_estimate(message))
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
            });
        }
    }
    let document = RichDocument::parse(&text, artifacts);
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

fn append_bounded(target: &mut String, value: &str, limit: usize) {
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
    target.push_str("...");
}

fn trim_front<T>(values: &mut VecDeque<T>, limit: usize) {
    while values.len() > limit {
        values.pop_front();
    }
}

#[cfg(test)]
mod tests;
