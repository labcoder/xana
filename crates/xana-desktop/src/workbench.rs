//! Application-owned state and the first real Desktop runtime projection.

use crate::projection::ConversationProjection;
use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, Subscription, Task, Window, div,
    prelude::*, rems,
};
use gpui_ai::prelude::{
    Chat, ChatEvent, ChatWelcome, LoadingState, ProgressState, PromptBar, PromptBarEvent,
    StatusBadge, StatusTone, Suggestion,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use std::{sync::Arc, time::Duration};
use xana::desktop::{DesktopClient, DesktopRoundBudgetSuspension, DesktopUpdate};

const UPDATE_INTERVAL: Duration = Duration::from_millis(16);
const MAX_UPDATES_PER_FRAME: usize = 64;

/// Owns retained GPUI entities, Xana's runtime client, and controlled snapshots.
pub(crate) struct Workbench {
    runtime: DesktopClient,
    projection: ConversationProjection,
    chat: Entity<Chat>,
    _chat_subscription: Subscription,
    _runtime_driver: Task<()>,
}

impl Workbench {
    pub(crate) fn new(runtime: DesktopClient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let projection = ConversationProjection::from_snapshot(runtime.initial_snapshot());
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
        let chat_subscription =
            cx.subscribe_in(&chat, window, |this, _, event: &ChatEvent, window, cx| {
                this.handle_chat_event(event, window, cx);
            });
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

        Self {
            runtime,
            projection,
            chat,
            _chat_subscription: chat_subscription,
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

    /// Returns false once the backend has stopped and there is nothing left to poll.
    fn drain_runtime_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
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
            match update {
                DesktopUpdate::Snapshot(snapshot) => self.projection.replace_snapshot(&snapshot),
                DesktopUpdate::Observation(observation) => {
                    if !self.projection.apply(observation)
                        && let Err(error) = self.runtime.request_snapshot()
                    {
                        self.projection.fail(error.message);
                    }
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
                    keep_running = false;
                }
            }
        }
        if changed {
            self.sync_components(window, cx);
        }
        keep_running
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

        h_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(self.chat.clone()),
            )
            .child(activity)
    }
}
