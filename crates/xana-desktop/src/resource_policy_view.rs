//! Focused bounded resource-policy editor for Xana Desktop.

use gpui::{
    AnyElement, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, Subscription, Task, Window, div, prelude::*, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};
use xana::desktop::{
    DesktopControlPlane, DesktopEntityMutationReceipt, DesktopResourceLimit,
    DesktopResourcePolicyDraft, DesktopResourcePolicyPreview, DesktopResourcePolicySnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourcePolicyViewEvent {
    Close,
}

pub(crate) struct ResourcePolicyView {
    control: DesktopControlPlane,
    snapshot: Result<DesktopResourcePolicySnapshot, String>,
    selected_key: Option<String>,
    value: Entity<InputState>,
    draft: DesktopResourcePolicyDraft,
    preview: Option<DesktopResourcePolicyPreview>,
    busy: Option<String>,
    error: Option<String>,
    receipt: Option<DesktopEntityMutationReceipt>,
    _task: Option<Task<()>>,
    _subscription: Subscription,
}

impl ResourcePolicyView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        snapshot: Result<DesktopResourcePolicySnapshot, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected_key = snapshot
            .as_ref()
            .ok()
            .and_then(|snapshot| snapshot.limits.first())
            .map(|limit| limit.key.clone());
        let initial = selected_key
            .as_deref()
            .and_then(|key| find_limit(&snapshot, key))
            .map(|limit| limit.configured.to_string())
            .unwrap_or_default();
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Positive integer")
                .default_value(initial)
        });
        let subscription = cx.subscribe_in(&value, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.preview = None;
                this.receipt = None;
                cx.notify();
            }
        });
        Self {
            control,
            snapshot,
            selected_key,
            value,
            draft: DesktopResourcePolicyDraft::default(),
            preview: None,
            busy: None,
            error: None,
            receipt: None,
            _task: None,
            _subscription: subscription,
        }
    }

    fn select(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let configured = self
            .draft
            .values
            .get(key)
            .copied()
            .or_else(|| find_limit(&self.snapshot, key).map(|limit| limit.configured));
        let Some(configured) = configured else { return };
        self.selected_key = Some(key.to_owned());
        self.value.update(cx, |value, cx| {
            value.set_value(configured.to_string(), window, cx)
        });
        self.error = None;
        cx.notify();
    }

    fn stage(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.selected_key.clone() else {
            return;
        };
        let raw = self.value.read(cx).value().trim().to_owned();
        let value = match raw.parse::<u64>() {
            Ok(value) if value > 0 => value,
            _ => {
                self.error =
                    Some("Enter a positive integer; zero never means unlimited.".to_owned());
                cx.notify();
                return;
            }
        };
        self.draft.values.insert(key, value);
        self.preview = None;
        self.error = None;
        self.receipt = None;
        cx.notify();
    }

    fn reset_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected_key.clone() else {
            return;
        };
        let Some(default) = find_limit(&self.snapshot, &key).map(|limit| limit.default) else {
            return;
        };
        self.draft.values.insert(key, default);
        self.value.update(cx, |value, cx| {
            value.set_value(default.to_string(), window, cx)
        });
        self.preview = None;
        self.error = None;
        cx.notify();
    }

    fn revert_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected_key.clone() else {
            return;
        };
        self.draft.values.remove(&key);
        if let Some(configured) = find_limit(&self.snapshot, &key).map(|limit| limit.configured) {
            self.value.update(cx, |value, cx| {
                value.set_value(configured.to_string(), window, cx)
            });
        }
        self.preview = None;
        self.error = None;
        cx.notify();
    }

    fn review(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.draft.values.is_empty() {
            return;
        }
        let control = self.control.clone();
        let draft = self.draft.clone();
        self.busy = Some("Validating all resource limits…".to_owned());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.preview_resource_policy(&draft) })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(preview) => this.preview = Some(preview),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some()
            || self
                .preview
                .as_ref()
                .is_none_or(|preview| preview.changed_keys.is_empty())
        {
            return;
        }
        let control = self.control.clone();
        let draft = self.draft.clone();
        self.busy = Some("Applying resource policy…".to_owned());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let receipt = control.save_resource_policy(draft)?;
                    let snapshot = control.resource_policy_snapshot()?;
                    Ok::<_, xana::desktop::DesktopError>((receipt, snapshot))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok((receipt, snapshot)) => {
                        this.receipt = Some(receipt);
                        this.snapshot = Ok(snapshot);
                        this.draft.values.clear();
                        this.preview = None;
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_list(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let limits = self
            .snapshot
            .as_ref()
            .ok()
            .map(|snapshot| snapshot.limits.clone())
            .unwrap_or_default();
        v_flex()
            .when(!compact, |list| list.w(rems(24.)).h_full())
            .when(compact, |list| list.w_full().h(rems(12.)).flex_none())
            .min_h_0()
            .overflow_y_scrollbar()
            .p(tokens.spacing.md)
            .gap(tokens.spacing.xs)
            .children(limits.into_iter().map(|limit| {
                let key = limit.key.clone();
                let staged = self.draft.values.get(&key).copied();
                Button::new(format!("resource-limit-{key}"))
                    .w_full()
                    .ghost()
                    .selected(self.selected_key.as_deref() == Some(key.as_str()))
                    .child(
                        v_flex()
                            .w_full()
                            .items_start()
                            .child(format!("{} · {}", limit.group, limit.label))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(match staged {
                                        Some(value) => format!("{value} {} · staged", limit.unit),
                                        None => format!("{} {}", limit.configured, limit.unit),
                                    }),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(&key, window, cx);
                    }))
            }))
            .into_any_element()
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let selected = self
            .selected_key
            .as_deref()
            .and_then(|key| find_limit(&self.snapshot, key))
            .cloned();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .p(tokens.spacing.lg)
            .gap(tokens.spacing.lg)
            .child(
                v_flex()
                    .gap(tokens.spacing.xs)
                    .child(div().text_2xl().child("Attachments & media"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.snapshot.as_ref().ok().map_or_else(
                                || "Resource policy is unavailable.".to_owned(),
                                |snapshot| snapshot.effective_note.clone(),
                            )),
                    ),
            )
            .when_some(selected, |panel, limit| {
                panel
                    .child(format!("{} · {}", limit.group, limit.label))
                    .child(Input::new(&self.value))
                    .child(format!(
                        "Current {} · default {} · immutable ceiling {} {}",
                        limit.configured, limit.default, limit.hard_ceiling, limit.unit
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(limit.key),
                    )
            })
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("resource-stage")
                            .label("Stage value")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.stage(cx))),
                    )
                    .child(
                        Button::new("resource-reset")
                            .label("Use default")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_selected(window, cx);
                            })),
                    )
                    .child(
                        Button::new("resource-revert")
                            .label("Revert staged")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.revert_selected(window, cx);
                            })),
                    ),
            )
            .when_some(self.preview.as_ref(), |panel, preview| {
                panel.child(
                    v_flex()
                        .gap(tokens.spacing.xs)
                        .p(tokens.spacing.md)
                        .rounded(tokens.radius.md)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(format!(
                            "{} effective change(s): {}",
                            preview.changed_keys.len(),
                            preview.changed_keys.join(", ")
                        ))
                        .child(preview.effect_timing.clone()),
                )
            })
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("resource-review")
                            .label(format!("Review {} staged", self.draft.values.len()))
                            .disabled(self.busy.is_some() || self.draft.values.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.review(cx))),
                    )
                    .child(
                        Button::new("resource-apply")
                            .label("Apply policy")
                            .primary()
                            .disabled(self.busy.is_some() || self.preview.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                    ),
            )
            .into_any_element()
    }
}

impl EventEmitter<ResourcePolicyViewEvent> for ResourcePolicyView {}

impl Render for ResourcePolicyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let compact = f32::from(window.viewport_size().width) < 880.;
        let body = if compact {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_list(true, cx))
                .child(self.render_editor(cx))
                .into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_list(false, cx))
                .child(self.render_editor(cx))
                .into_any_element()
        };
        v_flex()
            .id("xana-resource-policy")
            .role(Role::Region)
            .aria_label("Attachments and media resource policy")
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .justify_between()
                    .p(tokens.spacing.md)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child("Resource policy · bounded, never unlimited")
                    .child(
                        Button::new("resource-close")
                            .label("Back to Settings")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ResourcePolicyViewEvent::Close);
                            })),
                    ),
            )
            .child(body)
            .when_some(self.busy.clone(), |panel, busy| {
                panel.child(status(busy, false, cx))
            })
            .when_some(self.error.clone(), |panel, error| {
                panel.child(status(error, true, cx))
            })
            .when_some(self.receipt.as_ref(), |panel, receipt| {
                panel.child(status(
                    format!("{} · {}", receipt.semantic_code, receipt.detail),
                    false,
                    cx,
                ))
            })
    }
}

fn find_limit<'a>(
    snapshot: &'a Result<DesktopResourcePolicySnapshot, String>,
    key: &str,
) -> Option<&'a DesktopResourceLimit> {
    snapshot
        .as_ref()
        .ok()?
        .limits
        .iter()
        .find(|limit| limit.key == key)
}

fn status(
    message: impl Into<String>,
    danger: bool,
    cx: &mut Context<ResourcePolicyView>,
) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    div()
        .px(tokens.spacing.md)
        .py(tokens.spacing.sm)
        .border_t_1()
        .border_color(cx.theme().border)
        .when(danger, |card| card.text_color(cx.theme().danger))
        .child(message.into())
}
