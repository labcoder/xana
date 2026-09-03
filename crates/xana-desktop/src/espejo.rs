//! Retained Desktop Espejo view over bounded runtime-owned facts.

use gpui::{
    AnyElement, Context, EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, div,
    prelude::*, rems,
};
use gpui_ai::prelude::{StatusBadge, StatusTone};
use gpui_component::{
    ActiveTheme as _, Selectable as _, button::Button, h_flex, scroll::ScrollableElement as _,
    v_flex,
};
use std::collections::{HashMap, VecDeque};
use xana::desktop::{
    DesktopControllerLease, DesktopConversationNode, DesktopConversationState, DesktopGlobalNotice,
    DesktopHostEvent, DesktopHostObservation, DesktopHostedConversation,
    DesktopNavigationConversationState, DesktopNavigationSnapshot, DesktopSnapshot,
};

const MAX_NOTICES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EspejoScope {
    Global,
    Project(String),
}

impl EspejoScope {
    pub(crate) fn label(&self, navigation: &DesktopNavigationSnapshot) -> String {
        match self {
            Self::Global => "All Projects and ungrouped Conversations".to_owned(),
            Self::Project(project_id) => navigation
                .projects
                .iter()
                .find(|project| &project.id == project_id)
                .map_or_else(
                    || "Unavailable Project".to_owned(),
                    |project| format!("Project: {}", project.name),
                ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EspejoFilter {
    All,
    NeedsYou,
    InMotion,
    BlockedFailed,
    Completed,
    Idle,
}

impl EspejoFilter {
    pub(crate) const ALL: [Self; 6] = [
        Self::All,
        Self::NeedsYou,
        Self::InMotion,
        Self::BlockedFailed,
        Self::Completed,
        Self::Idle,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::NeedsYou => "Needs you",
            Self::InMotion => "In motion",
            Self::BlockedFailed => "Blocked / failed",
            Self::Completed => "Completed",
            Self::Idle => "Idle",
        }
    }

    fn accepts(self, group: EspejoGroup) -> bool {
        self == Self::All || self.group() == Some(group)
    }

    const fn group(self) -> Option<EspejoGroup> {
        match self {
            Self::All => None,
            Self::NeedsYou => Some(EspejoGroup::NeedsYou),
            Self::InMotion => Some(EspejoGroup::InMotion),
            Self::BlockedFailed => Some(EspejoGroup::BlockedFailed),
            Self::Completed => Some(EspejoGroup::Completed),
            Self::Idle => Some(EspejoGroup::Idle),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum EspejoGroup {
    NeedsYou,
    InMotion,
    BlockedFailed,
    Completed,
    Idle,
}

impl EspejoGroup {
    pub(crate) const ALL: [Self; 5] = [
        Self::NeedsYou,
        Self::InMotion,
        Self::BlockedFailed,
        Self::Completed,
        Self::Idle,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::InMotion => "In motion",
            Self::BlockedFailed => "Blocked or failed",
            Self::Completed => "Recently completed",
            Self::Idle => "Idle",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EspejoCard {
    pub(crate) conversation_id: String,
    pub(crate) project_id: Option<String>,
    pub(crate) title: String,
    pub(crate) workspace: String,
    pub(crate) owner: String,
    pub(crate) connection: String,
    pub(crate) model: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) permission_mode: Option<String>,
    pub(crate) state: String,
    pub(crate) group: EspejoGroup,
    pub(crate) selected: bool,
    pub(crate) active_operation: Option<String>,
    pub(crate) pending_approvals: usize,
    pub(crate) activity_count: usize,
    pub(crate) last_outcome: Option<String>,
    pub(crate) queued_count: usize,
    pub(crate) controller_state: Option<String>,
    pub(crate) modified_unix_ms: Option<u64>,
}

impl EspejoCard {
    pub(crate) fn needs_attention(&self) -> bool {
        self.group == EspejoGroup::NeedsYou
    }
}

/// Evolving, internal Espejo state. Runtime snapshots remain authoritative.
struct EspejoProjection {
    host_lifecycle: String,
    hosted: HashMap<String, DesktopHostedConversation>,
    notices: VecDeque<DesktopGlobalNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EspejoViewEvent {
    Close,
    OpenConversation {
        conversation_id: String,
        needs_attention: bool,
    },
    OpenDiagnostics,
}

pub(crate) struct EspejoView {
    projection: EspejoProjection,
    navigation: DesktopNavigationSnapshot,
    queue_counts: HashMap<String, usize>,
    selected_project: Option<String>,
    scope: EspejoScope,
    filter: EspejoFilter,
}

impl EspejoView {
    pub(crate) fn new(snapshot: &DesktopSnapshot) -> Self {
        let selected_project = snapshot
            .navigation
            .selected_conversation
            .as_deref()
            .and_then(|conversation_id| {
                project_for_conversation(&snapshot.navigation, conversation_id)
            });
        Self {
            projection: EspejoProjection::from_snapshot(snapshot),
            navigation: snapshot.navigation.clone(),
            queue_counts: HashMap::new(),
            selected_project,
            scope: EspejoScope::Global,
            filter: EspejoFilter::All,
        }
    }

    pub(crate) fn open(&mut self, scope: EspejoScope, cx: &mut Context<Self>) {
        self.scope = scope;
        cx.notify();
    }

    pub(crate) fn replace_snapshot(&mut self, snapshot: &DesktopSnapshot, cx: &mut Context<Self>) {
        self.projection.replace_snapshot(snapshot);
        self.navigation = snapshot.navigation.clone();
        cx.notify();
    }

    pub(crate) fn apply_host(
        &mut self,
        observation: &DesktopHostObservation,
        cx: &mut Context<Self>,
    ) {
        self.projection.apply_host(observation);
        cx.notify();
    }

    pub(crate) fn update_navigation(
        &mut self,
        navigation: DesktopNavigationSnapshot,
        selected_project: Option<String>,
        queue_counts: HashMap<String, usize>,
        cx: &mut Context<Self>,
    ) {
        self.navigation = navigation;
        self.selected_project = selected_project;
        self.queue_counts = queue_counts;
        cx.notify();
    }

    fn render_notices(
        &self,
        notices: Vec<DesktopGlobalNotice>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .p(tokens.spacing.md)
            .rounded(tokens.radius.lg)
            .border_1()
            .border_color(cx.theme().warning)
            .bg(cx.theme().warning.opacity(0.08))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap(tokens.spacing.md)
                    .child(
                        v_flex()
                            .gap(tokens.spacing.xs)
                            .child("Host notices")
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Redacted process-wide facts; Conversation errors remain in Activity."),
                            ),
                    )
                    .child(
                        Button::new("espejo-open-diagnostics")
                            .compact()
                            .label("Open Diagnostics")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(EspejoViewEvent::OpenDiagnostics);
                            })),
                    ),
            )
            .children(notices.into_iter().map(|notice| {
                let destination = notice
                    .conversation
                    .as_deref()
                    .map(short_identity)
                    .map_or_else(|| "host".to_owned(), |id| format!("Conversation {id}"));
                div()
                    .text_sm()
                    .child(format!("{} · {} · {destination}", notice.kind, notice.code))
            }))
            .into_any_element()
    }

    fn render_group(
        &self,
        group: EspejoGroup,
        cards: Vec<EspejoCard>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let count = cards.len();
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(group.label()),
                    )
                    .child(
                        StatusBadge::new(format!("espejo-group-{:?}", group), count.to_string())
                            .tone(espejo_tone(group)),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .flex_wrap()
                    .gap(tokens.spacing.md)
                    .children(cards.into_iter().map(|card| self.render_card(card, cx))),
            )
            .into_any_element()
    }

    fn render_card(&self, card: EspejoCard, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let conversation_id = card.conversation_id.clone();
        let needs_attention = card.needs_attention();
        let project = card.project_id.as_deref().map_or_else(
            || "Ungrouped".to_owned(),
            |project_id| project_title(&self.navigation, project_id),
        );
        let model = card.model.as_deref().unwrap_or("model unavailable");
        let profile = card.profile.as_deref().unwrap_or("default Profile");
        let permissions = card
            .permission_mode
            .as_deref()
            .unwrap_or("policy unavailable");
        let operation = card.active_operation.as_deref().map_or_else(
            || {
                card.last_outcome
                    .as_deref()
                    .unwrap_or("no active Run")
                    .to_owned()
            },
            |operation| format!("Run {}", short_identity(operation)),
        );
        let controller = card
            .controller_state
            .as_deref()
            .unwrap_or("observer state unavailable");
        Button::new(format!("espejo-conversation-{}", card.conversation_id))
            .w(rems(24.))
            .min_h(rems(12.))
            .selected(card.selected)
            .child(
                v_flex()
                    .w_full()
                    .items_start()
                    .gap(tokens.spacing.sm)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .gap(tokens.spacing.sm)
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(card.title),
                            )
                            .child(
                                StatusBadge::new(
                                    format!("espejo-state-{}", card.conversation_id),
                                    card.state,
                                )
                                .tone(espejo_tone(card.group)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{project} · {}", card.workspace)),
                    )
                    .child(div().text_sm().child(format!(
                        "{} · {} / {model} · {profile}",
                        display_owner(&card.owner),
                        card.connection,
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{operation} · permissions {permissions}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} approval(s) · {} queued · {} activity event(s)",
                                card.pending_approvals, card.queued_count, card.activity_count
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Controller: {controller}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Open for exact Activity, approvals, artifacts, usage, and receipts."),
                    ),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(EspejoViewEvent::OpenConversation {
                    conversation_id: conversation_id.clone(),
                    needs_attention,
                });
            }))
            .into_any_element()
    }
}

impl EventEmitter<EspejoViewEvent> for EspejoView {}

impl Render for EspejoView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let cards = self.projection.cards(
            &self.navigation,
            &self.scope,
            self.filter,
            &self.queue_counts,
        );
        let scope_label = self.scope.label(&self.navigation);
        let global_selected = self.scope == EspejoScope::Global;
        let project_scope = self.selected_project.as_ref().and_then(|project_id| {
            self.navigation
                .projects
                .iter()
                .find(|project| &project.id == project_id && !project.archived)
                .map(|project| (project.id.clone(), project.name.clone()))
        });
        let filters = EspejoFilter::ALL
            .into_iter()
            .map(|filter| {
                Button::new(format!("espejo-filter-{}", filter.label()))
                    .compact()
                    .label(filter.label())
                    .selected(self.filter == filter)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter = filter;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();
        let sections = EspejoGroup::ALL
            .into_iter()
            .filter_map(|group| {
                let group_cards = cards
                    .iter()
                    .filter(|card| card.group == group)
                    .cloned()
                    .collect::<Vec<_>>();
                (!group_cards.is_empty()).then(|| self.render_group(group, group_cards, cx))
            })
            .collect::<Vec<_>>();
        let notices = self
            .projection
            .notices()
            .rev()
            .take(8)
            .cloned()
            .collect::<Vec<_>>();

        v_flex()
            .id("xana-espejo")
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .w_full()
                    .flex_none()
                    .gap(tokens.spacing.sm)
                    .px(tokens.spacing.lg)
                    .py(tokens.spacing.md)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .gap(tokens.spacing.md)
                            .child(
                                v_flex()
                                    .gap(tokens.spacing.xs)
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Espejo"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{scope_label} · host {} · {} visible Conversation(s)",
                                                self.projection.host_lifecycle(),
                                                cards.len()
                                            )),
                                    ),
                            )
                            .child(
                                Button::new("espejo-back-to-conversation")
                                    .label("Back to Conversation")
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(EspejoViewEvent::Close);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .flex_wrap()
                            .gap(tokens.spacing.xs)
                            .child(
                                Button::new("espejo-scope-global")
                                    .compact()
                                    .label("Global")
                                    .selected(global_selected)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.scope = EspejoScope::Global;
                                        cx.notify();
                                    })),
                            )
                            .when_some(project_scope, |row, (project_id, project_name)| {
                                let selected = scope_is_project(&self.scope, &project_id);
                                row.child(
                                    Button::new("espejo-scope-project")
                                        .compact()
                                        .label(format!("Project · {project_name}"))
                                        .selected(selected)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.scope = EspejoScope::Project(project_id.clone());
                                            cx.notify();
                                        })),
                                )
                            })
                            .child(div().w(tokens.spacing.md))
                            .children(filters),
                    ),
            )
            .child(
                v_flex()
                    .id("xana-espejo-scroll")
                    .flex_1()
                    .min_h_0()
                    .gap(tokens.spacing.lg)
                    .p(tokens.spacing.lg)
                    .overflow_y_scrollbar()
                    .when(!notices.is_empty(), |body| {
                        body.child(self.render_notices(notices, cx))
                    })
                    .when(sections.is_empty(), |body| {
                        body.child(
                            v_flex()
                                .gap(tokens.spacing.xs)
                                .p(tokens.spacing.lg)
                                .rounded(tokens.radius.lg)
                                .border_1()
                                .border_color(cx.theme().border)
                                .child("Nothing matches this scope and filter.")
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "Change the filter or start a Conversation. Xana does not invent scheduled work.",
                                        ),
                                ),
                        )
                    })
                    .children(sections),
            )
    }
}

impl EspejoProjection {
    pub(crate) fn from_snapshot(snapshot: &DesktopSnapshot) -> Self {
        Self {
            host_lifecycle: snapshot.host_lifecycle.clone(),
            hosted: snapshot
                .hosted_conversations
                .iter()
                .cloned()
                .map(|conversation| (conversation.conversation.clone(), conversation))
                .collect(),
            notices: snapshot.global_notices.iter().cloned().collect(),
        }
    }

    pub(crate) fn replace_snapshot(&mut self, snapshot: &DesktopSnapshot) {
        *self = Self::from_snapshot(snapshot);
    }

    pub(crate) fn apply_host(&mut self, observation: &DesktopHostObservation) {
        match &observation.event {
            DesktopHostEvent::RunStarted {
                conversation,
                operation_id,
                ..
            } => {
                if let Some(hosted) = self.hosted.get_mut(conversation) {
                    hosted.state = DesktopConversationState::Running;
                    hosted.active_operation = Some(*operation_id);
                }
            }
            DesktopHostEvent::RunFinished {
                conversation,
                state,
                ..
            } => {
                if let Some(hosted) = self.hosted.get_mut(conversation) {
                    hosted.state = *state;
                    hosted.active_operation = None;
                }
            }
            DesktopHostEvent::ControllerChanged {
                conversation,
                controller,
                ..
            } => {
                if let Some(hosted) = self.hosted.get_mut(conversation) {
                    hosted.controller.clone_from(controller);
                }
            }
            DesktopHostEvent::GlobalNotice(notice) => self.push_notice(notice.clone()),
            DesktopHostEvent::LifecycleChanged { state } => self.host_lifecycle.clone_from(state),
            DesktopHostEvent::ShutdownCompleted { .. } => {
                self.host_lifecycle = "stopped".to_owned();
            }
            DesktopHostEvent::RuntimeObservation { conversation } => {
                if let Some(hosted) = self.hosted.get_mut(conversation) {
                    hosted.activity_count = hosted.activity_count.saturating_add(1);
                }
            }
            DesktopHostEvent::ConversationRegistered { .. }
            | DesktopHostEvent::ConversationAttached { .. } => {}
        }
    }

    pub(crate) fn host_lifecycle(&self) -> &str {
        &self.host_lifecycle
    }

    pub(crate) fn notices(&self) -> impl DoubleEndedIterator<Item = &DesktopGlobalNotice> {
        self.notices.iter()
    }

    pub(crate) fn cards(
        &self,
        navigation: &DesktopNavigationSnapshot,
        scope: &EspejoScope,
        filter: EspejoFilter,
        queue_counts: &HashMap<String, usize>,
    ) -> Vec<EspejoCard> {
        let mut cards = Vec::new();
        match scope {
            EspejoScope::Global => {
                for project in navigation
                    .projects
                    .iter()
                    .filter(|project| !project.archived)
                {
                    cards.extend(project.conversations.iter().map(|conversation| {
                        self.card_for(
                            conversation,
                            Some(project.id.clone()),
                            queue_counts
                                .get(&conversation.id)
                                .copied()
                                .unwrap_or_default(),
                        )
                    }));
                }
                cards.extend(navigation.ungrouped.iter().map(|conversation| {
                    self.card_for(
                        conversation,
                        None,
                        queue_counts
                            .get(&conversation.id)
                            .copied()
                            .unwrap_or_default(),
                    )
                }));
            }
            EspejoScope::Project(project_id) => {
                if let Some(project) = navigation
                    .projects
                    .iter()
                    .find(|project| &project.id == project_id && !project.archived)
                {
                    cards.extend(project.conversations.iter().map(|conversation| {
                        self.card_for(
                            conversation,
                            Some(project.id.clone()),
                            queue_counts
                                .get(&conversation.id)
                                .copied()
                                .unwrap_or_default(),
                        )
                    }));
                }
            }
        }
        cards.retain(|card| filter.accepts(card.group));
        cards.sort_by(|left, right| {
            left.group
                .cmp(&right.group)
                .then_with(|| right.modified_unix_ms.cmp(&left.modified_unix_ms))
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        cards
    }

    fn card_for(
        &self,
        conversation: &DesktopConversationNode,
        project_id: Option<String>,
        queued_count: usize,
    ) -> EspejoCard {
        let hosted = self.hosted.get(&conversation.id);
        let pending_approvals = hosted.map_or(0, |hosted| hosted.pending_approvals);
        let state = hosted.map(|hosted| hosted.state);
        let group = classify(conversation, state, pending_approvals, queued_count);
        EspejoCard {
            conversation_id: conversation.id.clone(),
            project_id,
            title: conversation.title.clone(),
            workspace: conversation.workspace_label.clone(),
            owner: conversation.owner.clone(),
            connection: conversation.connection.clone(),
            model: hosted.map(|hosted| hosted.model.clone()),
            profile: hosted.and_then(|hosted| hosted.profile.clone()),
            permission_mode: hosted.map(|hosted| hosted.permission_mode.clone()),
            state: state.map_or_else(
                || conversation.state.as_str().to_owned(),
                |state| state.as_str().to_owned(),
            ),
            group,
            selected: conversation.selected,
            active_operation: hosted
                .and_then(|hosted| hosted.active_operation)
                .map(|operation| operation.to_string()),
            pending_approvals,
            activity_count: hosted.map_or(0, |hosted| hosted.activity_count),
            last_outcome: hosted.and_then(|hosted| hosted.last_outcome.clone()),
            queued_count,
            controller_state: hosted
                .and_then(|hosted| hosted.controller.as_ref())
                .map(controller_label),
            modified_unix_ms: conversation.modified_unix_ms,
        }
    }

    fn push_notice(&mut self, notice: DesktopGlobalNotice) {
        if self.notices.iter().any(|candidate| candidate == &notice) {
            return;
        }
        self.notices.push_back(notice);
        while self.notices.len() > MAX_NOTICES {
            self.notices.pop_front();
        }
    }
}

fn classify(
    conversation: &DesktopConversationNode,
    state: Option<DesktopConversationState>,
    pending_approvals: usize,
    queued_count: usize,
) -> EspejoGroup {
    if conversation.needs_attention
        || pending_approvals > 0
        || state == Some(DesktopConversationState::Suspended)
    {
        return EspejoGroup::NeedsYou;
    }
    match state {
        Some(DesktopConversationState::Running) => EspejoGroup::InMotion,
        Some(
            DesktopConversationState::Failed
            | DesktopConversationState::Declined
            | DesktopConversationState::Interrupted,
        ) => EspejoGroup::BlockedFailed,
        Some(DesktopConversationState::Completed) => EspejoGroup::Completed,
        Some(DesktopConversationState::Idle) | None if queued_count > 0 => EspejoGroup::InMotion,
        Some(DesktopConversationState::Idle) | None => match conversation.state {
            DesktopNavigationConversationState::Active => EspejoGroup::InMotion,
            DesktopNavigationConversationState::Unavailable => EspejoGroup::BlockedFailed,
            DesktopNavigationConversationState::Idle
            | DesktopNavigationConversationState::Controlled
            | DesktopNavigationConversationState::Observable => EspejoGroup::Idle,
        },
        Some(DesktopConversationState::Suspended) => EspejoGroup::NeedsYou,
    }
}

fn controller_label(controller: &DesktopControllerLease) -> String {
    if controller.takeover_confirmed {
        format!("{} · takeover confirmed", controller.state)
    } else {
        controller.state.clone()
    }
}

fn project_for_conversation(snapshot: &DesktopNavigationSnapshot, id: &str) -> Option<String> {
    snapshot.projects.iter().find_map(|project| {
        project
            .conversations
            .iter()
            .any(|conversation| conversation.id == id)
            .then(|| project.id.clone())
    })
}

fn project_title(snapshot: &DesktopNavigationSnapshot, id: &str) -> String {
    snapshot
        .projects
        .iter()
        .find(|project| project.id == id)
        .map(|project| project.name.clone())
        .unwrap_or_else(|| "Project".to_owned())
}

fn scope_is_project(scope: &EspejoScope, project_id: &str) -> bool {
    matches!(scope, EspejoScope::Project(id) if id == project_id)
}

fn espejo_tone(group: EspejoGroup) -> StatusTone {
    match group {
        EspejoGroup::NeedsYou => StatusTone::Warning,
        EspejoGroup::InMotion => StatusTone::Info,
        EspejoGroup::BlockedFailed => StatusTone::Danger,
        EspejoGroup::Completed => StatusTone::Success,
        EspejoGroup::Idle => StatusTone::Neutral,
    }
}

fn display_owner(owner: &str) -> String {
    match owner {
        "native" | "xana" | "xana_root" => "Xana native".to_owned(),
        "managed" | "managed_codex" | "codex" => "Managed Codex".to_owned(),
        other => humanize_semantic_code(other),
    }
}

fn humanize_semantic_code(code: &str) -> String {
    let mut label = code
        .split(['.', '_'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        return "Unavailable".to_owned();
    }
    let first = label.remove(0).to_uppercase().to_string();
    label.insert_str(0, &first);
    label
}

fn short_identity(identity: &str) -> &str {
    identity.get(..8).unwrap_or(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xana::desktop::{DesktopNavigationSnapshot, DesktopProjectNode, DesktopWorkspaceStatus};

    fn conversation(id: &str, attention: bool) -> DesktopConversationNode {
        DesktopConversationNode {
            id: id.to_owned(),
            title: format!("Conversation {id}"),
            owner: "native".to_owned(),
            connection: "ollama".to_owned(),
            workspace_id: "workspace-a".to_owned(),
            workspace_label: "workspace".to_owned(),
            state: DesktopNavigationConversationState::Idle,
            selected: id == "a",
            needs_attention: attention,
            record_count: Some(2),
            modified_unix_ms: Some(1),
            branch_point: Some("2".to_owned()),
        }
    }

    fn hosted(id: &str, state: DesktopConversationState) -> DesktopHostedConversation {
        DesktopHostedConversation {
            conversation: id.to_owned(),
            workspace_id: "workspace-a".to_owned(),
            connection: "ollama".to_owned(),
            model: "fixture".to_owned(),
            profile: Some("default".to_owned()),
            permission_mode: "ask".to_owned(),
            state,
            active_operation: None,
            pending_approvals: 0,
            activity_count: 3,
            last_outcome: None,
            controller: None,
        }
    }

    fn navigation() -> DesktopNavigationSnapshot {
        DesktopNavigationSnapshot {
            version: 1,
            sidebar_mode: xana::desktop::DesktopSidebarMode::Full,
            projects: vec![DesktopProjectNode {
                id: "project-a".to_owned(),
                name: "Project A".to_owned(),
                workspace_label: "workspace".to_owned(),
                workspace_id: Some("workspace-a".to_owned()),
                workspace_status: DesktopWorkspaceStatus::Available,
                archived: false,
                conversations: vec![conversation("a", false), conversation("b", true)],
            }],
            ungrouped: vec![conversation("c", false)],
            selected_conversation: Some("a".to_owned()),
            project_count: 1,
            conversation_count: 3,
            truncated: false,
        }
    }

    fn projection(hosted: Vec<DesktopHostedConversation>) -> EspejoProjection {
        EspejoProjection {
            host_lifecycle: "running".to_owned(),
            hosted: hosted
                .into_iter()
                .map(|conversation| (conversation.conversation.clone(), conversation))
                .collect(),
            notices: VecDeque::new(),
        }
    }

    #[test]
    fn project_scope_excludes_ungrouped_and_other_groups() {
        let projection = projection(vec![
            hosted("a", DesktopConversationState::Running),
            hosted("b", DesktopConversationState::Idle),
            hosted("c", DesktopConversationState::Completed),
        ]);
        let cards = projection.cards(
            &navigation(),
            &EspejoScope::Project("project-a".to_owned()),
            EspejoFilter::All,
            &HashMap::new(),
        );

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].conversation_id, "b");
        assert_eq!(cards[0].group, EspejoGroup::NeedsYou);
        assert_eq!(cards[1].conversation_id, "a");
        assert_eq!(cards[1].group, EspejoGroup::InMotion);
    }

    #[test]
    fn filters_and_queue_state_are_deterministic() {
        let projection = projection(vec![hosted("a", DesktopConversationState::Idle)]);
        let queue_counts = HashMap::from([("a".to_owned(), 2)]);
        let cards = projection.cards(
            &navigation(),
            &EspejoScope::Global,
            EspejoFilter::InMotion,
            &queue_counts,
        );

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].conversation_id, "a");
        assert_eq!(cards[0].queued_count, 2);
    }

    #[test]
    fn host_events_update_terminal_state_and_deduplicate_notices() {
        let mut projection = projection(vec![hosted("a", DesktopConversationState::Running)]);
        let operation_id = xana::desktop::DesktopOperationId::new();
        projection.apply_host(&DesktopHostObservation {
            sequence: 1,
            event: DesktopHostEvent::RunFinished {
                conversation: "a".to_owned(),
                operation_id,
                state: DesktopConversationState::Failed,
                error: Some("redacted fixture".to_owned()),
            },
        });
        let notice = DesktopGlobalNotice {
            kind: "hostfailure".to_owned(),
            code: "host.failed".to_owned(),
            conversation: Some("a".to_owned()),
            operation_id: Some(operation_id),
        };
        for sequence in 2..=3 {
            projection.apply_host(&DesktopHostObservation {
                sequence,
                event: DesktopHostEvent::GlobalNotice(notice.clone()),
            });
        }

        let cards = projection.cards(
            &navigation(),
            &EspejoScope::Global,
            EspejoFilter::BlockedFailed,
            &HashMap::new(),
        );
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].conversation_id, "a");
        assert_eq!(projection.notices().count(), 1);
    }

    #[test]
    fn eight_conversation_four_stream_fixture_stays_bounded_and_grouped() {
        let conversations = (0..8)
            .map(|index| conversation(&format!("conversation-{index}"), index == 7))
            .collect::<Vec<_>>();
        let hosted = conversations
            .iter()
            .enumerate()
            .map(|(index, conversation)| {
                hosted(
                    &conversation.id,
                    if index < 4 {
                        DesktopConversationState::Running
                    } else {
                        DesktopConversationState::Idle
                    },
                )
            })
            .collect::<Vec<_>>();
        let navigation = DesktopNavigationSnapshot {
            version: 1,
            sidebar_mode: xana::desktop::DesktopSidebarMode::Full,
            projects: vec![DesktopProjectNode {
                id: "project-a".to_owned(),
                name: "Project A".to_owned(),
                workspace_label: "workspace".to_owned(),
                workspace_id: Some("workspace-a".to_owned()),
                workspace_status: DesktopWorkspaceStatus::Available,
                archived: false,
                conversations,
            }],
            ungrouped: Vec::new(),
            selected_conversation: None,
            project_count: 1,
            conversation_count: 8,
            truncated: false,
        };

        let cards = projection(hosted).cards(
            &navigation,
            &EspejoScope::Global,
            EspejoFilter::All,
            &HashMap::new(),
        );

        assert_eq!(cards.len(), 8);
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.group == EspejoGroup::InMotion)
                .count(),
            4
        );
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.group == EspejoGroup::NeedsYou)
                .count(),
            1
        );
    }

    #[test]
    fn global_notices_keep_a_hard_bound() {
        let mut projection = projection(Vec::new());
        for index in 0..(MAX_NOTICES + 10) {
            projection.push_notice(DesktopGlobalNotice {
                kind: "fixture".to_owned(),
                code: format!("fixture.{index}"),
                conversation: None,
                operation_id: None,
            });
        }

        assert_eq!(projection.notices().count(), MAX_NOTICES);
        assert_eq!(
            projection
                .notices()
                .next()
                .map(|notice| notice.code.as_str()),
            Some("fixture.10")
        );
    }
}
