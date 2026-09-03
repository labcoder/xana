//! Retained, native presentation state for Xana Desktop settings.
//!
//! The view owns only navigation, search, selection, and focus. All catalog,
//! draft, validation, and persistence truth remains behind typed Rust clients.

use crate::connection_manager::{ConnectionManager, ConnectionManagerEvent};
use crate::management_view::{ManagementTab, ManagementView, ManagementViewEvent};
use crate::permission_view::{PermissionView, PermissionViewEvent};

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render, Role,
    Subscription, Window, div, prelude::*, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};
use xana::desktop::{
    DesktopControlPlane, DesktopSettingEffect, DesktopSettingEntry, DesktopSettingSource,
    DesktopSettingTarget, DesktopSettingsBackup, DesktopSettingsDraftId,
    DesktopSettingsDraftSnapshot, DesktopSettingsOwner, DesktopSettingsReceipt,
    DesktopSettingsSection, DesktopSettingsSnapshot,
};

const WIDE_WINDOW_PX: f32 = 1_180.;
const MEDIUM_WINDOW_PX: f32 = 880.;

const SECTIONS: [DesktopSettingsSection; 9] = [
    DesktopSettingsSection::Overview,
    DesktopSettingsSection::Appearance,
    DesktopSettingsSection::Connections,
    DesktopSettingsSection::Profiles,
    DesktopSettingsSection::Permissions,
    DesktopSettingsSection::Execution,
    DesktopSettingsSection::Diagnostics,
    DesktopSettingsSection::Integrations,
    DesktopSettingsSection::Advanced,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsViewEvent {
    Close,
    Reload,
    Review,
    Apply,
    Discard,
    Set {
        draft_id: DesktopSettingsDraftId,
        key: String,
        value: String,
    },
    Reset {
        draft_id: DesktopSettingsDraftId,
        key: String,
    },
    Revert {
        draft_id: DesktopSettingsDraftId,
        key: String,
    },
}

pub(crate) struct SettingsView {
    control: DesktopControlPlane,
    snapshot: DesktopSettingsSnapshot,
    draft: Option<DesktopSettingsDraftSnapshot>,
    receipt: Option<DesktopSettingsReceipt>,
    selected_section: DesktopSettingsSection,
    selected_key: Option<String>,
    search: Entity<InputState>,
    value_editor: Entity<InputState>,
    editor_key: Option<String>,
    review_open: bool,
    busy_label: Option<String>,
    error: Option<String>,
    focused_manager: Option<FocusedManager>,
    _subscriptions: Vec<Subscription>,
}

enum FocusedManager {
    Connections(Entity<ConnectionManager>),
    Management(Entity<ManagementView>),
    Permissions(Entity<PermissionView>),
}

impl SettingsView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        snapshot: DesktopSettingsSnapshot,
        draft: Option<DesktopSettingsDraftSnapshot>,
        receipt: Option<DesktopSettingsReceipt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected_section = DesktopSettingsSection::Overview;
        let selected_key = snapshot
            .entries_in(selected_section)
            .next()
            .map(|entry| entry.key.clone());
        let editor_value = selected_key
            .as_deref()
            .and_then(|key| snapshot.entries.iter().find(|entry| entry.key == key))
            .and_then(|entry| entry.value.raw.clone())
            .unwrap_or_default();
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search settings, values, and actions")
        });
        let value_editor = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter a value")
                .default_value(editor_value)
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |_this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &value_editor,
                window,
                |_this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                },
            ),
        ];
        Self {
            control,
            snapshot: draft
                .as_ref()
                .map_or_else(|| snapshot.clone(), |draft| draft.preview.clone()),
            draft,
            receipt,
            selected_section,
            editor_key: selected_key.clone(),
            selected_key,
            search,
            value_editor,
            review_open: false,
            busy_label: None,
            error: None,
            focused_manager: None,
            _subscriptions: subscriptions,
        }
    }

    fn open_focused_manager(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            "xana connection list" | "xana model list" => {
                let manager = cx.new(|cx| ConnectionManager::new(self.control.clone(), window, cx));
                let subscription = cx.subscribe_in(
                    &manager,
                    window,
                    |this, _, event: &ConnectionManagerEvent, _, cx| {
                        if matches!(event, ConnectionManagerEvent::Close) {
                            this.focused_manager = None;
                            cx.notify();
                        }
                    },
                );
                self._subscriptions.push(subscription);
                self.focused_manager = Some(FocusedManager::Connections(manager));
                self.error = None;
            }
            "xana profile list"
            | "xana route list"
            | "xana project list"
            | "xana capabilities"
            | "xana plugin list"
            | "xana mcp list"
            | "xana external-agent list"
            | "xana image list" => {
                let tab = if action == "xana project list" {
                    ManagementTab::Projects
                } else if action == "xana profile list" || action == "xana route list" {
                    ManagementTab::Profiles
                } else {
                    ManagementTab::Capabilities
                };
                let manager =
                    cx.new(|cx| ManagementView::new(self.control.clone(), tab, window, cx));
                let subscription = cx.subscribe_in(
                    &manager,
                    window,
                    |this, _, event: &ManagementViewEvent, _, cx| {
                        if matches!(event, ManagementViewEvent::Close) {
                            this.focused_manager = None;
                            cx.notify();
                        }
                    },
                );
                self._subscriptions.push(subscription);
                self.focused_manager = Some(FocusedManager::Management(manager));
                self.error = None;
            }
            "xana setup --section permissions-shell" => {
                let manager = cx.new(|cx| PermissionView::new(self.control.clone(), window, cx));
                let subscription = cx.subscribe_in(
                    &manager,
                    window,
                    |this, _, event: &PermissionViewEvent, _, cx| {
                        if matches!(event, PermissionViewEvent::Close) {
                            this.focused_manager = None;
                            cx.notify();
                        }
                    },
                );
                self._subscriptions.push(subscription);
                self.focused_manager = Some(FocusedManager::Permissions(manager));
                self.error = None;
            }
            _ => {
                self.error = Some(format!(
                    "The {action} workflow is not available in Xana Desktop yet."
                ));
            }
        }
        cx.notify();
    }

    pub(crate) fn set_state(
        &mut self,
        snapshot: DesktopSettingsSnapshot,
        draft: Option<DesktopSettingsDraftSnapshot>,
        receipt: Option<DesktopSettingsReceipt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft_ended = self.draft.is_some() && draft.is_none();
        self.snapshot = draft
            .as_ref()
            .map_or_else(|| snapshot.clone(), |draft| draft.preview.clone());
        self.draft = draft;
        self.receipt = receipt;
        if self
            .selected_key
            .as_deref()
            .is_some_and(|key| !self.snapshot.entries.iter().any(|entry| entry.key == key))
        {
            self.selected_key = None;
        }
        if self.selected_key.is_none() {
            self.selected_key = self
                .snapshot
                .entries_in(self.selected_section)
                .next()
                .map(|entry| entry.key.clone());
        }
        if draft_ended || self.editor_key != self.selected_key {
            self.sync_editor(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn set_busy(&mut self, label: Option<String>, cx: &mut Context<Self>) {
        self.busy_label = label;
        cx.notify();
    }

    pub(crate) fn set_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.busy_label = None;
        self.error = Some(message);
        self.review_open = true;
        cx.notify();
    }

    pub(crate) fn clear_operation_state(&mut self, cx: &mut Context<Self>) {
        self.busy_label = None;
        self.error = None;
        cx.notify();
    }

    pub(crate) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    fn select_entry(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_key = Some(key);
        self.sync_editor(window, cx);
        cx.notify();
    }

    fn sync_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_key = self.selected_key.clone();
        let value = self
            .selected_key
            .as_deref()
            .and_then(|key| self.snapshot.entries.iter().find(|entry| entry.key == key))
            .and_then(|entry| entry.value.raw.clone())
            .unwrap_or_default();
        self.value_editor
            .update(cx, |editor, cx| editor.set_value(value, window, cx));
    }

    fn matching_entries<'a>(&'a self, cx: &App) -> Vec<&'a DesktopSettingEntry> {
        let query = self.query(cx);
        self.snapshot
            .entries
            .iter()
            .filter(|entry| {
                (query.is_empty() && entry.section == self.selected_section)
                    || (!query.is_empty() && entry_matches(entry, &query))
            })
            .collect()
    }

    fn selected_entry<'a>(
        &'a self,
        entries: &[&'a DesktopSettingEntry],
    ) -> Option<&'a DesktopSettingEntry> {
        self.selected_key
            .as_deref()
            .and_then(|key| entries.iter().copied().find(|entry| entry.key == key))
            .or_else(|| entries.first().copied())
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        h_flex()
            .w_full()
            .flex_none()
            .justify_between()
            .gap(tokens.spacing.md)
            .px(tokens.spacing.lg)
            .py(tokens.spacing.md)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap(tokens.spacing.xs)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Inspect effective values, stage changes, and apply them as one transaction."),
                    ),
            )
            .child(
                Button::new("settings-close")
                    .label("Back to Conversation")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SettingsViewEvent::Close);
                    })),
            )
            .into_any_element()
    }

    fn render_search(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        div()
            .w_full()
            .flex_none()
            .p(tokens.spacing.md)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.search)
                    .prefix(IconName::Search)
                    .aria_label("Search Xana settings"),
            )
            .into_any_element()
    }

    fn render_section_rail(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let section_buttons = SECTIONS.into_iter().map(|section| {
            Button::new(format!("settings-section-{}", section.id()))
                .label(section.title())
                .w_full()
                .ghost()
                .selected(self.selected_section == section)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected_section = section;
                    let selected_key = this
                        .snapshot
                        .entries_in(section)
                        .next()
                        .map(|entry| entry.key.clone());
                    if let Some(key) = selected_key {
                        this.select_entry(key, window, cx);
                    } else {
                        this.selected_key = None;
                        this.editor_key = None;
                        cx.notify();
                    }
                }))
        });
        if compact {
            h_flex()
                .id("settings-section-picker")
                .w_full()
                .flex_none()
                .gap(tokens.spacing.xs)
                .p(tokens.spacing.sm)
                .overflow_x_scrollbar()
                .children(section_buttons)
                .into_any_element()
        } else {
            v_flex()
                .id("settings-section-rail")
                .h_full()
                .w(rems(15.))
                .flex_none()
                .gap(tokens.spacing.xs)
                .p(tokens.spacing.sm)
                .border_r_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().sidebar)
                .children(section_buttons)
                .into_any_element()
        }
    }

    fn render_entry_list(
        &self,
        entries: &[&DesktopSettingEntry],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let selected_key = self.selected_key.as_deref();
        v_flex()
            .id("settings-entry-list")
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .p(tokens.spacing.md)
            .gap(tokens.spacing.sm)
            .when(entries.is_empty(), |list| {
                list.child(
                    v_flex()
                        .p(tokens.spacing.xl)
                        .gap(tokens.spacing.sm)
                        .items_center()
                        .child("No settings match this search.")
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Try a label, value, section, stable key, or manager action.",
                                ),
                        ),
                )
            })
            .children(entries.iter().map(|entry| {
                let key = entry.key.clone();
                let staged = entry.staged
                    || self.draft.as_ref().is_some_and(|draft| {
                        draft.changes.iter().any(|change| change.key == entry.key)
                    });
                Button::new(format!("settings-entry-{}", entry.key))
                    .w_full()
                    .ghost()
                    .selected(selected_key == Some(entry.key.as_str()))
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_start()
                            .gap(tokens.spacing.md)
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap(tokens.spacing.xs)
                                    .child(entry.label.fallback.clone())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} · {}",
                                                entry.section.title(),
                                                entry.key
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(tokens.spacing.xs)
                                    .when(staged, |value| {
                                        value.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().primary)
                                                .child("Staged"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(entry.value.display.clone()),
                                    ),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_entry(key.clone(), window, cx);
                    }))
            }))
            .into_any_element()
    }

    fn render_inspector(
        &self,
        entry: Option<&DesktopSettingEntry>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let content = match entry {
            None => v_flex()
                .gap(tokens.spacing.sm)
                .child("Nothing selected")
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Choose a setting to inspect its source, scope, and effect."),
                )
                .into_any_element(),
            Some(entry) => v_flex()
                .gap(tokens.spacing.md)
                .child(
                    v_flex()
                        .gap(tokens.spacing.xs)
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(entry.label.fallback.clone()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(entry.description.fallback.clone()),
                        ),
                )
                .child(inspector_fact("Current", entry.value.display.clone(), cx))
                .child(inspector_fact("Source", source_label(entry.source), cx))
                .child(inspector_fact("Scope", target_label(entry.target), cx))
                .child(inspector_fact("Effect", effect_label(entry.effect), cx))
                .when_some(entry.default.as_ref(), |panel, default| {
                    panel.child(inspector_fact("Default", default.display.clone(), cx))
                })
                .when_some(entry.focused_action.as_ref(), |panel, action| {
                    let action_for_click = action.clone();
                    panel
                        .child(inspector_fact("Managed by", action.clone(), cx))
                        .child(
                            Button::new(format!("settings-open-manager-{action}"))
                                .label("Open focused manager")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_focused_manager(&action_for_click, window, cx);
                                })),
                        )
                })
                .child(self.render_control(entry, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(entry.key.clone()),
                )
                .into_any_element(),
        };
        v_flex()
            .id("settings-inspector")
            .when(!compact, |panel| panel.w(rems(19.)).h_full().flex_none())
            .when(compact, |panel| panel.w_full().flex_none())
            .min_h_0()
            .overflow_y_scrollbar()
            .p(tokens.spacing.lg)
            .border_l_1()
            .when(compact, |panel| panel.border_l_0().border_t_1())
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(content)
            .into_any_element()
    }

    fn render_control(&self, entry: &DesktopSettingEntry, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let draft_id = self.draft.as_ref().map(|draft| draft.id);
        let disabled = !entry.editable || draft_id.is_none();
        let key = entry.key.clone();
        let field = match entry.kind {
            xana::desktop::DesktopSettingKind::Boolean => {
                let checked = entry.value.raw.as_deref() == Some("true");
                let event_key = key.clone();
                Switch::new(format!("settings-control-{key}"))
                    .label(if checked { "Enabled" } else { "Disabled" })
                    .checked(checked)
                    .disabled(disabled)
                    .on_click(cx.listener(move |_, checked: &bool, _, cx| {
                        if let Some(draft_id) = draft_id {
                            cx.emit(SettingsViewEvent::Set {
                                draft_id,
                                key: event_key.clone(),
                                value: checked.to_string(),
                            });
                        }
                    }))
                    .into_any_element()
            }
            xana::desktop::DesktopSettingKind::Choice => {
                let choices = entry.choices.iter().take(16).cloned().collect::<Vec<_>>();
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap(tokens.spacing.xs)
                    .children(choices.into_iter().map(|choice| {
                        let event_key = key.clone();
                        let event_value = choice.clone();
                        Button::new(format!("settings-choice-{key}-{choice}"))
                            .compact()
                            .label(choice.clone())
                            .selected(entry.value.raw.as_deref() == Some(choice.as_str()))
                            .disabled(disabled)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(draft_id) = draft_id {
                                    cx.emit(SettingsViewEvent::Set {
                                        draft_id,
                                        key: event_key.clone(),
                                        value: event_value.clone(),
                                    });
                                }
                            }))
                    }))
                    .when(entry.choices.len() > 16, |choices| {
                        choices.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Open the focused manager to browse all choices."),
                        )
                    })
                    .into_any_element()
            }
            xana::desktop::DesktopSettingKind::Integer
            | xana::desktop::DesktopSettingKind::Bytes
            | xana::desktop::DesktopSettingKind::DurationDays
            | xana::desktop::DesktopSettingKind::OptionalPath => {
                let editor = self.value_editor.clone();
                let event_key = key.clone();
                h_flex()
                    .w_full()
                    .gap(tokens.spacing.sm)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.value_editor).disabled(disabled)),
                    )
                    .child(
                        Button::new(format!("settings-stage-{key}"))
                            .label("Stage")
                            .disabled(disabled)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(draft_id) = draft_id {
                                    cx.emit(SettingsViewEvent::Set {
                                        draft_id,
                                        key: event_key.clone(),
                                        value: editor.read(cx).value().to_string(),
                                    });
                                }
                            })),
                    )
                    .into_any_element()
            }
            xana::desktop::DesktopSettingKind::ReadOnly => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Read-only status")
                .into_any_element(),
        };
        let reset_key = key.clone();
        let revert_key = key.clone();
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .pt(tokens.spacing.sm)
            .child(field)
            .when(!entry.editable, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("This value is derived or managed by a focused workflow."),
                )
            })
            .when(entry.editable && draft_id.is_none(), |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Preparing a settings draft…"),
                )
            })
            .when(entry.editable && draft_id.is_some(), |panel| {
                panel.child(
                    h_flex()
                        .gap(tokens.spacing.xs)
                        .child(
                            Button::new(format!("settings-reset-{key}"))
                                .compact()
                                .label("Reset to default")
                                .disabled(entry.default.is_none())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    if let Some(draft_id) = draft_id {
                                        cx.emit(SettingsViewEvent::Reset {
                                            draft_id,
                                            key: reset_key.clone(),
                                        });
                                    }
                                })),
                        )
                        .child(
                            Button::new(format!("settings-revert-{key}"))
                                .compact()
                                .label("Revert staged")
                                .disabled(!entry.staged)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    if let Some(draft_id) = draft_id {
                                        cx.emit(SettingsViewEvent::Revert {
                                            draft_id,
                                            key: revert_key.clone(),
                                        });
                                    }
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_draft_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let draft = self.draft.as_ref()?;
        if draft.pending_count == 0 {
            return None;
        }
        let tokens = cx.theme().semantic_tokens();
        Some(
            h_flex()
                .id("settings-draft-bar")
                .w_full()
                .flex_none()
                .justify_between()
                .gap(tokens.spacing.md)
                .px(tokens.spacing.lg)
                .py(tokens.spacing.sm)
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .child(format!(
                    "{} staged {}",
                    draft.pending_count,
                    if draft.pending_count == 1 {
                        "change"
                    } else {
                        "changes"
                    }
                ))
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .child(Button::new("settings-review").label("Review").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.review_open = true;
                                cx.emit(SettingsViewEvent::Review);
                            }),
                        ))
                        .child(Button::new("settings-discard").label("Discard").on_click(
                            cx.listener(|_, _, _, cx| {
                                cx.emit(SettingsViewEvent::Discard);
                            }),
                        ))
                        .child(
                            Button::new("settings-apply")
                                .primary()
                                .label("Apply")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.review_open = true;
                                    cx.emit(SettingsViewEvent::Review);
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_receipt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let receipt = self.receipt.as_ref()?;
        if receipt.dry_run {
            return None;
        }
        let tokens = cx.theme().semantic_tokens();
        Some(
            h_flex()
                .w_full()
                .flex_none()
                .justify_between()
                .gap(tokens.spacing.md)
                .px(tokens.spacing.lg)
                .py(tokens.spacing.sm)
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().accent.opacity(0.35))
                .child(format!(
                    "Applied {} {}",
                    receipt.changes.len(),
                    if receipt.changes.len() == 1 {
                        "change"
                    } else {
                        "changes"
                    }
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(receipt_recovery_summary(receipt)),
                )
                .child(
                    Button::new("settings-reload")
                        .compact()
                        .label("Refresh")
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(SettingsViewEvent::Reload);
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_operation_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tokens = cx.theme().semantic_tokens();
        if let Some(error) = self.error.as_ref() {
            return Some(
                h_flex()
                    .w_full()
                    .flex_none()
                    .justify_between()
                    .gap(tokens.spacing.md)
                    .px(tokens.spacing.lg)
                    .py(tokens.spacing.sm)
                    .bg(cx.theme().danger.opacity(0.12))
                    .text_color(cx.theme().danger)
                    .child(error.clone())
                    .child(
                        Button::new("settings-error-reload")
                            .label("Reload authoritative values")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.emit(SettingsViewEvent::Reload);
                            })),
                    )
                    .into_any_element(),
            );
        }
        self.busy_label.as_ref().map(|label| {
            h_flex()
                .w_full()
                .flex_none()
                .gap(tokens.spacing.sm)
                .px(tokens.spacing.lg)
                .py(tokens.spacing.sm)
                .bg(cx.theme().accent.opacity(0.24))
                .child("Working")
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(label.clone()),
                )
                .into_any_element()
        })
    }

    fn render_review_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.review_open {
            return None;
        }
        let draft = self.draft.as_ref()?;
        let tokens = cx.theme().semantic_tokens();
        let validation = self
            .receipt
            .as_ref()
            .filter(|receipt| receipt.dry_run && receipt.revision_before == draft.base_revision);
        let validated = validation.is_some() && self.error.is_none();
        let changes = validation.map_or(draft.changes.as_slice(), |receipt| {
            receipt.changes.as_slice()
        });
        Some(
            div()
                .id("settings-review-overlay")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_center()
                .p(tokens.spacing.xl)
                .bg(cx.theme().background.opacity(0.76))
                .child(
                    v_flex()
                        .id("settings-review-dialog")
                        .role(Role::Dialog)
                        .aria_label("Review staged Xana settings")
                        .w_full()
                        .max_w(rems(48.))
                        .max_h(rems(38.))
                        .overflow_hidden()
                        .rounded(tokens.radius.lg)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().popover)
                        .shadow_lg()
                        .child(
                            h_flex()
                                .w_full()
                                .flex_none()
                                .justify_between()
                                .gap(tokens.spacing.md)
                                .p(tokens.spacing.lg)
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    v_flex()
                                        .gap(tokens.spacing.xs)
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("Review settings changes"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(if validated {
                                                    "Validated against the current durable settings revision."
                                                } else {
                                                    "Validation is required before Apply becomes available."
                                                }),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(validation.map_or_else(
                                                    || "Awaiting validation receipt".to_owned(),
                                                    receipt_recovery_summary,
                                                )),
                                        ),
                                )
                                .child(
                                    Button::new("settings-review-close")
                                        .label("Close")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.review_open = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("settings-review-changes")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scrollbar()
                                .p(tokens.spacing.lg)
                                .gap(tokens.spacing.md)
                                .children(changes.iter().map(|change| {
                                    v_flex()
                                        .w_full()
                                        .gap(tokens.spacing.xs)
                                        .p(tokens.spacing.md)
                                        .rounded(tokens.radius.md)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(change.label.fallback.clone()),
                                        )
                                        .child(format!(
                                            "{}  →  {}",
                                            change.before.display, change.after.display
                                        ))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "{} · {} · {}",
                                                    change.key,
                                                    target_label(change.target),
                                                    effect_label(change.effect)
                                                )),
                                        )
                                })),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .flex_none()
                                .justify_end()
                                .gap(tokens.spacing.sm)
                                .p(tokens.spacing.lg)
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .child(
                                    Button::new("settings-review-revalidate")
                                        .label("Validate again")
                                        .disabled(self.busy_label.is_some())
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(SettingsViewEvent::Review);
                                        })),
                                )
                                .child(
                                    Button::new("settings-review-apply")
                                        .primary()
                                        .label("Apply transaction")
                                        .disabled(!validated || self.busy_label.is_some())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.error = None;
                                            cx.emit(SettingsViewEvent::Apply);
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl EventEmitter<SettingsViewEvent> for SettingsView {}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(manager) = &self.focused_manager {
            return match manager {
                FocusedManager::Connections(manager) => manager.clone().into_any_element(),
                FocusedManager::Management(manager) => manager.clone().into_any_element(),
                FocusedManager::Permissions(manager) => manager.clone().into_any_element(),
            };
        }
        let width = f32::from(window.viewport_size().width);
        let compact = width < MEDIUM_WINDOW_PX;
        let wide = width >= WIDE_WINDOW_PX;
        let entries = self.matching_entries(cx);
        let selected = self.selected_entry(&entries);
        let search = self.render_search(cx);
        let rail = self.render_section_rail(compact, cx);
        let list = self.render_entry_list(&entries, cx);
        let inspector = self.render_inspector(selected, compact || !wide, cx);
        let body = if compact {
            v_flex()
                .size_full()
                .min_h_0()
                .child(rail)
                .child(search)
                .child(div().flex_1().min_h_0().child(list))
                .child(inspector)
                .into_any_element()
        } else if wide {
            h_flex()
                .size_full()
                .min_h_0()
                .child(rail)
                .child(
                    v_flex()
                        .size_full()
                        .min_w_0()
                        .min_h_0()
                        .child(search)
                        .child(div().flex_1().min_h_0().child(list)),
                )
                .child(inspector)
                .into_any_element()
        } else {
            h_flex()
                .size_full()
                .min_h_0()
                .child(rail)
                .child(
                    v_flex()
                        .size_full()
                        .min_w_0()
                        .min_h_0()
                        .child(search)
                        .child(div().flex_1().min_h_0().child(list))
                        .child(inspector),
                )
                .into_any_element()
        };

        v_flex()
            .id("xana-settings")
            .role(Role::Region)
            .aria_label("Xana Settings")
            .relative()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .when_some(self.render_operation_notice(cx), |view, notice| {
                view.child(notice)
            })
            .when_some(self.render_receipt(cx), |view, receipt| view.child(receipt))
            .child(div().flex_1().min_h_0().child(body))
            .when_some(self.render_draft_bar(cx), |view, bar| view.child(bar))
            .when_some(self.render_review_dialog(cx), |view, dialog| {
                view.child(dialog)
            })
            .into_any_element()
    }
}

fn entry_matches(entry: &DesktopSettingEntry, query: &str) -> bool {
    [
        entry.section.title(),
        entry.key.as_str(),
        entry.label.fallback.as_str(),
        entry.description.fallback.as_str(),
        entry.value.display.as_str(),
        entry.focused_action.as_deref().unwrap_or_default(),
    ]
    .into_iter()
    .any(|candidate| candidate.to_lowercase().contains(query))
}

fn inspector_fact(label: &'static str, value: String, cx: &App) -> AnyElement {
    let tokens = cx.theme().semantic_tokens();
    v_flex()
        .gap(tokens.spacing.xs)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().text_sm().child(value))
        .into_any_element()
}

fn source_label(source: DesktopSettingSource) -> String {
    match source {
        DesktopSettingSource::BuiltInDefault => "Built-in default",
        DesktopSettingSource::ConfigurationFile => "Global configuration",
        DesktopSettingSource::PresentationFile => "This device",
        DesktopSettingSource::Derived => "Derived runtime status",
    }
    .to_owned()
}

fn target_label(target: DesktopSettingTarget) -> String {
    match target {
        DesktopSettingTarget::MachinePresentation => "This device",
        DesktopSettingTarget::GlobalConfiguration => "Global Xana defaults",
        DesktopSettingTarget::TaskSpecificManager => "Focused manager",
    }
    .to_owned()
}

fn effect_label(effect: DesktopSettingEffect) -> String {
    match effect {
        DesktopSettingEffect::Immediate => "Immediately after apply",
        DesktopSettingEffect::NewConversation => "New Conversations",
        DesktopSettingEffect::NextLaunch => "Next launch",
        DesktopSettingEffect::ManagedElsewhere => "Managed in a focused workflow",
    }
    .to_owned()
}

fn receipt_recovery_summary(receipt: &DesktopSettingsReceipt) -> String {
    let owners = if receipt.durable_owners.is_empty() {
        "no durable owners".to_owned()
    } else {
        receipt
            .durable_owners
            .iter()
            .map(|owner| match owner {
                DesktopSettingsOwner::GlobalConfiguration => "global configuration",
                DesktopSettingsOwner::MachinePresentation => "this-device presentation",
            })
            .collect::<Vec<_>>()
            .join(" + ")
    };
    let backup = match receipt.configuration_backup {
        DesktopSettingsBackup::NotNeeded => "no configuration backup needed",
        DesktopSettingsBackup::Planned => "configuration backup planned",
        DesktopSettingsBackup::Created => "configuration backup created",
    };
    format!("{owners} · {backup} · atomic rollback on failure")
}

#[cfg(test)]
mod tests {
    use super::*;
    use xana::desktop::{DesktopLocalizedText, DesktopSettingKind, DesktopSettingValue};

    fn entry() -> DesktopSettingEntry {
        DesktopSettingEntry {
            key: "appearance.theme".to_owned(),
            section: DesktopSettingsSection::Appearance,
            label: DesktopLocalizedText {
                code: "settings.appearance.theme.label".to_owned(),
                fallback: "Theme".to_owned(),
            },
            description: DesktopLocalizedText {
                code: "settings.appearance.theme.description".to_owned(),
                fallback: "Choose light, dark, or system appearance.".to_owned(),
            },
            value: DesktopSettingValue {
                raw: Some("dark".to_owned()),
                display: "Dark".to_owned(),
            },
            default: None,
            kind: DesktopSettingKind::Choice,
            choices: vec!["system".to_owned(), "light".to_owned(), "dark".to_owned()],
            source: DesktopSettingSource::PresentationFile,
            target: DesktopSettingTarget::MachinePresentation,
            effect: DesktopSettingEffect::Immediate,
            editable: true,
            focused_action: None,
            staged: false,
        }
    }

    #[test]
    fn search_matches_section_key_label_description_value_and_action() {
        let mut row = entry();
        row.focused_action = Some("Open appearance manager".to_owned());
        for query in ["appearance", "theme", "choose light", "dark", "manager"] {
            assert!(entry_matches(&row, query));
        }
        assert!(!entry_matches(&row, "credential"));
    }

    #[test]
    fn source_scope_and_effect_copy_is_explicit() {
        assert_eq!(
            source_label(DesktopSettingSource::PresentationFile),
            "This device"
        );
        assert_eq!(
            target_label(DesktopSettingTarget::GlobalConfiguration),
            "Global Xana defaults"
        );
        assert_eq!(
            effect_label(DesktopSettingEffect::NewConversation),
            "New Conversations"
        );
    }
}
