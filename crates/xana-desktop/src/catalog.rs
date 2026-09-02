//! Deterministic, provider-free review surface for Xana's Desktop primitives.

use crate::{
    component_inventory::INVENTORY,
    design_system::{self, AppearancePreferences, ColorScheme, Density, MotionMode, VisualSystem},
    localization::{InterfaceLocale, catalog_messages},
};
use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Subscription, Window, div, prelude::*, px,
};
use gpui_ai::prelude::*;
use gpui_component::{
    ActiveTheme as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CatalogPage {
    #[default]
    Foundations,
    Conversation,
    AgentWork,
    Navigation,
}

impl CatalogPage {
    const ALL: [Self; 4] = [
        Self::Foundations,
        Self::Conversation,
        Self::AgentWork,
        Self::Navigation,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Foundations => "Foundations",
            Self::Conversation => "Conversation",
            Self::AgentWork => "Agent work",
            Self::Navigation => "Navigation",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Foundations => "foundations",
            Self::Conversation => "conversation",
            Self::AgentWork => "agent-work",
            Self::Navigation => "navigation",
        }
    }
}

/// Application-owned fixtures and retained entities used by the review catalog.
pub(crate) struct ComponentCatalog {
    page: CatalogPage,
    appearance: AppearancePreferences,
    locale: InterfaceLocale,
    chat: Entity<Chat>,
    threads: Entity<ThreadList>,
    sidebar: Entity<SidebarNav>,
    commands: Entity<CommandSearch>,
    approval: ApprovalDecision,
    last_event: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl ComponentCatalog {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(|cx| PromptBar::new("catalog-composer", window, cx));
        prompt.update(cx, |prompt, cx| {
            prompt.set_draft(
                "Compare the recovery options, then recommend the safest next step.",
                window,
                cx,
            );
        });
        let chat = cx.new(|cx| Chat::new("catalog-chat", prompt, window, cx));
        chat.update(cx, |chat, cx| {
            chat.set_messages(catalog_conversation(), window, cx);
        });

        let threads = cx.new(|cx| ThreadList::new("catalog-threads", window, cx));
        threads.update(cx, |threads, cx| {
            threads.set_sections(catalog_threads(), cx);
            threads.set_active(Some("recovery-plan"), cx);
        });

        let sidebar = cx.new(|cx| SidebarNav::new("catalog-sidebar", window, cx));
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_sections(catalog_sidebar(), cx);
            sidebar.set_active_item("conversation", cx);
        });

        let commands = cx.new(|cx| CommandSearch::new("catalog-commands", window, cx));
        commands.update(cx, |commands, cx| {
            commands.set_items(catalog_commands(), window, cx);
        });

        let subscriptions = vec![
            cx.subscribe_in(&chat, window, |this, _, event: &ChatEvent, _, cx| {
                this.last_event = format!("Chat: {event:?}").into();
                cx.notify();
            }),
            cx.subscribe(&threads, |this, threads, event: &ThreadListEvent, cx| {
                if let ThreadListEvent::Selected { id } = event {
                    threads.update(cx, |threads, cx| {
                        threads.set_active(Some(id.clone()), cx);
                    });
                }
                this.last_event = format!("Threads: {event:?}").into();
                cx.notify();
            }),
            cx.subscribe(&sidebar, |this, sidebar, event: &SidebarNavEvent, cx| {
                if let SidebarNavEvent::Selected { item_id, .. } = event {
                    sidebar.update(cx, |sidebar, cx| {
                        sidebar.set_active_item(item_id.clone(), cx);
                    });
                }
                this.last_event = format!("Navigation: {event:?}").into();
                cx.notify();
            }),
            cx.subscribe(&commands, |this, _, event: &CommandSearchEvent, cx| {
                this.last_event = format!("Commands: {event:?}").into();
                cx.notify();
            }),
        ];

        Self {
            page: CatalogPage::default(),
            appearance: AppearancePreferences::default(),
            locale: InterfaceLocale::default(),
            chat,
            threads,
            sidebar,
            commands,
            approval: ApprovalDecision::Pending,
            last_event: "Catalog ready. Use Tab and arrow keys to inspect every control.".into(),
            _subscriptions: subscriptions,
        }
    }

    fn set_page(&mut self, page: CatalogPage, cx: &mut Context<Self>) {
        self.page = page;
        self.last_event = format!("Showing {}", page.label()).into();
        cx.notify();
    }

    fn set_scheme(&mut self, scheme: ColorScheme, cx: &mut Context<Self>) {
        self.appearance.scheme = scheme;
        design_system::apply(self.appearance, cx);
        cx.notify();
    }

    fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        self.appearance.density = density;
        design_system::apply(self.appearance, cx);
        cx.notify();
    }

    fn set_motion(&mut self, motion: MotionMode, cx: &mut Context<Self>) {
        self.appearance.motion = motion;
        design_system::apply(self.appearance, cx);
        cx.notify();
    }

    fn toggle_scale(&mut self, cx: &mut Context<Self>) {
        let next = if self.appearance.text_scale_percent == 100 {
            200
        } else {
            100
        };
        self.appearance = self.appearance.with_text_scale(next);
        design_system::apply(self.appearance, cx);
        cx.notify();
    }

    fn set_locale(&mut self, locale: InterfaceLocale, cx: &mut Context<Self>) {
        self.locale = locale;
        cx.notify();
    }

    fn section(
        &self,
        id: &'static str,
        title: &'static str,
        description: &'static str,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .id(id)
            .gap(tokens.spacing.md)
            .p(tokens.spacing.lg)
            .rounded(tokens.radius.lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(tokens.colors.surface)
            .child(
                div()
                    .id((id, 0_usize))
                    .role(Role::Heading)
                    .aria_label(title)
                    .text_size(tokens.typography.lg.size)
                    .line_height(tokens.typography.lg.line_height)
                    .font_semibold()
                    .child(title),
            )
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
            .child(body)
    }

    fn controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let applied = VisualSystem::read(cx).preferences();
        v_flex()
            .gap(tokens.spacing.sm)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.xs)
                    .children(CatalogPage::ALL.map(|page| {
                        Button::new(format!("catalog-page-{}", page.id()))
                            .label(page.label())
                            .when(self.page == page, |button| button.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_page(page, cx);
                            }))
                    })),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.xs)
                    .children(ColorScheme::ALL.map(|scheme| {
                        Button::new(format!("catalog-scheme-{}", scheme.label()))
                            .label(scheme.label())
                            .when(applied.scheme == scheme, |button| button.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_scheme(scheme, cx);
                            }))
                    }))
                    .children(Density::ALL.map(|density| {
                        Button::new(format!("catalog-density-{}", density.label()))
                            .label(density.label())
                            .when(applied.density == density, |button| button.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_density(density, cx);
                            }))
                    }))
                    .child(
                        Button::new("catalog-scale")
                            .label(format!("Text {}%", applied.text_scale_percent))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_scale(cx))),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.xs)
                    .children(MotionMode::ALL.map(|motion| {
                        Button::new(format!("catalog-motion-{}", motion.label()))
                            .label(motion.label())
                            .when(applied.motion == motion, |button| button.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_motion(motion, cx);
                            }))
                    }))
                    .children(InterfaceLocale::ALL.map(|locale| {
                        Button::new(format!("catalog-locale-{}", locale.label()))
                            .label(locale.label())
                            .when(self.locale == locale, |button| button.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_locale(locale, cx);
                            }))
                    })),
            )
    }

    fn foundations(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = VisualSystem::read(cx).tokens().clone();
        let localized = catalog_messages()
            .into_iter()
            .map(|message| message.localize(self.locale))
            .collect::<Vec<_>>();
        let swatches = [
            ("Primary", tokens.colors.primary),
            ("Success", cx.theme().success),
            ("Warning", cx.theme().warning),
            ("Danger", cx.theme().danger),
            ("Information", cx.theme().info),
        ];
        v_flex()
            .gap(tokens.spacing.lg)
            .child(self.section(
                "catalog-semantic-colors",
                "Semantic state colors",
                "Every state pairs color with a visible word or icon. The palette is Xana-owned.",
                h_flex().flex_wrap().gap(tokens.spacing.md).children(
                    swatches.map(|(label, color)| {
                        h_flex()
                            .gap(tokens.spacing.xs)
                            .items_center()
                            .child(
                                div()
                                    .w(px(28.))
                                    .h(px(28.))
                                    .rounded(tokens.radius.md)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(color),
                            )
                            .child(label)
                    }),
                ),
                cx,
            ))
            .child(self.section(
                "catalog-localized-copy",
                "Semantic copy and localization stress",
                "Stable codes preserve meaning; Spanish is deliberately representative, and fallback remains inspectable.",
                v_flex().gap(tokens.spacing.sm).children(localized.into_iter().map(|copy| {
                    v_flex()
                        .gap(tokens.spacing.xs)
                        .child(
                            div()
                                .text_size(tokens.typography.xs.size)
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{}{}",
                                    copy.code,
                                    if copy.used_fallback { " · fallback" } else { "" }
                                )),
                        )
                        .child(copy.text)
                })),
                cx,
            ))
            .child(self.section(
                "catalog-inventory",
                "Component ownership",
                "AI mechanics stay in gpui-ai; Xana retains domain state and product composition.",
                v_flex().gap(tokens.spacing.sm).children(INVENTORY.iter().map(|record| {
                    v_flex()
                        .gap(tokens.spacing.xs)
                        .child(format!("{} · {}", record.component, record.source.label()))
                        .child(
                            div()
                                .text_size(tokens.typography.xs.size)
                                .text_color(cx.theme().muted_foreground)
                                .child(record.ownership),
                        )
                })),
                cx,
            ))
    }

    fn conversation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex().gap(tokens.spacing.lg).child(self.section(
            "catalog-conversation",
            "Conversation and IME composer",
            "A retained Chat and PromptBar own focus, selection, scrolling, and IME mechanics; Xana owns snapshots and submissions.",
            div()
                .h(px(520.))
                .min_h(px(260.))
                .overflow_hidden()
                .child(self.chat.clone()),
            cx,
        ))
    }

    fn agent_work(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let answer = StreamedContent::done(
            "Xana compared the recovery receipts and found **no replayed side effects**. The next safe step is to inspect the interrupted operation before resuming.",
        );
        let thinking = Progressive::complete(
            ThinkingTrace::new()
                .thought_for(Duration::from_secs(4))
                .steps([
                    ThinkingStep::new("Read recovery receipts").status(StepStatus::Done),
                    ThinkingStep::new("Compare durable operation IDs").status(StepStatus::Done),
                    ThinkingStep::new("Recommend a reversible next step"),
                ]),
        );
        let tool = Progressive::complete(
            ToolInvocation::new("catalog-read", "read_file")
                .summary("data/receipts/latest.json")
                .input(r#"{"path":"data/receipts/latest.json"}"#)
                .output("Read 3 bounded receipts; no duplicate operation IDs."),
        );
        let usage = ContextUsage::new(68_400, 128_000)
            .input(52_100)
            .output(9_300)
            .reasoning(7_000)
            .cached(31_000)
            .cost("$0.42 estimated");
        let attachments = [
            Attachment::new("recovery-log", "recovery.log")
                .size_bytes(18_432)
                .detail("plain text"),
            Attachment::new("architecture", "architecture.svg")
                .size_bytes(42_700)
                .detail("vector image"),
        ];
        let queued = [
            QueuedMessage::new("queue-1", "Summarize the affected files")
                .note("after the current step"),
            QueuedMessage::new("queue-2", "Draft a recovery checklist"),
        ];
        v_flex()
            .gap(tokens.spacing.lg)
            .child(self.section(
                "catalog-progressive-content",
                "Progressive content",
                "Selectable output, thinking disclosure, and tool details share one application-owned lifecycle vocabulary.",
                v_flex()
                    .gap(tokens.spacing.md)
                    .child(StreamingText::new("catalog-answer", &answer))
                    .child(Thinking::new("catalog-thinking", &thinking).open(true))
                    .child(ToolCall::new(&tool).open(true)),
                cx,
            ))
            .child(self.section(
                "catalog-approval",
                "Approval and attention",
                "The action, scope, consequence, and decision remain visible without relying on color.",
                ApprovalCard::new("catalog-approval-card", "Write the recovery checklist?")
                    .description("This creates docs/recovery-checklist.md inside the current workspace.")
                    .allow_always(true)
                    .decision(self.approval)
                    .on_event(cx.listener(|this, event: &ApprovalEvent, _, cx| {
                        this.approval = match event {
                            ApprovalEvent::Approved { .. }
                            | ApprovalEvent::ApprovedAlways { .. } => ApprovalDecision::Approved,
                            ApprovalEvent::Rejected { .. } => ApprovalDecision::Rejected,
                        };
                        this.last_event = format!("Approval: {event:?}").into();
                        cx.notify();
                    })),
                cx,
            ))
            .child(self.section(
                "catalog-assets-queue-usage",
                "Attachments, queue, and usage",
                "Bounded snapshots use stable IDs; the application performs every mutation.",
                v_flex()
                    .gap(tokens.spacing.md)
                    .child(AttachmentStrip::new("catalog-attachments").items(attachments))
                    .child(MessageQueue::new("catalog-queue").items(queued).editable(true))
                    .child(ContextMeter::new("catalog-context", &usage)),
                cx,
            ))
    }

    fn navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .gap(tokens.spacing.lg)
            .child(self.section(
                "catalog-thread-list",
                "Virtual conversation list",
                "The retained list virtualizes stable conversation IDs and emits typed selection/archive intents.",
                div()
                    .w(px(360.))
                    .h(px(360.))
                    .overflow_hidden()
                    .child(self.threads.clone()),
                cx,
            ))
            .child(self.section(
                "catalog-sidebar-nav",
                "Application navigation",
                "Recursive navigation owns keyboard and focus mechanics while Xana owns routes and capability filtering.",
                div()
                    .w(px(360.))
                    .h(px(360.))
                    .overflow_hidden()
                    .child(self.sidebar.clone()),
                cx,
            ))
            .child(self.section(
                "catalog-command-search",
                "Command search",
                "Searchable actions preserve stable command identity and expose unavailable actions as disabled.",
                div()
                    .h(px(300.))
                    .overflow_hidden()
                    .child(self.commands.clone()),
                cx,
            ))
    }
}

impl Render for ComponentCatalog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let body = match self.page {
            CatalogPage::Foundations => self.foundations(cx).into_any_element(),
            CatalogPage::Conversation => self.conversation(cx).into_any_element(),
            CatalogPage::AgentWork => self.agent_work(cx).into_any_element(),
            CatalogPage::Navigation => self.navigation(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .flex_none()
                    .gap(tokens.spacing.md)
                    .p(tokens.spacing.lg)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("catalog-title")
                            .role(Role::Heading)
                            .aria_label("Xana Desktop component catalog")
                            .text_size(tokens.typography.xl.size)
                            .line_height(tokens.typography.xl.line_height)
                            .font_semibold()
                            .child("Xana Desktop component catalog"),
                    )
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child("Review foundation behavior here; this is not final Workbench layout approval."),
                    )
                    .child(self.controls(cx)),
            )
            .child(
                div()
                    .id(format!("catalog-scroll-{}", self.page.id()))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().p(tokens.spacing.lg).child(body)),
            )
            .child(
                div()
                    .id("catalog-status")
                    .flex_none()
                    .role(Role::Status)
                    .aria_label(self.last_event.clone())
                    .px(tokens.spacing.lg)
                    .py(tokens.spacing.sm)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(tokens.typography.xs.size)
                    .text_color(cx.theme().muted_foreground)
                    .child(self.last_event.clone()),
            )
    }
}

fn catalog_conversation() -> Arc<[ChatMessage]> {
    Arc::from([
        ChatMessage::new(
            "catalog-user",
            ChatRole::User,
            StreamedContent::done("Can you recover the interrupted report safely?"),
        )
        .author("You")
        .with_appearance(ChatMessageAppearance::new(
            MessageAlignment::Trailing,
            MessageBubble::Filled,
        )),
        ChatMessage::new(
            "catalog-assistant",
            ChatRole::Assistant,
            StreamedContent::done(
                "Yes. I found two durable receipts and no evidence that the write was replayed. I will ask before changing anything.",
            ),
        )
        .author("Xana")
        .sources(["Recovery receipt · local runtime"]),
    ])
}

fn catalog_threads() -> [ThreadSection; 2] {
    [
        ThreadSection::new("today", "Today").items([
            ThreadItem::new("recovery-plan", "Recovery plan").subtitle("2 minutes ago"),
            ThreadItem::new("release-check", "Release verification").subtitle("18 minutes ago"),
            ThreadItem::new(
                "long-localized-title",
                "A deliberately longer conversation title that must reflow instead of disappearing",
            )
            .subtitle("Earlier today"),
        ]),
        ThreadSection::new("earlier", "Earlier").items([
            ThreadItem::new("architecture", "Desktop architecture").subtitle("Yesterday"),
            ThreadItem::new("archived", "Archived investigation")
                .subtitle("Last week")
                .archived(true),
        ]),
    ]
}

fn catalog_sidebar() -> [SidebarSection; 2] {
    [
        SidebarSection::new("workspace", "Workspace").items([
            SidebarNavItem::new("conversation", "Conversation"),
            SidebarNavItem::new("activity", "Activity").badge("3"),
            SidebarNavItem::new("artifacts", "Artifacts").children([
                SidebarNavItem::new("files", "Files"),
                SidebarNavItem::new("previews", "Previews"),
            ]),
        ]),
        SidebarSection::new("system", "System").items([
            SidebarNavItem::new("connections", "Connections"),
            SidebarNavItem::new("unavailable", "Remote controller")
                .badge("Unavailable")
                .disabled(true),
        ]),
    ]
}

fn catalog_commands() -> [CommandSearchItem; 4] {
    [
        CommandSearchItem::new("new-conversation", "New conversation")
            .subtitle("Create a local Xana conversation")
            .keywords(["chat", "session"])
            .shortcut("Ctrl+N"),
        CommandSearchItem::new("open-settings", "Open settings")
            .subtitle("Connection, model, permissions, and appearance")
            .keywords(["config", "provider"])
            .shortcut("Ctrl+,"),
        CommandSearchItem::new("inspect-receipt", "Inspect latest receipt")
            .subtitle("Open the durable operation receipt")
            .keywords(["recovery", "operation"]),
        CommandSearchItem::new("remote-controller", "Connect remote controller")
            .subtitle("Unavailable in Milestone 4")
            .disabled(true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn pages_have_unique_stable_ids() {
        let ids = CatalogPage::ALL
            .into_iter()
            .map(CatalogPage::id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), CatalogPage::ALL.len());
    }

    #[test]
    fn fixture_identity_is_stable_and_unique() {
        let thread_ids = catalog_threads()
            .into_iter()
            .flat_map(|section| section.thread_items().to_vec())
            .map(|thread| thread.id().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            thread_ids.len(),
            thread_ids.iter().collect::<HashSet<_>>().len()
        );

        let command_ids = catalog_commands()
            .into_iter()
            .map(|command| command.id().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            command_ids.len(),
            command_ids.iter().collect::<HashSet<_>>().len()
        );
    }

    #[test]
    fn stress_fixture_exercises_reflow() {
        let longest = catalog_threads()
            .into_iter()
            .flat_map(|section| section.thread_items().to_vec())
            .map(|thread| thread.title().chars().count())
            .max();
        assert!(longest.is_some_and(|length| length > 80));
    }
}
