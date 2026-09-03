//! Focused Workbench preference explanation and safe reset.

use gpui::{
    Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render, Role,
    Task, Window, div, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use xana::desktop::{
    DesktopControlPlane, DesktopEntityMutationReceipt, DesktopWorkbenchPreferenceSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkbenchPreferencesEvent {
    Close,
}

pub(crate) struct WorkbenchPreferencesView {
    control: DesktopControlPlane,
    snapshot: DesktopWorkbenchPreferenceSnapshot,
    confirm_reset: bool,
    busy: bool,
    error: Option<String>,
    receipt: Option<DesktopEntityMutationReceipt>,
    _task: Option<Task<()>>,
}

impl WorkbenchPreferencesView {
    pub(crate) fn new(control: DesktopControlPlane) -> Self {
        let snapshot = control.workbench_preference_snapshot();
        Self {
            control,
            snapshot,
            confirm_reset: false,
            busy: false,
            error: None,
            receipt: None,
            _task: None,
        }
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.confirm_reset = false;
        self.error = None;
        let control = self.control.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.restore_builtin_workbench_layout() })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(receipt) => {
                        this.receipt = Some(receipt);
                        this.snapshot = this.control.workbench_preference_snapshot();
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl EventEmitter<WorkbenchPreferencesEvent> for WorkbenchPreferencesView {}

impl Render for WorkbenchPreferencesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .id("xana-workbench-preferences")
            .role(Role::Region)
            .aria_label("Workbench preferences")
            .size_full()
            .p(tokens.spacing.lg)
            .gap(tokens.spacing.lg)
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_2xl().child("Workbench preferences"))
                    .child(
                        Button::new("workbench-preferences-close")
                            .label("Back to Settings")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(WorkbenchPreferencesEvent::Close);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap(tokens.spacing.sm)
                    .p(tokens.spacing.lg)
                    .rounded(tokens.radius.md)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(format!("User-wide source: {}", self.snapshot.source))
                    .child(format!("Panels: {}", self.snapshot.panels.join(", ")))
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.snapshot.precedence.clone()),
                    )
                    .when_some(self.snapshot.warning.clone(), |card, warning| {
                        card.child(
                            div()
                                .text_color(cx.theme().warning)
                                .child(format!("Recovered safely: {warning}")),
                        )
                    }),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Use Save as user default in an open Workbench to capture its current arrangement. Reset here never deletes Conversation layouts."),
            )
            .child(
                Button::new("workbench-preferences-reset")
                    .label("Restore built-in default…")
                    .danger()
                    .disabled(self.busy || self.snapshot.source == "built_in")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_reset = true;
                        cx.notify();
                    })),
            )
            .when(self.confirm_reset, |panel| {
                panel.child(
                    h_flex()
                        .gap(tokens.spacing.md)
                        .p(tokens.spacing.md)
                        .border_1()
                        .border_color(cx.theme().danger)
                        .child("Remove the user-wide default? Per-Conversation layouts remain unchanged.")
                        .child(
                            Button::new("workbench-preferences-confirm")
                                .label("Restore built-in")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
                        )
                        .child(
                            Button::new("workbench-preferences-cancel")
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_reset = false;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when(self.busy, |panel| panel.child("Restoring built-in layout…"))
            .when_some(self.error.clone(), |panel, error| {
                panel.child(div().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.receipt.as_ref(), |panel, receipt| {
                panel.child(format!("{} · {}", receipt.semantic_code, receipt.detail))
            })
    }
}
