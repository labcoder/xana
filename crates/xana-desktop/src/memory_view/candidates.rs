//! A bounded owner-review surface. Markdown is data, never a Skill installation.
use gpui::{Context, Entity, IntoElement, Render, Task, Window, prelude::*};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    v_flex,
};
use xana::desktop::{
    DesktopCandidateCommand as Command, DesktopCandidateKind, DesktopCandidateResult as ResultView,
    DesktopCandidateRow, DesktopControlPlane, MemoryScope,
};

pub(super) struct CandidateView {
    control: DesktopControlPlane,
    scope: MemoryScope,
    rows: Vec<DesktopCandidateRow>,
    next_after: Option<u64>,
    selected: Option<(String, u64)>,
    can_approve: bool,
    selected_kind: Option<DesktopCandidateKind>,
    reviewed: bool,
    detail: Entity<TextareaState>,
    reason: Entity<InputState>,
    name: Entity<InputState>,
    markdown: Entity<TextareaState>,
    status: String,
    busy: bool,
    task: Option<Task<()>>,
}

impl CandidateView {
    pub(super) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            control,
            scope: MemoryScope::User,
            rows: Vec::new(),
            next_after: None,
            selected: None,
            can_approve: false,
            selected_kind: None,
            reviewed: false,
            detail: cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 16)),
            reason: cx.new(|cx| InputState::new(window, cx).placeholder("Reason for rejection")),
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Draft name")),
            markdown: cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8)),
            status: "Refresh to inspect learned suggestions. No model call.".into(),
            busy: false,
            task: None,
        }
    }

    pub(super) fn open(
        &mut self,
        scope: MemoryScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.busy {
            return false;
        }
        self.scope = scope;
        self.load(false, window, cx);
        true
    }

    fn load(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.run(
            Command::List {
                scope: self.scope.clone(),
                after: if next { self.next_after } else { None },
            },
            window,
            cx,
        );
    }

    fn run(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.reviewed = false;
        let control = self.control.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                control.learning_candidate(command)
            }).await;
            // Closing the view drops its task; a completed durable write remains inspectable.
            _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(ResultView::Page { rows, next_after }) => {
                        this.rows = rows;
                        this.next_after = next_after;
                        this.selected = None;
                        this.can_approve = false;
                        this.detail.update(cx, |field, cx| field.set_value("", window, cx));
                        this.status = format!("{} candidate(s) in this page · {}", this.rows.len(), this.scope);
                    }
                    Ok(ResultView::Inspected { id, revision, detail, can_approve, kind }) => {
                        this.selected = Some((id, revision));
                        this.selected_kind = Some(kind);
                        this.can_approve = can_approve;
                        this.detail.update(cx, |field, cx| field.set_value(detail, window, cx));
                        this.status = "Review the scope, source, before/after and validity below. Acceptance never installs a Skill.".into();
                    }
                    Ok(ResultView::Changed { detail }) => {
                        this.selected = None;
                        this.can_approve = false;
                        this.detail.update(cx, |field, cx| field.set_value(detail, window, cx));
                        this.status = "Review recorded. Refresh to see the current page; inspect again before another edit.".into();
                    }
                    Err(error) => {
                        this.selected = None;
                        this.can_approve = false;
                        this.detail.update(cx, |field, cx| field.set_value("", window, cx));
                        this.status = format!("{}. Refresh and inspect before retrying.", error.message);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn review(&mut self, action: Review, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, revision)) = self.selected.clone() else {
            return;
        };
        let command = match action {
            Review::Approve if self.reviewed && self.can_approve => Command::Approve {
                id,
                revision,
                confirm_sensitive: true,
            },
            Review::Approve => return,
            Review::Reject => Command::Reject {
                id,
                revision,
                reason: self.reason.read(cx).value().to_string(),
            },
            Review::Archive => Command::Archive { id, revision },
            Review::Undo => Command::Undo { id, revision },
        };
        self.run(command, window, cx);
    }
}

#[derive(Clone, Copy)]
enum Review {
    Approve,
    Reject,
    Archive,
    Undo,
}

impl Render for CandidateView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.iter().map(|row| {
            let id = row.id.clone();
            Button::new(format!("candidate-{id}"))
                .label(format!("{} · revision {}", row.label, row.revision))
                .disabled(self.busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run(Command::Inspect { id: id.clone() }, window, cx);
                }))
        });
        let actions = [
            (
                Review::Approve,
                "approve",
                if self.selected_kind == Some(DesktopCandidateKind::Skill) {
                    "Review draft (do not install)"
                } else {
                    "Accept reviewed memory"
                },
            ),
            (Review::Reject, "reject", "Reject"),
            (Review::Archive, "archive", "Archive"),
            (Review::Undo, "undo", "Undo acceptance"),
        ]
        .into_iter()
        .map(|(action, id, label)| {
            Button::new(format!("candidate-review-{id}"))
                .label(label)
                .disabled(
                    self.busy
                        || self.selected.is_none()
                        || (matches!(action, Review::Approve)
                            && (!self.reviewed || !self.can_approve)),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.review(action, window, cx)))
        });
        let body = v_flex().p_4().gap_3()
            .child("Learning candidates")
            .child(format!("Scope: {}", self.scope))
            .child("Memory acceptance changes only the inspected fact. Skill acceptance records review only; installation is a separate owner action. Archive is not secure forgetting.")
            .child(h_flex().gap_2()
                .child(Button::new("candidate-refresh").label("Refresh").disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.load(false, window, cx))))
                .child(Button::new("candidate-next").label("Next page").disabled(self.busy || self.next_after.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.load(true, window, cx)))))
            .child(if self.busy { "Working locally…".to_owned() } else { self.status.clone() })
            .children(rows)
            .child("Proposal, provenance and diff")
            .child(Textarea::new(&self.detail).readonly(true))
            .child(Checkbox::new("candidate-reviewed").label("I reviewed this exact scope and content, including any sensitive facts")
                .checked(self.reviewed).disabled(self.busy || !self.can_approve)
                .on_click(cx.listener(|this, value, _, cx| { this.reviewed = *value; cx.notify(); })))
            .child("Rejection reason")
            .child(Input::new(&self.reason).disabled(self.busy))
            .child(h_flex().gap_2().flex_wrap().children(actions))
            .child("New inert Skill draft")
            .child("Store Markdown for later review. It is not discovered, loaded, or executed by Xana.")
            .child(Input::new(&self.name).disabled(self.busy))
            .child(Textarea::new(&self.markdown).disabled(self.busy))
            .child(Button::new("candidate-stage").label("Stage draft").disabled(self.busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.run(Command::StageSkill {
                        scope: this.scope.clone(), name: this.name.read(cx).value().to_string(),
                        markdown: this.markdown.read(cx).value().to_string(),
                    }, window, cx);
                })));
        v_flex()
            .id("candidate-scroll")
            .size_full()
            .min_h_0()
            .text_color(cx.theme().foreground)
            .overflow_y_scrollbar()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Focusable as _, TestAppContext, VisualTestContext};
    use gpui_component::Root;

    #[gpui::test]
    fn candidate_input_and_unavailable_store_keep_review_safe(cx: &mut TestAppContext) {
        let home = tempfile::tempdir().expect("disposable home");
        let control = xana::desktop::DesktopLaunch::new(
            home.path(),
            Some(home.path().as_os_str().to_owned()),
        )
        .control_plane()
        .expect("temporary control plane");
        cx.update(gpui_component::init);
        let mut candidate = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| CandidateView::new(control, window, cx));
            candidate = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = candidate.expect("candidate view");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.review(Review::Approve, window, cx);
                assert!(!view.busy, "no selection cannot dispatch a write");
                view.reason.read(cx).focus_handle(cx).focus(window, cx);
            })
        });
        cx.simulate_keystrokes("n o");
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.reason.read(cx).value().as_ref(), "no");
                view.load(false, window, cx);
                assert!(view.busy);
                assert!(
                    !view.open(
                        MemoryScope::Profile(
                            "00000000-0000-0000-0000-000000000001"
                                .parse()
                                .expect("fixture profile id")
                        ),
                        window,
                        cx
                    )
                );
                assert_eq!(
                    view.scope,
                    MemoryScope::User,
                    "an in-flight scope cannot be replaced or silently reopened"
                );
            })
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                assert!(!view.busy);
                assert!(view.status.contains("protected storage"));
                assert!(view.selected.is_none());
                assert!(!view.can_approve);
            })
        });
        assert!(
            !home.path().join("data/protected").exists(),
            "inspection cannot initialize storage"
        );
    }
}
