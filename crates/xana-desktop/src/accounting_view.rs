//! On-demand bounded accounting projection; no storage or provider I/O in render.

use gpui::{Context, Entity, IntoElement, ParentElement as _, Render, Task, Window, prelude::*};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::Button,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    v_flex,
};
use xana::desktop::{DesktopBudgetEdit, DesktopBudgetSetting, DesktopControlPlane};

pub(crate) struct AccountingView {
    control: DesktopControlPlane,
    root: Entity<InputState>,
    job: Entity<InputState>,
    body: Entity<TextareaState>,
    next_after: Option<u64>,
    loaded_filters: (String, String),
    busy: bool,
    task: Option<Task<()>>,
    budget: Vec<(DesktopBudgetSetting, Entity<InputState>)>,
    restore_review_required: bool,
}

impl AccountingView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            control,
            root: cx.new(|cx| InputState::new(window, cx).placeholder("Root Conversation ID (empty: all)")),
            job: cx.new(|cx| InputState::new(window, cx).placeholder("Job / Run ID (empty: all)")),
            body: cx.new(|cx| TextareaState::new(window, cx).auto_grow(6, 24).default_value("Choose Refresh to read the encrypted usage ledger. This never calls a provider.")),
            next_after: None, loaded_filters: Default::default(), busy: false, task: None,
            budget: Vec::new(), restore_review_required: false,
        }
    }

    fn load(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let root = self.root.read(cx).value().trim().to_owned();
        let job = self.job.read(cx).value().trim().to_owned();
        let after = if next && self.loaded_filters == (root.clone(), job.clone()) {
            self.next_after
        } else {
            None
        };
        self.loaded_filters = (root.clone(), job.clone());
        self.busy = true;
        let control = self.control.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    control.local_usage_page(
                        (!root.is_empty()).then_some(root.as_str()),
                        (!job.is_empty()).then_some(job.as_str()),
                        after,
                    )
                })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                let text = match result {
                    Ok(page) => {
                        this.next_after = page.next_after;
                        this.restore_review_required = page.restore_review_required;
                        this.budget = page
                            .budget
                            .into_iter()
                            .map(|setting| {
                                let input = cx.new(|cx| {
                                    InputState::new(window, cx)
                                        .default_value(setting.value.to_string())
                                });
                                (setting, input)
                            })
                            .collect();
                        page.text
                    }
                    Err(error) => {
                        this.next_after = None;
                        error.message
                    }
                };
                this.body
                    .update(cx, |body, cx| body.set_value(text, window, cx));
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn save_budget(&mut self, acknowledge: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.budget.is_empty() {
            return;
        }
        let mut edits = Vec::new();
        if !acknowledge {
            for (setting, input) in &self.budget {
                let Ok(value) = input.read(cx).value().trim().parse::<u64>() else {
                    self.body.update(cx, |body, cx| {
                        body.set_value(
                            format!("{} must be a non-negative whole number.", setting.label),
                            window,
                            cx,
                        )
                    });
                    return;
                };
                if value != setting.value {
                    edits.push(DesktopBudgetEdit {
                        field: setting.field,
                        value,
                    });
                }
            }
        }
        self.busy = true;
        let control = self.control.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.update_local_budget(&edits, acknowledge) })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.load(false, window, cx),
                    Err(error) => {
                        this.body
                            .update(cx, |body, cx| body.set_value(error.message, window, cx));
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }
}

impl Render for AccountingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .id("accounting-scroll")
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .p(tokens.spacing.md)
            .gap(tokens.spacing.sm)
            .child("Local usage ledger")
            .child(Input::new(&self.root).disabled(self.busy))
            .child(Input::new(&self.job).disabled(self.busy))
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("usage-refresh")
                            .label(if self.busy { "Loading…" } else { "Refresh" })
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.load(false, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("usage-next")
                            .label("Next page")
                            .disabled(self.busy || self.next_after.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.load(true, window, cx)),
                            ),
                    ),
            )
            .child(Textarea::new(&self.body).readonly(true))
            .children(self.budget.iter().map(|(setting,input)| v_flex().gap_1().child(setting.label).child(Input::new(input).disabled(self.busy))))
            .when(!self.budget.is_empty(), |view| view.child(Button::new("budget-save").label("Save budget").disabled(self.busy).on_click(cx.listener(|this,_,window,cx| this.save_budget(false,window,cx)))))
            .when(self.restore_review_required, |view| view.child("This restored snapshot may omit later charges. Acknowledging permits new foreground work only; memory and automation stay disabled.").child(Button::new("budget-review").label("Acknowledge restored usage").disabled(self.busy).on_click(cx.listener(|this,_,window,cx| this.save_budget(true,window,cx)))))
            .into_any_element()
    }
}
