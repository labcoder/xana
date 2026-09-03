//! Focused Profile, Project, and capability management for Desktop settings.

use gpui::{
    AnyElement, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, Role, Subscription, Task, Window, div,
    prelude::*, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};
use std::path::PathBuf;
use xana::desktop::{
    DesktopCapabilitySnapshot, DesktopControlPlane, DesktopEntityMutationReceipt,
    DesktopManagementSnapshot, DesktopProfileDraft, DesktopProjectDraft,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagementTab {
    Profiles,
    Projects,
    Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagementViewEvent {
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirmation {
    DeleteProfile(String),
    ForgetProject(String),
}

pub(crate) struct ManagementView {
    control: DesktopControlPlane,
    tab: ManagementTab,
    snapshot: Result<DesktopManagementSnapshot, String>,
    capabilities: Result<DesktopCapabilitySnapshot, String>,
    selected_profile: Option<String>,
    selected_project: Option<String>,
    profile_name: Entity<InputState>,
    profile_connection: Entity<InputState>,
    profile_model: Entity<InputState>,
    project_name: Entity<InputState>,
    project_workspace: Option<PathBuf>,
    confirmation: Option<Confirmation>,
    busy: Option<String>,
    error: Option<String>,
    receipt: Option<DesktopEntityMutationReceipt>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ManagementView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        tab: ManagementTab,
        snapshot: Result<DesktopManagementSnapshot, String>,
        capabilities: Result<DesktopCapabilitySnapshot, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected_profile = snapshot
            .as_ref()
            .ok()
            .and_then(|snapshot| snapshot.profiles.first())
            .map(|profile| profile.name.clone());
        let selected_project = snapshot
            .as_ref()
            .ok()
            .and_then(|snapshot| snapshot.projects.first())
            .map(|project| project.id.clone());
        let defaults = snapshot.as_ref().ok().and_then(|snapshot| {
            snapshot
                .profiles
                .iter()
                .find(|profile| profile.name == snapshot.default_profile)
        });
        let profile_name = input(window, cx, "Profile name", "");
        let profile_connection = input(
            window,
            cx,
            "Connection id",
            defaults.map_or("", |profile| profile.connection.as_str()),
        );
        let profile_model = input(
            window,
            cx,
            "Exact model id",
            defaults.map_or("", |profile| profile.model.as_str()),
        );
        let project_name = input(window, cx, "Project name", "");
        let subscriptions = [
            &profile_name,
            &profile_connection,
            &profile_model,
            &project_name,
        ]
        .into_iter()
        .map(|input| {
            cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
        })
        .collect();
        Self {
            control,
            tab,
            snapshot,
            capabilities,
            selected_profile,
            selected_project,
            profile_name,
            profile_connection,
            profile_model,
            project_name,
            project_workspace: None,
            confirmation: None,
            busy: None,
            error: None,
            receipt: None,
            _task: None,
            _subscriptions: subscriptions,
        }
    }

    fn apply_snapshots(
        &mut self,
        snapshot: Result<DesktopManagementSnapshot, String>,
        capabilities: Result<DesktopCapabilitySnapshot, String>,
    ) {
        self.snapshot = snapshot;
        self.capabilities = capabilities;
        if let Ok(snapshot) = &self.snapshot {
            if self.selected_profile.as_ref().is_none_or(|selected| {
                !snapshot
                    .profiles
                    .iter()
                    .any(|profile| &profile.name == selected)
            }) {
                self.selected_profile = snapshot.profiles.first().map(|row| row.name.clone());
            }
            if self.selected_project.as_ref().is_none_or(|selected| {
                !snapshot
                    .projects
                    .iter()
                    .any(|project| &project.id == selected)
            }) {
                self.selected_project = snapshot.projects.first().map(|row| row.id.clone());
            }
        }
    }

    fn choose_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose the existing workspace for this Project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        this.project_workspace = paths.into_iter().next();
                        this.error = None;
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.error = Some(format!("Folder selection failed: {error}"))
                    }
                    Err(error) => this.error = Some(format!("Folder picker stopped: {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_profile(&mut self, cx: &mut Context<Self>) {
        let draft = DesktopProfileDraft {
            name: self.profile_name.read(cx).value().trim().to_owned(),
            connection: self.profile_connection.read(cx).value().trim().to_owned(),
            model: self.profile_model.read(cx).value().trim().to_owned(),
        };
        self.spawn_operation(
            "Creating Profile…",
            move |control| control.create_profile(draft),
            cx,
        );
    }

    fn create_project(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.project_workspace.clone() else {
            self.error = Some("Choose an existing workspace folder first.".to_owned());
            cx.notify();
            return;
        };
        let draft = DesktopProjectDraft {
            name: self.project_name.read(cx).value().trim().to_owned(),
            workspace,
        };
        self.spawn_operation(
            "Creating Project…",
            move |control| control.create_project(draft),
            cx,
        );
    }

    fn toggle_profile(&mut self, profile: String, archived: bool, cx: &mut Context<Self>) {
        self.spawn_operation(
            if archived {
                "Archiving Profile…"
            } else {
                "Restoring Profile…"
            },
            move |control| control.set_profile_archived(&profile, archived),
            cx,
        );
    }

    fn toggle_project(&mut self, project: String, archived: bool, cx: &mut Context<Self>) {
        self.spawn_operation(
            if archived {
                "Archiving Project…"
            } else {
                "Restoring Project…"
            },
            move |control| control.set_project_archived(&project, archived),
            cx,
        );
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        match confirmation {
            Confirmation::DeleteProfile(profile) => {
                self.spawn_operation(
                    "Deleting Profile…",
                    move |control| control.delete_profile(&profile),
                    cx,
                );
            }
            Confirmation::ForgetProject(project) => {
                self.spawn_operation(
                    "Forgetting Project…",
                    move |control| control.forget_project(&project),
                    cx,
                );
            }
        }
    }

    fn spawn_operation<F>(&mut self, label: &str, operation: F, cx: &mut Context<Self>)
    where
        F: FnOnce(
                DesktopControlPlane,
            ) -> Result<DesktopEntityMutationReceipt, xana::desktop::DesktopError>
            + Send
            + 'static,
    {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(label.to_owned());
        self.error = None;
        self.receipt = None;
        let control = self.control.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let receipt = operation(control.clone())?;
                    let snapshot = control.management_snapshot()?;
                    let capabilities = control.capability_snapshot()?;
                    Ok::<_, xana::desktop::DesktopError>((receipt, snapshot, capabilities))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok((receipt, snapshot, capabilities)) => {
                        this.receipt = Some(receipt);
                        this.apply_snapshots(Ok(snapshot), Ok(capabilities));
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_profiles(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let rows = self
            .snapshot
            .as_ref()
            .ok()
            .map(|snapshot| snapshot.profiles.as_slice())
            .unwrap_or_default();
        let selected = self.selected_profile.as_deref();
        let detail = selected
            .and_then(|selected| rows.iter().find(|profile| profile.name == selected))
            .cloned();
        h_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .w(rems(20.))
                    .h_full()
                    .overflow_y_scrollbar()
                    .p(tokens.spacing.md)
                    .gap(tokens.spacing.xs)
                    .children(rows.iter().map(|profile| {
                        let name = profile.name.clone();
                        Button::new(format!("profile-row-{name}"))
                            .w_full()
                            .ghost()
                            .selected(selected == Some(profile.name.as_str()))
                            .child(
                                v_flex()
                                    .w_full()
                                    .items_start()
                                    .child(profile.name.clone())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{}/{} · {}",
                                                profile.connection,
                                                profile.model,
                                                if profile.ready { "ready" } else { "attention" }
                                            )),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_profile = Some(name.clone());
                                cx.notify();
                            }))
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scrollbar()
                    .p(tokens.spacing.lg)
                    .gap(tokens.spacing.lg)
                    .child(section_heading(
                        "Profiles",
                        "Complete defaults for new Conversations; frozen Conversations remain unchanged.",
                        cx,
                    ))
                    .when_some(detail, |panel, profile| {
                        let profile_name = profile.name.clone();
                        let delete_name = profile.name.clone();
                        panel.child(
                            v_flex()
                                .gap(tokens.spacing.sm)
                                .child(format!("{} · id {}", profile.name, profile.id))
                                .child(format!("Connection/model: {}/{}", profile.connection, profile.model))
                                .child(format!("Permission: {}", profile.permission))
                                .children(profile.readiness.iter().map(|reason| format!("Attention: {reason}")))
                                .child(
                                    h_flex()
                                        .gap(tokens.spacing.sm)
                                        .child(
                                            Button::new("profile-toggle-archive")
                                                .label(if profile.archived { "Restore" } else { "Archive" })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.toggle_profile(profile_name.clone(), !profile.archived, cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("profile-delete")
                                                .label("Delete…")
                                                .danger()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.confirmation = Some(Confirmation::DeleteProfile(delete_name.clone()));
                                                    cx.notify();
                                                })),
                                        ),
                                ),
                        )
                    })
                    .child(
                        v_flex()
                            .gap(tokens.spacing.sm)
                            .child(div().text_lg().child("Create global Profile"))
                            .child(Input::new(&self.profile_name))
                            .child(Input::new(&self.profile_connection))
                            .child(Input::new(&self.profile_model))
                            .child(
                                Button::new("profile-create")
                                    .label("Create Profile")
                                    .primary()
                                    .disabled(
                                        self.busy.is_some()
                                            || self.profile_name.read(cx).value().trim().is_empty()
                                            || self.profile_connection.read(cx).value().trim().is_empty()
                                            || self.profile_model.read(cx).value().trim().is_empty(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.create_profile(cx))),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_projects(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let rows = self
            .snapshot
            .as_ref()
            .ok()
            .map(|snapshot| snapshot.projects.as_slice())
            .unwrap_or_default();
        let selected = self.selected_project.as_deref();
        let detail = selected
            .and_then(|selected| rows.iter().find(|row| row.id == selected))
            .cloned();
        h_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .w(rems(20.))
                    .h_full()
                    .overflow_y_scrollbar()
                    .p(tokens.spacing.md)
                    .gap(tokens.spacing.xs)
                    .children(rows.iter().map(|project| {
                        let id = project.id.clone();
                        Button::new(format!("project-row-{id}"))
                            .w_full()
                            .ghost()
                            .selected(selected == Some(project.id.as_str()))
                            .child(
                                v_flex()
                                    .w_full()
                                    .items_start()
                                    .child(project.name.clone())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} · {}",
                                                project.lifecycle, project.workspace_status
                                            )),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_project = Some(id.clone());
                                cx.notify();
                            }))
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scrollbar()
                    .p(tokens.spacing.lg)
                    .gap(tokens.spacing.lg)
                    .child(section_heading(
                        "Projects",
                        "Local organization over existing workspaces and Conversations.",
                        cx,
                    ))
                    .when_some(detail, |panel, project| {
                        let id = project.id.clone();
                        let forget_id = project.id.clone();
                        panel.child(
                            v_flex()
                                .gap(tokens.spacing.sm)
                                .child(format!("{} · {}", project.name, project.id))
                                .child(project.workspace.display().to_string())
                                .child(format!("{} Conversation(s)", project.conversation_count))
                                .child(
                                    h_flex()
                                        .gap(tokens.spacing.sm)
                                        .child(
                                            Button::new("project-toggle-archive")
                                                .label(if project.lifecycle == "archived" {
                                                    "Restore"
                                                } else {
                                                    "Archive"
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.toggle_project(
                                                        id.clone(),
                                                        project.lifecycle != "archived",
                                                        cx,
                                                    );
                                                })),
                                        )
                                        .child(
                                            Button::new("project-forget")
                                                .label("Forget…")
                                                .danger()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.confirmation =
                                                        Some(Confirmation::ForgetProject(
                                                            forget_id.clone(),
                                                        ));
                                                    cx.notify();
                                                })),
                                        ),
                                ),
                        )
                    })
                    .child(
                        v_flex()
                            .gap(tokens.spacing.sm)
                            .child(div().text_lg().child("Create Project"))
                            .child(Input::new(&self.project_name))
                            .child(
                                h_flex()
                                    .gap(tokens.spacing.sm)
                                    .child(
                                        Button::new("project-choose-workspace")
                                            .label("Choose workspace…")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.choose_workspace(window, cx);
                                            })),
                                    )
                                    .when_some(
                                        self.project_workspace.as_ref(),
                                        |row, workspace| row.child(workspace.display().to_string()),
                                    ),
                            )
                            .child(
                                Button::new("project-create")
                                    .label("Create Project")
                                    .primary()
                                    .disabled(
                                        self.busy.is_some()
                                            || self.project_name.read(cx).value().trim().is_empty()
                                            || self.project_workspace.is_none(),
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.create_project(cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_capabilities(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        match &self.capabilities {
            Err(error) => status(error.clone(), true, cx).into_any_element(),
            Ok(snapshot) => v_flex()
                .size_full()
                .min_h_0()
                .overflow_y_scrollbar()
                .p(tokens.spacing.lg)
                .gap(tokens.spacing.md)
                .child(section_heading(
                    "What can Xana do here?",
                    "Availability, permission, and selection are deliberately separate. This view is authoritative status; advanced integration lifecycle changes remain in Xana's typed terminal management flow in this build.",
                    cx,
                ))
                .child(format!(
                    "Profile {} · {}/{} · permission {}",
                    snapshot.profile, snapshot.connection, snapshot.model, snapshot.permission
                ))
                .children(snapshot.facts.iter().map(|fact| {
                    v_flex()
                        .gap(tokens.spacing.xs)
                        .p(tokens.spacing.md)
                        .rounded(tokens.radius.md)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(format!("{} · {}", fact.kind, fact.id))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "installed={} · enabled={} · available={} · permitted={} · selected={} · containment={}",
                                    known(fact.installed),
                                    known(fact.enabled),
                                    known(fact.available),
                                    known(fact.permitted),
                                    fact.selected,
                                    fact.containment
                                )),
                        )
                        .child(div().text_sm().child(fact.detail.clone()))
                }))
                .into_any_element(),
        }
    }

    fn render_confirmation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirmation = self.confirmation.as_ref()?;
        let tokens = cx.theme().semantic_tokens();
        let (title, detail) = match confirmation {
            Confirmation::DeleteProfile(profile) => (
                format!("Delete Profile {profile}?"),
                "Existing Conversations keep frozen snapshots; the global Profile definition is removed.",
            ),
            Confirmation::ForgetProject(project) => (
                format!("Forget Project {project}?"),
                "Only local organization is removed. Workspaces and Conversation history are preserved.",
            ),
        };
        Some(
            v_flex()
                .gap(tokens.spacing.sm)
                .p(tokens.spacing.md)
                .border_1()
                .border_color(cx.theme().danger)
                .child(title)
                .child(detail)
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .child(
                            Button::new("management-cancel-confirmation")
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirmation = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("management-confirm")
                                .label("Confirm")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl EventEmitter<ManagementViewEvent> for ManagementView {}

impl Render for ManagementView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let body = match self.tab {
            ManagementTab::Profiles => self.render_profiles(cx),
            ManagementTab::Projects => self.render_projects(cx),
            ManagementTab::Capabilities => self.render_capabilities(cx),
        };
        v_flex()
            .id("xana-management")
            .role(Role::Region)
            .aria_label("Profiles projects and capabilities")
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .justify_between()
                    .p(tokens.spacing.md)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex().gap(tokens.spacing.xs).children(
                            [
                                (ManagementTab::Profiles, "Profiles"),
                                (ManagementTab::Projects, "Projects"),
                                (ManagementTab::Capabilities, "Capabilities"),
                            ]
                            .into_iter()
                            .map(|(tab, label)| {
                                Button::new(format!("management-tab-{tab:?}"))
                                    .label(label)
                                    .selected(self.tab == tab)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.tab = tab;
                                        cx.notify();
                                    }))
                            }),
                        ),
                    )
                    .child(
                        Button::new("management-close")
                            .label("Back to Settings")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ManagementViewEvent::Close)),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .when_some(self.render_confirmation(cx), |panel, confirmation| {
                panel.child(confirmation)
            })
            .when_some(self.busy.clone(), |panel, busy| {
                panel.child(status(busy, false, cx))
            })
            .when_some(self.error.clone(), |panel, error| {
                panel.child(status(error, true, cx))
            })
            .when_some(self.receipt.as_ref(), |panel, receipt| {
                panel.child(status(
                    format!(
                        "{} · {} · {}",
                        receipt.semantic_code, receipt.effect, receipt.detail
                    ),
                    false,
                    cx,
                ))
            })
    }
}

fn input(
    window: &mut Window,
    cx: &mut Context<ManagementView>,
    placeholder: &str,
    value: &str,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder.to_owned())
            .default_value(value.to_owned())
    })
}

fn section_heading(
    title: impl Into<String>,
    detail: impl Into<String>,
    cx: &mut Context<ManagementView>,
) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    v_flex()
        .gap(tokens.spacing.xs)
        .child(div().text_2xl().child(title.into()))
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(detail.into()),
        )
}

fn status(
    message: impl Into<String>,
    danger: bool,
    cx: &mut Context<ManagementView>,
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

fn known(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}
