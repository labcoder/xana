//! Focused permission-rule review and mutation UI.

use gpui::{
    AnyElement, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    Subscription, Task, Window, div, prelude::*, rems,
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
    DesktopControlPlane, DesktopEntityMutationReceipt, DesktopPermissionDecision,
    DesktopPermissionEffect, DesktopPermissionPreview, DesktopPermissionRuleDraft,
    DesktopPermissionSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PermissionViewEvent {
    Close,
}

pub(crate) struct PermissionView {
    control: DesktopControlPlane,
    snapshot: Result<DesktopPermissionSnapshot, String>,
    selected_rule: Option<String>,
    id: Entity<InputState>,
    tool: Entity<InputState>,
    workspace: Entity<InputState>,
    command: Entity<InputState>,
    decision: DesktopPermissionDecision,
    effect: Option<DesktopPermissionEffect>,
    preview: Option<DesktopPermissionPreview>,
    confirm_remove: bool,
    busy: Option<String>,
    error: Option<String>,
    receipt: Option<DesktopEntityMutationReceipt>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl PermissionView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot = control.permission_snapshot().map_err(|error| error.message);
        let id = input(window, cx, "Rule id");
        let tool = input(window, cx, "Tool name (optional)");
        let workspace = input(window, cx, "Relative workspace path (optional)");
        let command = input(window, cx, "Exact command (optional)");
        let subscriptions = [&id, &tool, &workspace, &command]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.preview = None;
                        this.receipt = None;
                        cx.notify();
                    }
                })
            })
            .collect();
        Self {
            control,
            snapshot,
            selected_rule: None,
            id,
            tool,
            workspace,
            command,
            decision: DesktopPermissionDecision::Ask,
            effect: None,
            preview: None,
            confirm_remove: false,
            busy: None,
            error: None,
            receipt: None,
            _task: None,
            _subscriptions: subscriptions,
        }
    }

    fn reload(&mut self) {
        self.snapshot = self
            .control
            .permission_snapshot()
            .map_err(|error| error.message);
    }

    fn draft(&self, cx: &Context<Self>) -> DesktopPermissionRuleDraft {
        DesktopPermissionRuleDraft {
            id: self.id.read(cx).value().trim().to_owned(),
            decision: self.decision,
            tool: optional_value(&self.tool, cx),
            effect: self.effect,
            workspace: optional_value(&self.workspace, cx),
            command: optional_value(&self.command, cx),
        }
    }

    fn select_rule(&mut self, rule_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let rule = self.snapshot.as_ref().ok().and_then(|snapshot| {
            snapshot
                .rules
                .iter()
                .find(|rule| rule.id == rule_id)
                .cloned()
        });
        let Some(rule) = rule else { return };
        self.selected_rule = Some(rule.id.clone());
        self.decision = rule.decision;
        self.effect = rule.effect;
        set_input(&self.id, rule.id, window, cx);
        set_input(&self.tool, rule.tool.unwrap_or_default(), window, cx);
        set_input(
            &self.workspace,
            rule.workspace.unwrap_or_default(),
            window,
            cx,
        );
        set_input(&self.command, rule.command.unwrap_or_default(), window, cx);
        self.preview = None;
        self.confirm_remove = false;
        self.error = None;
        cx.notify();
    }

    fn clear_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_rule = None;
        self.decision = DesktopPermissionDecision::Ask;
        self.effect = None;
        for input in [&self.id, &self.tool, &self.workspace, &self.command] {
            set_input(input, String::new(), window, cx);
        }
        self.preview = None;
        self.confirm_remove = false;
        self.error = None;
        self.receipt = None;
        cx.notify();
    }

    fn review(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let draft = self.draft(cx);
        let control = self.control.clone();
        self.busy = Some("Validating rule…".to_owned());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.preview_permission_rule(&draft) })
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

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.preview.is_none() {
            return;
        }
        let draft = self.draft(cx);
        let control = self.control.clone();
        self.busy = Some("Saving rule…".to_owned());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.save_permission_rule(draft) })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => {
                        this.selected_rule = Some(receipt.subject.clone());
                        this.receipt = Some(receipt);
                        this.preview = None;
                        this.reload();
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_rule.clone() else {
            return;
        };
        let control = self.control.clone();
        self.busy = Some("Removing rule…".to_owned());
        self.error = None;
        self.confirm_remove = false;
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.remove_permission_rule(&id) })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => {
                        this.reload();
                        this.clear_form(window, cx);
                        this.receipt = Some(receipt);
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let rules = self
            .snapshot
            .as_ref()
            .ok()
            .map(|snapshot| snapshot.rules.clone())
            .unwrap_or_default();
        v_flex()
            .w(rems(22.))
            .h_full()
            .min_h_0()
            .p(tokens.spacing.md)
            .gap(tokens.spacing.xs)
            .overflow_y_scrollbar()
            .child(
                Button::new("permission-new")
                    .label("New rule")
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| this.clear_form(window, cx))),
            )
            .children(rules.into_iter().map(|rule| {
                let id = rule.id.clone();
                Button::new(format!("permission-rule-{}", rule.id))
                    .w_full()
                    .ghost()
                    .selected(self.selected_rule.as_deref() == Some(rule.id.as_str()))
                    .child(
                        v_flex().w_full().items_start().child(rule.id).child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} · {}",
                                    rule.decision.id(),
                                    rule.matcher_summary
                                )),
                        ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_rule(&id, window, cx);
                    }))
            }))
            .into_any_element()
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let (default, tools) = self.snapshot.as_ref().ok().map_or_else(
            || ("unknown", String::new()),
            |snapshot| (snapshot.default.id(), snapshot.tools.join(", ")),
        );
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
                    .child(div().text_2xl().child("Permission rules"))
                    .child(format!("Global fallback: {default}"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("All populated matchers are required. Existing Conversations keep their frozen policy."),
                    ),
            )
            .child(Input::new(&self.id))
            .child(choice_row(
                "Decision",
                DesktopPermissionDecision::ALL
                    .into_iter()
                    .map(|decision| {
                        Button::new(format!("permission-decision-{}", decision.id()))
                            .label(decision.id())
                            .selected(self.decision == decision)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.decision = decision;
                                this.preview = None;
                                cx.notify();
                            }))
                    })
                    .collect(),
                cx,
            ))
            .child(effect_row(self.effect, cx))
            .child(Input::new(&self.tool))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Known tools: {tools}")),
            )
            .child(Input::new(&self.workspace))
            .child(Input::new(&self.command))
            .when_some(self.preview.as_ref(), |panel, preview| {
                panel.child(preview_card(preview, cx))
            })
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("permission-review")
                            .label("Review rule")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.review(cx))),
                    )
                    .child(
                        Button::new("permission-save")
                            .label("Save rule")
                            .primary()
                            .disabled(self.busy.is_some() || self.preview.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                    .when(self.selected_rule.is_some(), |row| {
                        row.child(
                            Button::new("permission-remove")
                                .label("Remove…")
                                .danger()
                                .disabled(self.busy.is_some())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_remove = true;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl EventEmitter<PermissionViewEvent> for PermissionView {}

impl Render for PermissionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .justify_between()
                    .p(tokens.spacing.md)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child("Permissions · typed policy builder")
                    .child(
                        Button::new("permission-close")
                            .label("Back to Settings")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(PermissionViewEvent::Close);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_list(cx))
                    .child(self.render_editor(cx)),
            )
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
            .when(self.confirm_remove, |panel| {
                panel.child(
                    h_flex()
                        .gap(tokens.spacing.md)
                        .p(tokens.spacing.md)
                        .border_t_1()
                        .border_color(cx.theme().danger)
                        .child("Remove this rule? The global fallback will apply where no other rule matches.")
                        .child(
                            Button::new("permission-remove-confirm")
                                .label("Remove rule")
                                .danger()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.remove(window, cx);
                                })),
                        )
                        .child(
                            Button::new("permission-remove-cancel")
                                .label("Keep rule")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_remove = false;
                                    cx.notify();
                                })),
                        ),
                )
            })
    }
}

fn choice_row(
    label: &'static str,
    choices: Vec<Button>,
    cx: &mut Context<PermissionView>,
) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    v_flex()
        .gap(tokens.spacing.xs)
        .child(label)
        .child(h_flex().gap(tokens.spacing.xs).children(choices))
}

fn effect_row(
    selected: Option<DesktopPermissionEffect>,
    cx: &mut Context<PermissionView>,
) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    h_flex()
        .flex_wrap()
        .gap(tokens.spacing.xs)
        .child(
            Button::new("permission-effect-any")
                .label("any effect")
                .selected(selected.is_none())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.effect = None;
                    this.preview = None;
                    cx.notify();
                })),
        )
        .children(DesktopPermissionEffect::ALL.into_iter().map(|effect| {
            Button::new(format!("permission-effect-{}", effect.id()))
                .label(effect.id())
                .selected(selected == Some(effect))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.effect = Some(effect);
                    this.preview = None;
                    cx.notify();
                }))
        }))
}

fn preview_card(
    preview: &DesktopPermissionPreview,
    cx: &mut Context<PermissionView>,
) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    v_flex()
        .gap(tokens.spacing.xs)
        .p(tokens.spacing.md)
        .rounded(tokens.radius.md)
        .border_1()
        .border_color(cx.theme().border)
        .child(format!(
            "Effective for exact overlaps: {}",
            preview.effective_for_exact_overlap.id()
        ))
        .child(format!("Match: {}", preview.rule.matcher_summary))
        .when(!preview.exact_overlap_ids.is_empty(), |card| {
            card.child(format!(
                "Exact overlaps: {}",
                preview.exact_overlap_ids.join(", ")
            ))
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(preview.effect_timing.clone()),
        )
}

fn input(
    window: &mut Window,
    cx: &mut Context<PermissionView>,
    placeholder: &str,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()))
}

fn set_input(
    input: &Entity<InputState>,
    value: String,
    window: &mut Window,
    cx: &mut Context<PermissionView>,
) {
    input.update(cx, |input, cx| input.set_value(value, window, cx));
}

fn optional_value(input: &Entity<InputState>, cx: &Context<PermissionView>) -> Option<String> {
    let value = input.read(cx).value().trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn status(
    message: impl Into<String>,
    danger: bool,
    cx: &mut Context<PermissionView>,
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
