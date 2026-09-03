//! Application-owned state and the first real Desktop runtime projection.

use crate::{
    commands::{
        self, ArchiveSelectedProject, BranchSelectedConversation, ClearConversation, InterruptRun,
        MinimizeWindow, MoveSelectedConversation, OpenConfigurationFile, OpenDocumentation,
        QuitXana, RenameSelectedProject, RestoreSelectedProject, RevealLogs, ShowActivity,
        ShowCommandPalette, ShowEspejo, ShowSettings, UngroupSelectedConversation,
        WorkbenchCommand,
    },
    composer::{ComposerStore, QueuedSubmission},
    design_system,
    espejo::{EspejoScope, EspejoView, EspejoViewEvent},
    projection::ConversationProjection,
    settings_view::{SettingsView, SettingsViewEvent},
};
use gpui::{
    AnyElement, Context, Entity, ExternalPaths, IntoElement, ParentElement as _, PathPromptOptions,
    PromptLevel, Render, Role, Subscription, SystemNotification, Task, Window, div, prelude::*, px,
    rems,
};
use gpui_ai::prelude::{
    ApprovalCard, ApprovalEvent, Attachment, Chat, ChatEvent, ChatWelcome, CommandSearch,
    CommandSearchEvent, LoadingState, MessageQueue, ProgressState, PromptBar, PromptBarEvent,
    PromptModel, QueueEvent, QueuedMessage, SidebarNav, SidebarNavEvent, SidebarNavItem,
    SidebarNavPresentation, SidebarSection, StatusBadge, StatusTone, Suggestion,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName,
    button::{Button, ButtonVariants as _},
    h_flex, h_resizable,
    input::{Input, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu},
    resizable_panel,
    scroll::ScrollableElement as _,
    v_flex, v_resizable,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    sync::Arc,
    time::Duration,
};
use xana::desktop::{
    AttentionKind, AttentionSignal, ClientFocus, DesktopActivityItem, DesktopActivityOwner,
    DesktopActivityState, DesktopAttachment, DesktopClient, DesktopCommandReceipt,
    DesktopControlPlane, DesktopConversationState, DesktopDockPlacement, DesktopEvent,
    DesktopHostEvent, DesktopInstanceLease, DesktopLaunchIntent, DesktopLayoutNode,
    DesktopModelOption, DesktopNativePaths, DesktopNavigationSnapshot, DesktopNavigationTarget,
    DesktopOperationState, DesktopPanelId, DesktopRoundBudgetSuspension,
    DesktopSettingsDraftSnapshot, DesktopSettingsReceipt, DesktopSettingsSection,
    DesktopSettingsSnapshot, DesktopSidebarMode, DesktopSplitAxis, DesktopUpdate,
    DesktopWorkbenchLayout, DesktopWorkspaceStatus, LastWindowChoice, LastWindowEffect,
    NotificationDestination, NotificationPlanner, last_window_effect,
};

const UPDATE_INTERVAL: Duration = Duration::from_millis(16);
const MAX_UPDATES_PER_FRAME: usize = 64;
const MAX_RECOVERABLE_SUBMISSIONS: usize = 64;
const MAX_RETAINED_CONVERSATION_UIS: usize = 128;
const DOCUMENTATION_URL: &str = "https://github.com/labcoder/xana#readme";

#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarSelection {
    Project(String),
    Conversation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NavigationDialog {
    RenameProject { project_id: String },
    MoveConversation { conversation_id: String },
}

struct RetainedConversationUi {
    chat: Entity<Chat>,
    prompt: Entity<PromptBar>,
    _chat_subscription: Subscription,
    _prompt_subscription: Subscription,
}

/// Owns retained GPUI entities, Xana's runtime client, and controlled snapshots.
pub(crate) struct Workbench {
    runtime: DesktopClient,
    control: DesktopControlPlane,
    instance: DesktopInstanceLease,
    native_paths: DesktopNativePaths,
    projection: ConversationProjection,
    espejo: Entity<EspejoView>,
    navigation_snapshot: DesktopNavigationSnapshot,
    layout: DesktopWorkbenchLayout,
    layout_save_generation: u64,
    settings_snapshot: DesktopSettingsSnapshot,
    settings_draft: Option<DesktopSettingsDraftSnapshot>,
    settings_receipt: Option<DesktopSettingsReceipt>,
    pending_settings_commands: HashSet<u64>,
    selected_project: Option<String>,
    sidebar_selection: Option<SidebarSelection>,
    navigation_dialog: Option<NavigationDialog>,
    navigation_input: Entity<InputState>,
    navigation: DesktopNavigationTarget,
    chat: Entity<Chat>,
    prompt: Entity<PromptBar>,
    conversation_uis: HashMap<String, RetainedConversationUi>,
    conversation_ui_recency: VecDeque<String>,
    model_options: Vec<DesktopModelOption>,
    composer: ComposerStore,
    pending_submissions: HashMap<
        u64,
        (
            String,
            QueuedSubmission,
            Option<xana::desktop::DesktopOperationId>,
        ),
    >,
    recoverable_submissions: HashMap<xana::desktop::DesktopOperationId, (String, QueuedSubmission)>,
    recoverable_order: VecDeque<xana::desktop::DesktopOperationId>,
    pending_attachment_commands: HashMap<u64, String>,
    sidebar: Entity<SidebarNav>,
    command_search: Entity<CommandSearch>,
    settings_view: Entity<SettingsView>,
    palette_open: bool,
    shutdown_pending: bool,
    close_prompt_open: bool,
    notifications: NotificationPlanner,
    _sidebar_subscription: Subscription,
    _command_subscription: Subscription,
    _espejo_subscription: Subscription,
    _settings_subscription: Subscription,
    _runtime_driver: Task<()>,
}

impl Workbench {
    pub(crate) fn new(
        runtime: DesktopClient,
        control: DesktopControlPlane,
        instance: DesktopInstanceLease,
        native_paths: DesktopNativePaths,
        initial_intent: DesktopLaunchIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut projection = ConversationProjection::from_snapshot(runtime.initial_snapshot());
        let navigation_snapshot = runtime.initial_snapshot().navigation.clone();
        let layout = runtime.initial_snapshot().layout.layout.clone();
        let settings_snapshot = runtime.initial_snapshot().settings.clone();
        let appearance = design_system::appearance_from_settings(
            &settings_snapshot,
            design_system::VisualSystem::read(cx).preferences(),
        );
        design_system::apply(appearance, cx);
        let selected_project = navigation_snapshot
            .selected_conversation
            .as_deref()
            .and_then(|id| project_for_conversation(&navigation_snapshot, id));
        let sidebar_selection = navigation_snapshot
            .selected_conversation
            .clone()
            .map(SidebarSelection::Conversation);
        let navigation = navigation_for_intent(initial_intent);
        if matches!(
            navigation,
            DesktopNavigationTarget::Settings | DesktopNavigationTarget::Diagnostics
        ) && let Err(error) = runtime.begin_settings()
        {
            projection.fail(error.message);
        }
        let active_conversation = runtime
            .initial_snapshot()
            .attached_conversation
            .clone()
            .or_else(|| navigation_snapshot.selected_conversation.clone())
            .unwrap_or_else(|| runtime.initial_snapshot().session_id.clone());
        let composer = ComposerStore::new(active_conversation.clone());
        let model_options = model_options_for(&control, projection.connection());
        let prompt = cx.new(|cx| PromptBar::new("xana-composer", window, cx));
        prompt.update(cx, |prompt, cx| {
            prompt.set_progress(ProgressState::Pending, cx);
            prompt.set_models(
                prompt_models(&model_options, projection.connection(), projection.model()),
                cx,
            );
            prompt.set_selected_model(projection.model().to_owned(), cx);
        });

        // `gpui-ai::Chat` currently requires a PromptBar. Xana mounts one
        // inert, hidden instance so Chat can own only the virtual transcript;
        // the one interactive retained composer lives in the Message panel.
        let transcript_prompt =
            cx.new(|cx| PromptBar::new("xana-transcript-anchor", window, cx).hidden());
        let chat = cx.new(|cx| Chat::new("xana-conversation", transcript_prompt, window, cx));
        chat.update(cx, |chat, cx| {
            chat.set_welcome(
                Some(
                    ChatWelcome::new("What can I help you with?")
                        .description("Xana Desktop is connected to the local Xana runtime.")
                        .suggestions([Suggestion::new("capabilities", "What can Xana do?")]),
                ),
                cx,
            );
            chat.set_messages(Arc::from(projection.messages()), window, cx);
        });

        let command_search = cx.new(|cx| CommandSearch::new("xana-command-palette", window, cx));
        command_search.update(cx, |search, cx| {
            search.set_items(commands::palette_items(projection.is_running()), window, cx);
        });

        let sidebar = cx.new(|cx| {
            SidebarNav::new("xana-sidebar", window, cx)
                .with_presentation(SidebarNavPresentation::Embedded)
        });
        let navigation_input = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        let settings_view = cx.new(|cx| {
            SettingsView::new(
                control.clone(),
                settings_snapshot.clone(),
                None,
                None,
                window,
                cx,
            )
        });
        let espejo = cx.new(|_| EspejoView::new(runtime.initial_snapshot()));
        if navigation == DesktopNavigationTarget::Diagnostics {
            settings_view.update(cx, |settings, cx| {
                settings.open_section(DesktopSettingsSection::Diagnostics, window, cx);
            });
        }
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_sections(sidebar_sections(&navigation_snapshot), cx);
            if let Some(selected) = navigation_snapshot.selected_conversation.as_deref() {
                sidebar.set_active_item(conversation_item_id(selected), cx);
            }
            sidebar.set_collapsed(
                navigation_snapshot.sidebar_mode == DesktopSidebarMode::Mini,
                cx,
            );
        });

        let chat_subscription =
            cx.subscribe_in(&chat, window, |this, _, event: &ChatEvent, window, cx| {
                this.handle_chat_event(event, window, cx);
            });
        let prompt_subscription = cx.subscribe_in(
            &prompt,
            window,
            |this, _, event: &PromptBarEvent, window, cx| {
                this.handle_prompt_event(event, window, cx);
            },
        );
        let conversation_uis = HashMap::from([(
            active_conversation.clone(),
            RetainedConversationUi {
                chat: chat.clone(),
                prompt: prompt.clone(),
                _chat_subscription: chat_subscription,
                _prompt_subscription: prompt_subscription,
            },
        )]);
        let command_subscription = cx.subscribe_in(
            &command_search,
            window,
            |this, _, event: &CommandSearchEvent, window, cx| {
                this.handle_command_search_event(event, window, cx);
            },
        );
        let sidebar_subscription = cx.subscribe_in(
            &sidebar,
            window,
            |this, _, event: &SidebarNavEvent, window, cx| {
                this.handle_sidebar_event(event, window, cx);
            },
        );
        let settings_subscription = cx.subscribe_in(
            &settings_view,
            window,
            |this, _, event: &SettingsViewEvent, window, cx| {
                this.handle_settings_event(event, window, cx);
            },
        );
        let espejo_subscription = cx.subscribe_in(
            &espejo,
            window,
            |this, _, event: &EspejoViewEvent, window, cx| {
                this.handle_espejo_event(event, window, cx);
            },
        );
        let runtime_driver = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(UPDATE_INTERVAL).await;
                let Ok(keep_running) = this.update_in(cx, |this, window, cx| {
                    this.drain_runtime_updates(window, cx)
                }) else {
                    break;
                };
                if !keep_running {
                    break;
                }
            }
        });

        let workbench = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            workbench
                .update(cx, |this, cx| this.request_close(window, cx))
                .unwrap_or(true)
        });

        Self {
            runtime,
            control,
            instance,
            native_paths,
            projection,
            espejo,
            navigation_snapshot,
            layout,
            layout_save_generation: 0,
            settings_snapshot,
            settings_draft: None,
            settings_receipt: None,
            pending_settings_commands: HashSet::new(),
            selected_project,
            sidebar_selection,
            navigation_dialog: None,
            navigation_input,
            navigation,
            chat,
            prompt,
            conversation_uis,
            conversation_ui_recency: VecDeque::from([active_conversation]),
            model_options,
            composer,
            pending_submissions: HashMap::new(),
            recoverable_submissions: HashMap::new(),
            recoverable_order: VecDeque::new(),
            pending_attachment_commands: HashMap::new(),
            sidebar,
            command_search,
            settings_view,
            palette_open: false,
            shutdown_pending: false,
            close_prompt_open: false,
            notifications: NotificationPlanner::new(),
            _sidebar_subscription: sidebar_subscription,
            _command_subscription: command_subscription,
            _espejo_subscription: espejo_subscription,
            _settings_subscription: settings_subscription,
            _runtime_driver: runtime_driver,
        }
    }

    fn activate_conversation_ui(
        &mut self,
        conversation: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.conversation_uis.contains_key(conversation) {
            let prompt_id = format!("xana-composer-{conversation}");
            let prompt = cx.new(|cx| PromptBar::new(prompt_id, window, cx));
            prompt.update(cx, |prompt, cx| {
                prompt.set_models(
                    prompt_models(
                        &self.model_options,
                        self.projection.connection(),
                        self.projection.model(),
                    ),
                    cx,
                );
                prompt.set_selected_model(self.projection.model().to_owned(), cx);
            });

            let transcript_prompt_id = format!("xana-transcript-anchor-{conversation}");
            let transcript_prompt =
                cx.new(|cx| PromptBar::new(transcript_prompt_id, window, cx).hidden());
            let chat_id = format!("xana-conversation-{conversation}");
            let chat = cx.new(|cx| Chat::new(chat_id, transcript_prompt, window, cx));
            chat.update(cx, |chat, cx| {
                chat.set_welcome(
                    Some(
                        ChatWelcome::new("What can I help you with?")
                            .description("Xana Desktop is connected to the local Xana runtime.")
                            .suggestions([Suggestion::new("capabilities", "What can Xana do?")]),
                    ),
                    cx,
                );
                chat.set_messages(Arc::from(self.projection.messages()), window, cx);
            });

            let chat_subscription =
                cx.subscribe_in(&chat, window, |this, _, event: &ChatEvent, window, cx| {
                    this.handle_chat_event(event, window, cx);
                });
            let prompt_subscription = cx.subscribe_in(
                &prompt,
                window,
                |this, _, event: &PromptBarEvent, window, cx| {
                    this.handle_prompt_event(event, window, cx);
                },
            );
            self.conversation_uis.insert(
                conversation.to_owned(),
                RetainedConversationUi {
                    chat: chat.clone(),
                    prompt: prompt.clone(),
                    _chat_subscription: chat_subscription,
                    _prompt_subscription: prompt_subscription,
                },
            );
        }

        let retained = self
            .conversation_uis
            .get(conversation)
            .expect("the active Conversation UI is retained");
        self.chat = retained.chat.clone();
        self.prompt = retained.prompt.clone();
        self.conversation_ui_recency
            .retain(|candidate| candidate != conversation);
        self.conversation_ui_recency
            .push_back(conversation.to_owned());
        while self.conversation_uis.len() > MAX_RETAINED_CONVERSATION_UIS {
            let Some(candidate) = self.conversation_ui_recency.pop_front() else {
                break;
            };
            if candidate == conversation {
                self.conversation_ui_recency.push_back(candidate);
            } else {
                self.conversation_uis.remove(&candidate);
            }
        }
    }

    fn handle_chat_event(
        &mut self,
        event: &ChatEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ChatEvent::Prompt(event) => self.handle_prompt_event(event, window, cx),
            ChatEvent::SuggestionSelected { suggestion_id }
                if suggestion_id.as_ref() == "capabilities" =>
            {
                self.prompt.update(cx, |prompt, cx| {
                    prompt.set_draft("What can Xana do?", window, cx);
                });
            }
            ChatEvent::RetryRequested { message_id } => {
                self.retry_message(message_id.as_ref(), window, cx);
            }
            ChatEvent::RegenerateRequested { message_id } => {
                if let Some(text) = self.projection.preceding_user_text(message_id.as_ref()) {
                    let text = text.to_owned();
                    if let Err(reason) = self.composer.set_draft(text.clone()) {
                        self.projection.fail(reason);
                    } else {
                        self.projection.set_activity(
                            "Prior request copied to the composer; review it, then send or branch explicitly.",
                        );
                        self.prompt.update(cx, |prompt, cx| {
                            prompt.set_draft(text, window, cx);
                            prompt.focus(window, cx);
                        });
                    }
                }
            }
            ChatEvent::EditSubmitted { text, .. } => {
                let text = text.to_string();
                if let Err(reason) = self.composer.set_draft(text.clone()) {
                    self.projection.fail(reason);
                } else {
                    self.projection.set_activity(
                        "Edited text moved to the composer; immutable history is unchanged.",
                    );
                    self.prompt.update(cx, |prompt, cx| {
                        prompt.set_draft(text, window, cx);
                        prompt.focus(window, cx);
                    });
                }
            }
            _ => {}
        }
        self.sync_components(window, cx);
    }

    fn retry_message(&mut self, message_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(operation_id) = self.projection.operation_for_message(message_id) else {
            self.projection
                .fail("This response has no recoverable Xana Run identity.");
            return;
        };
        let Some((conversation, submission)) =
            self.recoverable_submissions.get(&operation_id).cloned()
        else {
            self.projection.fail(
                "The bounded retry record for this Run is no longer retained; copy the request instead.",
            );
            return;
        };
        if conversation != self.composer.active_key() {
            self.projection
                .fail("Retry is available only in the Conversation that owns this Run.");
        } else if self.projection.is_running() {
            self.projection
                .fail("Wait for or interrupt the active Run before retrying.");
        } else {
            self.submit_composer_submission(submission);
            self.projection
                .set_activity("Retrying the same bounded request as a new Turn…");
        }
        self.sync_components(window, cx);
    }

    fn handle_prompt_event(
        &mut self,
        event: &PromptBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PromptBarEvent::DraftChanged { draft, .. } => {
                if let Err(reason) = self.composer.set_draft(draft.to_string()) {
                    self.projection.fail(reason);
                }
            }
            PromptBarEvent::Submit { submission, .. } => {
                let text = submission.text().to_string();
                let attachments = self.composer.take_attachments();
                self.composer.clear_draft();
                let queued = self.composer.submission(text, attachments);
                if self.projection.is_running() {
                    match self.composer.queue(queued) {
                        Ok(()) => self
                            .projection
                            .set_activity("Follow-up queued for this Conversation"),
                        Err(reason) => self.projection.fail(reason),
                    }
                } else {
                    self.submit_composer_submission(queued);
                }
            }
            PromptBarEvent::CancelRequested { .. } => {
                if let Some(operation_id) = self.projection.active_operation()
                    && let Err(error) = self.runtime.interrupt(operation_id)
                {
                    self.projection.fail(error.message);
                }
                self.sync_components(window, cx);
                return;
            }
            PromptBarEvent::AttachRequested { .. } => {
                self.choose_attachments(window, cx);
                return;
            }
            PromptBarEvent::AttachmentRemoved { attachment_id, .. } => {
                self.composer.remove_attachment(attachment_id.as_ref());
            }
            PromptBarEvent::ModelChanged { model_id, .. } => {
                if model_id.as_ref() != self.projection.model() {
                    self.request_model_change(model_id.to_string(), window, cx);
                    return;
                }
            }
            PromptBarEvent::MentionSelected { .. }
            | PromptBarEvent::CommandSelected { .. }
            | PromptBarEvent::EnhanceRequested { .. } => {}
        }
        self.sync_components(window, cx);
    }

    fn request_model_change(&mut self, model: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.projection.is_running() {
            self.projection
                .fail("Wait for or interrupt the active Run before changing models.");
            self.sync_components(window, cx);
            return;
        }
        if self.projection.execution_owner() == "managed_codex" {
            match self.runtime.select_managed_model(model.clone()) {
                Ok(_) => self.projection.set_activity(format!(
                    "Changing later managed turns to {model}; the Codex thread is retained…"
                )),
                Err(error) => self.projection.fail(error.message),
            }
            self.sync_components(window, cx);
            return;
        }

        let connection = self.projection.connection().to_owned();
        let answer = window.prompt(
            PromptLevel::Warning,
            "Start a new Conversation with this model?",
            Some(
                "Native model changes do not rewrite this Conversation. Xana will update the default selection and open a fresh Conversation in the same workspace.",
            ),
            &["Start new Conversation", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                _ = this.update_in(cx, |this, window, cx| {
                    this.sync_components(window, cx);
                });
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                match this.control.select_model(&connection, &model, None) {
                    Ok(receipt) if receipt.requires_new_conversation => {
                        match this.runtime.new_conversation(this.selected_project.clone()) {
                            Ok(_) => this.projection.set_activity(format!(
                                "Opening a new native Conversation with {model}; prior history is unchanged…"
                            )),
                            Err(error) => this.projection.fail(format!(
                                "Model selection was saved, but the new Conversation could not open: {}",
                                error.message
                            )),
                        }
                    }
                    Ok(_) => this.projection.fail(
                        "The model selection did not require the expected new Conversation.",
                    ),
                    Err(error) => this.projection.fail(error.message),
                }
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn set_managed_reasoning(
        &mut self,
        effort: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projection.is_running() {
            self.projection
                .fail("Wait for or interrupt the active Run before changing reasoning.");
        } else {
            match self.runtime.set_managed_reasoning(effort.clone()) {
                Ok(_) => self.projection.set_activity(format!(
                    "Changing later managed turns to {} reasoning; the Codex thread is retained…",
                    effort.as_deref().unwrap_or("auto")
                )),
                Err(error) => self.projection.fail(error.message),
            }
        }
        self.sync_components(window, cx);
    }

    fn submit_composer_submission(&mut self, submission: QueuedSubmission) {
        let conversation = self.composer.active_key().to_owned();
        let text = submission.text.clone();
        match self
            .runtime
            .submit_with_attachments(text.clone(), submission.attachments.clone())
        {
            Ok(receipt) => {
                if let Some(operation_id) = receipt.operation_id {
                    self.recoverable_submissions
                        .insert(operation_id, (conversation.clone(), submission.clone()));
                    self.recoverable_order
                        .retain(|candidate| *candidate != operation_id);
                    self.recoverable_order.push_back(operation_id);
                    while self.recoverable_order.len() > MAX_RECOVERABLE_SUBMISSIONS {
                        if let Some(expired) = self.recoverable_order.pop_front() {
                            self.recoverable_submissions.remove(&expired);
                        }
                    }
                }
                self.pending_submissions.insert(
                    receipt.command_id,
                    (conversation, submission, receipt.operation_id),
                );
                if let Some(operation_id) = receipt.operation_id {
                    self.projection.append_user(operation_id, text);
                }
            }
            Err(error) => {
                self.composer.restore_submission(submission);
                self.projection.fail(error.message);
            }
        }
    }

    fn choose_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Choose one or more images for this Xana turn".into()),
        });
        let conversation = self.composer.active_key().to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let paths = match selection.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        this.projection
                            .fail(format!("Could not choose an attachment: {error}"));
                        this.sync_components(window, cx);
                    });
                    return;
                }
                Err(error) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        this.projection
                            .fail(format!("Attachment picker stopped: {error}"));
                        this.sync_components(window, cx);
                    });
                    return;
                }
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.stage_paths_for(&conversation, paths);
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn stage_paths_for(
        &mut self,
        conversation: &str,
        paths: impl IntoIterator<Item = std::path::PathBuf>,
    ) {
        for path in paths {
            match self.runtime.stage_image(path, true) {
                Ok(receipt) => {
                    self.pending_attachment_commands
                        .insert(receipt.command_id, conversation.to_owned());
                    self.projection.set_activity("Validating image attachment…");
                }
                Err(error) => self.projection.fail(error.message),
            }
        }
    }

    fn stage_dropped_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conversation = self.composer.active_key().to_owned();
        self.stage_paths_for(&conversation, paths.0.iter().cloned());
        self.sync_components(window, cx);
    }

    fn stage_clipboard_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.runtime.stage_clipboard_image() {
            Ok(receipt) => {
                self.pending_attachment_commands
                    .insert(receipt.command_id, self.composer.active_key().to_owned());
                self.projection.set_activity("Validating clipboard image…");
            }
            Err(error) => self.projection.fail(error.message),
        }
        self.sync_components(window, cx);
    }

    fn handle_queue_event(
        &mut self,
        event: &QueueEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            QueueEvent::Removed { id } => {
                self.composer.remove_queued(id.as_ref());
            }
            QueueEvent::SentNow { id } => {
                if let Some(submission) = self.composer.take_queued(id.as_ref()) {
                    if self.projection.is_running() {
                        let _ = self.composer.queue(submission);
                        self.projection.set_activity(
                            "The active Run must finish or be cancelled before this follow-up can send",
                        );
                    } else {
                        self.submit_composer_submission(submission);
                    }
                }
            }
            QueueEvent::MovedUp { id } => {
                self.composer.move_queued(id.as_ref(), true);
            }
            QueueEvent::MovedDown { id } => {
                self.composer.move_queued(id.as_ref(), false);
            }
            QueueEvent::EditRequested { id } => {
                if let Some(submission) = self.composer.take_queued(id.as_ref()) {
                    let draft = submission.text.clone();
                    self.composer.restore_submission(submission);
                    self.prompt.update(cx, |prompt, cx| {
                        prompt.set_draft(draft, window, cx);
                        prompt.focus(window, cx);
                    });
                }
            }
            QueueEvent::Cleared => self.composer.clear_queue(),
        }
        self.sync_components(window, cx);
    }

    fn handle_settings_event(
        &mut self,
        event: &SettingsViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsViewEvent::Close => self.request_close_settings(window, cx),
            SettingsViewEvent::StartNewConversation => {
                match self.runtime.new_conversation(self.selected_project.clone()) {
                    Ok(_) => self
                        .projection
                        .set_activity("Opening a new Conversation with the applied model/Profile…"),
                    Err(error) => self.projection.fail(error.message),
                }
            }
            SettingsViewEvent::Reload => {
                let result = self.runtime.reload_settings();
                self.track_settings_command(result, "Refreshing authoritative settings…", cx);
            }
            SettingsViewEvent::Review => {
                let Some(draft) = self.settings_draft.as_ref() else {
                    self.projection.fail("No settings changes are staged.");
                    self.sync_components(window, cx);
                    return;
                };
                let result = self.runtime.validate_settings(draft.id);
                self.track_settings_command(result, "Validating staged settings…", cx);
            }
            SettingsViewEvent::Apply => {
                let Some(draft) = self.settings_draft.as_ref() else {
                    self.projection.fail("No settings changes are staged.");
                    self.sync_components(window, cx);
                    return;
                };
                let result = self.runtime.commit_settings(draft.id);
                self.track_settings_command(result, "Applying settings transaction…", cx);
            }
            SettingsViewEvent::Discard => self.request_discard_settings(window, cx),
            SettingsViewEvent::Set {
                draft_id,
                key,
                value,
            } => {
                self.settings_receipt = None;
                let result = self
                    .runtime
                    .set_setting(*draft_id, key.clone(), value.clone());
                self.track_settings_command(result, format!("Staging {key}…"), cx);
            }
            SettingsViewEvent::Reset { draft_id, key } => {
                self.settings_receipt = None;
                let result = self.runtime.reset_setting(*draft_id, key.clone());
                self.track_settings_command(result, format!("Resetting {key}…"), cx);
            }
            SettingsViewEvent::Revert { draft_id, key } => {
                self.settings_receipt = None;
                let result = self.runtime.revert_setting(*draft_id, key.clone());
                self.track_settings_command(result, format!("Reverting staged {key}…"), cx);
            }
        }
        self.sync_components(window, cx);
    }

    fn track_settings_command(
        &mut self,
        result: Result<DesktopCommandReceipt, xana::desktop::DesktopError>,
        activity: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let activity = activity.into();
        match result {
            Ok(receipt) => {
                self.pending_settings_commands.insert(receipt.command_id);
                self.projection.set_activity(activity.clone());
                self.settings_view.update(cx, |settings, cx| {
                    settings.set_busy(Some(activity), cx);
                });
            }
            Err(error) => {
                self.projection.fail(error.message.clone());
                self.settings_view.update(cx, |settings, cx| {
                    settings.set_error(error.message, cx);
                });
            }
        }
    }

    fn request_close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .settings_draft
            .as_ref()
            .is_none_or(|draft| draft.pending_count == 0)
        {
            self.navigation = DesktopNavigationTarget::Conversation;
            self.projection.set_activity("Conversation opened");
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Keep staged settings?",
            Some("Return to the Conversation without applying. Your staged settings stay available until Xana exits or you discard them."),
            &["Keep draft and leave", "Stay in Settings"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, _window, cx| {
                this.navigation = DesktopNavigationTarget::Conversation;
                this.projection
                    .set_activity("Conversation opened; settings draft retained");
                cx.notify();
            });
        })
        .detach();
    }

    fn request_discard_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.settings_draft.as_ref() else {
            return;
        };
        let draft_id = draft.id;
        let answer = window.prompt(
            PromptLevel::Warning,
            "Discard staged settings?",
            Some("This removes only the process-local draft. Durable settings remain unchanged and the appearance preview is reverted."),
            &["Discard draft", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                let result = this.runtime.discard_settings(draft_id);
                this.track_settings_command(result, "Discarding staged settings…", cx);
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation = DesktopNavigationTarget::Settings;
        if self.settings_draft.is_none() {
            let result = self.runtime.begin_settings();
            self.track_settings_command(result, "Preparing a settings draft…", cx);
        }
        self.projection.set_activity("Settings opened");
        self.settings_view.update(cx, |settings, cx| {
            settings.focus_search(window, cx);
        });
        self.sync_components(window, cx);
    }

    fn open_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation = DesktopNavigationTarget::Diagnostics;
        if self.settings_draft.is_none() {
            let result = self.runtime.begin_settings();
            self.track_settings_command(result, "Preparing Diagnostics…", cx);
        }
        self.projection.set_activity("Diagnostics opened");
        self.settings_view.update(cx, |settings, cx| {
            settings.open_section(DesktopSettingsSection::Diagnostics, window, cx);
        });
        self.sync_components(window, cx);
    }

    fn open_espejo(&mut self, scope: EspejoScope, cx: &mut Context<Self>) {
        self.espejo.update(cx, |espejo, cx| espejo.open(scope, cx));
        self.navigation = DesktopNavigationTarget::Espejo;
        self.projection.set_activity("Espejo opened");
        cx.notify();
    }

    fn handle_espejo_event(
        &mut self,
        event: &EspejoViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EspejoViewEvent::Close => {
                self.navigation = DesktopNavigationTarget::Conversation;
                cx.notify();
            }
            EspejoViewEvent::OpenConversation {
                conversation_id,
                needs_attention,
            } => {
                self.open_espejo_conversation(conversation_id.clone(), *needs_attention, window, cx)
            }
            EspejoViewEvent::OpenDiagnostics => self.open_diagnostics(window, cx),
        }
    }

    fn open_espejo_conversation(
        &mut self,
        conversation_id: String,
        needs_attention: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_selection = Some(SidebarSelection::Conversation(conversation_id.clone()));
        self.selected_project =
            project_for_conversation(&self.navigation_snapshot, &conversation_id);
        if self.navigation_snapshot.selected_conversation.as_deref() != Some(&conversation_id) {
            match self.runtime.switch_conversation(&conversation_id) {
                Ok(_) => self.projection.set_activity(format!(
                    "Opening {}",
                    conversation_title(&self.navigation_snapshot, &conversation_id)
                )),
                Err(error) => {
                    self.projection.fail(error.message);
                    self.sync_components(window, cx);
                    return;
                }
            }
        }
        self.navigation = espejo_destination(needs_attention);
        self.sync_components(window, cx);
    }

    fn handle_command_search_event(
        &mut self,
        event: &CommandSearchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandSearchEvent::Selected { item_id, .. } => {
                let selected = WorkbenchCommand::from_stable_id(item_id.as_ref());
                self.dismiss_palette(window, cx);
                if let Some(command) = selected {
                    self.dispatch(command, window, cx);
                }
            }
            CommandSearchEvent::Dismissed { .. } => self.dismiss_palette(window, cx),
            CommandSearchEvent::QueryChanged { .. } => {}
        }
    }

    fn handle_sidebar_event(
        &mut self,
        event: &SidebarNavEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarNavEvent::CollapsedChanged { collapsed, .. } => {
                let mode = if *collapsed {
                    DesktopSidebarMode::Mini
                } else {
                    DesktopSidebarMode::Full
                };
                self.navigation_snapshot.sidebar_mode = mode;
                if let Err(error) = self.runtime.set_sidebar_mode(mode) {
                    self.projection.fail(error.message);
                }
            }
            SidebarNavEvent::Selected { item_id, .. } => {
                let item_id = item_id.as_ref();
                if let Some(conversation_id) = item_id.strip_prefix("conversation:") {
                    self.sidebar_selection =
                        Some(SidebarSelection::Conversation(conversation_id.to_owned()));
                    if self.navigation_snapshot.selected_conversation.as_deref()
                        != Some(conversation_id)
                    {
                        self.selected_project =
                            project_for_conversation(&self.navigation_snapshot, conversation_id);
                        match self.runtime.switch_conversation(conversation_id) {
                            Ok(_) => self.projection.set_activity(format!(
                                "Opening {}",
                                conversation_title(&self.navigation_snapshot, conversation_id)
                            )),
                            Err(error) => self.projection.fail(error.message),
                        }
                    }
                } else if let Some(project_id) = item_id.strip_prefix("project:") {
                    self.sidebar_selection = Some(SidebarSelection::Project(project_id.to_owned()));
                    self.selected_project = Some(project_id.to_owned());
                    self.projection.set_activity(format!(
                        "Project {} selected for the next Conversation",
                        project_title(&self.navigation_snapshot, project_id)
                    ));
                }
            }
            SidebarNavEvent::NewTaskRequested { .. } => {
                match self.runtime.new_conversation(self.selected_project.clone()) {
                    Ok(_) => self.projection.set_activity("Creating a new Conversation…"),
                    Err(error) => self.projection.fail(error.message),
                }
            }
            SidebarNavEvent::QueryChanged { .. } => {}
        }
        self.sync_components(window, cx);
    }

    fn open_project_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(SidebarSelection::Project(project_id)) = self.sidebar_selection.clone() else {
            self.projection
                .fail("Select a Project before choosing Rename.");
            self.sync_components(window, cx);
            return;
        };
        let name = project_title(&self.navigation_snapshot, &project_id);
        self.navigation_input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
        });
        self.navigation_dialog = Some(NavigationDialog::RenameProject { project_id });
        cx.notify();
    }

    fn commit_project_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(NavigationDialog::RenameProject { project_id }) = self.navigation_dialog.clone()
        else {
            return;
        };
        let name = self.navigation_input.read(cx).value().to_string();
        match self.runtime.rename_project(project_id, name) {
            Ok(_) => {
                self.navigation_dialog = None;
                self.projection.set_activity("Renaming Project…");
            }
            Err(error) => self.projection.fail(error.message),
        }
        self.sync_components(window, cx);
    }

    fn set_selected_project_archived(
        &mut self,
        archived: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(SidebarSelection::Project(project_id)) = self.sidebar_selection.clone() else {
            self.projection
                .fail("Select a Project before changing its lifecycle.");
            self.sync_components(window, cx);
            return;
        };
        let project = project_title(&self.navigation_snapshot, &project_id);
        let title = if archived {
            "Archive this Project?"
        } else {
            "Restore this Project?"
        };
        let detail = if archived {
            format!(
                "Archive {project}. Its workspace, Conversations, history, and artifacts will be preserved."
            )
        } else {
            format!("Restore {project} to the active Project list.")
        };
        let answer = window.prompt(
            if archived {
                PromptLevel::Warning
            } else {
                PromptLevel::Info
            },
            title,
            Some(&detail),
            &[if archived { "Archive" } else { "Restore" }, "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                match this.runtime.set_project_archived(project_id, archived) {
                    Ok(_) => this.projection.set_activity(if archived {
                        "Archiving Project; workspace and Conversations remain preserved…"
                    } else {
                        "Restoring Project…"
                    }),
                    Err(error) => this.projection.fail(error.message),
                }
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn ungroup_selected_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(SidebarSelection::Conversation(conversation_id)) = self.sidebar_selection.clone()
        else {
            self.projection
                .fail("Select a Conversation before choosing Ungroup.");
            self.sync_components(window, cx);
            return;
        };
        match self.runtime.ungroup_conversation(conversation_id) {
            Ok(_) => self
                .projection
                .set_activity("Moving Conversation to Ungrouped…"),
            Err(error) => self.projection.fail(error.message),
        }
        self.sync_components(window, cx);
    }

    fn open_conversation_move(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(SidebarSelection::Conversation(conversation_id)) = self.sidebar_selection.clone()
        else {
            self.projection
                .fail("Select a Conversation before choosing Move.");
            self.sync_components(window, cx);
            return;
        };
        self.navigation_dialog = Some(NavigationDialog::MoveConversation { conversation_id });
        cx.notify();
    }

    fn move_conversation_to_project(
        &mut self,
        project_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(NavigationDialog::MoveConversation { conversation_id }) =
            self.navigation_dialog.clone()
        else {
            return;
        };
        let Some(conversation) = self
            .navigation_snapshot
            .conversation(&conversation_id)
            .cloned()
        else {
            self.projection
                .fail("The selected Conversation is no longer available.");
            self.navigation_dialog = None;
            self.sync_components(window, cx);
            return;
        };
        let Some(project) = self
            .navigation_snapshot
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
        else {
            self.projection
                .fail("The selected Project is no longer available.");
            self.navigation_dialog = None;
            self.sync_components(window, cx);
            return;
        };
        self.navigation_dialog = None;
        let crosses_workspace = project.workspace_id.as_deref() != Some(&conversation.workspace_id);
        if !crosses_workspace {
            match self
                .runtime
                .move_conversation(conversation_id, project_id, false)
            {
                Ok(_) => self
                    .projection
                    .set_activity("Assigning Conversation to Project…"),
                Err(error) => self.projection.fail(error.message),
            }
            self.sync_components(window, cx);
            return;
        }

        let answer = window.prompt(
            PromptLevel::Warning,
            "Continue in another workspace?",
            Some(&format!(
                "{} belongs to {}. Continuing in {} creates a fresh linked Conversation, preserves the source, and copies no transcript text automatically.",
                conversation.title, conversation.workspace_label, project.name
            )),
            &["Create continuation", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                match this
                    .runtime
                    .move_conversation(conversation_id, project_id, true)
                {
                    Ok(_) => this
                        .projection
                        .set_activity("Creating source-preserving Project continuation…"),
                    Err(error) => this.projection.fail(error.message),
                }
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn branch_selected_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(SidebarSelection::Conversation(conversation_id)) = self.sidebar_selection.clone()
        else {
            self.projection
                .fail("Select a Conversation before choosing Branch.");
            self.sync_components(window, cx);
            return;
        };
        let Some(conversation) = self.navigation_snapshot.conversation(&conversation_id) else {
            self.projection
                .fail("The selected Conversation is no longer available.");
            self.sync_components(window, cx);
            return;
        };
        let Some(source_point) = conversation.branch_point.clone() else {
            self.projection
                .fail("This Conversation has no committed point that can be branched.");
            self.sync_components(window, cx);
            return;
        };
        let answer = window.prompt(
            PromptLevel::Info,
            "Branch this Conversation?",
            Some(&format!(
                "Create a new Conversation at exact committed point {source_point}. The source remains unchanged."
            )),
            &["Create branch", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                match this
                    .runtime
                    .branch_conversation(conversation_id, source_point)
                {
                    Ok(_) => this
                        .projection
                        .set_activity("Creating source-preserving Conversation branch…"),
                    Err(error) => this.projection.fail(error.message),
                }
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn dismiss_navigation_dialog(&mut self, cx: &mut Context<Self>) {
        self.navigation_dialog = None;
        cx.notify();
    }

    /// Returns false once the backend has stopped and there is nothing left to poll.
    fn drain_runtime_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut changed = self.drain_launch_intents(window, cx);
        let mut keep_running = true;
        for _ in 0..MAX_UPDATES_PER_FRAME {
            let update = match self.runtime.try_next() {
                Ok(Some(update)) => update,
                Ok(None) => break,
                Err(error) => {
                    self.projection.fail(error.message);
                    changed = true;
                    keep_running = false;
                    break;
                }
            };
            changed = true;
            self.notify_for_update(&update, window, cx);
            match update {
                DesktopUpdate::Snapshot(snapshot) => {
                    self.espejo.update(cx, |espejo, cx| {
                        espejo.replace_snapshot(&snapshot, cx);
                    });
                    self.navigation_snapshot = snapshot.navigation.clone();
                    self.layout = snapshot.layout.layout.clone();
                    self.selected_project = self
                        .navigation_snapshot
                        .selected_conversation
                        .as_deref()
                        .and_then(|id| project_for_conversation(&self.navigation_snapshot, id));
                    self.sidebar_selection = self
                        .navigation_snapshot
                        .selected_conversation
                        .clone()
                        .map(SidebarSelection::Conversation);
                    let conversation = snapshot
                        .attached_conversation
                        .clone()
                        .or_else(|| snapshot.navigation.selected_conversation.clone())
                        .unwrap_or_else(|| snapshot.session_id.clone());
                    self.composer.switch_to(conversation.clone());
                    self.projection.replace_snapshot(&snapshot);
                    self.model_options =
                        model_options_for(&self.control, self.projection.connection());
                    self.activate_conversation_ui(&conversation, window, cx);
                }
                DesktopUpdate::Navigation(navigation) => {
                    self.navigation_snapshot = navigation;
                }
                DesktopUpdate::Layout(layout) => {
                    self.layout = layout.layout.clone();
                    if let Some(warning) = layout.warning {
                        self.projection.set_activity(warning);
                    }
                }
                DesktopUpdate::Settings(snapshot) => {
                    self.apply_settings_appearance(&snapshot, cx);
                    self.settings_snapshot = snapshot;
                }
                DesktopUpdate::SettingsDraft(draft) => {
                    if let Some(draft) = draft.as_ref() {
                        self.settings_receipt = None;
                        self.apply_settings_appearance(&draft.preview, cx);
                    } else {
                        let snapshot = self.settings_snapshot.clone();
                        self.apply_settings_appearance(&snapshot, cx);
                    }
                    self.settings_draft = draft;
                }
                DesktopUpdate::SettingsReceipt(receipt) => {
                    self.settings_receipt = Some(receipt);
                    self.settings_view.update(cx, |settings, cx| {
                        settings.clear_operation_state(cx);
                    });
                }
                DesktopUpdate::AttachmentStaged {
                    command_id,
                    attachment,
                } => {
                    if let Some(conversation) = self.pending_attachment_commands.get(&command_id) {
                        if self.composer.stage_for(conversation, attachment) {
                            self.projection.set_activity("Image attachment is ready");
                        } else {
                            self.projection
                                .set_activity("Image attachment was already staged");
                        }
                    }
                }
                DesktopUpdate::Observation(observation) => {
                    let terminal = match &observation.event {
                        DesktopEvent::OperationState {
                            operation_id,
                            state,
                        } => Some((*operation_id, *state)),
                        DesktopEvent::ConversationCleared => {
                            self.recoverable_submissions.clear();
                            self.recoverable_order.clear();
                            None
                        }
                        _ => None,
                    };
                    if !self.projection.apply(observation)
                        && let Err(error) = self.runtime.request_snapshot()
                    {
                        self.projection.fail(error.message);
                    }
                    if let Some((operation_id, DesktopOperationState::Completed)) = terminal {
                        self.recoverable_submissions.remove(&operation_id);
                        self.recoverable_order
                            .retain(|candidate| *candidate != operation_id);
                    }
                }
                DesktopUpdate::HostObservation(observation) => {
                    self.projection.apply_host(&observation);
                    self.espejo.update(cx, |espejo, cx| {
                        espejo.apply_host(&observation, cx);
                    });
                }
                DesktopUpdate::CommandResult {
                    command_id,
                    accepted,
                    error,
                } => {
                    let settings_command = self.pending_settings_commands.remove(&command_id);
                    if settings_command {
                        if accepted {
                            self.settings_view.update(cx, |settings, cx| {
                                settings.clear_operation_state(cx);
                            });
                        } else {
                            let message = error.map(|error| error.message).unwrap_or_else(|| {
                                "Runtime rejected the settings command".to_owned()
                            });
                            self.projection.fail(message.clone());
                            self.settings_view.update(cx, |settings, cx| {
                                settings.set_error(message, cx);
                            });
                        }
                    } else if self
                        .pending_attachment_commands
                        .remove(&command_id)
                        .is_some()
                    {
                        if !accepted {
                            self.projection.fail(
                                error.map(|error| error.message).unwrap_or_else(|| {
                                    "Runtime rejected the attachment".to_owned()
                                }),
                            );
                        }
                    } else if let Some((conversation, submission, operation_id)) =
                        self.pending_submissions.remove(&command_id)
                    {
                        if !accepted {
                            if let Some(operation_id) = operation_id {
                                self.projection.reject_user(operation_id);
                                self.recoverable_submissions.remove(&operation_id);
                                self.recoverable_order
                                    .retain(|candidate| *candidate != operation_id);
                            }
                            self.composer
                                .restore_submission_for(&conversation, submission);
                            self.projection
                                .fail(error.map(|error| error.message).unwrap_or_else(|| {
                                    "Runtime rejected the submitted message".to_owned()
                                }));
                        }
                    } else if !accepted {
                        self.projection.fail(
                            error.map(|error| error.message).unwrap_or_else(|| {
                                "Runtime rejected the Desktop command".to_owned()
                            }),
                        );
                    }
                }
                DesktopUpdate::ResyncRequired { .. } => {
                    if let Err(error) = self.runtime.request_snapshot() {
                        self.projection.fail(error.message);
                    }
                }
                DesktopUpdate::BackendStopped { expected, error } => {
                    if !expected {
                        self.projection.fail(
                            error
                                .map(|error| error.message)
                                .unwrap_or_else(|| "Desktop runtime stopped".to_owned()),
                        );
                    }
                    if self.shutdown_pending && expected {
                        window.remove_window();
                    }
                    keep_running = false;
                }
            }
        }
        if keep_running
            && !self.projection.is_running()
            && self.pending_submissions.is_empty()
            && let Some(submission) = self.composer.pop_queued()
        {
            self.submit_composer_submission(submission);
            changed = true;
        }
        if changed {
            self.sync_components(window, cx);
        }
        keep_running
    }

    fn drain_launch_intents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        for _ in 0..MAX_UPDATES_PER_FRAME {
            let Some(intent) = self.instance.try_next() else {
                break;
            };
            changed = true;
            window.activate_window();
            self.navigation = navigation_for_intent(intent);
            if matches!(
                self.navigation,
                DesktopNavigationTarget::Settings | DesktopNavigationTarget::Diagnostics
            ) {
                let diagnostics = self.navigation == DesktopNavigationTarget::Diagnostics;
                if self.settings_draft.is_none()
                    && let Err(error) = self.runtime.begin_settings()
                {
                    self.projection.fail(error.message);
                }
                self.settings_view.update(cx, |settings, cx| {
                    if diagnostics {
                        settings.open_section(DesktopSettingsSection::Diagnostics, window, cx);
                    } else {
                        settings.focus_search(window, cx);
                    }
                });
            }
            self.projection.set_activity(format!(
                "Opened {} from another Xana launch",
                self.navigation.as_str()
            ));
        }
        changed
    }

    fn apply_settings_appearance(
        &self,
        snapshot: &DesktopSettingsSnapshot,
        cx: &mut Context<Self>,
    ) {
        let appearance = design_system::appearance_from_settings(
            snapshot,
            design_system::VisualSystem::read(cx).preferences(),
        );
        design_system::apply(appearance, cx);
    }

    fn notify_for_update(
        &mut self,
        update: &DesktopUpdate,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let kind = match update {
            DesktopUpdate::Observation(observation) => match &observation.event {
                DesktopEvent::PermissionRequired { .. } => Some(AttentionKind::Approval),
                DesktopEvent::OperationState {
                    state: xana::desktop::DesktopOperationState::Completed,
                    ..
                } => Some(AttentionKind::Completed),
                DesktopEvent::OperationState {
                    state: xana::desktop::DesktopOperationState::Failed,
                    ..
                }
                | DesktopEvent::Error(_) => Some(AttentionKind::Failed),
                _ => None,
            },
            DesktopUpdate::HostObservation(observation) => match &observation.event {
                DesktopHostEvent::ControllerChanged { change, .. }
                    if matches!(change.as_str(), "released" | "expired")
                        || change.starts_with("disconnected:") =>
                {
                    Some(AttentionKind::ControllerLost)
                }
                DesktopHostEvent::GlobalNotice(notice) if notice.kind == "hostfailure" => {
                    Some(AttentionKind::HostFailure)
                }
                DesktopHostEvent::RunFinished {
                    state: DesktopConversationState::Failed,
                    ..
                } => Some(AttentionKind::Failed),
                _ => None,
            },
            DesktopUpdate::BackendStopped {
                expected: false, ..
            } => Some(AttentionKind::HostFailure),
            _ => None,
        };
        let Some(kind) = kind else { return };
        let focus = if window.is_window_active() {
            ClientFocus::Focused
        } else {
            ClientFocus::Unfocused
        };
        let policy = self.projection.notification_policy().clone();
        let Some(candidate) = self
            .notifications
            .plan(&policy, focus, &AttentionSignal::new(kind))
        else {
            return;
        };
        let destination = notification_destination(candidate.destination);
        cx.show_system_notification(SystemNotification {
            tag: format!("xana-desktop-{destination}").into(),
            title: candidate.title.into(),
            body: candidate.body.into(),
            actions: Vec::new(),
        });
    }

    fn sync_components(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let messages = Arc::from(self.projection.messages());
        let progress = if let Some(failure) = self.projection.failure() {
            ProgressState::Failed(failure.to_owned().into())
        } else if self.projection.is_running() {
            ProgressState::Running
        } else {
            ProgressState::Complete
        };
        self.chat.update(cx, |chat, cx| {
            chat.set_messages(messages, window, cx);
        });
        let composer = self.composer.current().clone();
        let prompt_attachments = composer
            .attachments
            .iter()
            .map(prompt_attachment)
            .collect::<Vec<_>>();
        self.prompt.update(cx, |prompt, cx| {
            prompt.set_progress(progress, cx);
            prompt.set_draft(composer.draft, window, cx);
            prompt.set_attachments(prompt_attachments, cx);
            prompt.set_models(
                prompt_models(
                    &self.model_options,
                    self.projection.connection(),
                    self.projection.model(),
                ),
                cx,
            );
            prompt.set_selected_model(self.projection.model().to_owned(), cx);
        });
        self.command_search.update(cx, |search, cx| {
            search.set_items(
                commands::palette_items(self.projection.is_running()),
                window,
                cx,
            );
        });
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_sections(sidebar_sections(&self.navigation_snapshot), cx);
            if let Some(selected) = self.navigation_snapshot.selected_conversation.as_deref() {
                sidebar.set_active_item(conversation_item_id(selected), cx);
            }
            sidebar.set_collapsed(
                self.navigation_snapshot.sidebar_mode == DesktopSidebarMode::Mini,
                cx,
            );
        });
        self.settings_view.update(cx, |settings, cx| {
            settings.set_state(
                self.settings_snapshot.clone(),
                self.settings_draft.clone(),
                self.settings_receipt.clone(),
                window,
                cx,
            );
        });
        let navigation = self.navigation_snapshot.clone();
        let selected_project = self.selected_project.clone();
        let queue_counts = self.composer.queue_counts();
        self.espejo.update(cx, |espejo, cx| {
            espejo.update_navigation(navigation, selected_project, queue_counts, cx);
        });
        cx.notify();
    }

    fn schedule_layout_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout_save_generation = self.layout_save_generation.wrapping_add(1);
        let generation = self.layout_save_generation;
        let layout = self.layout.clone();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                if this.layout_save_generation != generation {
                    return;
                }
                if let Err(error) = this.runtime.save_layout(layout) {
                    this.projection.fail(error.message);
                    this.sync_components(window, cx);
                }
            });
        })
        .detach();
    }

    fn activate_layout_panel(
        &mut self,
        panel: DesktopPanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.layout.activate_panel(panel) {
            self.projection.fail(error.message);
        } else {
            self.schedule_layout_save(window, cx);
        }
        cx.notify();
    }

    fn close_layout_panel(
        &mut self,
        panel: DesktopPanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.layout.close_panel(panel) {
            self.projection.fail(error.message);
        } else {
            self.schedule_layout_save(window, cx);
        }
        cx.notify();
    }

    fn toggle_layout_maximize(
        &mut self,
        panel: DesktopPanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.layout.maximized().is_some() {
            self.layout.restore();
            self.schedule_layout_save(window, cx);
        } else if let Err(error) = self.layout.maximize(panel) {
            self.projection.fail(error.message);
        } else {
            self.schedule_layout_save(window, cx);
        }
        cx.notify();
    }

    fn reopen_layout_panel(
        &mut self,
        panel: DesktopPanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.layout.reopen_panel(panel) {
            self.projection.fail(error.message);
        } else {
            self.schedule_layout_save(window, cx);
        }
        cx.notify();
    }

    fn dock_layout_panel_at_root(
        &mut self,
        panel: DesktopPanelId,
        placement: DesktopDockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.layout.dock_panel_at_root(panel, placement) {
            self.projection.fail(error.message);
        } else {
            self.schedule_layout_save(window, cx);
        }
        cx.notify();
    }

    fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.runtime.reset_layout() {
            Ok(_) => self
                .projection
                .set_activity("Restoring this Conversation's Workbench layout…"),
            Err(error) => self.projection.fail(error.message),
        }
        self.sync_components(window, cx);
    }

    fn save_layout_as_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.runtime.save_layout_as_default() {
            Ok(_) => self
                .projection
                .set_activity("Saved this Workbench layout as the default"),
            Err(error) => self.projection.fail(error.message),
        }
        self.sync_components(window, cx);
    }

    fn clear_default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.runtime.clear_default_layout() {
            Ok(_) => self
                .projection
                .set_activity("Removed the saved default Workbench layout"),
            Err(error) => self.projection.fail(error.message),
        }
        self.sync_components(window, cx);
    }

    fn export_layout_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.native_paths.config_file.parent().map_or_else(
            || std::path::PathBuf::from("."),
            std::path::Path::to_path_buf,
        );
        let selection = cx.prompt_for_new_path(&directory, Some("xana-workbench-layout.toml"));
        let layout = self.layout.clone();
        cx.spawn_in(window, async move |this, cx| {
            let path = match selection.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        this.projection
                            .fail(format!("Could not choose a layout destination: {error}"));
                        this.sync_components(window, cx);
                    });
                    return;
                }
                Err(error) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        this.projection
                            .fail(format!("Layout destination picker stopped: {error}"));
                        this.sync_components(window, cx);
                    });
                    return;
                }
            };
            let written = cx
                .background_executor()
                .spawn(async move { layout.write_inert_file(&path).map(|()| path) })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                match written {
                    Ok(path) => this
                        .projection
                        .set_activity(format!("Exported inert layout to {}", path.display())),
                    Err(error) => this.projection.fail(error.message),
                }
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn import_layout_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select a Xana Workbench TOML layout".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match selection.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => path,
                    None => return,
                },
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        this.projection
                            .fail(format!("Could not choose a layout file: {error}"));
                        this.sync_components(window, cx);
                    });
                    return;
                }
                Err(error) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        this.projection
                            .fail(format!("Layout file picker stopped: {error}"));
                        this.sync_components(window, cx);
                    });
                    return;
                }
            };
            let loaded = cx
                .background_executor()
                .spawn(async move { DesktopWorkbenchLayout::read_inert_file(&path) })
                .await;
            let layout = match loaded {
                Ok(layout) => layout,
                Err(error) => {
                    _ = this.update_in(cx, |this, window, cx| {
                        this.projection.fail(error.message);
                        this.sync_components(window, cx);
                    });
                    return;
                }
            };
            let panel_names = layout
                .panels()
                .into_iter()
                .map(DesktopPanelId::label)
                .collect::<Vec<_>>()
                .join(", ");
            let confirmation = this.update_in(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    "Import this Workbench layout?",
                    Some(&format!(
                        "Panels: {panel_names}. Only bounded panel IDs, splits, sizes, and visibility will be applied."
                    )),
                    &["Import", "Cancel"],
                    cx,
                )
            });
            let Ok(confirmation) = confirmation else {
                return;
            };
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                match this.runtime.save_layout(layout) {
                    Ok(_) => this
                        .projection
                        .set_activity("Importing validated Workbench layout…"),
                    Err(error) => this.projection.fail(error.message),
                }
                this.sync_components(window, cx);
            });
        })
        .detach();
    }

    fn render_panel_library(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let open_panels = self.layout.panels();
        let optional_panels = [
            DesktopPanelId::Summary,
            DesktopPanelId::Artifacts,
            DesktopPanelId::Usage,
            DesktopPanelId::WorkingSet,
        ];
        h_flex()
            .w_full()
            .flex_none()
            .gap(tokens.spacing.xs)
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xs)
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .mr(tokens.spacing.xs)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Panels"),
            )
            .children(
                optional_panels
                    .into_iter()
                    .filter(|panel| !open_panels.contains(panel))
                    .map(|panel| {
                        Button::new(format!("reopen-panel-{panel:?}"))
                            .label(panel.label())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reopen_layout_panel(panel, window, cx);
                            }))
                    }),
            )
            .child(div().flex_1())
            .child(
                Button::new("import-layout")
                    .compact()
                    .label("Import…")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.import_layout_file(window, cx);
                    })),
            )
            .child(
                Button::new("export-layout")
                    .compact()
                    .label("Export…")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.export_layout_file(window, cx);
                    })),
            )
            .child(
                Button::new("save-layout-default")
                    .compact()
                    .label("Use as default")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save_layout_as_default(window, cx);
                    })),
            )
            .child(
                Button::new("clear-layout-default")
                    .compact()
                    .label("Clear default")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_default_layout(window, cx);
                    })),
            )
            .child(
                Button::new("reset-conversation-layout")
                    .compact()
                    .label("Reset")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reset_layout(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_layout_node(
        &self,
        node: &DesktopLayoutNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            DesktopLayoutNode::Split {
                id,
                axis,
                ratio_permille,
                first,
                second,
            } => {
                let first = self.render_layout_node(first, window, cx);
                let second = self.render_layout_node(second, window, cx);
                let split_id = id.clone();
                let workbench = cx.weak_entity();
                let group = match axis {
                    DesktopSplitAxis::Horizontal => h_resizable(id.clone()),
                    DesktopSplitAxis::Vertical => v_resizable(id.clone()),
                }
                .child(
                    resizable_panel()
                        .size(px(f32::from(*ratio_permille)))
                        .child(first),
                )
                .child(
                    resizable_panel()
                        .size(px(f32::from(1000_u16.saturating_sub(*ratio_permille))))
                        .child(second),
                )
                .on_resize(move |state, window, cx| {
                    let sizes = state.read(cx).sizes();
                    let Some((first, second)) = sizes.first().zip(sizes.get(1)) else {
                        return;
                    };
                    let total = *first + *second;
                    if total <= px(0.) {
                        return;
                    }
                    let ratio = ((*first / total) * 1000.).round().clamp(100., 900.) as u16;
                    _ = workbench.update(cx, |this, cx| {
                        if this.layout.resize_split(&split_id, ratio).is_ok() {
                            this.schedule_layout_save(window, cx);
                            cx.notify();
                        }
                    });
                });
                group.into_any_element()
            }
            DesktopLayoutNode::Stack { id, panels, active } => {
                self.render_panel_stack(id, panels, *active, window, cx)
            }
        }
    }

    fn render_panel_stack(
        &self,
        id: &str,
        panels: &[DesktopPanelId],
        active: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let panel = panels
            .get(active)
            .copied()
            .unwrap_or(DesktopPanelId::Unavailable);
        let tabs = h_flex()
            .gap(tokens.spacing.xs)
            .children(panels.iter().copied().map(|candidate| {
                let mut button =
                    Button::new(format!("{id}-tab-{candidate:?}")).label(candidate.label());
                if candidate == panel {
                    button = button.primary();
                }
                button.on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_layout_panel(candidate, window, cx);
                }))
            }));
        let can_close = panel != DesktopPanelId::Message;
        let dock_controls = [
            (
                DesktopDockPlacement::Tab,
                "Tab",
                "Move into the first panel stack",
            ),
            (DesktopDockPlacement::Left, "←", "Dock at the left edge"),
            (DesktopDockPlacement::Above, "↑", "Dock at the top edge"),
            (DesktopDockPlacement::Below, "↓", "Dock at the bottom edge"),
            (DesktopDockPlacement::Right, "→", "Dock at the right edge"),
        ];
        let controls = h_flex()
            .gap(tokens.spacing.xs)
            .children(
                dock_controls
                    .into_iter()
                    .map(|(placement, label, tooltip)| {
                        Button::new(format!("{id}-dock-{placement:?}"))
                            .compact()
                            .label(label)
                            .tooltip(tooltip)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.dock_layout_panel_at_root(panel, placement, window, cx);
                            }))
                    }),
            )
            .child(
                Button::new(format!("{id}-maximize"))
                    .compact()
                    .label(if self.layout.maximized().is_some() {
                        "Restore"
                    } else {
                        "Maximize"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_layout_maximize(panel, window, cx);
                    })),
            )
            .when(can_close, |controls| {
                controls.child(
                    Button::new(format!("{id}-close"))
                        .compact()
                        .label("Close")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_layout_panel(panel, window, cx);
                        })),
                )
            });
        v_flex()
            .id(id.to_owned())
            .size_full()
            .min_w_0()
            .min_h_0()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .justify_between()
                    .gap(tokens.spacing.sm)
                    .p(tokens.spacing.xs)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .child(tabs)
                    .child(controls),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_panel_body(panel, window, cx)),
            )
            .into_any_element()
    }

    fn render_panel_body(
        &self,
        panel: DesktopPanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match panel {
            DesktopPanelId::Conversation => self.chat.clone().into_any_element(),
            DesktopPanelId::Activity => self.render_activity_panel(window, cx),
            DesktopPanelId::Message => self.render_message_panel(window, cx),
            DesktopPanelId::Summary => self.placeholder_panel(
                "Summary",
                format!(
                    "{} · {} · {}",
                    self.projection.connection(),
                    self.projection.model(),
                    self.projection.host_lifecycle()
                ),
                cx,
            ),
            DesktopPanelId::Artifacts => self.placeholder_panel(
                "Artifacts",
                format!("{} retained artifact(s)", self.projection.artifact_count()),
                cx,
            ),
            DesktopPanelId::Usage => self.placeholder_panel(
                "Usage",
                "Usage observations remain source-qualified in Activity.",
                cx,
            ),
            DesktopPanelId::WorkingSet => self.placeholder_panel(
                "Working Set",
                "Pin trusted items here without changing their lifecycle.",
                cx,
            ),
            DesktopPanelId::Unavailable => self.placeholder_panel(
                "Unavailable panel",
                "This imported panel is not part of Xana's trusted catalog.",
                cx,
            ),
        }
    }

    fn render_message_panel(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let queue = self
            .composer
            .current()
            .queue
            .iter()
            .map(|submission| {
                let attachment_count = submission.attachments.len();
                let note = if attachment_count == 0 {
                    "after the active Run".to_owned()
                } else {
                    format!(
                        "after the active Run · {attachment_count} attachment{}",
                        if attachment_count == 1 { "" } else { "s" }
                    )
                };
                QueuedMessage::new(submission.id.clone(), submission.text.clone()).note(note)
            })
            .collect::<Vec<_>>();
        let workbench = cx.weak_entity();
        let clipboard_workbench = cx.weak_entity();
        let dropped_workbench = cx.weak_entity();
        let settings_workbench = cx.weak_entity();
        let reasoning_controls =
            (self.projection.execution_owner() == "managed_codex").then(|| {
                let descriptor = self
                    .model_options
                    .iter()
                    .find(|model| model.id == self.projection.model());
                let mut efforts = vec![None];
                efforts.extend(
                    descriptor
                        .into_iter()
                        .flat_map(|model| model.reasoning_efforts.iter().cloned().map(Some)),
                );
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Reasoning"),
                    )
                    .children(efforts.into_iter().map(|effort| {
                        let selected = self.projection.reasoning_effort() == effort.as_deref();
                        let label = effort.clone().unwrap_or_else(|| "auto".to_owned());
                        let workbench = cx.weak_entity();
                        let mut button = Button::new(format!("reasoning-{label}"))
                            .compact()
                            .label(label);
                        if selected {
                            button = button.primary();
                        }
                        button.on_click(move |_, window, cx| {
                            _ = workbench.update(cx, |this, cx| {
                                this.set_managed_reasoning(effort.clone(), window, cx);
                            });
                        })
                    }))
                    .into_any_element()
            });
        v_flex()
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.sm)
            .p(tokens.spacing.sm)
            .drag_over::<ExternalPaths>(|panel, _, _, _| panel.border_2())
            .on_drop(move |paths: &ExternalPaths, window, cx| {
                _ = dropped_workbench.update(cx, |this, cx| {
                    this.stage_dropped_paths(paths, window, cx);
                });
            })
            .when(!queue.is_empty(), |panel| {
                panel.child(
                    div().max_h(rems(12.)).overflow_y_scrollbar().child(
                        MessageQueue::new("xana-message-queue")
                            .items(queue)
                            .editable(true)
                            .on_event(move |event, window, cx| {
                                _ = workbench.update(cx, |this, cx| {
                                    this.handle_queue_event(event, window, cx);
                                });
                            }),
                    ),
                )
            })
            .child(
                h_flex()
                    .justify_between()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("xana-open-conversation-settings")
                            .compact()
                            .label("Model & Profile settings")
                            .tooltip("Native changes apply to a new Conversation")
                            .on_click(move |_, window, cx| {
                                _ = settings_workbench.update(cx, |this, cx| {
                                    this.open_settings(window, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("xana-paste-clipboard-image")
                            .compact()
                            .label("Paste clipboard image")
                            .tooltip("Stage the image currently on the native clipboard")
                            .on_click(move |_, window, cx| {
                                _ = clipboard_workbench.update(cx, |this, cx| {
                                    this.stage_clipboard_image(window, cx);
                                });
                            }),
                    ),
            )
            .when_some(reasoning_controls, |panel, controls| panel.child(controls))
            .child(div().w_full().flex_none().child(self.prompt.clone()))
            .into_any_element()
    }

    fn render_activity_panel(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let facts = self.projection.conversation_facts();
        let status = if self.projection.failure().is_some() {
            StatusBadge::new("activity-runtime-status", "Needs attention")
                .tone(StatusTone::Danger)
                .into_any_element()
        } else if self.projection.is_running() {
            LoadingState::new()
                .label(self.projection.latest_activity().to_owned())
                .into_any_element()
        } else {
            StatusBadge::new("activity-runtime-status", "Ready")
                .tone(StatusTone::Success)
                .into_any_element()
        };
        let round_controls = self
            .projection
            .pending_round_budget()
            .cloned()
            .map(|suspension| {
                let continue_suspension = suspension.clone();
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("activity-continue-round-budget")
                            .primary()
                            .label("Continue")
                            .disabled(!suspension.can_continue)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.decide_round_budget(
                                    continue_suspension.clone(),
                                    true,
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("activity-stop-round-budget")
                            .label("Stop")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.decide_round_budget(suspension.clone(), false, window, cx);
                            })),
                    )
            });
        let profile = facts.profile.as_deref().unwrap_or("Unspecified");
        let execution = facts.execution.last();
        let execution_summary = execution.map_or_else(
            || {
                format!(
                    "{} · embedded · authority unavailable",
                    self.projection.connection()
                )
            },
            |execution| {
                format!(
                    "{} · {} · {} · approvals {}",
                    execution.owner,
                    execution.host_location,
                    execution.workspace_authority,
                    execution.approval_policy
                )
            },
        );
        let prompt_summary = facts
            .prompt_ledger
            .estimated_input_tokens
            .zip(facts.prompt_ledger.input_budget_tokens)
            .map_or_else(
                || {
                    facts
                        .prompt_ledger
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "Prompt ledger unavailable".to_owned())
                },
                |(used, budget)| format!("Prompt estimate {used} / {budget} input tokens"),
            );
        let activity_items = facts
            .activity
            .iter()
            .map(|activity| render_activity_item(activity, cx))
            .collect::<Vec<_>>();
        let approval_cards = self
            .projection
            .pending_approvals()
            .iter()
            .map(|approval| {
                let permission_id = approval.id;
                ApprovalCard::new(
                    format!("desktop-approval-{permission_id}"),
                    format!("Allow {}?", approval.tool),
                )
                .description(format!(
                    "{}\nScope: {}\nThis decision applies once to the current Run.",
                    approval.effect, approval.scope
                ))
                .approve_label("Allow once")
                .reject_label("Deny")
                .on_event(cx.listener(move |this, event: &ApprovalEvent, window, cx| {
                    let allow_once = matches!(event, ApprovalEvent::Approved { .. });
                    match this.runtime.decide_permission(permission_id, allow_once) {
                        Ok(_) => this.projection.set_activity(if allow_once {
                            "Sending one-time approval…"
                        } else {
                            "Sending denial…"
                        }),
                        Err(error) => this.projection.fail(error.message),
                    }
                    this.sync_components(window, cx);
                }))
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let completion = facts.completions.last().map(|receipt| {
            let checks_passed = receipt.checks.iter().filter(|check| check.passed).count();
            v_flex()
                .gap(tokens.spacing.xs)
                .p(tokens.spacing.sm)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(tokens.radius.md)
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Latest receipt"),
                        )
                        .child(
                            StatusBadge::new("latest-completion-status", receipt.status.clone())
                                .tone(if receipt.status == "completed" {
                                    StatusTone::Success
                                } else {
                                    StatusTone::Danger
                                }),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Run {} · {checks_passed}/{} checks · {} artifact(s)",
                            receipt.operation_id,
                            receipt.checks.len(),
                            receipt.artifact_ids.len()
                        )),
                )
        });
        v_flex()
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.lg)
            .child(status)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.projection.latest_activity().to_owned()),
            )
            .when_some(round_controls, |panel, controls| panel.child(controls))
            .child(
                v_flex()
                    .gap(tokens.spacing.xs)
                    .p(tokens.spacing.sm)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(tokens.radius.md)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("Profile: {profile}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(execution_summary),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(prompt_summary),
                    ),
            )
            .when_some(completion, |panel, receipt| panel.child(receipt))
            .child(
                v_flex()
                    .id("activity-scroll")
                    .flex_1()
                    .min_h_0()
                    .gap(tokens.spacing.sm)
                    .overflow_y_scrollbar()
                    .when(approval_cards.is_empty() && activity_items.is_empty(), |list| {
                        list.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No detailed Activity has been recorded for this Conversation yet."),
                        )
                    })
                    .children(approval_cards)
                    .children(activity_items),
            )
            .into_any_element()
    }

    fn placeholder_panel(
        &self,
        title: impl Into<gpui::SharedString>,
        body: impl Into<gpui::SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let title = title.into();
        let body = body.into();
        v_flex()
            .size_full()
            .gap(tokens.spacing.sm)
            .p(tokens.spacing.lg)
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(body),
            )
            .into_any_element()
    }

    fn render_navigation_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.navigation_dialog.clone()?;
        let tokens = cx.theme().semantic_tokens();
        let body = match dialog {
            NavigationDialog::RenameProject { project_id } => v_flex()
                .gap(tokens.spacing.md)
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Rename Project"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Change only Xana's local Project label for {}. The workspace and Conversations are unchanged.",
                            project_title(&self.navigation_snapshot, &project_id)
                        )),
                )
                .child(Input::new(&self.navigation_input))
                .child(
                    h_flex()
                        .justify_end()
                        .gap(tokens.spacing.sm)
                        .child(
                            Button::new("cancel-project-rename")
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dismiss_navigation_dialog(cx);
                                })),
                        )
                        .child(
                            Button::new("commit-project-rename")
                                .primary()
                                .label("Rename")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.commit_project_rename(window, cx);
                                })),
                        ),
                )
                .into_any_element(),
            NavigationDialog::MoveConversation { conversation_id } => {
                let current_project =
                    project_for_conversation(&self.navigation_snapshot, &conversation_id);
                let targets = self
                    .navigation_snapshot
                    .projects
                    .iter()
                    .filter(|project| {
                        !project.archived
                            && project.workspace_status == DesktopWorkspaceStatus::Available
                            && current_project.as_deref() != Some(project.id.as_str())
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                v_flex()
                    .gap(tokens.spacing.md)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Move or continue in Project"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Same-workspace moves preserve this Conversation. Cross-workspace choices require a second confirmation and create a linked continuation."),
                    )
                    .child(
                        v_flex()
                            .max_h(rems(24.))
                            .overflow_y_scrollbar()
                            .gap(tokens.spacing.xs)
                            .when(targets.is_empty(), |list| {
                                list.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No other active, available Project is configured."),
                                )
                            })
                            .children(targets.into_iter().map(|project| {
                                let project_id = project.id.clone();
                                Button::new(format!("move-to-project-{}", project.id))
                                    .label(format!(
                                        "{} — {}",
                                        project.name, project.workspace_label
                                    ))
                                    .w_full()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.move_conversation_to_project(
                                            project_id.clone(),
                                            window,
                                            cx,
                                        );
                                    }))
                            })),
                    )
                    .child(
                        h_flex().justify_end().child(
                            Button::new("cancel-conversation-move")
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dismiss_navigation_dialog(cx);
                                })),
                        ),
                    )
                    .into_any_element()
            }
        };
        Some(
            div()
                .id("xana-navigation-dialog-overlay")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_center()
                .p(tokens.spacing.xl)
                .bg(cx.theme().background.opacity(0.72))
                .child(
                    div()
                        .id("xana-navigation-dialog")
                        .role(Role::Dialog)
                        .aria_label("Xana navigation action")
                        .w_full()
                        .max_w(rems(36.))
                        .max_h(rems(34.))
                        .overflow_hidden()
                        .rounded(tokens.radius.lg)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().popover)
                        .shadow_lg()
                        .p(tokens.spacing.lg)
                        .child(body),
                )
                .into_any_element(),
        )
    }

    fn decide_round_budget(
        &mut self,
        suspension: DesktopRoundBudgetSuspension,
        should_continue: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = if should_continue {
            self.runtime.continue_round_budget(&suspension)
        } else {
            self.runtime.stop_round_budget(&suspension)
        };
        if let Err(error) = result {
            self.projection.fail(error.message);
        }
        self.sync_components(window, cx);
    }

    fn dispatch(&mut self, command: WorkbenchCommand, window: &mut Window, cx: &mut Context<Self>) {
        debug_assert_eq!(
            WorkbenchCommand::from_stable_id(command.stable_id()),
            Some(command)
        );
        match command {
            WorkbenchCommand::ShowCommandPalette => self.show_palette(window, cx),
            WorkbenchCommand::Quit => {
                self.request_close(window, cx);
            }
            WorkbenchCommand::Minimize => window.minimize_window(),
            WorkbenchCommand::OpenDocumentation => {
                if documentation_url_is_safe(DOCUMENTATION_URL) {
                    cx.open_url(DOCUMENTATION_URL);
                }
            }
            WorkbenchCommand::OpenConfigurationFile => {
                if trusted_regular_file(&self.native_paths.config_file) {
                    cx.open_with_system(&self.native_paths.config_file);
                } else {
                    self.projection.fail(
                        "Xana's configuration file is unavailable; run setup or Diagnostics.",
                    );
                    self.sync_components(window, cx);
                }
            }
            WorkbenchCommand::RevealLogs => {
                if trusted_directory(&self.native_paths.logs_directory) {
                    cx.reveal_path(&self.native_paths.logs_directory);
                } else {
                    self.projection
                        .fail("Xana's logs directory is unavailable; run Diagnostics.");
                    self.sync_components(window, cx);
                }
            }
            WorkbenchCommand::ClearConversation => {
                if let Err(error) = self.runtime.clear() {
                    self.projection.fail(error.message);
                    self.sync_components(window, cx);
                }
            }
            WorkbenchCommand::InterruptRun => {
                if let Some(operation_id) = self.projection.active_operation()
                    && let Err(error) = self.runtime.interrupt(operation_id)
                {
                    self.projection.fail(error.message);
                    self.sync_components(window, cx);
                }
            }
            WorkbenchCommand::ShowActivity => {
                self.navigation = DesktopNavigationTarget::Activity;
                self.projection.set_activity("Activity opened");
                cx.notify();
            }
            WorkbenchCommand::ShowEspejo => self.open_espejo(EspejoScope::Global, cx),
            WorkbenchCommand::ShowSettings => self.open_settings(window, cx),
        }
    }

    fn show_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = true;
        self.command_search.update(cx, |search, cx| {
            search.set_query("", window, cx);
            search.focus(window, cx);
        });
        cx.notify();
    }

    fn dismiss_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.palette_open {
            return;
        }
        self.palette_open = false;
        let prompt = self.chat.read(cx).prompt_bar().clone();
        prompt.update(cx, |prompt, cx| {
            prompt.focus(window, cx);
        });
        cx.notify();
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.shutdown_pending || self.close_prompt_open {
            return false;
        }
        if !self.projection.is_running() {
            self.begin_shutdown(window, cx);
            return false;
        }

        self.close_prompt_open = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "Xana is still working",
            Some("Keep Xana open, cancel the active work and quit, or return to the Conversation."),
            &["Keep Xana open", "Cancel work and quit", "Return"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let choice = match answer.await {
                Ok(0) => LastWindowChoice::KeepXanaOpen,
                Ok(1) => LastWindowChoice::CancelAndQuit,
                _ => LastWindowChoice::Return,
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.close_prompt_open = false;
                match last_window_effect(choice) {
                    LastWindowEffect::RequestShutdown => this.begin_shutdown(window, cx),
                    LastWindowEffect::KeepOpen => {
                        this.projection
                            .set_activity("Xana remains open while work continues");
                        window.activate_window();
                        cx.notify();
                    }
                    LastWindowEffect::NoChange => {
                        window.activate_window();
                    }
                }
            });
        })
        .detach();
        false
    }

    fn begin_shutdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shutdown_pending {
            return;
        }
        match self.runtime.request_shutdown() {
            Ok(_) => {
                self.shutdown_pending = true;
                self.projection.set_activity("Closing Xana safely…");
            }
            Err(error) => {
                self.projection.fail(error.message);
                window.request_attention();
            }
        }
        self.sync_components(window, cx);
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let layout = self.layout.clone();
        let showing_settings = matches!(
            self.navigation,
            DesktopNavigationTarget::Settings | DesktopNavigationTarget::Diagnostics
        );
        let showing_espejo = self.navigation == DesktopNavigationTarget::Espejo;
        let canvas = if showing_settings {
            self.settings_view.clone().into_any_element()
        } else if showing_espejo {
            self.espejo.clone().into_any_element()
        } else {
            match layout.maximized() {
                Some(panel) => self.render_panel_stack(
                    "maximized-panel",
                    std::slice::from_ref(&panel),
                    0,
                    window,
                    cx,
                ),
                None => self.render_layout_node(layout.root(), window, cx),
            }
        };
        let panel_library =
            (!showing_settings && !showing_espejo).then(|| self.render_panel_library(cx));
        let navigation_dialog = self.render_navigation_dialog(cx);
        let sidebar_is_full = self.navigation_snapshot.sidebar_mode == DesktopSidebarMode::Full;
        let sidebar_selection = self.sidebar_selection.clone();
        let menu_snapshot = self.navigation_snapshot.clone();
        let context_selection = sidebar_selection.clone();
        let context_snapshot = menu_snapshot.clone();
        let sidebar_actions = h_flex()
            .w_full()
            .gap(tokens.spacing.xs)
            .p(tokens.spacing.sm)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("sidebar-new-conversation")
                    .label(if sidebar_is_full { "New" } else { "+" })
                    .w_full()
                    .on_click(cx.listener(|this, _, window, cx| {
                        match this.runtime.new_conversation(this.selected_project.clone()) {
                            Ok(_) => this.projection.set_activity("Creating a new Conversation…"),
                            Err(error) => this.projection.fail(error.message),
                        }
                        this.sync_components(window, cx);
                    })),
            )
            .child(
                Button::new("sidebar-navigation-actions")
                    .label(if sidebar_is_full { "Actions…" } else { "…" })
                    .disabled(sidebar_selection.is_none())
                    .dropdown_menu(move |menu, _, _| {
                        navigation_menu(menu, sidebar_selection.clone(), &menu_snapshot)
                    }),
            );
        let sidebar_footer = v_flex()
            .w_full()
            .flex_none()
            .gap(tokens.spacing.xs)
            .p(tokens.spacing.sm)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("open-espejo")
                    .label(if sidebar_is_full { "Espejo" } else { "E" })
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_espejo(EspejoScope::Global, cx);
                    })),
            )
            .child(
                Button::new("open-settings")
                    .label(if sidebar_is_full { "Settings" } else { "S" })
                    .w_full()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings(window, cx);
                    })),
            );
        let sidebar = v_flex()
            .h_full()
            .flex_none()
            .w(if sidebar_is_full {
                rems(20.)
            } else {
                rems(4.5)
            })
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .id("xana-sidebar-navigation")
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar.clone())
                    .context_menu(move |menu, _, _| {
                        navigation_menu(menu, context_selection.clone(), &context_snapshot)
                    }),
            )
            .child(sidebar_actions)
            .child(sidebar_footer);
        let status_bar = h_flex()
            .w_full()
            .flex_none()
            .justify_between()
            .gap(tokens.spacing.md)
            .px(tokens.spacing.md)
            .py(tokens.spacing.xs)
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!(
                "{} · {} · {} active Run · {} approval(s)",
                self.projection.host_lifecycle(),
                self.navigation.as_str(),
                usize::from(self.projection.is_running()),
                self.projection.pending_approval_count(),
            ))
            .child(format!(
                "{} notice(s) · {}",
                self.projection.global_notice_count(),
                self.projection.latest_activity()
            ));
        let main = h_flex().size_full().min_h_0().child(sidebar).child(
            v_flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(div().flex_1().min_h_0().child(canvas))
                .when_some(panel_library, |main, library| main.child(library)),
        );

        div()
            .id("xana-workbench")
            .key_context(commands::WORKBENCH_KEY_CONTEXT)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &ShowCommandPalette, window, cx| {
                this.dispatch(WorkbenchCommand::ShowCommandPalette, window, cx);
            }))
            .on_action(cx.listener(|this, _: &QuitXana, window, cx| {
                this.dispatch(WorkbenchCommand::Quit, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MinimizeWindow, window, cx| {
                this.dispatch(WorkbenchCommand::Minimize, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenDocumentation, window, cx| {
                this.dispatch(WorkbenchCommand::OpenDocumentation, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenConfigurationFile, window, cx| {
                this.dispatch(WorkbenchCommand::OpenConfigurationFile, window, cx);
            }))
            .on_action(cx.listener(|this, _: &RevealLogs, window, cx| {
                this.dispatch(WorkbenchCommand::RevealLogs, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearConversation, window, cx| {
                this.dispatch(WorkbenchCommand::ClearConversation, window, cx);
            }))
            .when(self.projection.is_running(), |root| {
                root.on_action(cx.listener(|this, _: &InterruptRun, window, cx| {
                    this.dispatch(WorkbenchCommand::InterruptRun, window, cx);
                }))
            })
            .on_action(cx.listener(|this, _: &ShowActivity, window, cx| {
                this.dispatch(WorkbenchCommand::ShowActivity, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowEspejo, window, cx| {
                this.dispatch(WorkbenchCommand::ShowEspejo, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowSettings, window, cx| {
                this.dispatch(WorkbenchCommand::ShowSettings, window, cx);
            }))
            .on_action(cx.listener(|this, _: &RenameSelectedProject, window, cx| {
                this.open_project_rename(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ArchiveSelectedProject, window, cx| {
                this.set_selected_project_archived(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &RestoreSelectedProject, window, cx| {
                this.set_selected_project_archived(false, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &MoveSelectedConversation, window, cx| {
                    this.open_conversation_move(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &UngroupSelectedConversation, window, cx| {
                    this.ungroup_selected_conversation(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &BranchSelectedConversation, window, cx| {
                    this.branch_selected_conversation(window, cx);
                }),
            )
            .child(main)
            .child(status_bar)
            .when(self.palette_open, |root| {
                root.child(
                    div()
                        .id("xana-command-palette-overlay")
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_center()
                        .items_start()
                        .pt(rems(6.))
                        .px(tokens.spacing.xl)
                        .bg(cx.theme().background.opacity(0.72))
                        .child(
                            div()
                                .id("xana-command-palette-dialog")
                                .role(Role::Dialog)
                                .aria_label("Xana command palette")
                                .w_full()
                                .max_w(rems(48.))
                                .max_h(rems(34.))
                                .overflow_hidden()
                                .rounded(tokens.radius.lg)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().popover)
                                .shadow_lg()
                                .p(tokens.spacing.md)
                                .child(self.command_search.clone()),
                        ),
                )
            })
            .when_some(navigation_dialog, |root, dialog| root.child(dialog))
    }
}

fn prompt_attachment(attachment: &DesktopAttachment) -> Attachment {
    let detail = attachment
        .width
        .zip(attachment.height)
        .map(|(width, height)| format!("{width}×{height}"))
        .unwrap_or_else(|| attachment.media_type.clone());
    Attachment::new(attachment.id.clone(), attachment.name.clone())
        .size_bytes(attachment.byte_len)
        .detail(detail)
}

fn sidebar_sections(snapshot: &DesktopNavigationSnapshot) -> Vec<SidebarSection> {
    let projects = snapshot.projects.iter().map(|project| {
        let status = match project.workspace_status {
            DesktopWorkspaceStatus::Available if project.archived => Some("Archived"),
            DesktopWorkspaceStatus::Available => None,
            DesktopWorkspaceStatus::Missing => Some("Missing"),
            DesktopWorkspaceStatus::ChangedIdentity => Some("Changed"),
        };
        let mut item = SidebarNavItem::new(project_item_id(&project.id), project.name.clone())
            .icon(IconName::Folder)
            .children(project.conversations.iter().map(conversation_item));
        if let Some(status) = status {
            item = item.badge(status);
        }
        item
    });
    let mut sections = vec![SidebarSection::new("projects", "Projects").items(projects)];
    sections.push(
        SidebarSection::new("ungrouped", "Conversations")
            .items(snapshot.ungrouped.iter().map(conversation_item)),
    );
    sections
}

fn navigation_menu(
    menu: PopupMenu,
    selection: Option<SidebarSelection>,
    snapshot: &DesktopNavigationSnapshot,
) -> PopupMenu {
    match selection {
        Some(SidebarSelection::Project(project_id)) => {
            let project = snapshot
                .projects
                .iter()
                .find(|project| project.id == project_id);
            let archived = project.is_some_and(|project| project.archived);
            menu.label(project.map_or("Selected Project", |project| project.name.as_str()))
                .menu("Rename…", Box::new(RenameSelectedProject))
                .menu_with_disabled("Archive…", Box::new(ArchiveSelectedProject), archived)
                .menu_with_disabled("Restore…", Box::new(RestoreSelectedProject), !archived)
        }
        Some(SidebarSelection::Conversation(conversation_id)) => {
            let conversation = snapshot.conversation(&conversation_id);
            let grouped = project_for_conversation(snapshot, &conversation_id).is_some();
            let branchable = conversation
                .and_then(|item| item.branch_point.as_ref())
                .is_some();
            menu.label(conversation.map_or("Selected Conversation", |item| item.title.as_str()))
                .menu(
                    "Move or continue in Project…",
                    Box::new(MoveSelectedConversation),
                )
                .menu_with_disabled(
                    "Move to Ungrouped",
                    Box::new(UngroupSelectedConversation),
                    !grouped,
                )
                .menu_with_disabled(
                    "Branch at latest committed point…",
                    Box::new(BranchSelectedConversation),
                    !branchable,
                )
        }
        None => menu.label("Select a Project or Conversation"),
    }
}

fn conversation_item(conversation: &xana::desktop::DesktopConversationNode) -> SidebarNavItem {
    let badge = if conversation.needs_attention {
        "Needs you"
    } else {
        conversation.state.as_str()
    };
    SidebarNavItem::new(
        conversation_item_id(&conversation.id),
        conversation.title.clone(),
    )
    .icon(IconName::SquareTerminal)
    .badge(badge)
}

fn conversation_item_id(id: &str) -> String {
    format!("conversation:{id}")
}

fn project_item_id(id: &str) -> String {
    format!("project:{id}")
}

fn project_for_conversation(snapshot: &DesktopNavigationSnapshot, id: &str) -> Option<String> {
    snapshot.projects.iter().find_map(|project| {
        project
            .conversations
            .iter()
            .any(|conversation| conversation.id == id)
            .then(|| project.id.clone())
    })
}

fn conversation_title(snapshot: &DesktopNavigationSnapshot, id: &str) -> String {
    snapshot
        .conversation(id)
        .map(|conversation| conversation.title.clone())
        .unwrap_or_else(|| "Conversation".to_owned())
}

fn project_title(snapshot: &DesktopNavigationSnapshot, id: &str) -> String {
    snapshot
        .projects
        .iter()
        .find(|project| project.id == id)
        .map(|project| project.name.clone())
        .unwrap_or_else(|| "Project".to_owned())
}

fn espejo_destination(needs_attention: bool) -> DesktopNavigationTarget {
    if needs_attention {
        DesktopNavigationTarget::Activity
    } else {
        DesktopNavigationTarget::Conversation
    }
}

fn model_options_for(control: &DesktopControlPlane, connection: &str) -> Vec<DesktopModelOption> {
    control
        .connections()
        .ok()
        .and_then(|snapshot| {
            snapshot
                .connections
                .into_iter()
                .find(|candidate| candidate.id == connection)
                .map(|candidate| candidate.models)
        })
        .unwrap_or_default()
}

fn prompt_models(
    options: &[DesktopModelOption],
    connection: &str,
    selected: &str,
) -> Vec<PromptModel> {
    let mut models = options
        .iter()
        .map(|model| {
            let capabilities = [
                model
                    .reasoning
                    .is_some_and(|value| value)
                    .then_some("reasoning"),
                model
                    .input_modalities
                    .iter()
                    .any(|input| input == "image")
                    .then_some("vision"),
                model.tools.is_some_and(|value| value).then_some("tools"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            let description = if capabilities.is_empty() {
                model.source.clone()
            } else {
                format!("{} · {capabilities}", model.source)
            };
            let mut option = PromptModel::new(model.id.clone(), model.display_name.clone())
                .provider(connection.to_owned())
                .description(description);
            if let Some(context) = model
                .context_tokens
                .and_then(|value| u64::try_from(value).ok())
            {
                option = option.context_window(context);
            }
            option
        })
        .collect::<Vec<_>>();
    if !models.iter().any(|model| model.id().as_ref() == selected) {
        models.insert(
            0,
            PromptModel::new(selected.to_owned(), selected.to_owned())
                .provider(connection.to_owned())
                .description("Current retained Conversation selection"),
        );
    }
    models
}

fn navigation_for_intent(intent: DesktopLaunchIntent) -> DesktopNavigationTarget {
    match intent {
        DesktopLaunchIntent::Focus => DesktopNavigationTarget::Conversation,
        DesktopLaunchIntent::Navigate(target) => target,
    }
}

fn render_activity_item(activity: &DesktopActivityItem, cx: &mut Context<Workbench>) -> AnyElement {
    let tokens = cx.theme().semantic_tokens();
    let owner = match &activity.owner {
        DesktopActivityOwner::XanaRoot => "Xana root".to_owned(),
        DesktopActivityOwner::NativeChild { agent_id } => {
            format!("Native child {}", short_identity(agent_id))
        }
        DesktopActivityOwner::Managed { runtime } => format!("Managed {runtime}"),
        DesktopActivityOwner::Mcp { server } => format!("MCP {server}"),
        DesktopActivityOwner::A2a { agent } => format!("A2A {agent}"),
    };
    let (state, tone) = match activity.state {
        DesktopActivityState::Queued => ("Queued", StatusTone::Neutral),
        DesktopActivityState::Working => ("Working", StatusTone::Info),
        DesktopActivityState::Waiting => ("Needs you", StatusTone::Warning),
        DesktopActivityState::Completed => ("Completed", StatusTone::Success),
        DesktopActivityState::Failed => ("Failed", StatusTone::Danger),
        DesktopActivityState::Cancelled => ("Cancelled", StatusTone::Neutral),
    };
    let indentation = if activity.parent_id.is_some() {
        tokens.spacing.lg
    } else {
        gpui::Pixels::ZERO
    };
    v_flex()
        .ml(indentation)
        .gap(tokens.spacing.xs)
        .p(tokens.spacing.sm)
        .border_1()
        .border_color(cx.theme().border)
        .rounded(tokens.radius.md)
        .child(
            h_flex()
                .justify_between()
                .gap(tokens.spacing.sm)
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(humanize_semantic_code(&activity.summary_code)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(owner),
                        ),
                )
                .child(
                    StatusBadge::new(format!("activity-status-{}", activity.id), state).tone(tone),
                ),
        )
        .when_some(activity.disclosed_text.clone(), |card, detail| {
            card.child(
                div()
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
        })
        .into_any_element()
}

fn humanize_semantic_code(code: &str) -> String {
    let mut label = code
        .split(['.', '_'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        return "Activity".to_owned();
    }
    let first = label.remove(0).to_uppercase().to_string();
    label.insert_str(0, &first);
    label
}

fn short_identity(identity: &str) -> &str {
    identity.get(..8).unwrap_or(identity)
}

fn notification_destination(destination: NotificationDestination) -> &'static str {
    match destination {
        NotificationDestination::Conversation => "conversation",
        NotificationDestination::Activity => "activity",
        NotificationDestination::Diagnostics => "diagnostics",
    }
}

fn documentation_url_is_safe(url: &str) -> bool {
    url.starts_with("https://github.com/labcoder/xana")
}

fn trusted_regular_file(path: &std::path::Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        let file_type = metadata.file_type();
        file_type.is_file() && !file_type.is_symlink()
    })
}

fn trusted_directory(path: &std::path::Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        let file_type = metadata.file_type();
        file_type.is_dir() && !file_type.is_symlink()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_intent_has_a_closed_navigation_projection() {
        assert_eq!(
            navigation_for_intent(DesktopLaunchIntent::Focus),
            DesktopNavigationTarget::Conversation
        );
        assert_eq!(
            navigation_for_intent(DesktopLaunchIntent::Navigate(
                DesktopNavigationTarget::Diagnostics
            )),
            DesktopNavigationTarget::Diagnostics
        );
    }

    #[test]
    fn espejo_attention_routes_to_activity_without_over_clearing() {
        assert_eq!(espejo_destination(true), DesktopNavigationTarget::Activity);
        assert_eq!(
            espejo_destination(false),
            DesktopNavigationTarget::Conversation
        );
    }

    #[test]
    fn external_documentation_is_allowlisted() {
        assert!(documentation_url_is_safe(DOCUMENTATION_URL));
        assert!(!documentation_url_is_safe(
            "http://github.com/labcoder/xana"
        ));
        assert!(!documentation_url_is_safe("https://example.com"));
    }

    #[test]
    fn native_file_actions_reject_missing_targets() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("config.toml");
        fs::write(&file, "version = 1").expect("fixture file");
        assert!(trusted_regular_file(&file));
        assert!(trusted_directory(directory.path()));
        assert!(!trusted_regular_file(&directory.path().join("missing")));
    }

    #[test]
    fn prompt_catalog_retains_current_model_and_capability_metadata() {
        let models = prompt_models(
            &[DesktopModelOption {
                id: "vision-model".to_owned(),
                display_name: "Vision Model".to_owned(),
                input_modalities: vec!["text".to_owned(), "image".to_owned()],
                output_modalities: vec!["text".to_owned()],
                tools: Some(true),
                reasoning: Some(true),
                reasoning_efforts: vec!["high".to_owned()],
                default_reasoning_effort: Some("high".to_owned()),
                context_tokens: Some(128_000),
                max_output_tokens: Some(8_000),
                pricing: None,
                source: "managed runtime".to_owned(),
            }],
            "codex",
            "missing-current",
        );

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id().as_ref(), "missing-current");
        assert_eq!(models[1].id().as_ref(), "vision-model");
    }
}
