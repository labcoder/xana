//! Schedule drafts and bounded inspection. The domain owns grants, revisions,
//! host lifecycle and effects; this view only issues typed owner intent.
use gpui::{Context, Entity, IntoElement, ParentElement as _, Render, Task, Window, prelude::*};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    radio::{Radio, RadioGroup},
    scroll::ScrollableElement as _,
    v_flex,
};
use xana::desktop::{
    DesktopAutonomySnapshot, DesktopControlPlane, DesktopError, DesktopGithubCredential,
    DesktopHostEdit, DesktopScheduleEdit, DesktopScheduledTask, DesktopTaskDraft,
    DesktopTaskPreview, DesktopTaskTrigger,
};

pub(crate) struct AutonomyView {
    control: DesktopControlPlane,
    workers: Entity<crate::worker_view::WorkerView>,
    name: Entity<InputState>,
    workspace: Entity<InputState>,
    profile: Entity<InputState>,
    project: Entity<InputState>,
    time: Entity<InputState>,
    timezone: Entity<InputState>,
    watch_root: Entity<InputState>,
    github_run: Entity<InputState>,
    credential_reference: Entity<InputState>,
    expires: Entity<InputState>,
    text: Entity<TextareaState>,
    detail: Entity<TextareaState>,
    reminder: bool,
    trigger: TriggerChoice,
    stored_credential: bool,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum TriggerChoice {
    Once,
    Daily,
    Files,
    GithubRun,
}

impl TriggerChoice {
    const ALL: [Self; 4] = [Self::Once, Self::Daily, Self::Files, Self::GithubRun];
    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|choice| *choice == self)
            .expect("all trigger choices listed")
    }
}
enum ViewResult {
    Reviewed(DesktopScheduledTask, String),
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
            workers: cx.new(|cx| crate::worker_view::WorkerView::new(control.clone(), window, cx)),
            control,name:cx.new(|cx|InputState::new(window,cx)),workspace:cx.new(|cx|InputState::new(window,cx)),profile:cx.new(|cx|InputState::new(window,cx)),project:cx.new(|cx|InputState::new(window,cx)),
            time:cx.new(|cx|InputState::new(window,cx).placeholder("RFC3339 instant, or daily HH:MM")),timezone:cx.new(|cx|InputState::new(window,cx).placeholder("America/Los_Angeles")),expires:cx.new(|cx|InputState::new(window,cx).placeholder("RFC3339 expiry with timezone offset")),
            watch_root:cx.new(|cx|InputState::new(window,cx).placeholder("Exact directory within the workspace, e.g. src")),
            github_run:cx.new(|cx|InputState::new(window,cx).placeholder("OWNER/REPO/RUN_ID")),
            credential_reference:cx.new(|cx|InputState::new(window,cx).placeholder("Environment variable name or stored credential ID; never a token")),
            text:cx.new(|cx|TextareaState::new(window,cx).auto_grow(3,8)),detail:cx.new(|cx|TextareaState::new(window,cx).auto_grow(3,12)),
            reminder:true,trigger:TriggerChoice::Once,stored_credential:false,workspace_reads:false,authorize:false,review_unknown:false,preview:None,snapshot:None,selected:None,
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
            trigger: match self.trigger {
                TriggerChoice::Once => DesktopTaskTrigger::Once {
                    at: self.time.read(cx).value().to_string(),
                },
                TriggerChoice::Daily => DesktopTaskTrigger::Daily {
                    time: self.time.read(cx).value().to_string(),
                    timezone: self.timezone.read(cx).value().to_string(),
                },
                TriggerChoice::Files => DesktopTaskTrigger::Files {
                    root: self.watch_root.read(cx).value().to_string(),
                },
                TriggerChoice::GithubRun => DesktopTaskTrigger::GithubRun {
                    run: self.github_run.read(cx).value().to_string(),
                    credential: if self.stored_credential {
                        DesktopGithubCredential::Stored {
                            id: self.credential_reference.read(cx).value().to_string(),
                        }
                    } else {
                        DesktopGithubCredential::Environment {
                            variable: self.credential_reference.read(cx).value().to_string(),
                        }
                    },
                },
            },
            expires: self.expires.read(cx).value().to_string(),
        }
    }
    pub(crate) fn review_task(
        &mut self,
        task: DesktopScheduledTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Espejo rows may be stale. Load selection and review from one owner
        // record instead of pairing an old revision with fresh detail.
        self.review_id(task.id, window, cx);
    }
    pub(crate) fn review_id(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.status =
                "Finish the current schedule operation, then open this task again.".into();
            cx.notify();
            return;
        }
        self.perform(window, cx, move |control| {
            control
                .review_scheduled_task(&id)
                .map(|(task, detail)| ViewResult::Reviewed(task, detail))
        });
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
                    Ok(ViewResult::Reviewed(task,detail))=>{this.selected=Some(task);this.review_unknown=false;this.preview=None;this.authorize=false;this.detail.update(cx,|input,cx|input.set_value(detail,window,cx));this.status="Current task scope loaded; opening it did not grant authority.".into();}
                    Ok(ViewResult::Snapshot(snapshot))=>{
                        this.selected=this.selected.as_ref().and_then(|selected|snapshot.jobs.iter().find(|job|job.id==selected.id).cloned());
                        this.snapshot=Some(snapshot);this.status="Current protected schedule page loaded. Select a task to inspect its exact intent.".into();
                    }
                    Ok(ViewResult::Preview(draft,preview))=>{this.detail.update(cx,|input,cx|input.set_value(preview.text.clone(),window,cx));this.preview=Some((draft,preview));this.authorize=false;this.status="Review the exact task, recipient and bounds below, then confirm creation.".into();}
                    Ok(ViewResult::Detail(text))=>{this.preview=None;this.authorize=false;this.detail.update(cx,|input,cx|input.set_value(text,window,cx));this.status="Read-only inspection. This does not grant new authority or replay work.".into();}
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
        let preview = preview.clone();
        self.perform(window, cx, move |control| {
            control
                .create_scheduled_task(draft, preview)
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

    fn render_trigger(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let fields: Vec<(&str, &Entity<InputState>)> = match self.trigger {
            TriggerChoice::Once => vec![("Exact time (RFC3339 with offset)", &self.time)],
            TriggerChoice::Daily => vec![
                ("Daily time (HH:MM)", &self.time),
                ("IANA timezone", &self.timezone),
            ],
            TriggerChoice::Files => vec![("Selected directory", &self.watch_root)],
            TriggerChoice::GithubRun => vec![("GitHub run (OWNER/REPO/RUN_ID)", &self.github_run)],
        };
        v_flex().gap_2()
            .child("Trigger")
            .child(RadioGroup::horizontal("task-trigger").selected_index(Some(self.trigger.index())).disabled(self.busy)
                .children([("trigger-once", "Once"), ("trigger-daily", "Daily"), ("trigger-files", "Selected files"), ("trigger-github", "GitHub run")]
                    .into_iter().map(|(id,label)|Radio::new(id).label(label)))
                .on_click(cx.listener(|this, index, _, cx| {
                    if let Some(choice) = TriggerChoice::ALL.get(*index) {
                        this.trigger = *choice; this.authorize = false; this.preview = None; cx.notify();
                    }
                })))
            .children(fields.into_iter().map(|(label,input)|v_flex().gap_1().child(label.to_owned()).child(Input::new(input).disabled(self.busy))))
            .when(self.trigger == TriggerChoice::Files, |view|view.child("Metadata changes only; at most 256 entries, no links or Xana-managed state. No model call while unchanged."))
            .when(self.trigger == TriggerChoice::GithubRun, |view|view
                .child("Credential source (not an API key)")
                .child(RadioGroup::horizontal("task-ci-credential-source").selected_index(Some(usize::from(self.stored_credential))).disabled(self.busy)
                    .child(Radio::new("ci-credential-env").label("Environment variable"))
                    .child(Radio::new("ci-credential-stored").label("Stored credential"))
                    .on_click(cx.listener(|this,index,_,cx|{this.stored_credential = *index == 1;this.authorize=false;this.preview=None;cx.notify();})))
                .child(if self.stored_credential {"Stored credential ID"} else {"Environment variable name"})
                .child(Input::new(&self.credential_reference).disabled(self.busy))
                .child("Actions-read access to only this named repository/run. Preview does not fetch a token or call GitHub. No Codex or GitHub CLI account fallback."))
            .into_any_element()
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
                Button::new(gpui::SharedString::from(format!("scheduled-{}",job.id))).label(format!("{} · {} · {} · next {}",job.name,job.group.label(),job.trigger,job.next_at)).selected(self.selected.as_ref().is_some_and(|s|s.id==job.id)).disabled(self.busy).on_click(cx.listener(move|this,_,_,cx|{this.selected=Some(chosen.clone());this.review_unknown=false;cx.notify();}))
            }))
            .when(self.snapshot.as_ref().is_some_and(|s|s.jobs.is_empty()),|view|view.child("No schedules on this page. Create an explicitly scoped task below."))
            .child(h_flex().flex_wrap().gap_2()
                .child(Button::new("schedule-inspect").label("Review grant and memory scope").disabled(self.busy || !selected).on_click(cx.listener(|this,_,window,cx|this.inspect(false,window,cx))))
                .child(Button::new("schedule-receipts").label("Receipts").disabled(self.busy || !selected).on_click(cx.listener(|this,_,window,cx|this.inspect(true,window,cx))))
                .children([(DesktopScheduleEdit::Pause,"schedule-pause","Pause"),(DesktopScheduleEdit::Resume,"schedule-resume","Resume"),(DesktopScheduleEdit::Cancel,"schedule-cancel","Cancel task")].into_iter().map(|(edit,id,label)|Button::new(id).label(label).disabled(self.busy || !selected).on_click(cx.listener(move|this,_,window,cx|this.edit(edit,window,cx))))))
            .child(Checkbox::new("schedule-review-unknown").label("I inspected the uncertain outcome and authorize a new attempt").checked(self.review_unknown).disabled(self.busy || !selected).on_click(cx.listener(|this,checked,_,cx|{this.review_unknown = *checked;cx.notify();})))
            .child("Create a schedule")
            .children([("Name",&self.name),("Workspace (exact existing folder)",&self.workspace),("Global Profile name",&self.profile),("Project ID (optional)",&self.project)].into_iter().map(|(label,input)|v_flex().gap_1().child(label).child(Input::new(input).disabled(self.busy))))
            .child(self.render_trigger(cx))
            .child(v_flex().gap_1().child("Authority expiry (RFC3339 with offset)").child(Input::new(&self.expires).disabled(self.busy)))
            .child(Checkbox::new("schedule-reminder").label("Local reminder (otherwise native task)").checked(self.reminder).disabled(self.busy).on_click(cx.listener(|this,checked,_,cx|{this.reminder = *checked;this.authorize=false;cx.notify();})))
            .child(Checkbox::new("schedule-reads").label("Authorize Profile-selected bounded workspace reads and their disclosure").checked(self.workspace_reads).disabled(self.busy || self.reminder).on_click(cx.listener(|this,checked,_,cx|{this.workspace_reads = *checked;this.authorize=false;cx.notify();})))
            .child("Task / reminder text (maximum 16 KiB)").child(Textarea::new(&self.text).disabled(self.busy))
            .child(Button::new("schedule-preview").label("Preview exact task").disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.preview(window,cx))))
            .child(Textarea::new(&self.detail).readonly(true))
            .child(Checkbox::new("schedule-authorize").label("Authorize this exact reviewed task, recipient, expiry and budgets").checked(self.authorize).disabled(self.busy || self.preview.is_none()).on_click(cx.listener(|this,checked,_,cx|{this.authorize = *checked;cx.notify();})))
            .child(Button::new("schedule-create").label("Create schedule").disabled(self.busy || !can_create).on_click(cx.listener(|this,_,window,cx|this.create(window,cx))))
            .child(self.status.clone())
            .child(self.workers.clone())).into_any_element()
    }
}
