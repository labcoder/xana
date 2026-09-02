//! Application-owned state and the first real Desktop runtime projection.

use crate::{
    commands::{
        self, ClearConversation, InterruptRun, MinimizeWindow, OpenConfigurationFile,
        OpenDocumentation, QuitXana, RevealLogs, ShowActivity, ShowCommandPalette,
        WorkbenchCommand,
    },
    projection::ConversationProjection,
};
use gpui::{
    Context, Entity, IntoElement, ParentElement as _, PromptLevel, Render, Role, Subscription,
    SystemNotification, Task, Window, div, prelude::*, rems,
};
use gpui_ai::prelude::{
    Chat, ChatEvent, ChatWelcome, CommandSearch, CommandSearchEvent, LoadingState, ProgressState,
    PromptBar, PromptBarEvent, StatusBadge, StatusTone, Suggestion,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use std::{fs, sync::Arc, time::Duration};
use xana::desktop::{
    AttentionKind, AttentionSignal, ClientFocus, DesktopClient, DesktopConversationState,
    DesktopEvent, DesktopHostEvent, DesktopInstanceLease, DesktopLaunchIntent, DesktopNativePaths,
    DesktopNavigationTarget, DesktopRoundBudgetSuspension, DesktopUpdate, LastWindowChoice,
    LastWindowEffect, NotificationDestination, NotificationPlanner, last_window_effect,
};

const UPDATE_INTERVAL: Duration = Duration::from_millis(16);
const MAX_UPDATES_PER_FRAME: usize = 64;
const DOCUMENTATION_URL: &str = "https://github.com/labcoder/xana#readme";

/// Owns retained GPUI entities, Xana's runtime client, and controlled snapshots.
pub(crate) struct Workbench {
    runtime: DesktopClient,
    instance: DesktopInstanceLease,
    native_paths: DesktopNativePaths,
    projection: ConversationProjection,
    navigation: DesktopNavigationTarget,
    chat: Entity<Chat>,
    command_search: Entity<CommandSearch>,
    palette_open: bool,
    shutdown_pending: bool,
    close_prompt_open: bool,
    notifications: NotificationPlanner,
    _chat_subscription: Subscription,
    _command_subscription: Subscription,
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
        let projection = ConversationProjection::from_snapshot(runtime.initial_snapshot());
        let navigation = navigation_for_intent(initial_intent);
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
            navigation,
            chat,
            command_search,
            palette_open: false,
            shutdown_pending: false,
            close_prompt_open: false,
            notifications: NotificationPlanner::new(),
            _chat_subscription: chat_subscription,
            _command_subscription: command_subscription,
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

    /// Returns false once the backend has stopped and there is nothing left to poll.
    fn drain_runtime_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut changed = self.drain_launch_intents(window);
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
                DesktopUpdate::Snapshot(snapshot) => self.projection.replace_snapshot(&snapshot),
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

    fn drain_launch_intents(&mut self, window: &mut Window) -> bool {
        let mut changed = false;
        for _ in 0..MAX_UPDATES_PER_FRAME {
            let Some(intent) = self.instance.try_next() else {
                break;
            };
            changed = true;
            window.activate_window();
            self.navigation = navigation_for_intent(intent);
            self.projection.set_activity(format!(
                "Opened {} from another Xana launch",
                self.navigation.as_str()
            ));
        }
        changed
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
        cx.notify();
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if self.projection.failure().is_some() {
            StatusBadge::new("runtime-status", "Needs attention")
                .tone(StatusTone::Danger)
                .into_any_element()
        } else if self.projection.is_running() {
            LoadingState::new()
                .label(self.projection.latest_activity().to_owned())
                .into_any_element()
        } else {
            StatusBadge::new("runtime-status", "Ready")
                .tone(StatusTone::Success)
                .into_any_element()
        };
        let tokens = cx.theme().semantic_tokens();
        let round_controls = self
            .projection
            .pending_round_budget()
            .cloned()
            .map(|suspension| {
                let continue_suspension = suspension.clone();
                v_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} committed tool result(s). Continue the same operation or stop it.",
                                suspension.committed_results
                            )),
                    )
                    .child(
                        h_flex()
                            .gap(tokens.spacing.sm)
                            .child(
                                Button::new("continue-round-budget")
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
                                Button::new("stop-round-budget").label("Stop").on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        this.decide_round_budget(
                                            suspension.clone(),
                                            false,
                                            window,
                                            cx,
                                        );
                                    }),
                                ),
                            ),
                    )
            });
        let activity = v_flex()
            .h_full()
            .w(rems(18.))
            .flex_none()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.lg)
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Activity"),
            )
            .child(status)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.projection.latest_activity().to_owned()),
            )
            .when_some(round_controls, |activity, controls| {
                activity.child(controls)
            });
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
        let main = h_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(self.chat.clone()),
            )
            .child(activity);

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
    }
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
