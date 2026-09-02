//! Retained, native presentation state for Xana Desktop settings.
//!
//! The view owns only navigation, search, selection, and focus. All catalog,
//! draft, validation, and persistence truth remains behind `DesktopClient`.

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render, Role,
    Subscription, Window, div, prelude::*, rems,
};
use gpui_component::{
    ActiveTheme as _, IconName, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};
use xana::desktop::{
    DesktopSettingEffect, DesktopSettingEntry, DesktopSettingSource, DesktopSettingTarget,
    DesktopSettingsDraftSnapshot, DesktopSettingsReceipt, DesktopSettingsSection,
    DesktopSettingsSnapshot,
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
}

pub(crate) struct SettingsView {
    snapshot: DesktopSettingsSnapshot,
    draft: Option<DesktopSettingsDraftSnapshot>,
    receipt: Option<DesktopSettingsReceipt>,
    selected_section: DesktopSettingsSection,
    selected_key: Option<String>,
    search: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub(crate) fn new(
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
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search settings, values, and actions")
        });
        let subscriptions =
            vec![
                cx.subscribe_in(&search, window, |_this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }),
            ];
        Self {
            snapshot,
            draft,
            receipt,
            selected_section,
            selected_key,
            search,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn set_state(
        &mut self,
        snapshot: DesktopSettingsSnapshot,
        draft: Option<DesktopSettingsDraftSnapshot>,
        receipt: Option<DesktopSettingsReceipt>,
        cx: &mut Context<Self>,
    ) {
        self.snapshot = snapshot;
        self.draft = draft;
        self.receipt = receipt;
        if self
            .selected_key
            .as_deref()
            .is_some_and(|key| !self.snapshot.entries.iter().any(|entry| entry.key == key))
        {
            self.selected_key = None;
        }
        cx.notify();
    }

    pub(crate) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_section = section;
                    this.selected_key = this
                        .snapshot
                        .entries_in(section)
                        .next()
                        .map(|entry| entry.key.clone());
                    cx.notify();
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_key = Some(key.clone());
                        cx.notify();
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
        let content = entry.map_or_else(
            || {
                v_flex()
                    .gap(tokens.spacing.sm)
                    .child("Nothing selected")
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Choose a setting to inspect its source, scope, and effect."),
                    )
                    .into_any_element()
            },
            |entry| {
                v_flex()
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
                        panel.child(inspector_fact("Managed by", action.clone(), cx))
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(entry.key.clone()),
                    )
                    .into_any_element()
            },
        );
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
                            cx.listener(|_, _, _, cx| {
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
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(SettingsViewEvent::Apply);
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
}

impl EventEmitter<SettingsViewEvent> for SettingsView {}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .when_some(self.render_receipt(cx), |view, receipt| view.child(receipt))
            .child(div().flex_1().min_h_0().child(body))
            .when_some(self.render_draft_bar(cx), |view, bar| view.child(bar))
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
