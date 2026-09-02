//! Cold-launch selection and transition into the workspace-owned Workbench.

use crate::workbench::Workbench;
use gpui::{
    AnyElement, Context, Entity, PathPromptOptions, Render, Task, Window, div, prelude::*, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use std::path::PathBuf;
use xana::desktop::{
    DesktopClient, DesktopInstanceLease, DesktopLaunch, DesktopLaunchCatalog, DesktopLaunchChoice,
    DesktopLaunchChoiceKind, DesktopLaunchIntent, DesktopNativePaths,
};

enum ShellSurface {
    Launcher,
    Workbench(Entity<Workbench>),
}

/// Keeps the process-global instance lease alive while no workspace is selected.
pub(crate) struct DesktopShell {
    launch: DesktopLaunch,
    instance: Option<DesktopInstanceLease>,
    native_paths: DesktopNativePaths,
    catalog: DesktopLaunchCatalog,
    initial_intent: DesktopLaunchIntent,
    surface: ShellSurface,
    selected_folder: Option<PathBuf>,
    opening: bool,
    error: Option<String>,
    _opening_task: Option<Task<()>>,
}

impl DesktopShell {
    pub(crate) fn new(
        launch: DesktopLaunch,
        instance: DesktopInstanceLease,
        native_paths: DesktopNativePaths,
        catalog: DesktopLaunchCatalog,
        initial_intent: DesktopLaunchIntent,
    ) -> Self {
        Self {
            launch,
            instance: Some(instance),
            native_paths,
            catalog,
            initial_intent,
            surface: ShellSurface::Launcher,
            selected_folder: None,
            opening: false,
            error: None,
            _opening_task: None,
        }
    }

    fn choose_catalog_item(
        &mut self,
        choice: DesktopLaunchChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let launch = self.launch.with_choice(&choice);
        self.start_workbench(launch, window, cx);
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a workspace folder for Xana".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = selection.await;
            _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        this.selected_folder = paths.into_iter().next();
                        this.error = None;
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.error = Some(format!("Could not choose a folder: {error}"));
                    }
                    Err(error) => {
                        this.error = Some(format!("Folder picker stopped: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_selected_folder(
        &mut self,
        force_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.selected_folder.clone() else {
            return;
        };
        let launch = self.launch.with_workspace(workspace, force_new);
        self.start_workbench(launch, window, cx);
    }

    fn start_workbench(
        &mut self,
        launch: DesktopLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.opening {
            return;
        }
        let Some(instance) = self.instance.take() else {
            self.error = Some("Xana Desktop no longer owns its instance lease".to_owned());
            cx.notify();
            return;
        };
        self.opening = true;
        self.error = None;
        let task = cx.spawn_in(window, async move |this, cx| {
            let (runtime, instance) = cx
                .background_executor()
                .spawn(async move { (DesktopClient::launch(launch), instance) })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                this.opening = false;
                match runtime {
                    Ok(runtime) => {
                        let native_paths = this.native_paths.clone();
                        let initial_intent = this.initial_intent;
                        let workbench = cx.new(|cx| {
                            Workbench::new(
                                runtime,
                                instance,
                                native_paths,
                                initial_intent,
                                window,
                                cx,
                            )
                        });
                        this.surface = ShellSurface::Workbench(workbench);
                    }
                    Err(error) => {
                        this.instance = Some(instance);
                        this.error = Some(error.message);
                    }
                }
                cx.notify();
            });
        });
        self._opening_task = Some(task);
        cx.notify();
    }

    fn render_choice(&self, choice: &DesktopLaunchChoice, cx: &mut Context<Self>) -> AnyElement {
        let choice_for_click = choice.clone();
        let kind = match choice.kind {
            DesktopLaunchChoiceKind::Project => "Project",
            DesktopLaunchChoiceKind::Conversation => "Conversation",
            DesktopLaunchChoiceKind::Workspace => "Workspace",
        };
        Button::new(choice.id.clone())
            .w_full()
            .disabled(self.opening)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap(rems(1.))
                    .child(
                        v_flex()
                            .items_start()
                            .min_w_0()
                            .child(choice.label.clone())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(choice.detail.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(kind),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.choose_catalog_item(choice_for_click.clone(), window, cx);
            }))
            .into_any_element()
    }

    fn render_launcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let projects = self
            .catalog
            .projects
            .iter()
            .map(|choice| self.render_choice(choice, cx))
            .collect::<Vec<_>>();
        let recent = self
            .catalog
            .recent
            .iter()
            .map(|choice| self.render_choice(choice, cx))
            .collect::<Vec<_>>();
        let folder = self.selected_folder.as_ref().map(|folder| {
            v_flex()
                .w_full()
                .gap(tokens.spacing.sm)
                .p(tokens.spacing.md)
                .rounded(tokens.radius.md)
                .border_1()
                .border_color(cx.theme().border)
                .child("Selected workspace")
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(folder.display().to_string()),
                )
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .child(
                            Button::new("open-selected-workspace")
                                .label("Open latest Conversation")
                                .disabled(self.opening)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_selected_folder(false, window, cx);
                                })),
                        )
                        .child(
                            Button::new("new-ungrouped-conversation")
                                .label("New ungrouped Conversation")
                                .primary()
                                .disabled(self.opening)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_selected_folder(true, window, cx);
                                })),
                        ),
                )
        });

        v_flex()
            .size_full()
            .items_center()
            .overflow_y_scrollbar()
            .p(tokens.spacing.xl)
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(52.))
                    .gap(tokens.spacing.lg)
                    .child(
                        v_flex()
                            .gap(tokens.spacing.xs)
                            .child(div().text_2xl().child("Open Xana"))
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Choose where to work. Nothing is created until you make an explicit selection."),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "Configuration: {}",
                                        self.catalog.configuration_state
                                    )),
                            ),
                    )
                    .when(!recent.is_empty(), |content| {
                        content.child(
                            v_flex()
                                .gap(tokens.spacing.sm)
                                .child(div().text_lg().child("Recent"))
                                .children(recent),
                        )
                    })
                    .when(!projects.is_empty(), |content| {
                        content.child(
                            v_flex()
                                .gap(tokens.spacing.sm)
                                .child(div().text_lg().child("Projects"))
                                .children(projects),
                        )
                    })
                    .when(
                        self.catalog.projects.is_empty() && self.catalog.recent.is_empty(),
                        |content| {
                            content.child(
                                div()
                                    .p(tokens.spacing.md)
                                    .rounded(tokens.radius.md)
                                    .bg(cx.theme().muted)
                                    .child("No recent workspace or Project is available yet. Choose a folder to begin."),
                            )
                        },
                    )
                    .child(
                        Button::new("choose-workspace-folder")
                            .label("Choose a folder…")
                            .primary()
                            .disabled(self.opening)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_folder(window, cx);
                            })),
                    )
                    .when_some(folder, |content, folder| content.child(folder))
                    .when(self.opening, |content| {
                        content.child(
                            div()
                                .text_color(cx.theme().accent_foreground)
                                .child("Opening the local Xana runtime…"),
                        )
                    })
                    .when_some(self.error.clone(), |content, error| {
                        content.child(
                            div()
                                .p(tokens.spacing.md)
                                .rounded(tokens.radius.md)
                                .bg(cx.theme().danger.opacity(0.12))
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
                    }),
            )
    }
}

impl Render for DesktopShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.surface {
            ShellSurface::Launcher => self.render_launcher(cx).into_any_element(),
            ShellSurface::Workbench(workbench) => workbench.clone().into_any_element(),
        }
    }
}
