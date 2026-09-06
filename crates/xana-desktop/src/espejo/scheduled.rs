//! Espejo's bounded durable-work page. Reads run off the GPUI thread; opening
//! a row only navigates to review and never resumes a background execution.
use super::{EspejoScope, EspejoViewEvent};
use gpui::{
    Context, EventEmitter, IntoElement, ParentElement as _, Render, Task, Window, prelude::*,
};
use gpui_component::{ActiveTheme as _, Disableable as _, button::Button, h_flex, v_flex};
use xana::desktop::{DesktopAutonomySnapshot, DesktopControlPlane, DesktopScheduledTask};

pub(super) struct ScheduledWorkView {
    control: DesktopControlPlane,
    snapshot: Option<DesktopAutonomySnapshot>,
    scope: EspejoScope,
    busy: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    attention: std::collections::VecDeque<xana::desktop::DesktopBackgroundAttention>,
}
impl ScheduledWorkView {
    pub(super) fn new(control: DesktopControlPlane) -> Self {
        Self {
            control,
            snapshot: None,
            scope: EspejoScope::Global,
            busy: false,
            error: None,
            task: None,
            attention: Default::default(),
        }
    }
    pub(super) fn set_scope(&mut self, scope: EspejoScope, cx: &mut Context<Self>) {
        self.scope = scope;
        self.refresh(false, cx);
    }
    pub(super) fn attention(
        &mut self,
        notes: Vec<xana::desktop::DesktopBackgroundAttention>,
        cx: &mut Context<Self>,
    ) {
        for note in notes {
            self.attention.retain(|old| old.task != note.task);
            self.attention.push_back(note);
            if self.attention.len() > 32 {
                self.attention.pop_front();
            }
        }
        cx.notify();
    }
    fn refresh(&mut self, next: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let after = if next {
            self.snapshot
                .as_ref()
                .and_then(|s| s.next_after)
                .unwrap_or(0)
        } else {
            0
        };
        let control = self.control.clone();
        self.busy = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.autonomy_snapshot(after) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(snapshot) => this.snapshot = Some(snapshot),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
fn in_scope(job: &DesktopScheduledTask, scope: &EspejoScope) -> bool {
    match scope {
        EspejoScope::Global => true,
        EspejoScope::Project(id) => job.project.as_ref() == Some(id),
    }
}
impl EventEmitter<EspejoViewEvent> for ScheduledWorkView {}
impl Render for ScheduledWorkView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_2().child(h_flex().gap_2().flex_wrap()
            .child("Coming up and background work")
            .child(Button::new("work-refresh").label(if self.busy {"Loading…"} else {"Refresh work"}).disabled(self.busy).on_click(cx.listener(|this,_,_,cx|this.refresh(false,cx))))
            .child(Button::new("work-next").label("Next work page").disabled(self.busy||self.snapshot.as_ref().and_then(|s|s.next_after).is_none()).on_click(cx.listener(|this,_,_,cx|this.refresh(true,cx)))))
            .child(gpui::div().text_sm().text_color(cx.theme().muted_foreground).child("Saved intent only · up to 32 tasks per page · Refresh for current state. New authority waits for you; opening a row does not approve it."))
            .when_some(self.error.clone(),|view,error|view.child(error))
            .when(self.scope == EspejoScope::Global, |view| view.children(self.attention.iter().map(|note| {
                let task=note.task.clone();
                Button::new(gpui::SharedString::from(format!("work-attention-{}",note.task)))
                    .label(format!("Background {:?} · task {} · review current state", note.kind, note.task))
                    .on_click(cx.listener(move|_,_,_,cx|cx.emit(EspejoViewEvent::OpenScheduledId(task.clone()))))
            })))
            .children(self.snapshot.iter().flat_map(|s|s.jobs.iter()).filter(|job|in_scope(job,&self.scope)).map(|job| {
                let task=job.clone();
                Button::new(gpui::SharedString::from(format!("espejo-work-{}",job.id))).label(format!("{} · {} · {} · next {} · {}",job.group.label(),job.name,job.trigger,job.next_at,job.receipt.unwrap_or("No execution receipt")))
                    .disabled(self.busy).on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(EspejoViewEvent::OpenScheduled(Box::new(task.clone())));
                    }))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xana::desktop::DesktopWorkGroup;
    #[test]
    fn project_filter_does_not_include_other_or_ungrouped_work() {
        let mut job = DesktopScheduledTask {
            id: "task".into(),
            revision: 1,
            conversation: "task-conversation".into(),
            project: Some("project-a".into()),
            name: "task".into(),
            group: DesktopWorkGroup::ComingUp,
            next_at: 1,
            expires_at: 2,
            connection: "local".into(),
            model: "local".into(),
            profile: "default".into(),
            trigger: "calendar",
            observation_at: None,
            last_event_at: None,
            source_status: None,
            event_pending: false,
            receipt: None,
            receipt_id: None,
            pause_pending: false,
            cancellation_pending: false,
            token_ceiling: 8192,
            seconds_ceiling: 120,
        };
        assert!(in_scope(&job, &EspejoScope::Global));
        assert!(in_scope(&job, &EspejoScope::Project("project-a".into())));
        assert!(!in_scope(&job, &EspejoScope::Project("project-b".into())));
        job.project = None;
        assert!(!in_scope(&job, &EspejoScope::Project("project-a".into())));
    }
}
