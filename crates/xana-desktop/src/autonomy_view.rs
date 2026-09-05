//! Schedule drafts and bounded inspection. The domain owns grants, revisions,
//! host lifecycle and effects; this view only issues typed owner intent.
use gpui::{Context, Entity, IntoElement, ParentElement as _, Render, Task, Window, prelude::*};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    v_flex,
};
use xana::desktop::{
    DesktopAutonomySnapshot, DesktopControlPlane, DesktopError, DesktopHostEdit,
    DesktopScheduleEdit, DesktopScheduledTask, DesktopTaskDraft, DesktopTaskPreview,
};

pub(crate) struct AutonomyView {
    control: DesktopControlPlane,
    name: Entity<InputState>,
    workspace: Entity<InputState>,
    profile: Entity<InputState>,
    project: Entity<InputState>,
    time: Entity<InputState>,
    timezone: Entity<InputState>,
    expires: Entity<InputState>,
    text: Entity<TextareaState>,
    detail: Entity<TextareaState>,
    reminder: bool,
    daily: bool,
    workspace_reads: bool,
    authorize: bool,
    review_unknown: bool,
    preview: Option<(DesktopTaskDraft, DesktopTaskPreview)>,
    snapshot: Option<DesktopAutonomySnapshot>,
    selected: Option<DesktopScheduledTask>,
    status: String,
    busy: bool,
    task: Option<Task<()>>,
}
enum ViewResult {
    Snapshot(DesktopAutonomySnapshot),
    Preview(DesktopTaskDraft, DesktopTaskPreview),
    Detail(String),
    Changed(String),
}
impl AutonomyView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            control,name:cx.new(|cx|InputState::new(window,cx)),workspace:cx.new(|cx|InputState::new(window,cx)),profile:cx.new(|cx|InputState::new(window,cx)),project:cx.new(|cx|InputState::new(window,cx)),
            time:cx.new(|cx|InputState::new(window,cx).placeholder("RFC3339 instant, or daily HH:MM")),timezone:cx.new(|cx|InputState::new(window,cx).placeholder("America/Los_Angeles")),expires:cx.new(|cx|InputState::new(window,cx).placeholder("RFC3339 expiry with timezone offset")),
            text:cx.new(|cx|TextareaState::new(window,cx).auto_grow(3,8)),detail:cx.new(|cx|TextareaState::new(window,cx).auto_grow(3,12)),
            reminder:true,daily:false,workspace_reads:false,authorize:false,review_unknown:false,preview:None,snapshot:None,selected:None,
            status:"Refresh to inspect schedules and host status. Nothing runs until you explicitly start the detached host.".into(),busy:false,task:None,
        }
    }
    fn draft(&self, cx: &Context<Self>) -> DesktopTaskDraft {
        let project = self.project.read(cx).value().trim().to_owned();
        DesktopTaskDraft {
            name: self.name.read(cx).value().to_string(),
            workspace: self.workspace.read(cx).value().to_string(),
            profile: self.profile.read(cx).value().to_string(),
            project: (!project.is_empty()).then_some(project),
            text: self.text.read(cx).value().to_string(),
            reminder: self.reminder,
            workspace_reads: self.workspace_reads && !self.reminder,
            daily: self.daily,
            time: self.time.read(cx).value().to_string(),
            timezone: self.timezone.read(cx).value().to_string(),
            expires: self.expires.read(cx).value().to_string(),
        }
    }
    fn perform(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        operation: impl FnOnce(DesktopControlPlane) -> Result<ViewResult, DesktopError> + Send + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = "Working…".into();
        let control = self.control.clone();
        self.task=Some(cx.spawn_in(window,async move |this,cx| {
            let result=cx.background_executor().spawn(async move {operation(control)}).await;
            _=this.update_in(cx,|this,window,cx| {
                this.busy=false;
                match result {
                    Ok(ViewResult::Snapshot(snapshot))=>{
                        this.selected=this.selected.as_ref().and_then(|selected|snapshot.jobs.iter().find(|job|job.id==selected.id).cloned());
                        this.snapshot=Some(snapshot);this.status="Current protected schedule page loaded. Select a task to inspect its exact intent.".into();
                    }
                    Ok(ViewResult::Preview(draft,preview))=>{this.detail.update(cx,|input,cx|input.set_value(preview.text.clone(),window,cx));this.preview=Some((draft,preview));this.authorize=false;this.status="Review the exact task, recipient and bounds below, then confirm creation.".into();}
                    Ok(ViewResult::Detail(text))=>{this.detail.update(cx,|input,cx|input.set_value(text,window,cx));this.status="Read-only inspection. This does not grant new authority or replay work.".into();}
                    Ok(ViewResult::Changed(receipt))=>{this.status=receipt;this.snapshot=None;this.selected=None;this.preview=None;this.authorize=false;this.review_unknown=false;}
                    Err(error)=>this.status=error.message,
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn refresh(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        let after = if next {
            self.snapshot
                .as_ref()
                .and_then(|s| s.next_after)
                .unwrap_or(0)
        } else {
            0
        };
        self.perform(window, cx, move |control| {
            control.autonomy_snapshot(after).map(ViewResult::Snapshot)
        });
    }
    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        self.perform(window, cx, move |control| {
            control
                .preview_scheduled_task(&draft)
                .map(|preview| ViewResult::Preview(draft, preview))
        });
    }
    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        let Some((reviewed, preview)) = &self.preview else {
            return;
        };
        if !self.authorize || *reviewed != draft {
            self.status = "Draft changed or unconfirmed. Preview the exact task again.".into();
            cx.notify();
            return;
        }
        let digest = preview.route_digest.clone();
        self.perform(window, cx, move |control| {
            control
                .create_scheduled_task(draft, digest)
                .map(ViewResult::Changed)
        });
    }
    fn inspect(&mut self, receipts: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = &self.selected else {
            return;
        };
        let id = selected.id.clone();
        self.perform(window, cx, move |control| {
            control
                .inspect_scheduled_task(&id, receipts)
                .map(ViewResult::Detail)
        });
    }
    fn edit(&mut self, edit: DesktopScheduleEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected.clone() else {
            return;
        };
        let edit = if matches!(edit, DesktopScheduleEdit::Resume) && self.review_unknown {
            DesktopScheduleEdit::ReviewUnknownAndResume
        } else {
            edit
        };
        self.perform(window, cx, move |control| {
            control
                .edit_scheduled_task(&selected.id, selected.revision, edit)
                .map(ViewResult::Changed)
        });
    }
    fn host(&mut self, edit: DesktopHostEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let revision = snapshot.policy_revision;
        self.perform(window, cx, move |control| {
            control
                .edit_background_host(revision, edit)
                .map(ViewResult::Changed)
        });
    }
}
impl Render for AutonomyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let can_create = self.authorize
            && self
                .preview
                .as_ref()
                .is_some_and(|(draft, _)| *draft == self.draft(cx));
        let host_known = self.snapshot.is_some();
        let selected = self.selected.is_some();
        v_flex().id("autonomy-scroll").size_full().min_h_0().overflow_y_scrollbar().child(v_flex().p(tokens.spacing.md).gap(tokens.spacing.sm)
            .child("Schedules")
            .child(h_flex().flex_wrap().gap_2()
                .child(Button::new("schedules-refresh").label(if self.busy {"Loading…"} else {"Refresh"}).disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.refresh(false,window,cx))))
                .child(Button::new("schedules-next").label("Next page").disabled(self.busy || self.snapshot.as_ref().and_then(|s|s.next_after).is_none()).on_click(cx.listener(|this,_,window,cx|this.refresh(true,window,cx)))))
            .when_some(self.snapshot.as_ref(),|view,snapshot|view.child(format!("Host: {} · detached {} · OS startup permission {}",snapshot.host_status,snapshot.detached_enabled,snapshot.startup_enabled)))
            .child(h_flex().flex_wrap().gap_2()
                .children([(DesktopHostEdit::Start,"host-start","Start detached host"),(DesktopHostEdit::Stop,"host-stop","Stop host"),(DesktopHostEdit::StopAndLock,"host-lock","Stop and lock"),(DesktopHostEdit::Disable,"host-disable","Disable detached host"),(DesktopHostEdit::EnableStartup,"host-startup-on","Install login startup"),(DesktopHostEdit::DisableStartup,"host-startup-off","Remove login startup")].into_iter().map(|(edit,id,label)|Button::new(id).label(label).disabled(self.busy || !host_known).on_click(cx.listener(move|this,_,window,cx|this.host(edit,window,cx))))))
            .child("Closing this panel detaches it; it does not stop the host. Login startup is a separate explicit per-user installation.")
            .children(self.snapshot.iter().flat_map(|snapshot|snapshot.jobs.iter()).map(|job| {
                let chosen=job.clone();
                Button::new(gpui::SharedString::from(format!("scheduled-{}",job.id))).label(format!("{} · {} · next {}",job.name,job.state,job.next_at)).selected(self.selected.as_ref().is_some_and(|s|s.id==job.id)).disabled(self.busy).on_click(cx.listener(move|this,_,_,cx|{this.selected=Some(chosen.clone());this.review_unknown=false;cx.notify();}))
            }))
            .when(self.snapshot.as_ref().is_some_and(|s|s.jobs.is_empty()),|view|view.child("No schedules on this page. Create an explicitly scoped task below."))
            .child(h_flex().flex_wrap().gap_2()
                .child(Button::new("schedule-inspect").label("Inspect task").disabled(self.busy || !selected).on_click(cx.listener(|this,_,window,cx|this.inspect(false,window,cx))))
                .child(Button::new("schedule-receipts").label("Receipts").disabled(self.busy || !selected).on_click(cx.listener(|this,_,window,cx|this.inspect(true,window,cx))))
                .children([(DesktopScheduleEdit::Pause,"schedule-pause","Pause"),(DesktopScheduleEdit::Resume,"schedule-resume","Resume"),(DesktopScheduleEdit::Cancel,"schedule-cancel","Cancel task")].into_iter().map(|(edit,id,label)|Button::new(id).label(label).disabled(self.busy || !selected).on_click(cx.listener(move|this,_,window,cx|this.edit(edit,window,cx))))))
            .child(Checkbox::new("schedule-review-unknown").label("I inspected the uncertain outcome and authorize a new attempt").checked(self.review_unknown).disabled(self.busy || !selected).on_click(cx.listener(|this,checked,_,cx|{this.review_unknown = *checked;cx.notify();})))
            .child("Create a schedule")
            .children([("Name",&self.name),("Workspace (exact existing folder)",&self.workspace),("Global Profile name",&self.profile),("Project ID (optional)",&self.project),("Trigger time",&self.time),("IANA timezone (daily only)",&self.timezone),("Authority expiry",&self.expires)].into_iter().map(|(label,input)|v_flex().gap_1().child(label).child(Input::new(input).disabled(self.busy))))
            .child(Checkbox::new("schedule-daily").label("Repeat daily (otherwise one-shot)").checked(self.daily).disabled(self.busy).on_click(cx.listener(|this,checked,_,cx|{this.daily = *checked;this.authorize=false;cx.notify();})))
            .child(Checkbox::new("schedule-reminder").label("Local reminder (otherwise native task)").checked(self.reminder).disabled(self.busy).on_click(cx.listener(|this,checked,_,cx|{this.reminder = *checked;this.authorize=false;cx.notify();})))
            .child(Checkbox::new("schedule-reads").label("Authorize Profile-selected bounded workspace reads and their disclosure").checked(self.workspace_reads).disabled(self.busy || self.reminder).on_click(cx.listener(|this,checked,_,cx|{this.workspace_reads = *checked;this.authorize=false;cx.notify();})))
            .child("Task / reminder text (maximum 16 KiB)").child(Textarea::new(&self.text).disabled(self.busy))
            .child(Button::new("schedule-preview").label("Preview exact task").disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.preview(window,cx))))
            .child(Textarea::new(&self.detail).readonly(true))
            .child(Checkbox::new("schedule-authorize").label("Authorize this exact reviewed task, recipient, expiry and budgets").checked(self.authorize).disabled(self.busy || self.preview.is_none()).on_click(cx.listener(|this,checked,_,cx|{this.authorize = *checked;cx.notify();})))
            .child(Button::new("schedule-create").label("Create schedule").disabled(self.busy || !can_create).on_click(cx.listener(|this,_,window,cx|this.create(window,cx))))
            .child(self.status.clone())).into_any_element()
    }
}
