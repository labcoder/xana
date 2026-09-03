//! Retained graphical setup state over Xana's typed control plane.

use crate::model_filter;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, Subscription, Task, Window, div, prelude::*, px, rems, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    scroll::ScrollableElement as _,
    v_flex, v_virtual_list,
};
use std::rc::Rc;
use xana::desktop::{
    DesktopControlPlane, DesktopCredentialInput, DesktopModelOption, DesktopPermissionMode,
    DesktopProviderKind, DesktopSecret, DesktopSetupDraft, DesktopSetupMode, DesktopSetupReceipt,
    DesktopSetupSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetupViewEvent {
    Cancel,
    Completed {
        mode: DesktopSetupMode,
        receipt: DesktopSetupReceipt,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    ChooseMode,
    Configure,
    ChooseModel,
    Review,
}

pub(crate) struct SetupView {
    control: DesktopControlPlane,
    snapshot: Result<DesktopSetupSnapshot, String>,
    step: SetupStep,
    mode: DesktopSetupMode,
    provider: DesktopProviderKind,
    permission: DesktopPermissionMode,
    connection: Entity<InputState>,
    endpoint: Entity<InputState>,
    credential: Entity<InputState>,
    credential_source: Entity<InputState>,
    codex_program: Entity<InputState>,
    model_search: Entity<InputState>,
    models: Vec<DesktopModelOption>,
    selected_model: Option<String>,
    busy: Option<String>,
    error: Option<String>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SetupView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let provider = DesktopProviderKind::Ollama;
        let draft =
            DesktopSetupDraft::for_provider(provider, DesktopSetupMode::StartWithConnection);
        let connection = input(window, cx, "Connection name", &draft.connection, false);
        let endpoint = input(
            window,
            cx,
            "Provider endpoint",
            draft.endpoint.as_deref().unwrap_or_default(),
            false,
        );
        let credential = input(window, cx, "API key", "", true);
        let credential_source = input(window, cx, "Environment variable", "", false);
        let codex_program = input(window, cx, "Codex executable", "codex", false);
        let model_search = input(window, cx, "Filter models", "", false);
        let subscriptions = [
            &connection,
            &endpoint,
            &credential_source,
            &codex_program,
            &model_search,
        ]
        .into_iter()
        .map(|state| {
            cx.subscribe_in(state, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
        })
        .collect();
        Self {
            snapshot: control.setup_snapshot().map_err(|error| error.message),
            control,
            step: SetupStep::ChooseMode,
            mode: DesktopSetupMode::StartWithConnection,
            provider,
            permission: DesktopPermissionMode::Ask,
            connection,
            endpoint,
            credential,
            credential_source,
            codex_program,
            model_search,
            models: Vec::new(),
            selected_model: None,
            busy: None,
            error: None,
            _task: None,
            _subscriptions: subscriptions,
        }
    }

    fn choose_mode(&mut self, mode: DesktopSetupMode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.error = None;
        if mode == DesktopSetupMode::Blank {
            self.commit_blank(cx);
        } else {
            self.step = SetupStep::Configure;
            cx.notify();
        }
    }

    fn choose_provider(
        &mut self,
        provider: DesktopProviderKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.provider = provider;
        let draft = DesktopSetupDraft::for_provider(provider, self.mode);
        self.connection.update(cx, |state, cx| {
            state.set_value(draft.connection, window, cx)
        });
        self.endpoint.update(cx, |state, cx| {
            state.set_value(draft.endpoint.unwrap_or_default(), window, cx)
        });
        self.codex_program.update(cx, |state, cx| {
            state.set_value(
                draft.codex_program.unwrap_or_else(|| "codex".to_owned()),
                window,
                cx,
            )
        });
        self.credential
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.credential_source
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.models.clear();
        self.selected_model = None;
        self.error = None;
        cx.notify();
    }

    fn draft(&self, cx: &App) -> Result<(DesktopSetupDraft, Option<DesktopSecret>), String> {
        let connection = self.connection.read(cx).value().trim().to_owned();
        let endpoint = nonblank(self.endpoint.read(cx).value().as_ref());
        let credential = if self.provider.uses_managed_account()
            || matches!(self.provider, DesktopProviderKind::Ollama)
        {
            DesktopCredentialInput::None
        } else if let Some(variable) = nonblank(self.credential_source.read(cx).value().as_ref()) {
            DesktopCredentialInput::Environment { variable }
        } else if self.provider == DesktopProviderKind::OpenAiCompatible
            && self.credential.read(cx).value().trim().is_empty()
        {
            DesktopCredentialInput::None
        } else {
            DesktopCredentialInput::Stored {
                id: connection.clone(),
            }
        };
        let secret = if matches!(credential, DesktopCredentialInput::Stored { .. }) {
            Some(
                DesktopSecret::new(self.credential.read(cx).value().to_string())
                    .map_err(|error| error.message)?,
            )
        } else {
            None
        };
        Ok((
            DesktopSetupDraft {
                mode: self.mode,
                provider: self.provider,
                connection,
                endpoint,
                codex_program: self
                    .provider
                    .uses_managed_account()
                    .then(|| self.codex_program.read(cx).value().to_string()),
                codex_home: None,
                credential,
                model: self.selected_model.clone(),
                reasoning_effort: None,
                permission_mode: self.permission,
            },
            secret,
        ))
    }

    fn discover(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let (draft, secret) = match self.draft(cx) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = Some("Establishing connection and discovering models…".to_owned());
        self.error = None;
        let control = self.control.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.discover_setup(&draft, secret.as_ref()).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(models) if !models.is_empty() => {
                        this.models = models;
                        this.selected_model = None;
                        this.step = SetupStep::ChooseModel;
                    }
                    Ok(_) => {
                        this.error = Some(
                            "The connection was reached but advertised no selectable models."
                                .to_owned(),
                        )
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let (draft, secret) = match self.draft(cx) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let mode = self.mode;
        self.busy = Some("Revalidating and installing configuration…".to_owned());
        self.error = None;
        let control = self.control.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.commit_setup(&draft, secret.as_ref()).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => cx.emit(SetupViewEvent::Completed { mode, receipt }),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn commit_blank(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Creating an intentionally blank Xana home…".to_owned());
        self.error = None;
        let control = self.control.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.commit_blank() })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => cx.emit(SetupViewEvent::Completed {
                        mode: DesktopSetupMode::Blank,
                        receipt,
                    }),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn filtered_model_indices(&self, cx: &App) -> Vec<usize> {
        let query = self.model_search.read(cx).value().trim().to_lowercase();
        self.models
            .iter()
            .enumerate()
            .filter(|(_, model)| model_filter::matches(model, &query))
            .map(|(index, _)| index)
            .collect()
    }

    fn render_mode(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let options = [
            (
                DesktopSetupMode::StartWithConnection,
                "Start with one connection",
                "Connect one local, API-key, or managed provider; discover its live models before choosing one.",
            ),
            (
                DesktopSetupMode::FullCustomize,
                "Full customize",
                "Create the connection first, then continue directly into the complete Settings workbench.",
            ),
            (
                DesktopSetupMode::Blank,
                "Blank",
                "Create no provider or Conversation. You can connect later from Settings.",
            ),
        ];
        v_flex()
            .gap(tokens.spacing.md)
            .child(div().text_2xl().child("Welcome to Xana"))
            .child(div().text_color(cx.theme().muted_foreground).child(
                "Choose how much to configure now. Xana does not recommend or preselect a vendor.",
            ))
            .children(options.into_iter().map(|(mode, title, description)| {
                Button::new(format!("setup-mode-{mode:?}"))
                    .w_full()
                    .disabled(self.busy.is_some())
                    .child(
                        v_flex()
                            .w_full()
                            .items_start()
                            .gap(tokens.spacing.xs)
                            .child(title)
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(description),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_mode(mode, cx)))
            }))
            .into_any_element()
    }

    fn render_configure(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let provider_buttons = DesktopProviderKind::ALL
            .into_iter()
            .map(|provider| {
                Button::new(format!("setup-provider-{}", provider.id()))
                    .compact()
                    .label(provider.title())
                    .selected(provider == self.provider)
                    .disabled(self.busy.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_provider(provider, window, cx);
                    }))
            })
            .collect::<Vec<_>>();
        v_flex()
            .gap(tokens.spacing.lg)
            .child(step_heading(
                "Connection",
                "Choose authority before model selection",
                cx,
            ))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.xs)
                    .children(provider_buttons),
            )
            .child(field("Connection name", Input::new(&self.connection), cx))
            .when(!self.provider.uses_managed_account(), |form| {
                form.child(field("Endpoint", Input::new(&self.endpoint), cx))
            })
            .when(self.provider.uses_managed_account(), |form| {
                form.child(field(
                    "Codex executable",
                    Input::new(&self.codex_program),
                    cx,
                ))
            })
            .when(
                !self.provider.uses_managed_account()
                    && !matches!(self.provider, DesktopProviderKind::Ollama),
                |form| {
                    form.child(
                        v_flex()
                            .gap(tokens.spacing.sm)
                            .child(field(
                                "API key (stored in the operating-system credential store)",
                                Input::new(&self.credential)
                                    .content_type(InputContentType::Password)
                                    .mask_toggle(),
                                cx,
                            ))
                            .child(field(
                                "Or use an environment variable",
                                Input::new(&self.credential_source),
                                cx,
                            )),
                    )
                },
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Button::new("setup-back-mode")
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step = SetupStep::ChooseMode;
                                this.error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("setup-discover")
                            .label("Establish connection")
                            .primary()
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.discover(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let indices = self.filtered_model_indices(cx);
        let count = indices.len();
        let sizes = Rc::new(vec![size(px(760.), px(72.)); count]);
        let selected = self.selected_model.clone();
        let models = self.models.clone();
        let index_snapshot = indices.clone();
        let list = v_virtual_list(
            cx.entity(),
            "setup-model-list",
            sizes,
            move |_this, range, _, cx| {
                range
                    .filter_map(|visible| index_snapshot.get(visible).copied())
                    .filter_map(|index| models.get(index))
                    .map(|model| {
                        let id = model.id.clone();
                        let subtitle = model_summary(model);
                        Button::new(format!("setup-model-{}", model.id))
                            .w_full()
                            .ghost()
                            .selected(selected.as_deref() == Some(model.id.as_str()))
                            .child(
                                v_flex()
                                    .w_full()
                                    .items_start()
                                    .child(model.display_name.clone())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(subtitle),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_model = Some(id.clone());
                                cx.notify();
                            }))
                    })
                    .collect::<Vec<_>>()
            },
        )
        .size_full();
        v_flex()
            .id("xana-setup")
            .role(Role::Region)
            .aria_label("Xana setup")
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .child(step_heading(
                "Model",
                &format!("{} live model(s) available", self.models.len()),
                cx,
            ))
            .child(Input::new(&self.model_search))
            .child(div().flex_1().min_h(rems(14.)).child(list))
            .when(count == 0, |content| {
                content.child("No models match this filter.")
            })
            .child(
                h_flex()
                    .justify_between()
                    .child(Button::new("setup-back-connection").label("Back").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.step = SetupStep::Configure;
                            cx.notify();
                        }),
                    ))
                    .child(
                        Button::new("setup-review")
                            .label("Review")
                            .primary()
                            .disabled(self.selected_model.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step = SetupStep::Review;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let facts = [
            ("Setup path", format!("{:?}", self.mode)),
            ("Provider", self.provider.title().to_owned()),
            ("Connection", self.connection.read(cx).value().to_string()),
            (
                "Model",
                self.selected_model
                    .clone()
                    .unwrap_or_else(|| "Not selected".to_owned()),
            ),
            ("Permissions", self.permission.id().to_owned()),
            (
                "Credential",
                if self.provider.uses_managed_account() {
                    "Owned by the managed runtime".to_owned()
                } else if !self.credential_source.read(cx).value().trim().is_empty() {
                    "Named environment variable".to_owned()
                } else if self.credential.read(cx).value().trim().is_empty() {
                    "Not required".to_owned()
                } else {
                    "New value will be stored in the OS credential store".to_owned()
                },
            ),
        ];
        v_flex()
            .gap(tokens.spacing.lg)
            .child(step_heading(
                "Review",
                "The connection is revalidated immediately before the atomic commit",
                cx,
            ))
            .children(facts.into_iter().map(|(label, value)| {
                h_flex()
                    .justify_between()
                    .gap(tokens.spacing.lg)
                    .child(div().text_color(cx.theme().muted_foreground).child(label))
                    .child(value)
            }))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.xs)
                    .child("Default permission")
                    .children(DesktopPermissionMode::ALL.into_iter().map(|permission| {
                        Button::new(format!("setup-permission-{}", permission.id()))
                            .compact()
                            .label(permission.id())
                            .selected(permission == self.permission)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.permission = permission;
                                cx.notify();
                            }))
                    })),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Button::new("setup-back-model")
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step = SetupStep::ChooseModel;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("setup-commit")
                            .label("Install configuration")
                            .primary()
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
                    ),
            )
            .into_any_element()
    }
}

impl EventEmitter<SetupViewEvent> for SetupView {}

impl Render for SetupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let state = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.configuration_state.as_str())
            .unwrap_or("unavailable");
        let body = match self.step {
            SetupStep::ChooseMode => self.render_mode(cx),
            SetupStep::Configure => self.render_configure(cx),
            SetupStep::ChooseModel => self.render_models(cx),
            SetupStep::Review => self.render_review(cx),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .justify_between()
                    .px(tokens.spacing.lg)
                    .py(tokens.spacing.md)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex().child(div().text_lg().child("Xana Setup")).child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Configuration: {state}")),
                        ),
                    )
                    .child(
                        Button::new("setup-cancel")
                            .label("Cancel")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SetupViewEvent::Cancel))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p(tokens.spacing.xl)
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(rems(58.))
                            .mx_auto()
                            .gap(tokens.spacing.md)
                            .child(body)
                            .when_some(self.busy.clone(), |content, busy| {
                                content.child(
                                    div()
                                        .p(tokens.spacing.md)
                                        .rounded(tokens.radius.md)
                                        .bg(cx.theme().accent.opacity(0.1))
                                        .child(busy),
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
                    ),
            )
    }
}

fn input(
    window: &mut Window,
    cx: &mut Context<SetupView>,
    placeholder: &str,
    value: &str,
    masked: bool,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder.to_owned())
            .default_value(value.to_owned())
            .masked(masked)
    })
}

fn field(
    label: &'static str,
    input: impl IntoElement,
    cx: &mut Context<SetupView>,
) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    v_flex().gap(tokens.spacing.xs).child(label).child(input)
}

fn step_heading(
    title: impl Into<String>,
    detail: impl Into<String>,
    cx: &mut Context<SetupView>,
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

fn nonblank(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn model_summary(model: &DesktopModelOption) -> String {
    let mut facts = vec![format!("id {}", model.id)];
    if !model.input_modalities.is_empty() {
        facts.push(format!("input {}", model.input_modalities.join(", ")));
    }
    if let Some(context) = model.context_tokens {
        facts.push(format!("context {context}"));
    }
    if let Some(reasoning) = model.reasoning {
        facts.push(if reasoning {
            "reasoning".to_owned()
        } else {
            "no reasoning".to_owned()
        });
    }
    if let Some(pricing) = &model.pricing {
        facts.push(pricing.clone());
    }
    facts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_summary_preserves_known_capability_facts() {
        let model = DesktopModelOption {
            id: "model-a".to_owned(),
            display_name: "Model A".to_owned(),
            input_modalities: vec!["text".to_owned(), "image".to_owned()],
            output_modalities: Vec::new(),
            tools: Some(true),
            reasoning: Some(true),
            reasoning_efforts: vec!["high".to_owned()],
            default_reasoning_effort: Some("high".to_owned()),
            context_tokens: Some(128_000),
            max_output_tokens: None,
            pricing: Some("in $1.00/M".to_owned()),
            source: "remote".to_owned(),
        };
        let summary = model_summary(&model);
        assert!(summary.contains("text, image"));
        assert!(summary.contains("128000"));
        assert!(summary.contains("reasoning"));
        assert!(summary.contains("$1.00/M"));
    }
}
