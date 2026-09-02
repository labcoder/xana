//! Authentication and destructive actions for one selected connection.

use gpui::{
    AnyElement, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    Subscription, Task, Window, div, prelude::*, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    v_flex,
};
use xana::desktop::{
    DesktopConnection, DesktopConnectionMutationReceipt, DesktopConnectionRemovalPlan,
    DesktopControlPlane, DesktopExecutionKind, DesktopManagedLogin, DesktopSecret,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectionActionsEvent {
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    DeleteCredential,
    LogoutManaged,
    RemoveConnection,
}

pub(crate) struct ConnectionActions {
    control: DesktopControlPlane,
    connection: DesktopConnection,
    secret: Entity<InputState>,
    confirmation: Option<Confirmation>,
    removal_plan: Option<DesktopConnectionRemovalPlan>,
    login: Option<DesktopManagedLogin>,
    busy: Option<String>,
    error: Option<String>,
    receipt: Option<DesktopConnectionMutationReceipt>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionActions {
    pub(crate) fn new(
        control: DesktopControlPlane,
        connection: DesktopConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let secret = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter replacement API key")
                .masked(true)
        });
        let subscription = cx.subscribe_in(&secret, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            control,
            connection,
            secret,
            confirmation: None,
            removal_plan: None,
            login: None,
            busy: None,
            error: None,
            receipt: None,
            _task: None,
            _subscriptions: vec![subscription],
        }
    }

    fn replace_credential(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let raw = self.secret.read(cx).value().to_string();
        let secret = match DesktopSecret::new(raw) {
            Ok(secret) => secret,
            Err(error) => {
                self.error = Some(error.message);
                cx.notify();
                return;
            }
        };
        self.secret
            .update(cx, |input, cx| input.set_value("", window, cx));
        let control = self.control.clone();
        let connection = self.connection.id.clone();
        self.busy = Some("Validating replacement before changing the OS credential store…".into());
        self.error = None;
        self.receipt = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.replace_credential(&connection, secret).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.finish(result, cx);
            });
        }));
        cx.notify();
    }

    fn begin_login(&mut self, device_code: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.login.is_some() {
            return;
        }
        let control = self.control.clone();
        let connection = self.connection.id.clone();
        self.busy = Some("Starting the vendor-owned Codex login…".into());
        self.error = None;
        self.receipt = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.begin_managed_login(&connection, device_code).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(login) if login.is_pending() => this.login = Some(login),
                    Ok(_) => {
                        this.receipt = Some(already_logged_in_receipt(&this.connection.id));
                        cx.emit(ConnectionActionsEvent::Changed);
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn complete_login(&mut self, cx: &mut Context<Self>) {
        let Some(login) = self.login.take() else {
            return;
        };
        self.busy = Some("Waiting for Codex authorization…".into());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = login.complete().await;
            _ = this.update(cx, |this, cx| this.finish(result, cx));
        }));
        cx.notify();
    }

    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        let Some(login) = self.login.take() else {
            return;
        };
        self.busy = Some("Cancelling the vendor-owned login attempt…".into());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = login.cancel().await;
            _ = this.update(cx, |this, cx| this.finish(result, cx));
        }));
        cx.notify();
    }

    fn prepare_removal(&mut self, cx: &mut Context<Self>) {
        match self
            .control
            .connection_removal_plan(&self.connection.id, Vec::new())
        {
            Ok(plan) => {
                self.removal_plan = Some(plan);
                self.confirmation = Some(Confirmation::RemoveConnection);
                self.error = None;
            }
            Err(error) => self.error = Some(error.message),
        }
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        self.busy = Some(
            match confirmation {
                Confirmation::DeleteCredential => "Deleting the named OS credential…",
                Confirmation::LogoutManaged => "Logging out the shared Codex account…",
                Confirmation::RemoveConnection => "Removing the reviewed connection declaration…",
            }
            .into(),
        );
        self.error = None;
        self.receipt = None;
        let control = self.control.clone();
        let connection = self.connection.id.clone();
        let removal_plan = self.removal_plan.take();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result: Result<DesktopConnectionMutationReceipt, String> = match confirmation {
                Confirmation::DeleteCredential => control
                    .delete_credential(&connection)
                    .map_err(|error| error.message),
                Confirmation::LogoutManaged => control
                    .logout_managed(&connection)
                    .await
                    .map_err(|error| error.message),
                Confirmation::RemoveConnection => removal_plan
                    .as_ref()
                    .ok_or_else(|| "the reviewed removal plan is unavailable".to_owned())
                    .and_then(|plan| {
                        control
                            .remove_connection(plan)
                            .map_err(|error| error.message)
                    }),
            };
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => {
                        this.receipt = Some(receipt);
                        cx.emit(ConnectionActionsEvent::Changed);
                    }
                    Err(message) => this.error = Some(message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn finish(
        &mut self,
        result: Result<DesktopConnectionMutationReceipt, xana::desktop::DesktopError>,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        match result {
            Ok(receipt) => {
                self.receipt = Some(receipt);
                cx.emit(ConnectionActionsEvent::Changed);
            }
            Err(error) => self.error = Some(error.message),
        }
        cx.notify();
    }

    fn render_authentication(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let managed = self.connection.execution == DesktopExecutionKind::Managed;
        let stored = self.connection.credential_source.starts_with("stored:");
        v_flex()
            .gap(tokens.spacing.sm)
            .child(div().text_lg().child("Authentication"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if managed {
                        "Codex owns this account under its configured CODEX_HOME. Login and logout may affect other Codex clients using that home."
                    } else if stored {
                        "A replacement is tested before the previous OS-stored key changes. The value is cleared from the form immediately."
                    } else {
                        "This connection does not use a Xana-owned stored credential."
                    }),
            )
            .when(stored, |panel| {
                panel
                    .child(
                        Input::new(&self.secret)
                            .content_type(InputContentType::Password)
                            .mask_toggle(),
                    )
                    .child(
                        h_flex()
                            .gap(tokens.spacing.sm)
                            .child(
                                Button::new("connection-replace-credential")
                                    .label("Validate and replace key")
                                    .primary()
                                    .disabled(
                                        self.secret.read(cx).value().trim().is_empty()
                                            || self.busy.is_some(),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.replace_credential(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("connection-delete-credential")
                                    .label("Delete stored key…")
                                    .disabled(self.busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirmation = Some(Confirmation::DeleteCredential);
                                        cx.notify();
                                    })),
                            ),
                    )
            })
            .when(managed && self.login.is_none(), |panel| {
                panel.child(
                    h_flex()
                        .flex_wrap()
                        .gap(tokens.spacing.sm)
                        .child(
                            Button::new("connection-login-browser")
                                .label("Login in browser")
                                .primary()
                                .disabled(self.busy.is_some())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.begin_login(false, cx);
                                })),
                        )
                        .child(
                            Button::new("connection-login-device")
                                .label("Use device code")
                                .disabled(self.busy.is_some())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.begin_login(true, cx);
                                })),
                        )
                        .child(
                            Button::new("connection-logout")
                                .label("Logout…")
                                .disabled(self.busy.is_some())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirmation = Some(Confirmation::LogoutManaged);
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.login.as_ref(), |panel, login| {
                panel.child(self.render_login(login, cx))
            })
            .into_any_element()
    }

    fn render_login(&self, login: &DesktopManagedLogin, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let url = login.authorization_url().unwrap_or_default().to_owned();
        v_flex()
            .gap(tokens.spacing.sm)
            .p(tokens.spacing.md)
            .rounded(tokens.radius.md)
            .border_1()
            .border_color(cx.theme().border)
            .child("Authorization started by Codex")
            .child(div().text_sm().child(url.clone()))
            .when_some(login.user_code(), |panel, code| {
                panel.child(format!("Device code: {code}"))
            })
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("connection-open-login")
                            .label("Open authorization page")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Button::new("connection-complete-login")
                            .label("I completed authorization")
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.complete_login(cx))),
                    )
                    .child(
                        Button::new("connection-cancel-login")
                            .label("Cancel login")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_confirmation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirmation = self.confirmation?;
        let tokens = cx.theme().semantic_tokens();
        let (title, detail) = match confirmation {
            Confirmation::DeleteCredential => (
                "Delete stored credential?",
                "The connection declaration remains and will report a missing credential.",
            ),
            Confirmation::LogoutManaged => (
                "Logout shared Codex account?",
                "This changes vendor-owned state under CODEX_HOME and may affect other Codex clients.",
            ),
            Confirmation::RemoveConnection => (
                "Remove connection declaration?",
                "Provider history and separately owned credential or managed login are preserved.",
            ),
        };
        let blockers = self
            .removal_plan
            .as_ref()
            .map(|plan| plan.blockers.clone())
            .unwrap_or_default();
        let removal_blocked = !blockers.is_empty();
        let retained = self.removal_plan.as_ref().map(|plan| {
            let mut values = Vec::new();
            if plan.retains_credential {
                values.push("OS-stored credential");
            }
            if plan.retains_managed_account {
                values.push("vendor-owned managed account");
            }
            values
        });
        Some(
            v_flex()
                .gap(tokens.spacing.sm)
                .p(tokens.spacing.md)
                .rounded(tokens.radius.md)
                .border_1()
                .border_color(cx.theme().danger)
                .child(title)
                .child(div().text_sm().child(detail))
                .when(removal_blocked, |panel| {
                    panel.child("Removal is blocked by:").children(
                        blockers
                            .clone()
                            .into_iter()
                            .map(|blocker| format!("• {blocker}")),
                    )
                })
                .when_some(retained, |panel, retained| {
                    if retained.is_empty() {
                        panel
                    } else {
                        panel.child(format!("Retained after removal: {}", retained.join(", ")))
                    }
                })
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .child(
                            Button::new("connection-cancel-confirmation")
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirmation = None;
                                    this.removal_plan = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("connection-confirm-action")
                                .label("Confirm")
                                .danger()
                                .disabled(removal_blocked)
                                .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl EventEmitter<ConnectionActionsEvent> for ConnectionActions {}

impl Render for ConnectionActions {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(self.render_authentication(cx))
            .child(
                v_flex()
                    .gap(tokens.spacing.sm)
                    .child(div().text_lg().child("Danger"))
                    .child(
                        Button::new("connection-prepare-removal")
                            .label("Review connection removal…")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.prepare_removal(cx))),
                    ),
            )
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
                        "{} · effect {}{}",
                        receipt.semantic_code,
                        receipt.effect,
                        if receipt.retained_authority.is_empty() {
                            String::new()
                        } else {
                            format!(" · retained {}", receipt.retained_authority.join(", "))
                        }
                    ),
                    false,
                    cx,
                ))
            })
    }
}

fn status(
    message: impl Into<String>,
    danger: bool,
    cx: &mut Context<ConnectionActions>,
) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    div()
        .max_w(rems(54.))
        .p(tokens.spacing.sm)
        .rounded(tokens.radius.sm)
        .bg(if danger {
            cx.theme().danger.opacity(0.12)
        } else {
            cx.theme().accent.opacity(0.16)
        })
        .when(danger, |notice| notice.text_color(cx.theme().danger))
        .child(message.into())
}

fn already_logged_in_receipt(connection: &str) -> DesktopConnectionMutationReceipt {
    DesktopConnectionMutationReceipt {
        semantic_code: "connection.managed_login.already_complete.v1".to_owned(),
        connection: connection.to_owned(),
        effect: "managed_login_completed".to_owned(),
        backup: None,
        retained_authority: Vec::new(),
        warnings: Vec::new(),
    }
}
