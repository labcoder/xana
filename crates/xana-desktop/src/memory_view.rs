//! Retained memory controls. Database work is explicit, bounded, and off-thread.
use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, SharedString, Subscription, Task,
    Window, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use xana::desktop::{
    DesktopControlPlane, DesktopMemoryMutation, MemoryControlEdit, MemoryControls, MemoryEdit,
    MemoryRecord, MemoryScope,
};

#[derive(Clone)]
struct ScopeChoice {
    key: String,
    label: SharedString,
}
impl SelectItem for ScopeChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &String {
        &self.key
    }
}

#[derive(Clone, Copy)]
enum EditAction {
    Remember,
    Correct,
    Move,
    Disable,
    Controls,
}

pub(crate) struct MemoryView {
    control: DesktopControlPlane,
    scope: Entity<InputState>,
    choices: Entity<SelectState<SearchableVec<ScopeChoice>>>,
    statement: Entity<TextareaState>,
    expires: Entity<InputState>,
    detail: Entity<TextareaState>,
    records: Vec<MemoryRecord>,
    selected: Option<MemoryRecord>,
    controls: Option<MemoryControls>,
    loaded_scope: Option<MemoryScope>,
    next_after: Option<u64>,
    conversation: Option<String>,
    use_enabled: bool,
    learning_enabled: bool,
    no_memory: bool,
    confirm_scope: bool,
    restore_review_required: bool,
    status: String,
    busy: bool,
    task: Option<Task<()>>,
    _scope_subscription: Subscription,
}

impl MemoryView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choices = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![ScopeChoice {
                    key: "user".into(),
                    label: "All conversations (user-wide)".into(),
                }]),
                Some(IndexPath::default()),
                window,
                cx,
            )
            .searchable(true)
        });
        let subscription = cx.subscribe_in(&choices, window, |this, _, event, window, cx| {
            if let SelectEvent::Confirm(Some(scope)) = event {
                if this.busy {
                    return;
                }
                this.scope
                    .update(cx, |input, cx| input.set_value(scope.clone(), window, cx));
                this.confirm_scope = false;
                this.status = "Scope selected. Refresh to browse it, or explicitly move the inspected record here.".into();
                cx.notify();
            }
        });
        Self {
            control,
            scope: cx.new(|cx| InputState::new(window,cx).default_value("user")),
            choices,
            statement: cx.new(|cx| TextareaState::new(window,cx).auto_grow(3,8)),
            expires: cx.new(|cx| InputState::new(window,cx).placeholder("Optional UTC Unix seconds; empty: until changed")),
            detail: cx.new(|cx| TextareaState::new(window,cx).auto_grow(2,10)),
            records: Vec::new(),
            selected: None,
            controls: None,
            loaded_scope: None,
            next_after: None,
            conversation: None,
            use_enabled: true,
            learning_enabled: true,
            no_memory: false,
            confirm_scope: false,
            restore_review_required: false,
            status: "Refresh to inspect protected personal memory. No provider call. Automatic learning and prompt selection are separate features.".into(),
            busy: false,
            task: None,
            _scope_subscription: subscription,
        }
    }
    pub(crate) fn open_for(
        &mut self,
        conversation: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.conversation = conversation;
        if self.loaded_scope.is_none()
            && let Some(id) = &self.conversation
        {
            self.scope.update(cx, |input, cx| {
                input.set_value(format!("conversation:{id}"), window, cx)
            });
        }
        self.load(false, window, cx);
    }
    fn scope(&self, cx: &Context<Self>) -> Result<MemoryScope, String> {
        self.scope
            .read(cx)
            .value()
            .trim()
            .parse()
            .map_err(|error| format!("{error:#}"))
    }
    fn load(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let scope = match self.scope(cx) {
            Ok(scope) => scope,
            Err(error) => {
                self.status = error;
                cx.notify();
                return;
            }
        };
        let after = if next && self.loaded_scope.as_ref() == Some(&scope) {
            self.next_after
        } else {
            None
        };
        self.busy = true;
        let control = self.control.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.personal_memory_snapshot(scope, after) })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(snapshot) => this.apply_snapshot(snapshot, window, cx),
                    Err(error) => {
                        this.status = error.message;
                        this.next_after = None;
                        this.controls = None;
                        this.records.clear();
                        this.selected = None;
                        this.detail
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn apply_snapshot(
        &mut self,
        snapshot: xana::desktop::DesktopMemorySnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restore_review_required = snapshot.restore_review_required;
        self.loaded_scope = Some(snapshot.controls.scope.clone());
        self.use_enabled = snapshot.controls.use_enabled;
        self.learning_enabled = snapshot.controls.learning_enabled;
        self.no_memory = snapshot.controls.no_memory;
        self.controls = Some(snapshot.controls);
        self.records = snapshot.page.records;
        self.next_after = snapshot.page.next_after;
        self.selected = None;
        self.confirm_scope = false;
        self.detail
            .update(cx, |input, cx| input.set_value("", window, cx));
        let mut choices = snapshot
            .scope_options
            .into_iter()
            .map(|(key, label)| ScopeChoice {
                key,
                label: label.into(),
            })
            .collect::<Vec<_>>();
        if let Some(id) = &self.conversation {
            let key = format!("conversation:{id}");
            if !choices.iter().any(|choice| choice.key == key) {
                choices.push(ScopeChoice {
                    key,
                    label: "Current Conversation".into(),
                });
            }
        }
        self.choices.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(choices), window, cx)
        });
        self.status = format!(
            "{} record(s) in this page. Inspect a record to correct, disable or move it. Disabling is not robust forgetting.",
            self.records.len()
        );
    }
    fn inspect(&mut self, row: MemoryRecord, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.statement.update(cx, |input, cx| {
            input.set_value(row.statement.clone(), window, cx)
        });
        self.expires.update(cx, |input, cx| {
            input.set_value(
                row.valid_until_unix_seconds
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                window,
                cx,
            )
        });
        self.detail.update(cx,|input,cx|input.set_value(format!("ID: {}\nRevision: {} · {:?} · {:?}\nScope: {}\nCreated: {} · Changed: {}\nOwner request: {}\nOrigin Conversation: {:?}",row.id,row.revision,row.state,row.claim,row.scope,row.created.at_unix_seconds,row.changed.at_unix_seconds,row.changed.owner_request,row.created.conversation),window,cx));
        self.selected = Some(row);
        self.confirm_scope = false;
        cx.notify();
    }
    fn request(
        &self,
        action: EditAction,
        cx: &Context<Self>,
    ) -> Result<DesktopMemoryMutation, String> {
        let scope = self.scope(cx)?;
        if matches!(action, EditAction::Controls) {
            let controls = self
                .controls
                .as_ref()
                .ok_or("Refresh before editing controls")?;
            if controls.scope != scope {
                return Err("Refresh the chosen scope before editing its controls".into());
            }
            return Ok(DesktopMemoryMutation::Controls {
                scope,
                edit: MemoryControlEdit {
                    expected_revision: Some(controls.revision),
                    use_enabled: Some(self.use_enabled),
                    learning_enabled: Some(self.learning_enabled),
                    no_memory: Some(self.no_memory),
                },
            });
        }
        let statement = self.statement.read(cx).value().to_string();
        let expiry = self.expires.read(cx).value();
        let expires_at = if expiry.trim().is_empty() {
            None
        } else {
            Some(
                expiry
                    .trim()
                    .parse()
                    .map_err(|_| "Expiry must be UTC Unix seconds or empty")?,
            )
        };
        if matches!(action, EditAction::Remember) {
            return Ok(DesktopMemoryMutation::Remember {
                scope,
                statement,
                expires_at,
            });
        }
        let row = self.selected.as_ref().ok_or("Inspect a record first")?;
        let edit = match action {
            EditAction::Correct => MemoryEdit::Correct {
                statement,
                valid_until_unix_seconds: expires_at,
            },
            EditAction::Move => MemoryEdit::Scope {
                target: scope,
                confirm: self.confirm_scope,
            },
            EditAction::Disable => MemoryEdit::Disable,
            _ => unreachable!("creation and controls returned above"),
        };
        Ok(DesktopMemoryMutation::Revise {
            id: row.id,
            revision: row.revision,
            edit,
        })
    }
    fn save(&mut self, action: EditAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let request = match self.request(action, cx) {
            Ok(request) => request,
            Err(error) => {
                self.status = error;
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let control = self.control.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.mutate_personal_memory(request) })
                .await;
            _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                this.status = match result {
                    Ok(receipt) => {
                        this.selected = None;
                        this.controls = None;
                        format!("{receipt} Refresh to inspect the latest state.")
                    }
                    Err(error) => error.message,
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let scope = match self.scope(cx) {
            Ok(scope) => scope,
            Err(error) => {
                self.status = error;
                cx.notify();
                return;
            }
        };
        let picker =
            cx.prompt_for_new_path(std::path::Path::new("."), Some("xana-memory-readable.json"));
        self.busy = true;
        let control = self.control.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = match picker.await {
                Ok(Ok(Some(path))) => cx.background_executor().spawn(async move {
                    control.export_personal_memory(scope, &path)
                        .map(|count| format!("Exported {count} records to {}. This readable copy is outside managed encryption.",path.display()))
                        .map_err(|error| error.message)
                }).await,
                Ok(Ok(None)) => Ok("Export cancelled; no file created".into()),
                _ => Err("The export destination picker failed; no file created".into()),
            };
            _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                this.status = result.unwrap_or_else(|error| error);
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for MemoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let selected = self.selected.is_some();
        let records = self.records.iter().map(|row| {
            let row = row.clone();
            Button::new(format!("memory-{}", row.id))
                .label(format!(
                    "Inspect · {:?} · {}",
                    row.state,
                    row.statement.chars().take(96).collect::<String>()
                ))
                .disabled(self.busy)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.inspect(row.clone(), window, cx)),
                )
        });
        let actions = [
            (EditAction::Remember, "Remember in named scope"),
            (EditAction::Correct, "Correct selected"),
            (EditAction::Disable, "Disable selected"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (action, label))| {
            Button::new(format!("memory-edit-{i}"))
                .label(label)
                .disabled(self.busy || (!matches!(action, EditAction::Remember) && !selected))
                .on_click(cx.listener(move |this, _, window, cx| this.save(action, window, cx)))
        });
        let navigation = h_flex()
            .gap_2()
            .child(
                Button::new("memory-refresh")
                    .label("Refresh")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.load(false, window, cx))),
            )
            .child(
                Button::new("memory-next")
                    .label("Next page")
                    .disabled(self.busy || self.next_after.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.load(true, window, cx))),
            )
            .child(
                Button::new("memory-export")
                    .label("Export readable copy…")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
            );
        let controls = v_flex().gap(tokens.spacing.sm)
            .child("Scope controls — no-memory overrides use and learning without erasing either setting")
            .when(self.restore_review_required, |view| {
                view.child("Restore review required: memory use and learning are blocked independently of these settings. Inspect xana storage status; scope controls cannot clear this gate.")
            })
            .child(Checkbox::new("memory-use").label("Allow memory use").checked(self.use_enabled)
                .disabled(self.busy || self.controls.is_none())
                .on_click(cx.listener(|this, value, _, cx| { this.use_enabled = *value; cx.notify(); })))
            .child(Checkbox::new("memory-learn").label("Allow future authorized automatic learning (not running yet)")
                .checked(self.learning_enabled).disabled(self.busy || self.controls.is_none())
                .on_click(cx.listener(|this, value, _, cx| { this.learning_enabled = *value; cx.notify(); })))
            .child(Checkbox::new("memory-none").label("No memory in this scope").checked(self.no_memory)
                .disabled(self.busy || self.controls.is_none())
                .on_click(cx.listener(|this, value, _, cx| { this.no_memory = *value; cx.notify(); })))
            .child(Button::new("memory-controls-save").label("Save scope controls")
                .disabled(self.busy || self.controls.is_none())
                .on_click(cx.listener(|this, _, window, cx| this.save(EditAction::Controls, window, cx))));
        let content = v_flex().p(tokens.spacing.md).gap(tokens.spacing.sm)
            .child("Personal memory — explicit owner controls")
            .child("Scope to browse or target: select below, then Refresh to browse. A selected record stays available for an explicit Move.")
            .child(Select::new(&self.choices).disabled(self.busy))
            .child(Input::new(&self.scope).disabled(self.busy))
            .child(navigation)
            .child(if self.busy { "Working locally…".to_owned() } else { self.status.clone() })
            .children(records)
            .child(Textarea::new(&self.detail).readonly(true))
            .child("Statement (up to 4,096 UTF-8 bytes)")
            .child(Textarea::new(&self.statement).disabled(self.busy))
            .child("Valid until (optional UTC Unix timestamp)")
            .child(Input::new(&self.expires).disabled(self.busy))
            .child(h_flex().gap_2().flex_wrap().children(actions))
            .child(Checkbox::new("memory-confirm-scope")
                .label("I explicitly approve moving the selected record to the named scope")
                .checked(self.confirm_scope).disabled(self.busy || !selected)
                .on_click(cx.listener(|this, value, _, cx| { this.confirm_scope = *value; cx.notify(); })))
            .child(Button::new("memory-move").label("Move selected to named scope")
                .disabled(self.busy || !selected || !self.confirm_scope)
                .on_click(cx.listener(|this, _, window, cx| this.save(EditAction::Move, window, cx))))
            .child(controls);
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .id("memory-scroll")
                    .size_full()
                    .overflow_y_scrollbar()
                    .child(content),
            )
            .into_any_element()
    }
}
