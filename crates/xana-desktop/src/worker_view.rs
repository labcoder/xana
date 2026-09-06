//! Retained workers are durable records, not UI-owned agents or resident heaps.
//! Every mutation carries the selected revision; execution stays off the UI thread.
use gpui::{Context, Entity, IntoElement, Render, Task, Window, prelude::*};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    v_flex,
};
use xana::desktop::{
    DesktopControlPlane, DesktopError, DesktopWorkerCancellation, DesktopWorkerIntent,
    DesktopWorkerSummary,
};

pub(crate) struct WorkerView {
    control: DesktopControlPlane,
    rows: Vec<DesktopWorkerSummary>,
    selected: Option<DesktopWorkerSummary>,
    session: Entity<InputState>,
    agent: Entity<InputState>,
    goal: Entity<InputState>,
    expires: Entity<InputState>,
    evidence: Entity<InputState>,
    request_id: Entity<InputState>,
    text: Entity<TextareaState>,
    operation: Entity<TextareaState>,
    detail: Entity<TextareaState>,
    authorize: bool,
    review_unknown: bool,
    busy: bool,
    status: String,
    cancellation: DesktopWorkerCancellation,
    task: Option<Task<()>>,
}

enum WorkerResult {
    Page(Vec<DesktopWorkerSummary>),
    Detail(String),
    Changed(String),
}
#[derive(Clone, Copy)]
enum WorkerAction {
    Inspect,
    FollowUp,
    Run,
    Drain,
    Stop,
    Recover,
    Context,
}

impl WorkerView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            control,
            rows: Vec::new(),
            selected: None,
            session: cx
                .new(|cx| InputState::new(window, cx).placeholder("Source Conversation UUID")),
            agent: cx.new(|cx| InputState::new(window, cx).placeholder("Completed child Agent ID")),
            goal: cx.new(|cx| InputState::new(window, cx).placeholder("Bounded retained goal")),
            expires: cx
                .new(|cx| InputState::new(window, cx).placeholder("RFC3339 authority expiry")),
            evidence: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Selected artifact IDs, separated by spaces")
            }),
            request_id: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Unique follow-up UUID; reuse only for retry")
            }),
            text: cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6)),
            operation: cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6)),
            detail: cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 12)),
            authorize: false,
            review_unknown: false,
            busy: false,
            status: "Refresh to inspect retained workers. Retaining a child does not start it."
                .into(),
            cancellation: DesktopWorkerCancellation::default(),
            task: None,
        }
    }

    fn perform(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        operation: impl FnOnce(
            DesktopControlPlane,
            DesktopWorkerCancellation,
        ) -> Result<WorkerResult, DesktopError>
        + Send
        + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = "Working within the retained scope and cumulative budget…".into();
        self.cancellation = DesktopWorkerCancellation::default();
        let cancellation = self.cancellation.clone();
        let control = self.control.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { operation(control, cancellation) }).await;
            _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(WorkerResult::Page(rows)) => {
                        this.selected = this.selected.as_ref().and_then(|chosen| rows.iter().find(|row| row.id == chosen.id).cloned());
                        this.rows = rows;
                        this.status = "Current protected page. Inspect a worker before using its exact revision.".into();
                    }
                    Ok(WorkerResult::Detail(detail)) => {
                        this.detail.update(cx, |input, cx| input.set_value(detail, window, cx));
                        this.status = "Read-only inspection; no authority changed.".into();
                    }
                    Ok(WorkerResult::Changed(receipt)) => {
                        this.detail.update(cx, |input, cx| input.set_value(receipt, window, cx));
                        this.selected = None;
                        this.rows.clear();
                        this.authorize = false;
                        this.review_unknown = false;
                        this.status = "Receipt recorded. Refresh before the next revision-bound operation.".into();
                    }
                    Err(error) => this.status = error.message,
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        let after = next
            .then(|| self.rows.last().map(|row| row.id.clone()))
            .flatten();
        self.perform(window, cx, move |control, _| {
            control
                .retained_workers(after.as_deref())
                .map(WorkerResult::Page)
        });
    }

    fn retain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.authorize {
            return;
        }
        let intent = DesktopWorkerIntent::Retain {
            session: self.session.read(cx).value().trim().to_owned(),
            agent: self.agent.read(cx).value().trim().to_owned(),
            goal: self.goal.read(cx).value().to_string(),
            expires: self.expires.read(cx).value().trim().to_owned(),
            evidence: self
                .evidence
                .read(cx)
                .value()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            authorize: self.authorize,
        };
        self.perform(window, cx, move |control, cancellation| {
            control
                .edit_retained_worker_blocking(intent, cancellation)
                .map(WorkerResult::Changed)
        });
    }

    fn act(&mut self, action: WorkerAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worker) = self.selected.clone() else {
            return;
        };
        let request_id = self.request_id.read(cx).value().trim().to_owned();
        let text = self.text.read(cx).value().to_string();
        let operation = self.operation.read(cx).value().to_string();
        let review_unknown = self.review_unknown;
        self.perform(window, cx, move |control, cancellation| {
            let id = worker.id;
            let revision = worker.revision;
            let intent = match action {
                WorkerAction::Inspect => {
                    return control
                        .inspect_retained_worker(&id)
                        .map(WorkerResult::Detail);
                }
                WorkerAction::Run => {
                    return control
                        .run_retained_worker_blocking(&id, revision, cancellation)
                        .map(WorkerResult::Changed);
                }
                WorkerAction::FollowUp => DesktopWorkerIntent::FollowUp {
                    id,
                    revision,
                    request_id,
                    text,
                },
                WorkerAction::Drain => DesktopWorkerIntent::Drain { id, revision },
                WorkerAction::Stop => DesktopWorkerIntent::Stop { id, revision },
                WorkerAction::Recover => DesktopWorkerIntent::Recover {
                    id,
                    revision,
                    review_unknown,
                },
                WorkerAction::Context => DesktopWorkerIntent::Context {
                    id,
                    revision,
                    operation,
                },
            };
            control
                .edit_retained_worker_blocking(intent, cancellation)
                .map(WorkerResult::Changed)
        });
    }
}

impl Drop for WorkerView {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Render for WorkerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex().gap(tokens.spacing.sm)
            .child("Retained workers")
            .child("Each run is a fresh bounded execution of the same retained identity. The original parent ceiling still applies; there is no resident model heap or implicit vendor session resume.")
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("workers-refresh").label("Refresh workers").disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.refresh(false, window, cx))))
                .child(Button::new("workers-next").label("Next worker page").disabled(self.busy || self.rows.is_empty()).on_click(cx.listener(|this, _, window, cx| this.refresh(true, window, cx))))
                .child(Button::new("workers-cancel-run").label("Interrupt this operation").disabled(!self.busy).on_click(cx.listener(|this, _, _, cx| {
                    this.cancellation.cancel(); this.status = "Cancellation requested; waiting for the durable outcome.".into(); cx.notify();
                }))))
            .children(self.rows.iter().map(|worker| {
                let selected = worker.clone();
                Button::new(gpui::SharedString::from(format!("worker-{}", worker.id)))
                    .label(format!("{} · {} · {} · {} queued · revision {}", worker.goal, worker.state, worker.route, worker.queued, worker.revision))
                    .selected(self.selected.as_ref().is_some_and(|current| current.id == worker.id)).disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| { this.selected = Some(selected.clone()); this.review_unknown = false; cx.notify(); }))
            }))
            .child(h_flex().gap_2().flex_wrap().children([
                (WorkerAction::Inspect, "worker-inspect", "Inspect scope and receipts"),
                (WorkerAction::Run, "worker-run", "Run queued follow-up"),
                (WorkerAction::Drain, "worker-drain", "Drain"),
                (WorkerAction::Stop, "worker-stop", "Stop worker"),
                (WorkerAction::Recover, "worker-recover", "Recover uncertain run"),
            ].into_iter().map(|(action, id, label)| Button::new(id).label(label).disabled(self.busy || self.selected.is_none())
                .on_click(cx.listener(move |this, _, window, cx| this.act(action, window, cx))))))
            .child(Checkbox::new("worker-review-unknown").label("I inspected the uncertain outcome; do not replay it automatically")
                .checked(self.review_unknown).disabled(self.busy).on_click(cx.listener(|this, checked, _, cx| { this.review_unknown = *checked; cx.notify(); })))
            .child("Queue a bounded follow-up (does not run until requested)")
            .child(Input::new(&self.request_id).disabled(self.busy))
            .child(Textarea::new(&self.text).disabled(self.busy))
            .child(Button::new("worker-follow-up").label("Queue follow-up").disabled(self.busy || self.selected.is_none())
                .on_click(cx.listener(|this, _, window, cx| this.act(WorkerAction::FollowUp, window, cx))))
            .child("Context operation: closed JSON search / slice / filter / map / reduce / derive / cite; only selected immutable evidence, never executable code")
            .child(Textarea::new(&self.operation).disabled(self.busy))
            .child(Button::new("worker-context").label("Run bounded context operation").disabled(self.busy || self.selected.is_none())
                .on_click(cx.listener(|this, _, window, cx| this.act(WorkerAction::Context, window, cx))))
            .child("Retain an existing completed child")
            .children([("Source Conversation", &self.session), ("Child Agent ID", &self.agent), ("Goal", &self.goal),
                ("Authority expiry", &self.expires), ("Evidence artifact IDs", &self.evidence)].into_iter()
                .map(|(label, input)| v_flex().gap_1().child(label).child(Input::new(input).disabled(self.busy))))
            .child(Checkbox::new("worker-retain-authorize").label("Retain this child with the listed goal/evidence; never expand its original authority or budget")
                .checked(self.authorize).disabled(self.busy).on_click(cx.listener(|this, checked, _, cx| { this.authorize = *checked; cx.notify(); })))
            .child(Button::new("worker-retain").label("Retain worker").disabled(self.busy || !self.authorize)
                .on_click(cx.listener(|this, _, window, cx| this.retain(window, cx))))
            .child(self.status.clone())
            .child(Textarea::new(&self.detail).readonly(true))
    }
}
