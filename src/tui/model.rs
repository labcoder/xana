//! Terminal-independent state and update policy for the full-screen client.

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
    permission::ControllerDecision,
    presentation::{ActivityPaneChoice, ComposerPreset},
    runtime::{AgentEvent, OperationState},
    vision::ImageAttachment,
    workspace_host::{ConversationRef, WorkspaceSnapshot},
};
use std::{collections::VecDeque, ops::Range};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MoveDirection {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
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
    Interrupt,
    Scroll(i16),
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
    OpenModelPicker,
    OpenReasoningPicker,
    OpenSessionPicker,
    ViewSession(ConversationRef),
    LoadOlder(ConversationRef),
    PersistRail(bool),
    PersistActivity(ActivityPaneChoice),
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
pub(super) struct Composer {
    pub(super) text: String,
    pub(super) cursor: usize,
    anchor: Option<usize>,
}

impl Composer {
    fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            anchor: None,
        }
    }

    pub(super) fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    fn insert(&mut self, value: &str) -> Result<(), String> {
        if value.len() > MAX_INPUT_BYTES {
            return Err("composer input reached the 1 MiB limit".to_owned());
        }
        let value = sanitize_input(value);
        let removed = self.selection().map_or(0, |range| range.len());
        if self
            .text
            .len()
            .saturating_sub(removed)
            .saturating_add(value.len())
            > MAX_INPUT_BYTES
        {
            return Err("composer input reached the 1 MiB limit".to_owned());
        }
        self.delete_selection();
        self.text.insert_str(self.cursor, &value);
        self.cursor += value.len();
        Ok(())
    }

    fn move_cursor(&mut self, direction: MoveDirection, select: bool) {
        let old = self.cursor;
        self.cursor = match direction {
            MoveDirection::Left => previous_boundary(&self.text, self.cursor),
            MoveDirection::Right => next_boundary(&self.text, self.cursor),
            MoveDirection::Home => line_start(&self.text, self.cursor),
            MoveDirection::End => line_end(&self.text, self.cursor),
            MoveDirection::Up => vertical_move(&self.text, self.cursor, false),
            MoveDirection::Down => vertical_move(&self.text, self.cursor, true),
        };
        if select {
            self.anchor.get_or_insert(old);
        } else {
            self.anchor = None;
        }
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let previous = previous_boundary(&self.text, self.cursor);
        if previous != self.cursor {
            self.text.drain(previous..self.cursor);
            self.cursor = previous;
        }
    }

    fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let next = next_boundary(&self.text, self.cursor);
        if next != self.cursor {
            self.text.drain(self.cursor..next);
        }
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.text.drain(range.clone());
        self.cursor = range.start;
        self.anchor = None;
        true
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        self.anchor = None;
        std::mem::take(&mut self.text)
    }

    fn replace(&mut self, value: String) {
        self.text = bounded(sanitize_input(&value), MAX_INPUT_BYTES);
        self.cursor = self.text.len();
        self.anchor = None;
    }
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
            messages: VecDeque::from([VisibleMessage {
                kind: MessageKind::System,
                text: "Xana is preparing the workspace runtime. The interface is ready.".to_owned(),
                document: RichDocument::plain(
                    "Xana is preparing the workspace runtime. The interface is ready.",
                ),
            }]),
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
            messages: VecDeque::from([VisibleMessage {
                kind: MessageKind::System,
                text: "Codex owns this managed thread and inner loop; Xana projects its emitted activity and approvals.".to_owned(),
                document: RichDocument::plain(
                    "Codex owns this managed thread and inner loop; Xana projects its emitted activity and approvals.",
                ),
            }]),
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

    pub(super) fn update_input(&mut self, action: InputAction) -> UpdateEffect {
        if self.overlay.is_some() {
            return self.update_overlay(action);
        }
        match action {
            InputAction::Insert(text) => {
                if let Err(reason) = self.composer.insert(&text) {
                    self.status = reason;
                }
                UpdateEffect::None
            }
            InputAction::Paste(text) => {
                let text = bounded(sanitize_input(&text), MAX_INPUT_BYTES);
                if text.is_empty() {
                    self.status = "Paste contained no displayable text".to_owned();
                } else {
                    self.overlay = Some(Overlay::PastePreview { text });
                }
                UpdateEffect::None
            }
            InputAction::Move { direction, select } => {
                self.composer.move_cursor(direction, select);
                UpdateEffect::None
            }
            InputAction::Backspace => {
                self.composer.backspace();
                UpdateEffect::None
            }
            InputAction::Delete => {
                self.composer.delete();
                UpdateEffect::None
            }
            InputAction::Submit => self.submit_composer(),
            InputAction::Newline => {
                if let Err(reason) = self.composer.insert("\n") {
                    self.status = reason;
                }
                UpdateEffect::None
            }
            InputAction::OpenPalette => {
                self.overlay = Some(Overlay::Palette {
                    query: String::new(),
                    selected: 0,
                });
                UpdateEffect::None
            }
            InputAction::Interrupt => self.interrupt(),
            InputAction::Scroll(delta) => {
                self.scroll = if delta.is_negative() {
                    self.scroll.saturating_add(delta.unsigned_abs())
                } else {
                    self.scroll.saturating_sub(delta as u16)
                };
                if delta.is_negative()
                    && self.history_has_older
                    && usize::from(self.scroll) >= self.messages.len().saturating_sub(1)
                {
                    UpdateEffect::LoadOlder(self.viewed_conversation.clone())
                } else {
                    UpdateEffect::None
                }
            }
            InputAction::Cancel => {
                self.composer.anchor = None;
                UpdateEffect::None
            }
            InputAction::Quit => UpdateEffect::Quit,
            InputAction::PaletteUp | InputAction::PaletteDown | InputAction::Confirm => {
                UpdateEffect::None
            }
        }
    }

    fn update_overlay(&mut self, action: InputAction) -> UpdateEffect {
        match action {
            InputAction::Cancel => {
                self.overlay = None;
                UpdateEffect::None
            }
            InputAction::Insert(text) => {
                match &mut self.overlay {
                    Some(Overlay::Palette { query, selected })
                    | Some(Overlay::SessionPicker {
                        query, selected, ..
                    }) => {
                        append_bounded(query, &sanitize_input(&text), 256);
                        *selected = 0;
                    }
                    _ => {}
                }
                UpdateEffect::None
            }
            InputAction::Backspace => {
                match &mut self.overlay {
                    Some(Overlay::Palette { query, selected })
                    | Some(Overlay::SessionPicker {
                        query, selected, ..
                    }) => {
                        query.pop();
                        *selected = 0;
                    }
                    _ => {}
                }
                UpdateEffect::None
            }
            InputAction::PaletteUp => {
                self.move_overlay_selection(false);
                UpdateEffect::None
            }
            InputAction::PaletteDown => {
                self.move_overlay_selection(true);
                UpdateEffect::None
            }
            InputAction::Confirm | InputAction::Submit => self.confirm_overlay(),
            InputAction::Quit => UpdateEffect::Quit,
            _ => UpdateEffect::None,
        }
    }

    fn move_overlay_selection(&mut self, down: bool) {
        let (selected, len) = match &mut self.overlay {
            Some(Overlay::Palette { query, selected }) => (selected, command::search(query).len()),
            Some(Overlay::ModelPicker { choices, selected })
            | Some(Overlay::ReasoningPicker { choices, selected }) => (selected, choices.len()),
            Some(Overlay::Approval { prompt, selected }) => {
                (selected, approval_choice_count(prompt))
            }
            Some(Overlay::Artifact { selected, .. }) => (selected, 4),
            Some(Overlay::SessionPicker {
                query,
                choices,
                selected,
            }) => (
                selected,
                choices
                    .iter()
                    .filter(|row| session_matches(row, query))
                    .count(),
            ),
            _ => return,
        };
        if len == 0 {
            *selected = 0;
        } else if down {
            *selected = (*selected + 1).min(len - 1);
        } else {
            *selected = selected.saturating_sub(1);
        }
    }

    fn confirm_overlay(&mut self) -> UpdateEffect {
        let Some(overlay) = self.overlay.take() else {
            return UpdateEffect::None;
        };
        match overlay {
            Overlay::PastePreview { text } => {
                if let Err(reason) = self.composer.insert(&text) {
                    self.status = reason;
                } else {
                    self.status = "Pasted text inserted as untrusted draft data".to_owned();
                }
                UpdateEffect::None
            }
            Overlay::Palette { query, selected } => {
                let Some(command) = command::search(&query).get(selected).copied() else {
                    return UpdateEffect::None;
                };
                self.execute_command(
                    ParsedCommand {
                        id: command.id,
                        arguments: String::new(),
                    },
                    true,
                )
            }
            Overlay::ModelPicker { choices, selected } => choices
                .get(selected)
                .cloned()
                .map_or(UpdateEffect::None, UpdateEffect::SelectModel),
            Overlay::ReasoningPicker { choices, selected } => choices
                .get(selected)
                .cloned()
                .map_or(UpdateEffect::None, UpdateEffect::SetReasoning),
            Overlay::SessionPicker {
                query,
                choices,
                selected,
            } => choices
                .into_iter()
                .filter(|row| session_matches(row, &query))
                .nth(selected)
                .map_or(UpdateEffect::None, |row| {
                    UpdateEffect::ViewSession(row.conversation)
                }),
            Overlay::Approval { prompt, selected } => self.confirm_approval(*prompt, selected),
            Overlay::Artifact {
                artifact, selected, ..
            } => {
                let action = [
                    ArtifactAction::Preview,
                    ArtifactAction::InsertReference,
                    ArtifactAction::Reveal,
                    ArtifactAction::Open,
                ]
                .get(selected)
                .copied();
                action.map_or(UpdateEffect::None, |action| UpdateEffect::ArtifactAction {
                    record: artifact.record.clone(),
                    action,
                })
            }
            Overlay::Help | Overlay::Queue => UpdateEffect::None,
        }
    }

    fn submit_composer(&mut self) -> UpdateEffect {
        let trimmed = self.composer.text.trim();
        if trimmed.starts_with('/') {
            return match command::parse(trimmed) {
                Ok(command) => self.execute_command(command, false),
                Err(reason) => {
                    self.status = reason;
                    UpdateEffect::None
                }
            };
        }
        let input = self.composer.take().trim().to_owned();
        self.submit_text(input)
    }

    fn execute_command(&mut self, command: ParsedCommand, from_palette: bool) -> UpdateEffect {
        match command.id {
            CommandId::Help => {
                self.overlay = Some(Overlay::Help);
                UpdateEffect::None
            }
            CommandId::Send => {
                let input = if command.arguments.is_empty() {
                    if from_palette {
                        self.composer.take().trim().to_owned()
                    } else {
                        self.status = "/send requires MESSAGE when used as slash input".to_owned();
                        return UpdateEffect::None;
                    }
                } else {
                    self.composer.take();
                    command.arguments
                };
                self.submit_text(input)
            }
            CommandId::Newline => {
                self.composer.take();
                if let Err(reason) = self.composer.insert("\n") {
                    self.status = reason;
                }
                UpdateEffect::None
            }
            CommandId::Interrupt => {
                self.composer.take();
                self.interrupt()
            }
            CommandId::Steer => {
                self.composer.take();
                let Some(operation_id) = self.active_operation else {
                    self.status = "Steering requires an active turn".to_owned();
                    return UpdateEffect::None;
                };
                if !self.capabilities.steer {
                    self.status = "This execution owner does not support same-turn steering; submit a queued follow-up instead".to_owned();
                    return UpdateEffect::None;
                }
                if command.arguments.is_empty() {
                    self.status = command_usage(CommandId::Steer);
                    return UpdateEffect::None;
                }
                UpdateEffect::Steer {
                    operation_id,
                    input: command.arguments,
                }
            }
            CommandId::Model => {
                self.composer.take();
                if !self.capabilities.model {
                    self.status = "This execution owner cannot change models in place".to_owned();
                    return UpdateEffect::None;
                }
                if self.busy {
                    self.status =
                        "Wait for or interrupt the active turn before changing model".to_owned();
                    return UpdateEffect::None;
                }
                if command.arguments.is_empty() {
                    UpdateEffect::OpenModelPicker
                } else {
                    UpdateEffect::SelectModel(command.arguments)
                }
            }
            CommandId::Reasoning => {
                self.composer.take();
                if !self.capabilities.reasoning {
                    self.status = "Native Xana reasoning is selected by the model; this owner has no in-thread reasoning control".to_owned();
                    return UpdateEffect::None;
                }
                if command.arguments.is_empty() {
                    UpdateEffect::OpenReasoningPicker
                } else {
                    UpdateEffect::SetReasoning(command.arguments)
                }
            }
            CommandId::Sessions => {
                self.composer.take();
                match command.arguments.as_str() {
                    "" => UpdateEffect::OpenSessionPicker,
                    "expanded" => {
                        self.rail_expanded = true;
                        self.status = "Wide session rail expanded".to_owned();
                        UpdateEffect::PersistRail(true)
                    }
                    "collapsed" => {
                        self.rail_expanded = false;
                        self.status = "Wide session rail collapsed".to_owned();
                        UpdateEffect::PersistRail(false)
                    }
                    _ => {
                        self.status = command_usage(CommandId::Sessions);
                        UpdateEffect::None
                    }
                }
            }
            CommandId::Setup => {
                self.composer.take();
                if self.busy {
                    self.status = "Wait for or interrupt the active turn before setup".to_owned();
                    UpdateEffect::None
                } else {
                    match crate::setup::args_for_request(&command.arguments) {
                        Ok(_) => UpdateEffect::Setup(command.arguments),
                        Err(error) => {
                            self.status = error.to_string();
                            UpdateEffect::None
                        }
                    }
                }
            }
            CommandId::Doctor => {
                self.composer.take();
                if self.busy {
                    self.status = "Wait for or interrupt the active turn before doctor".to_owned();
                    UpdateEffect::None
                } else {
                    UpdateEffect::Doctor
                }
            }
            CommandId::Reset => {
                self.composer.take();
                if !from_palette {
                    self.status =
                        "Reset is a guarded command-palette lifecycle action, not a slash command"
                            .to_owned();
                    UpdateEffect::None
                } else if self.busy {
                    self.status = "Wait for or interrupt the active turn before reset".to_owned();
                    UpdateEffect::None
                } else {
                    UpdateEffect::Reset
                }
            }
            CommandId::Activity => {
                self.composer.take();
                self.activity_visibility = match command.arguments.as_str() {
                    "hidden" | "quiet" => ActivityVisibility::Hidden,
                    "open" | "verbose" => ActivityVisibility::Open,
                    "auto" | "normal" | "" => ActivityVisibility::Auto,
                    _ => {
                        self.status = command_usage(CommandId::Activity);
                        return UpdateEffect::None;
                    }
                };
                self.status = format!("Activity display: {:?}", self.activity_visibility);
                UpdateEffect::PersistActivity(self.activity_visibility.into())
            }
            CommandId::Artifact => {
                self.composer.take();
                let requested = command.arguments.trim();
                if requested.is_empty() {
                    self.status = command_usage(CommandId::Artifact);
                    return UpdateEffect::None;
                }
                let artifact = self.messages.iter().rev().find_map(|message| {
                    message
                        .document
                        .artifacts
                        .iter()
                        .find(|artifact| artifact.record.reference.id.to_string() == requested)
                });
                if let Some(artifact) = artifact.cloned() {
                    self.overlay = Some(Overlay::Artifact {
                        artifact: Box::new(artifact),
                        selected: 0,
                        preview: None,
                    });
                } else {
                    self.status =
                        "Artifact is not visible in the bounded conversation view".to_owned();
                }
                UpdateEffect::None
            }
            CommandId::Attach => {
                self.composer.take();
                if command.arguments.is_empty() {
                    self.status = command_usage(CommandId::Attach);
                    UpdateEffect::None
                } else {
                    UpdateEffect::Attach(command.arguments)
                }
            }
            CommandId::Queue => {
                self.composer.take();
                self.queue_command(&command.arguments)
            }
            CommandId::Clear => {
                self.composer.take();
                if self.busy {
                    self.status = "Interrupt or finish the active turn before clearing".to_owned();
                    UpdateEffect::None
                } else {
                    UpdateEffect::ClearConversation
                }
            }
            CommandId::Composer => {
                self.composer.take();
                let preset = match command.arguments.as_str() {
                    "submit" => ComposerPreset::Submit,
                    "newline" => ComposerPreset::Newline,
                    _ => {
                        self.status = command_usage(CommandId::Composer);
                        return UpdateEffect::None;
                    }
                };
                self.composer_preset = preset;
                self.status = format!("Composer preset: {preset:?}");
                UpdateEffect::PersistComposer(preset)
            }
            CommandId::Quit => UpdateEffect::Quit,
        }
    }

    fn queue_command(&mut self, arguments: &str) -> UpdateEffect {
        let mut parts = arguments.split_whitespace();
        match parts.next() {
            None => {
                self.overlay = Some(Overlay::Queue);
            }
            Some("remove") => match parse_queue_index(parts.next(), self.followups.len()) {
                Ok(index) => {
                    self.followups.remove(index);
                    self.status = format!("Removed queued follow-up {}", index + 1);
                }
                Err(reason) => self.status = reason,
            },
            Some("edit") => match parse_queue_index(parts.next(), self.followups.len()) {
                Ok(index) => {
                    if let Some(turn) = self.followups.remove(index) {
                        self.composer.replace(turn.input);
                        self.pending_images = turn.images;
                        self.status = format!("Editing queued follow-up {}", index + 1);
                    }
                }
                Err(reason) => self.status = reason,
            },
            Some(_) => self.status = command_usage(CommandId::Queue),
        }
        UpdateEffect::None
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

    pub(super) fn history_before(&self) -> Option<usize> {
        self.history_has_older.then_some(self.history_start)
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

    pub(super) fn apply_runtime(&mut self, event: &AgentEvent) {
        let viewing_background = self.viewed_conversation != self.runtime_conversation;
        if viewing_background {
            let background = self.background_messages.get_or_insert_with(VecDeque::new);
            std::mem::swap(&mut self.messages, background);
        }
        match event {
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Running,
            } => {
                self.busy = true;
                self.active_operation = Some(*operation_id);
                self.status = "Working…".to_owned();
            }
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(outcome),
            } => {
                self.busy = false;
                self.active_operation = None;
                self.status = format!("Turn {outcome:?}");
                self.push_card(ActivityCard::new(
                    "Xana root",
                    operation_id.to_string(),
                    ActivityKind::Status,
                    ActivityState::Complete,
                    format!("operation {outcome:?}"),
                    "",
                ));
            }
            AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Suspended,
            } => {
                self.busy = false;
                self.active_operation = None;
                self.status =
                    "Turn interrupted; any uncertain effects remain recoverable".to_owned();
                self.push_card(ActivityCard::new(
                    "Xana root",
                    operation_id.to_string(),
                    ActivityKind::Warning,
                    ActivityState::Failed,
                    "operation suspended",
                    "Any uncertain effects remain recoverable.",
                ));
            }
            AgentEvent::AssistantTextDelta {
                operation_id, text, ..
            } => self.push_assistant_delta(*operation_id, text),
            AgentEvent::AssistantMessage {
                operation_id,
                message,
            } => self.finish_assistant(*operation_id, message),
            AgentEvent::PermissionRequested { request } => {
                self.status = format!("Approval required: {}", request.tool_name);
                self.open_approval(ApprovalPrompt::native(request.clone()));
            }
            AgentEvent::OperationFailed { reason, .. } => {
                self.busy = false;
                self.active_operation = None;
                self.status = bounded(reason.clone(), MAX_ACTIVITY_BYTES);
                self.push_card(ActivityCard::new(
                    "Xana root",
                    "turn",
                    ActivityKind::Error,
                    ActivityState::Failed,
                    "turn failed",
                    reason.clone(),
                ));
            }
            AgentEvent::ConversationCleared => {
                self.messages.clear();
                self.status = "Conversation cleared".to_owned();
            }
            AgentEvent::CommandRejected { reason } => {
                self.status = bounded(format!("Command rejected: {reason}"), MAX_ACTIVITY_BYTES);
            }
            AgentEvent::InvocationIntentCommitted { intent } => self.push_card(ActivityCard::new(
                "Xana root",
                intent.invocation_id.to_string(),
                ActivityKind::Tool,
                ActivityState::Running,
                format!("tool planned: {}", intent.permission.request.tool_name),
                intent.permission.request.final_arguments.to_string(),
            )),
            AgentEvent::ToolFinished { invocation_id, .. } => self.push_card(ActivityCard::new(
                "Xana root",
                invocation_id.to_string(),
                ActivityKind::Tool,
                ActivityState::Complete,
                "tool finished",
                "",
            )),
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle,
            } => self.push_card(ActivityCard::new(
                format!("Xana child {}", attribution.agent_id),
                attribution.agent_id.to_string(),
                ActivityKind::Child,
                ActivityState::Running,
                format!("{}: {lifecycle:?}", attribution.route),
                format!(
                    "{}/{} via {}",
                    attribution.connection, attribution.model, attribution.profile
                ),
            )),
            AgentEvent::ChildActivity {
                attribution,
                activity,
            } => match activity {
                crate::orchestration::ChildActivity::PermissionRequested { request } => {
                    self.open_approval(ApprovalPrompt::child(attribution.clone(), request.clone()));
                }
                crate::orchestration::ChildActivity::ManagedRuntime { notification } => {
                    if let Some(event) = ManagedClientEvent::from_notification(notification.clone())
                    {
                        self.apply_managed_activity(
                            format!("Codex child {}", attribution.agent_id),
                            &event,
                        );
                    }
                }
                crate::orchestration::ChildActivity::Warning { message } => {
                    self.push_card(ActivityCard::new(
                        format!("Xana child {}", attribution.agent_id),
                        attribution.agent_id.to_string(),
                        ActivityKind::Warning,
                        ActivityState::Failed,
                        "child warning",
                        message.clone(),
                    ))
                }
                crate::orchestration::ChildActivity::AssistantTextDelta { .. }
                | crate::orchestration::ChildActivity::PermissionAudited { .. }
                | crate::orchestration::ChildActivity::ToolFinished { .. }
                | crate::orchestration::ChildActivity::Suspended => {
                    self.push_card(ActivityCard::new(
                        format!("Xana child {}", attribution.agent_id),
                        attribution.agent_id.to_string(),
                        ActivityKind::Child,
                        ActivityState::Running,
                        format!("{} activity", attribution.route),
                        "",
                    ))
                }
            },
            AgentEvent::ChildReportCommitted { report } => self.push_card(ActivityCard::new(
                format!("Xana child {}", report.attribution.agent_id),
                report.attribution.agent_id.to_string(),
                ActivityKind::Child,
                ActivityState::Complete,
                format!("child report: {:?}", report.status),
                report
                    .output
                    .clone()
                    .or_else(|| report.error.clone())
                    .unwrap_or_default(),
            )),
            AgentEvent::InvocationResultCommitted { .. }
            | AgentEvent::PermissionAudited { .. }
            | AgentEvent::ChildListSnapshot { .. }
            | AgentEvent::ChildInspectionSnapshot { .. }
            | AgentEvent::ChildCancellationRequested { .. } => {}
        }
        if viewing_background {
            let background = self.background_messages.get_or_insert_with(VecDeque::new);
            std::mem::swap(&mut self.messages, background);
            if let Some(row) = self
                .sessions
                .iter_mut()
                .find(|row| row.conversation == self.runtime_conversation)
            {
                row.unread = matches!(
                    event,
                    AgentEvent::AssistantMessage { .. }
                        | AgentEvent::OperationStateChanged {
                            state: OperationState::Finished(_),
                            ..
                        }
                );
                row.error |= matches!(event, AgentEvent::OperationFailed { .. });
            }
        }
    }

    fn interrupt(&mut self) -> UpdateEffect {
        let Some(operation_id) = self.active_operation else {
            self.status = "No root turn is active".to_owned();
            return UpdateEffect::None;
        };
        if !self.capabilities.interrupt {
            self.status = "This execution owner does not support interruption".to_owned();
            return UpdateEffect::None;
        }
        self.status = "Interrupt requested…".to_owned();
        UpdateEffect::Interrupt { operation_id }
    }

    fn push_assistant_delta(&mut self, operation_id: OperationId, text: &str) {
        if self.active_operation != Some(operation_id)
            || !matches!(self.messages.back(), Some(message) if message.kind == MessageKind::Assistant)
        {
            self.push_message(MessageKind::Assistant, String::new());
            self.active_operation = Some(operation_id);
        }
        if let Some(message) = self.messages.back_mut() {
            append_bounded(&mut message.text, text, MAX_MESSAGE_BYTES);
            message.document.stream_append(text);
        }
    }

    fn finish_assistant(&mut self, operation_id: OperationId, message: &Message) {
        let final_message = message_projection(message);
        if self.active_operation == Some(operation_id)
            && matches!(self.messages.back(), Some(message) if message.kind == MessageKind::Assistant)
        {
            if let Some(message) = self.messages.back_mut() {
                *message = final_message;
            }
        } else {
            if self.scroll > 0 {
                self.scroll = self.scroll.saturating_add(1);
            }
            self.messages.push_back(final_message);
            trim_front(&mut self.messages, MAX_VISIBLE_MESSAGES);
        }
    }

    fn push_message(&mut self, kind: MessageKind, text: impl Into<String>) {
        let text = bounded(text.into(), MAX_MESSAGE_BYTES);
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_add(1);
        }
        self.messages.push_back(VisibleMessage {
            kind,
            document: RichDocument::plain(&text),
            text,
        });
        trim_front(&mut self.messages, MAX_VISIBLE_MESSAGES);
    }

    fn push_activity(&mut self, text: impl Into<String>) {
        self.push_card(ActivityCard::new(
            "Xana",
            format!("event-{}", self.activity.len()),
            ActivityKind::Status,
            ActivityState::Complete,
            text,
            "",
        ));
    }

    fn push_card(&mut self, card: ActivityCard) {
        if self.activity_visibility == ActivityVisibility::Auto && card.is_substantive() {
            self.auto_activity_open = true;
        }
        activity::push(&mut self.activity, card);
        trim_front(&mut self.activity, MAX_ACTIVITY);
    }

    fn open_approval(&mut self, prompt: ApprovalPrompt) {
        self.push_card(ActivityCard::new(
            prompt.owner.clone(),
            "approval",
            ActivityKind::Approval,
            ActivityState::Waiting,
            prompt.title.clone(),
            prompt.details.join("\n"),
        ));
        self.overlay = Some(Overlay::Approval {
            prompt: Box::new(prompt),
            selected: 0,
        });
        self.status = "Waiting for approval".to_owned();
    }

    pub(super) fn open_managed_approval(
        &mut self,
        request: crate::managed::codex::ApprovalRequest,
    ) {
        self.open_approval(ApprovalPrompt::managed(request));
    }

    fn confirm_approval(&mut self, prompt: ApprovalPrompt, selected: usize) -> UpdateEffect {
        let choice = approval_choices(&prompt).get(selected).copied();
        let Some(choice) = choice else {
            self.status = "Approval offered no safe decision".to_owned();
            return UpdateEffect::None;
        };
        self.status = format!("Approval decision: {choice:?}");
        match prompt.target {
            ApprovalTarget::Native(request) => UpdateEffect::DecideNativeApproval {
                operation_id: request.operation_id,
                invocation_id: request.invocation_id,
                decision: controller_decision(choice, request.scope),
            },
            ApprovalTarget::Child {
                attribution,
                request,
            } => UpdateEffect::DecideChildApproval {
                agent_id: attribution.agent_id,
                operation_id: request.operation_id,
                invocation_id: request.invocation_id,
                decision: controller_decision(choice, request.scope),
            },
            ApprovalTarget::Managed(request) => {
                let decision = match choice {
                    ApprovalChoice::Once => crate::managed::codex::ApprovalDecision::AcceptOnce,
                    ApprovalChoice::Session => {
                        crate::managed::codex::ApprovalDecision::AcceptForSession
                    }
                    ApprovalChoice::Deny if request.available_decisions.contains("decline") => {
                        crate::managed::codex::ApprovalDecision::Decline
                    }
                    ApprovalChoice::Deny => crate::managed::codex::ApprovalDecision::Cancel,
                };
                UpdateEffect::DecideManagedApproval(decision)
            }
        }
    }

    pub(super) fn apply_managed_event(&mut self, event: &ManagedClientEvent) {
        match event {
            ManagedClientEvent::AssistantDelta(delta) => {
                let operation_id = self.active_operation.unwrap_or_else(OperationId::new);
                self.push_assistant_delta(operation_id, delta);
            }
            ManagedClientEvent::TurnCompleted { status, error } => {
                self.busy = false;
                self.active_operation = None;
                self.status = error.as_ref().map_or_else(
                    || format!("Managed turn {status}"),
                    |error| format!("Managed turn failed: {error}"),
                );
            }
            ManagedClientEvent::ModelRerouted { to_model, .. } => {
                self.model = to_model.clone();
            }
            _ => {}
        }
        self.apply_managed_activity("Codex".to_owned(), event);
    }

    pub(super) fn set_managed_thread(&mut self, connection: &str, thread_id: String) {
        self.session = thread_id.clone();
        let conversation = ConversationRef::Managed {
            connection: connection.to_owned(),
            thread_id,
        };
        self.runtime_conversation = conversation.clone();
        self.viewed_conversation = conversation;
    }

    pub(super) fn finish_managed_turn(&mut self, operation_id: OperationId, error: Option<String>) {
        if self.active_operation == Some(operation_id) {
            self.active_operation = None;
            self.busy = false;
        }
        if let Some(error) = error {
            self.status = bounded(format!("Managed turn failed: {error}"), MAX_ACTIVITY_BYTES);
            self.push_card(ActivityCard::new(
                "Codex",
                operation_id.to_string(),
                ActivityKind::Error,
                ActivityState::Failed,
                "managed turn failed",
                error,
            ));
        } else {
            self.status = "Managed turn completed".to_owned();
        }
    }

    pub(super) fn managed_cleared(&mut self, connection: &str) {
        self.messages.clear();
        self.activity.clear();
        self.session = "new".to_owned();
        self.runtime_conversation = ConversationRef::NewManaged {
            connection: connection.to_owned(),
        };
        self.viewed_conversation = self.runtime_conversation.clone();
        self.status = "New managed thread will start with the next message".to_owned();
    }

    pub(super) fn set_model(&mut self, model: String) {
        self.model = model;
        self.status = "Model updated for subsequent turns; managed context is unchanged".to_owned();
    }

    pub(super) fn show_artifact_preview(
        &mut self,
        record: crate::artifact::ArtifactRecord,
        preview: String,
    ) {
        let label = format!("{} · {} bytes", record.media_type, record.byte_len);
        self.overlay = Some(Overlay::Artifact {
            artifact: Box::new(ArtifactView { record, label }),
            selected: 0,
            preview: Some(bounded(preview, 64 * 1024)),
        });
    }

    pub(super) fn insert_artifact_reference(&mut self, record: &crate::artifact::ArtifactRecord) {
        let reference = format!("artifact:{}", record.reference.id);
        if let Err(reason) = self.composer.insert(&reference) {
            self.status = reason;
        } else {
            self.status = "Artifact reference inserted into the draft".to_owned();
        }
    }

    fn apply_managed_activity(&mut self, owner: String, event: &ManagedClientEvent) {
        if let Some(mut card) = activity::from_managed(event) {
            card.owner = owner;
            self.push_card(card);
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

fn command_usage(id: CommandId) -> String {
    command::COMMANDS
        .iter()
        .find(|command| command.id == id)
        .map_or_else(
            || "Invalid command".to_owned(),
            |command| format!("Usage: {}", command.usage),
        )
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

fn parse_queue_index(value: Option<&str>, len: usize) -> Result<usize, String> {
    let index = value
        .ok_or_else(|| command_usage(CommandId::Queue))?
        .parse::<usize>()
        .map_err(|_| command_usage(CommandId::Queue))?;
    if index == 0 || index > len {
        return Err(format!(
            "Queued follow-up index must be between 1 and {len}"
        ));
    }
    Ok(index - 1)
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

fn sanitize_input(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(MAX_INPUT_BYTES));
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        let character = if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            '\n'
        } else {
            character
        };
        match character {
            '\n' => output.push('\n'),
            '\t' => output.push_str("    "),
            character if !character.is_control() => output.push(character),
            _ => {}
        }
        if output.len() >= MAX_INPUT_BYTES {
            break;
        }
    }
    while output.len() > MAX_INPUT_BYTES || !output.is_char_boundary(output.len()) {
        output.pop();
    }
    output
}

fn previous_boundary(value: &str, cursor: usize) -> usize {
    value[..cursor]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(value: &str, cursor: usize) -> usize {
    value[cursor..]
        .char_indices()
        .nth(1)
        .map_or(value.len(), |(index, _)| cursor + index)
}

fn line_start(value: &str, cursor: usize) -> usize {
    value[..cursor].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(value: &str, cursor: usize) -> usize {
    value[cursor..]
        .find('\n')
        .map_or(value.len(), |index| cursor + index)
}

fn vertical_move(value: &str, cursor: usize, down: bool) -> usize {
    let start = line_start(value, cursor);
    let column = value[start..cursor].chars().count();
    let target = if down {
        let end = line_end(value, cursor);
        if end == value.len() {
            return cursor;
        }
        end + 1
    } else {
        if start == 0 {
            return cursor;
        }
        line_start(value, start - 1)
    };
    let target_end = line_end(value, target);
    value[target..target_end]
        .char_indices()
        .nth(column)
        .map_or(target_end, |(index, _)| target + index)
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
mod tests {
    use super::*;
    use crate::{
        identity::{SessionId, StepId},
        runtime::OperationOutcome,
        workspace_host::{ConversationProjection, ConversationState, WorkspaceSnapshot},
    };

    #[test]
    fn composer_edits_unicode_multiline_and_selection_safely() {
        let mut composer = Composer::new();
        composer.insert("one\ntwø").unwrap();
        composer.move_cursor(MoveDirection::Left, true);
        assert!(composer.selection().is_some());
        composer.insert("o").unwrap();
        assert_eq!(composer.text, "one\ntwo");
        composer.move_cursor(MoveDirection::Home, false);
        assert_eq!(composer.cursor, 4);
        composer.move_cursor(MoveDirection::Up, false);
        assert_eq!(composer.cursor, 0);
    }

    #[test]
    fn paste_is_previewed_sanitized_and_never_executed() {
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.busy = false;
        state.update_input(InputAction::Paste("/quit\u{1b}[31m\r\ntext".to_owned()));
        assert!(matches!(state.overlay, Some(Overlay::PastePreview { .. })));
        state.update_input(InputAction::Confirm);
        assert_eq!(state.composer.text, "/quit[31m\ntext");
        assert!(!matches!(
            state.update_input(InputAction::Cancel),
            UpdateEffect::Quit
        ));
    }

    #[test]
    fn busy_submissions_queue_in_order_and_can_be_edited_or_removed() {
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.busy = true;
        for input in ["first", "second", "third"] {
            state.update_input(InputAction::Insert(input.to_owned()));
            assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
        }
        assert_eq!(state.followups.len(), 3);
        state.composer.replace("/queue remove 2".to_owned());
        state.update_input(InputAction::Submit);
        state.composer.replace("/queue edit 1".to_owned());
        state.update_input(InputAction::Submit);
        assert_eq!(state.composer.text, "first");
        assert_eq!(state.followups.front().unwrap().input, "third");
    }

    #[test]
    fn interrupt_and_steer_are_distinct_and_capability_gated() {
        let operation_id = OperationId::new();
        let mut native = TuiState::starting(ComposerPreset::Submit);
        native.busy = true;
        native.active_operation = Some(operation_id);
        assert_eq!(
            native.update_input(InputAction::Interrupt),
            UpdateEffect::Interrupt { operation_id }
        );
        native.composer.replace("/steer focus".to_owned());
        assert_eq!(native.update_input(InputAction::Submit), UpdateEffect::None);
        assert!(native.status.contains("does not support"));

        let mut managed = TuiState::starting(ComposerPreset::Submit)
            .with_capabilities(OwnerCapabilities::managed());
        managed.busy = true;
        managed.active_operation = Some(operation_id);
        managed.composer.replace("/steer focus".to_owned());
        assert_eq!(
            managed.update_input(InputAction::Submit),
            UpdateEffect::None
        );
        assert!(managed.status.contains("does not support"));
    }

    #[test]
    fn input_and_runtime_events_follow_one_explicit_update_path() {
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.busy = false;
        state.update_input(InputAction::Insert("hello".to_owned()));
        let UpdateEffect::Submit {
            operation_id,
            input,
            images,
        } = state.update_input(InputAction::Submit)
        else {
            panic!("submit effect");
        };
        assert_eq!(input, "hello");
        assert!(images.is_empty());
        state.mark_submitted(operation_id, input);
        state.apply_runtime(&AgentEvent::AssistantTextDelta {
            operation_id,
            step_id: StepId::new(),
            text: "hi".to_owned(),
        });
        state.apply_runtime(&AgentEvent::AssistantMessage {
            operation_id,
            message: Message::text(Role::Assistant, "hi there"),
        });
        state.apply_runtime(&AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(OperationOutcome::Completed),
        });
        assert!(!state.busy);
        assert_eq!(state.messages.back().unwrap().text, "hi there");
    }

    #[test]
    fn composer_and_retained_views_are_bounded() {
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.busy = false;
        state.update_input(InputAction::Insert("x".repeat(MAX_INPUT_BYTES + 1)));
        assert!(state.composer.text.is_empty());
        assert!(state.status.contains("limit"));
        for index in 0..(MAX_ACTIVITY + 20) {
            state.push_activity(format!("event {index}"));
        }
        assert_eq!(state.activity.len(), MAX_ACTIVITY);
    }

    #[test]
    fn hidden_activity_cannot_hide_or_duplicate_a_native_approval() {
        let operation_id = OperationId::new();
        let invocation_id = ToolInvocationId::new();
        let request = crate::permission::PermissionRequest {
            operation_id,
            invocation_id,
            tool_name: "run_command".to_owned(),
            effect_class: crate::tool::EffectClass::Execute,
            final_arguments: serde_json::json!({"command": "cargo test"}),
            scope: crate::permission::PermissionScope::Unscoped,
        };
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.activity_visibility = ActivityVisibility::Hidden;
        state.apply_runtime(&AgentEvent::PermissionRequested {
            request: request.clone(),
        });
        assert!(matches!(state.overlay, Some(Overlay::Approval { .. })));
        assert!(
            state
                .activity
                .back()
                .is_some_and(|card| card.kind == ActivityKind::Approval)
        );
        assert_eq!(
            state.update_input(InputAction::Confirm),
            UpdateEffect::DecideNativeApproval {
                operation_id,
                invocation_id,
                decision: ControllerDecision::AllowOnce,
            }
        );
        assert_eq!(state.update_input(InputAction::Confirm), UpdateEffect::None);
        assert_eq!(state.activity_visibility, ActivityVisibility::Hidden);
    }

    #[test]
    fn fake_codex_transcript_preserves_reasoning_and_managed_ownership() {
        let mut state = TuiState::from_managed(
            "codex".to_owned(),
            "gpt-test".to_owned(),
            "thread-test".to_owned(),
            ComposerPreset::Submit,
            ActivityVisibility::Auto,
            ConversationRef::NewManaged {
                connection: "codex".to_owned(),
            },
        );
        state.apply_managed_event(&ManagedClientEvent::ReasoningSummaryDelta(
            "checking the workspace".to_owned(),
        ));
        state.apply_managed_event(&ManagedClientEvent::ItemStarted(
            crate::frontend::ManagedClientItem {
                id: "command-1".to_owned(),
                kind: "commandExecution".to_owned(),
                status: Some("inProgress".to_owned()),
                label: "cargo test".to_owned(),
                details: "running tests".to_owned(),
            },
        ));
        state.apply_managed_event(&ManagedClientEvent::AssistantDelta(
            "Tests are passing.".to_owned(),
        ));
        assert!(state.auto_activity_open);
        assert!(
            state.activity.iter().any(|card| {
                card.kind == ActivityKind::ReasoningSummary && card.owner == "Codex"
            })
        );
        assert!(
            state
                .activity
                .iter()
                .any(|card| { card.kind == ActivityKind::Tool && card.identity == "command-1" })
        );
        assert_eq!(state.messages.back().unwrap().kind, MessageKind::Assistant);
    }

    #[test]
    fn streaming_at_bottom_and_scrolled_history_preserve_the_expected_anchor() {
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.messages.clear();
        for index in 0..20 {
            state.push_message(MessageKind::Assistant, format!("message {index}"));
        }
        assert_eq!(state.scroll, 0);
        state.update_input(InputAction::Scroll(-6));
        assert_eq!(state.scroll, 6);
        state.push_message(MessageKind::Assistant, "late message");
        assert_eq!(state.scroll, 7, "distance from the newest edge is anchored");
        state.update_input(InputAction::Scroll(7));
        assert_eq!(state.scroll, 0);
        state.push_message(MessageKind::Assistant, "newest message");
        assert_eq!(state.scroll, 0, "the newest edge stays anchored");
    }

    #[test]
    fn session_inspection_keeps_the_runtime_transcript_and_draft_separate() {
        let runtime = ConversationRef::Native {
            session_id: SessionId::new(),
        };
        let other = ConversationRef::Native {
            session_id: SessionId::new(),
        };
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.busy = true;
        state.runtime_conversation = runtime.clone();
        state.viewed_conversation = runtime.clone();
        state.refresh_sessions(WorkspaceSnapshot {
            workspace: std::env::current_dir().unwrap(),
            conversations: vec![
                ConversationProjection {
                    conversation: runtime.clone(),
                    state: ConversationState::Controlled,
                    record_count: Some(5),
                    modified: None,
                    selected: false,
                },
                ConversationProjection {
                    conversation: other.clone(),
                    state: ConversationState::Inactive,
                    record_count: Some(3),
                    modified: None,
                    selected: false,
                },
            ],
            active: None,
        });

        state.view_session(
            other.clone(),
            Some(vec![Message::text(Role::User, "retained history")]),
        );
        state.update_input(InputAction::Insert("local draft".to_owned()));
        assert_eq!(state.update_input(InputAction::Submit), UpdateEffect::None);
        assert_eq!(state.composer.text, "local draft");

        let operation_id = OperationId::new();
        state.active_operation = Some(operation_id);
        state.apply_runtime(&AgentEvent::AssistantMessage {
            operation_id,
            message: Message::text(Role::Assistant, "background result"),
        });
        assert_eq!(state.messages.back().unwrap().text, "retained history");
        assert!(
            state
                .sessions
                .iter()
                .find(|row| row.conversation == runtime)
                .unwrap()
                .unread
        );

        state.view_session(runtime, None);
        assert_eq!(state.messages.back().unwrap().text, "background result");
    }
}
