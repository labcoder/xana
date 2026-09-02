//! Focused connection and model manager for Desktop settings.

use crate::setup_view::{SetupView, SetupViewEvent};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    Subscription, Task, Window, div, prelude::*, px, rems, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    v_flex, v_virtual_list,
};
use std::rc::Rc;
use xana::desktop::{
    DesktopConnection, DesktopConnectionOperationReceipt, DesktopConnectionSnapshot,
    DesktopControlPlane, DesktopModelOption,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectionManagerEvent {
    Close,
}

pub(crate) struct ConnectionManager {
    control: DesktopControlPlane,
    snapshot: Result<DesktopConnectionSnapshot, String>,
    selected_connection: Option<String>,
    selected_model: Option<String>,
    filter: Entity<InputState>,
    setup: Option<Entity<SetupView>>,
    receipt: Option<DesktopConnectionOperationReceipt>,
    setup_receipt: Option<String>,
    busy: Option<String>,
    error: Option<String>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionManager {
    pub(crate) fn new(
        control: DesktopControlPlane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot = control.connections().map_err(|error| error.message);
        let selected_connection = snapshot
            .as_ref()
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .connections
                    .iter()
                    .find(|connection| connection.selected_for_new_conversations)
                    .or_else(|| snapshot.connections.first())
            })
            .map(|connection| connection.id.clone());
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter live models"));
        let filter_subscription =
            cx.subscribe_in(&filter, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
        Self {
            control,
            snapshot,
            selected_connection,
            selected_model: None,
            filter,
            setup: None,
            receipt: None,
            setup_receipt: None,
            busy: None,
            error: None,
            _task: None,
            _subscriptions: vec![filter_subscription],
        }
    }

    fn reload(&mut self) {
        self.snapshot = self.control.connections().map_err(|error| error.message);
        if let Ok(snapshot) = &self.snapshot
            && self.selected_connection.as_deref().is_none_or(|selected| {
                !snapshot
                    .connections
                    .iter()
                    .any(|connection| connection.id == selected)
            })
        {
            self.selected_connection = snapshot
                .connections
                .iter()
                .find(|connection| connection.selected_for_new_conversations)
                .or_else(|| snapshot.connections.first())
                .map(|connection| connection.id.clone());
        }
    }

    fn selected(&self) -> Option<&DesktopConnection> {
        let selected = self.selected_connection.as_deref()?;
        self.snapshot
            .as_ref()
            .ok()?
            .connections
            .iter()
            .find(|connection| connection.id == selected)
    }

    fn open_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let setup = cx.new(|cx| SetupView::new(self.control.clone(), window, cx));
        let subscription = cx.subscribe_in(
            &setup,
            window,
            |this, _, event: &SetupViewEvent, _, cx| match event {
                SetupViewEvent::Cancel => {
                    this.setup = None;
                    cx.notify();
                }
                SetupViewEvent::Completed { receipt, .. } => {
                    this.setup_receipt = Some(format!(
                        "{} · {} model(s) discovered",
                        receipt.semantic_code, receipt.discovered_model_count
                    ));
                    this.setup = None;
                    this.reload();
                    cx.notify();
                }
            },
        );
        self._subscriptions.push(subscription);
        self.setup = Some(setup);
        cx.notify();
    }

    fn test_selected(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.selected_connection.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(format!("Testing {connection}…"));
        self.error = None;
        self.receipt = None;
        let control = self.control.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.test_connection(&connection).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => this.receipt = Some(receipt),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh_selected(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.selected_connection.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(format!("Refreshing {connection}…"));
        self.error = None;
        self.receipt = None;
        let control = self.control.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.refresh_connection(&connection).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => {
                        this.receipt = Some(receipt);
                        this.reload();
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn select_model(&mut self, cx: &mut Context<Self>) {
        let (Some(connection), Some(model)) = (
            self.selected_connection.clone(),
            self.selected_model.clone(),
        ) else {
            return;
        };
        match self.control.select_model(&connection, &model, None) {
            Ok(receipt) => {
                self.setup_receipt = Some(format!(
                    "{} · applies to new Conversations",
                    receipt.semantic_code
                ));
                self.error = None;
                self.reload();
            }
            Err(error) => self.error = Some(error.message),
        }
        cx.notify();
    }

    fn filtered_models(&self, cx: &App) -> Vec<DesktopModelOption> {
        let query = self.filter.read(cx).value().trim().to_lowercase();
        self.selected()
            .into_iter()
            .flat_map(|connection| &connection.models)
            .filter(|model| {
                query.is_empty()
                    || model.id.to_lowercase().contains(&query)
                    || model.display_name.to_lowercase().contains(&query)
                    || model
                        .input_modalities
                        .iter()
                        .any(|value| value.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    fn render_connections(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        match &self.snapshot {
            Err(error) => div()
                .p(tokens.spacing.md)
                .bg(cx.theme().danger.opacity(0.12))
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element(),
            Ok(snapshot) => v_flex()
                .gap(tokens.spacing.xs)
                .children(snapshot.connections.iter().map(|connection| {
                    let id = connection.id.clone();
                    Button::new(format!("connection-card-{}", connection.id))
                        .w_full()
                        .ghost()
                        .selected(
                            self.selected_connection.as_deref() == Some(connection.id.as_str()),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .items_start()
                                .child(
                                    h_flex()
                                        .w_full()
                                        .justify_between()
                                        .child(connection.id.clone())
                                        .child(connection.health.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{} · {} · model {}",
                                            connection.provider.title(),
                                            connection.credential_source,
                                            connection
                                                .selected_model
                                                .as_deref()
                                                .unwrap_or("not selected")
                                        )),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected_connection = Some(id.clone());
                            this.selected_model = None;
                            this.error = None;
                            cx.notify();
                        }))
                }))
                .into_any_element(),
        }
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let Some(connection) = self.selected() else {
            return div()
                .p(tokens.spacing.lg)
                .child("Choose a connection.")
                .into_any_element();
        };
        let model_count = connection.models.len();
        let selected_for_new = connection.selected_for_new_conversations;
        v_flex()
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        v_flex()
                            .child(div().text_xl().child(connection.id.clone()))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} available model(s)", model_count)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(tokens.spacing.xs)
                            .child(
                                Button::new("connection-test")
                                    .compact()
                                    .label("Test")
                                    .disabled(self.busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| this.test_selected(cx))),
                            )
                            .child(
                                Button::new("connection-refresh")
                                    .compact()
                                    .label("Refresh models")
                                    .disabled(self.busy.is_some())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.refresh_selected(cx)),
                                    ),
                            ),
                    ),
            )
            .child(h_flex().flex_wrap().gap(tokens.spacing.sm).children([
                facet("Declaration", "configured".to_owned(), cx),
                facet("Credential", format!("{:?}", connection.credential), cx),
                facet("Managed account", connection.managed_account.clone(), cx),
                facet("Reachability", connection.reachability.clone(), cx),
                facet("Catalog", connection.catalog_freshness.clone(), cx),
                facet(
                    "Selected model",
                    connection.selected_model_state.clone(),
                    cx,
                ),
            ]))
            .when(selected_for_new, |content| {
                content.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().success)
                        .child("Used for new Conversations"),
                )
            })
            .child(Input::new(&self.filter))
            .child(self.render_model_list(cx))
            .child(
                Button::new("connection-select-model")
                    .label("Use selected model for new Conversations")
                    .primary()
                    .disabled(self.selected_model.is_none() || self.busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.select_model(cx))),
            )
            .into_any_element()
    }

    fn render_model_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let models = self.filtered_models(cx);
        let count = models.len();
        let sizes = Rc::new(vec![size(px(680.), px(64.)); count]);
        let selected = self.selected_model.clone();
        v_virtual_list(
            cx.entity(),
            "connection-model-list",
            sizes,
            move |_, range, _, cx| {
                range
                    .filter_map(|index| models.get(index))
                    .map(|model| {
                        let id = model.id.clone();
                        Button::new(format!("connection-model-{}", model.id))
                            .w_full()
                            .ghost()
                            .selected(selected.as_deref() == Some(model.id.as_str()))
                            .child(
                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .child(
                                        v_flex()
                                            .items_start()
                                            .child(model.display_name.clone())
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "{} · input {}",
                                                        model.id,
                                                        model.input_modalities.join(", ")
                                                    )),
                                            ),
                                    )
                                    .when_some(model.pricing.clone(), |row, pricing| {
                                        row.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(pricing),
                                        )
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_model = Some(id.clone());
                                cx.notify();
                            }))
                    })
                    .collect::<Vec<_>>()
            },
        )
        .h(rems(18.))
        .w_full()
        .into_any_element()
    }
}

impl EventEmitter<ConnectionManagerEvent> for ConnectionManager {}

impl Render for ConnectionManager {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(setup) = &self.setup {
            return setup.clone().into_any_element();
        }
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .justify_between()
                    .p(tokens.spacing.md)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .child(div().text_xl().child("Connections & Models"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Declaration, authority, reachability, catalog, and model are separate facts."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(tokens.spacing.xs)
                            .child(
                                Button::new("connection-add")
                                    .label("Add connection")
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_setup(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("connection-manager-close")
                                    .label("Back to Settings")
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ConnectionManagerEvent::Close);
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(rems(22.))
                            .h_full()
                            .overflow_y_scrollbar()
                            .p(tokens.spacing.md)
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(self.render_connections(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .p(tokens.spacing.lg)
                            .child(self.render_detail(cx)),
                    ),
            )
            .when_some(self.busy.clone(), |content, busy| {
                content.child(status_card(busy, false, cx))
            })
            .when_some(self.error.clone(), |content, error| {
                content.child(status_card(error, true, cx))
            })
            .when_some(self.setup_receipt.clone(), |content, receipt| {
                content.child(status_card(receipt, false, cx))
            })
            .when_some(self.receipt.as_ref(), |content, receipt| {
                content.child(status_card(
                    format!(
                        "{} · {} model(s) · recovery {}",
                        receipt.semantic_code,
                        receipt.discovered_model_count,
                        receipt.recovery.as_deref().unwrap_or("none")
                    ),
                    receipt.usable == Some(false),
                    cx,
                ))
            })
            .into_any_element()
    }
}

fn facet(
    label: impl Into<String>,
    value: impl Into<String>,
    cx: &mut Context<ConnectionManager>,
) -> AnyElement {
    let tokens = cx.theme().semantic_tokens();
    v_flex()
        .gap(tokens.spacing.xs)
        .p(tokens.spacing.sm)
        .rounded(tokens.radius.sm)
        .bg(cx.theme().muted)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(value.into())
        .into_any_element()
}

fn status_card(
    message: impl Into<String>,
    danger: bool,
    cx: &mut Context<ConnectionManager>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_event_is_explicit_navigation_only() {
        assert_eq!(ConnectionManagerEvent::Close, ConnectionManagerEvent::Close);
    }
}
