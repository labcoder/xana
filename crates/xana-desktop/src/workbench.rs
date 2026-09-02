//! Application-owned state and the first real Desktop runtime projection.

use crate::{
    commands::{
        self, ArchiveSelectedProject, BranchSelectedConversation, ClearConversation, InterruptRun,
        MinimizeWindow, MoveSelectedConversation, OpenConfigurationFile, OpenDocumentation,
        QuitXana, RenameSelectedProject, RestoreSelectedProject, RevealLogs, ShowActivity,
        ShowCommandPalette, ShowSettings, UngroupSelectedConversation, WorkbenchCommand,
    },
    design_system,
    projection::ConversationProjection,
    settings_view::{SettingsView, SettingsViewEvent},
};
use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement as _, PathPromptOptions, PromptLevel,
    Render, Role, Subscription, SystemNotification, Task, Window, div, prelude::*, px, rems,
};
use gpui_ai::prelude::{
    Chat, ChatEvent, ChatWelcome, CommandSearch, CommandSearchEvent, LoadingState, ProgressState,
    PromptBar, PromptBarEvent, SidebarNav, SidebarNavEvent, SidebarNavItem, SidebarNavPresentation,
    SidebarSection, StatusBadge, StatusTone, Suggestion,
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
use std::{fs, sync::Arc, time::Duration};
use xana::desktop::{
    AttentionKind, AttentionSignal, ClientFocus, DesktopClient, DesktopConversationState,
    DesktopDockPlacement, DesktopEvent, DesktopHostEvent, DesktopInstanceLease,
    DesktopLaunchIntent, DesktopLayoutNode, DesktopNativePaths, DesktopNavigationSnapshot,
    DesktopNavigationTarget, DesktopPanelId, DesktopRoundBudgetSuspension,
    DesktopSettingsDraftSnapshot, DesktopSettingsReceipt, DesktopSettingsSnapshot,
    DesktopSidebarMode, DesktopSplitAxis, DesktopUpdate, DesktopWorkbenchLayout,
    DesktopWorkspaceStatus, LastWindowChoice, LastWindowEffect, NotificationDestination,
    NotificationPlanner, last_window_effect,
};

const UPDATE_INTERVAL: Duration = Duration::from_millis(16);
const MAX_UPDATES_PER_FRAME: usize = 64;
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

/// Owns retained GPUI entities, Xana's runtime client, and controlled snapshots.
pub(crate) struct Workbench {
    runtime: DesktopClient,
    instance: DesktopInstanceLease,
    native_paths: DesktopNativePaths,
    projection: ConversationProjection,
    navigation_snapshot: DesktopNavigationSnapshot,
    layout: DesktopWorkbenchLayout,
    layout_save_generation: u64,
    settings_snapshot: DesktopSettingsSnapshot,
    settings_draft: Option<DesktopSettingsDraftSnapshot>,
    settings_receipt: Option<DesktopSettingsReceipt>,
    selected_project: Option<String>,
    sidebar_selection: Option<SidebarSelection>,
    navigation_dialog: Option<NavigationDialog>,
    navigation_input: Entity<InputState>,
    navigation: DesktopNavigationTarget,
    chat: Entity<Chat>,
    sidebar: Entity<SidebarNav>,
    command_search: Entity<CommandSearch>,
    settings_view: Entity<SettingsView>,
    palette_open: bool,
    shutdown_pending: bool,
    close_prompt_open: bool,
    notifications: NotificationPlanner,
    _chat_subscription: Subscription,
    _sidebar_subscription: Subscription,
    _command_subscription: Subscription,
    _settings_subscription: Subscription,
    _runtime_driver: Task<()>,
}

impl Workbench {
    pub(crate) fn new(
        runtime: DesktopClient,
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
        if navigation == DesktopNavigationTarget::Settings
            && let Err(error) = runtime.begin_settings()
        {
            projection.fail(error.message);
        }
        let prompt = cx.new(|cx| PromptBar::new("xana-composer", window, cx));
        prompt.update(cx, |prompt, cx| {
            prompt.set_progress(ProgressState::Pending, cx);
        });

        let chat = cx.new(|cx| Chat::new("xana-conversation", prompt, window, cx));
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
        let settings_view =
            cx.new(|cx| SettingsView::new(settings_snapshot.clone(), None, None, window, cx));
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
            instance,
            native_paths,
            projection,
            navigation_snapshot,
            layout,
            layout_save_generation: 0,
            settings_snapshot,
            settings_draft: None,
            settings_receipt: None,
            selected_project,
            sidebar_selection,
            navigation_dialog: None,
            navigation_input,
            navigation,
            chat,
            sidebar,
            command_search,
            settings_view,
            palette_open: false,
            shutdown_pending: false,
            close_prompt_open: false,
            notifications: NotificationPlanner::new(),
            _chat_subscription: chat_subscription,
            _sidebar_subscription: sidebar_subscription,
            _command_subscription: command_subscription,
            _settings_subscription: settings_subscription,
            _runtime_driver: runtime_driver,
        }
    }

    fn handle_chat_event(
        &mut self,
        event: &ChatEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ChatEvent::Prompt(PromptBarEvent::Submit { submission, .. }) => {
                let input = submission.text().to_string();
                match self.runtime.submit(input.clone()) {
                    Ok(receipt) => {
                        if let Some(operation_id) = receipt.operation_id {
                            self.projection.append_user(operation_id, input);
                        }
                    }
                    Err(error) => self.projection.fail(error.message),
                }
                self.sync_components(window, cx);
            }
            ChatEvent::SuggestionSelected { suggestion_id }
                if suggestion_id.as_ref() == "capabilities" =>
            {
                self.chat.update(cx, |chat, cx| {
                    chat.prompt_bar().update(cx, |prompt, cx| {
                        prompt.set_draft("What can Xana do?", window, cx);
                    });
                });
            }
            _ => {}
        }
    }

    fn handle_settings_event(
        &mut self,
        event: &SettingsViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsViewEvent::Close => {
                self.navigation = DesktopNavigationTarget::Conversation;
                self.projection.set_activity("Conversation opened");
            }
            SettingsViewEvent::Reload => match self.runtime.reload_settings() {
                Ok(_) => self.projection.set_activity("Refreshing settings…"),
                Err(error) => self.projection.fail(error.message),
            },
            SettingsViewEvent::Review => {
                let Some(draft) = self.settings_draft.as_ref() else {
                    self.projection.fail("No settings changes are staged.");
                    self.sync_components(window, cx);
                    return;
                };
                match self.runtime.validate_settings(draft.id) {
                    Ok(_) => self.projection.set_activity("Validating staged settings…"),
                    Err(error) => self.projection.fail(error.message),
                }
            }
            SettingsViewEvent::Apply => {
                let Some(draft) = self.settings_draft.as_ref() else {
                    self.projection.fail("No settings changes are staged.");
                    self.sync_components(window, cx);
                    return;
                };
                match self.runtime.commit_settings(draft.id) {
                    Ok(_) => self
                        .projection
                        .set_activity("Applying settings transaction…"),
                    Err(error) => self.projection.fail(error.message),
                }
            }
            SettingsViewEvent::Discard => {
                let Some(draft) = self.settings_draft.as_ref() else {
                    return;
                };
                match self.runtime.discard_settings(draft.id) {
                    Ok(_) => self.projection.set_activity("Discarding staged settings…"),
                    Err(error) => self.projection.fail(error.message),
                }
            }
            SettingsViewEvent::Set {
                draft_id,
                key,
                value,
            } => match self
                .runtime
                .set_setting(*draft_id, key.clone(), value.clone())
            {
                Ok(_) => self.projection.set_activity(format!("Staging {key}…")),
                Err(error) => self.projection.fail(error.message),
            },
            SettingsViewEvent::Reset { draft_id, key } => {
                match self.runtime.reset_setting(*draft_id, key.clone()) {
                    Ok(_) => self.projection.set_activity(format!("Resetting {key}…")),
                    Err(error) => self.projection.fail(error.message),
                }
            }
            SettingsViewEvent::Revert { draft_id, key } => {
                match self.runtime.revert_setting(*draft_id, key.clone()) {
                    Ok(_) => self
                        .projection
                        .set_activity(format!("Reverting staged {key}…")),
                    Err(error) => self.projection.fail(error.message),
                }
            }
        }
        self.sync_components(window, cx);
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation = DesktopNavigationTarget::Settings;
        if self.settings_draft.is_none()
            && let Err(error) = self.runtime.begin_settings()
        {
            self.projection.fail(error.message);
        }
        self.projection.set_activity("Settings opened");
        self.settings_view.update(cx, |settings, cx| {
            settings.focus_search(window, cx);
        });
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
                    self.projection.replace_snapshot(&snapshot);
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
                        self.apply_settings_appearance(&draft.preview, cx);
                    } else {
                        let snapshot = self.settings_snapshot.clone();
                        self.apply_settings_appearance(&snapshot, cx);
                    }
                    self.settings_draft = draft;
                }
                DesktopUpdate::SettingsReceipt(receipt) => {
                    self.settings_receipt = Some(receipt);
                }
                DesktopUpdate::Observation(observation) => {
                    if !self.projection.apply(observation)
                        && let Err(error) = self.runtime.request_snapshot()
                    {
                        self.projection.fail(error.message);
                    }
                }
                DesktopUpdate::HostObservation(observation) => {
                    self.projection.apply_host(&observation);
                }
                DesktopUpdate::CommandResult {
                    accepted: false,
                    error,
                    ..
                } => self.projection.fail(
                    error
                        .map(|error| error.message)
                        .unwrap_or_else(|| "Runtime rejected the Desktop command".to_owned()),
                ),
                DesktopUpdate::CommandResult { .. } => {}
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
            if self.navigation == DesktopNavigationTarget::Settings {
                if self.settings_draft.is_none()
                    && let Err(error) = self.runtime.begin_settings()
                {
                    self.projection.fail(error.message);
                }
                self.settings_view.update(cx, |settings, cx| {
                    settings.focus_search(window, cx);
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
            chat.prompt_bar().update(cx, |prompt, cx| {
                prompt.set_progress(progress, cx);
            });
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
        let tokens = cx.theme().semantic_tokens();
        match panel {
            DesktopPanelId::Conversation => self.chat.clone().into_any_element(),
            DesktopPanelId::Activity => self.render_activity_panel(window, cx),
            DesktopPanelId::Message => v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .p(tokens.spacing.lg)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("The retained composer remains attached to Conversation until M4-17A.")
                .into_any_element(),
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

    fn render_activity_panel(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
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
        v_flex()
            .size_full()
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
        let showing_settings = self.navigation == DesktopNavigationTarget::Settings;
        let canvas = if showing_settings {
            self.settings_view.clone().into_any_element()
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
        let panel_library = (!showing_settings).then(|| self.render_panel_library(cx));
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
                        this.navigation = DesktopNavigationTarget::Espejo;
                        this.projection.set_activity("Espejo opened");
                        cx.notify();
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

fn navigation_for_intent(intent: DesktopLaunchIntent) -> DesktopNavigationTarget {
    match intent {
        DesktopLaunchIntent::Focus => DesktopNavigationTarget::Conversation,
        DesktopLaunchIntent::Navigate(target) => target,
    }
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
}
