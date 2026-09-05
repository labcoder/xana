//! Read-only saved pages, independent of the current live projection and composer.

use super::*;
use xana::desktop::{DesktopHistoryPage, DesktopHistoryRequest};

struct SavedPage {
    page: DesktopHistoryPage,
    messages: Arc<[gpui_ai::prelude::ChatMessage]>,
}

enum HistoryMode {
    Live,
    Browsing {
        saved: Option<Box<SavedPage>>,
        loading: bool,
        error: Option<String>,
    },
}

pub(super) struct HistoryView {
    session: String,
    start: usize,
    total: usize,
    generation: u64,
    mode: HistoryMode,
}

impl HistoryView {
    pub(super) fn new(session: String, start: usize, total: usize) -> Self {
        Self {
            session,
            start,
            total,
            generation: 0,
            mode: HistoryMode::Live,
        }
    }

    pub(super) fn reset(&mut self, session: String, start: usize, total: usize) {
        self.live();
        self.session = session;
        self.observe(start, total);
    }

    pub(super) fn observe(&mut self, start: usize, total: usize) {
        self.start = start;
        self.total = total;
    }

    pub(super) fn clear(&mut self, start: usize, total: usize) {
        self.live();
        self.observe(start, total);
    }

    pub(super) fn is_browsing(&self) -> bool {
        !matches!(self.mode, HistoryMode::Live)
    }

    pub(super) fn live(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.mode = HistoryMode::Live;
    }

    fn loading(&self) -> bool {
        matches!(self.mode, HistoryMode::Browsing { loading: true, .. })
    }

    fn saved(&self) -> Option<&SavedPage> {
        match &self.mode {
            HistoryMode::Browsing { saved, .. } => saved.as_deref(),
            HistoryMode::Live => None,
        }
    }

    pub(super) fn messages(&self) -> Option<Arc<[gpui_ai::prelude::ChatMessage]>> {
        self.saved().map(|saved| Arc::clone(&saved.messages))
    }

    fn older(&self, desktop_omitted: bool) -> Option<DesktopHistoryRequest> {
        if self.loading() {
            return None;
        }
        match self.saved() {
            Some(saved) => saved.page.older.clone().map(DesktopHistoryRequest::Older),
            None if self.start > 0 => Some(DesktopHistoryRequest::Latest {
                before: Some(self.start),
            }),
            // The Desktop resource/byte window can evict rows independently
            // of the backend. Open a canonical saved tail, then use its cursor;
            // visual row counts cannot identify a durable source boundary.
            None if desktop_omitted => Some(DesktopHistoryRequest::Latest { before: None }),
            None => None,
        }
    }

    fn newer(&self) -> Option<DesktopHistoryRequest> {
        if self.loading() {
            return None;
        }
        self.saved()?
            .page
            .newer
            .clone()
            .map(DesktopHistoryRequest::Newer)
    }

    fn begin(&mut self) -> (String, u64) {
        self.generation = self.generation.wrapping_add(1);
        let prior = std::mem::replace(&mut self.mode, HistoryMode::Live);
        let saved = match prior {
            HistoryMode::Browsing { saved, .. } => saved,
            HistoryMode::Live => None,
        };
        self.mode = HistoryMode::Browsing {
            saved,
            loading: true,
            error: None,
        };
        (self.session.clone(), self.generation)
    }

    fn finish(
        &mut self,
        session: &str,
        generation: u64,
        result: Result<DesktopHistoryPage, String>,
    ) -> bool {
        if self.session != session || self.generation != generation || !self.loading() {
            return false;
        }
        let HistoryMode::Browsing {
            saved,
            loading,
            error,
        } = &mut self.mode
        else {
            return false;
        };
        *loading = false;
        match result {
            Ok(page) if page.session_id == session => {
                let messages = ConversationProjection::saved_messages(&page.messages);
                *saved = Some(Box::new(SavedPage { page, messages }));
                *error = None;
            }
            Ok(_) => {
                *error = Some(
                    "History response belongs to another Conversation. Return to Live and retry."
                        .into(),
                )
            }
            Err(reason) => {
                *error = Some(format!(
                    "Could not read saved history: {reason}. Return to Live and retry."
                ))
            }
        }
        true
    }

    fn label(&self) -> String {
        if self.loading() {
            return "Loading saved history… Sending returns to Live.".into();
        }
        if let HistoryMode::Browsing {
            error: Some(reason),
            ..
        } = &self.mode
        {
            return reason.clone();
        }
        match self.saved() {
            Some(saved) => format!(
                "Saved entries {}–{} of {} · read-only · live total {}",
                saved.page.start.saturating_add(1).min(saved.page.end),
                saved.page.end,
                saved.page.total,
                self.total
            ),
            None => format!("Live · {} saved entries", self.total),
        }
    }
}

impl Workbench {
    pub(super) fn older_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.projection.execution_owner() != "native" {
            return;
        }
        if let Some(request) = self.history.older(self.projection.history_omitted()) {
            self.load_history(request, window, cx);
        }
    }

    pub(super) fn newer_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(request) = self.history.newer() {
            self.load_history(request, window, cx);
        }
    }

    pub(super) fn live_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history_task = None;
        self.history.live();
        self.sync_components(window, cx);
    }

    fn load_history(
        &mut self,
        request: DesktopHistoryRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reader = self.runtime.history_reader();
        let (session, generation) = self.history.begin();
        self.sync_components(window, cx);
        self.history_task = Some(cx.spawn_in(window, async move |this, cx| {
            let requested_session = session.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    reader
                        .page(&requested_session, request)
                        .map_err(|error| error.message)
                })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                if this.history.finish(&session, generation, result) {
                    this.sync_components(window, cx);
                }
            });
        }));
    }

    pub(super) fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let native = self.projection.execution_owner() == "native";
        v_flex().size_full().min_h_0()
            .child(h_flex().flex_wrap().gap(tokens.spacing.sm).p(tokens.spacing.xs)
                .child(Button::new("history-older").compact().label("Older")
                    .tooltip("Read an older saved page · Ctrl/Command+Alt+Up")
                    .disabled(!native || self.history.older(self.projection.history_omitted()).is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.older_history(window, cx))))
                .child(Button::new("history-newer").compact().label("Newer")
                    .tooltip("Read a newer saved page · Ctrl/Command+Alt+Down")
                    .disabled(!native || self.history.newer().is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.newer_history(window, cx))))
                .child(Button::new("history-live").compact().label("Live")
                    .tooltip("Return to live messages · Ctrl/Command+Alt+End")
                    .disabled(!self.history.is_browsing())
                    .on_click(cx.listener(|this, _, window, cx| this.live_history(window, cx))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(if native { self.history.label() } else { "Managed history stays vendor-owned; Xana saved paging is unavailable.".into() })))
            .child(div().flex_1().min_h_0().child(self.chat.clone()))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(session: &str, start: usize, end: usize) -> DesktopHistoryPage {
        DesktopHistoryPage {
            session_id: session.into(),
            messages: Vec::new(),
            start,
            end,
            total: 500,
            older: None,
            newer: None,
        }
    }

    #[test]
    fn saved_page_survives_live_appends_and_live_restores_current_offsets() {
        let mut view = HistoryView::new("session".into(), 400, 500);
        assert!(matches!(
            view.older(false),
            Some(DesktopHistoryRequest::Latest { before: Some(400) })
        ));
        let (session, generation) = view.begin();
        assert!(view.is_browsing());
        assert!(view.older(false).is_none());
        assert!(view.finish(&session, generation, Ok(page("session", 300, 400))));
        view.observe(410, 510);
        assert_eq!(view.saved().expect("saved page retained").page.start, 300);
        assert!(view.is_browsing());
        view.live();
        assert!(!view.is_browsing());
        assert!(view.messages().is_none());
        assert!(matches!(
            view.older(false),
            Some(DesktopHistoryRequest::Latest { before: Some(410) })
        ));
    }

    #[test]
    fn desktop_window_omission_at_zero_backend_start_opens_bounded_saved_metadata() {
        use xana::desktop::{
            DesktopContent, DesktopContentTier, DesktopContentValue, DesktopMessage, DesktopRole,
        };

        // The backend retained all 200 entries, but Desktop's separate
        // resource window reports omissions after its 128-resource cap.
        let mut view = HistoryView::new("session".into(), 0, 200);
        assert!(view.older(false).is_none());
        assert_eq!(
            view.older(true),
            Some(DesktopHistoryRequest::Latest { before: None })
        );
        let (session, generation) = view.begin();
        assert!(view.older(true).is_none());
        let messages = (72..200)
            .map(|index| {
                let metadata = format!("Saved artifact-{index}: binary, 1 byte; metadata only");
                DesktopMessage {
                    id: format!("session:history:{index}"),
                    role: DesktopRole::Tool,
                    content: vec![DesktopContent {
                        tier: DesktopContentTier::Text,
                        outcome: "content.text".into(),
                        fallback_text: metadata.clone(),
                        value: DesktopContentValue::Text(metadata),
                        actions: Vec::new(),
                    }],
                }
            })
            .collect();
        assert!(view.finish(
            &session,
            generation,
            Ok(DesktopHistoryPage {
                session_id: session.clone(),
                messages,
                start: 72,
                end: 200,
                total: 200,
                older: None,
                newer: None,
            })
        ));
        let rows = view.messages().expect("canonical saved page is visible");
        assert_eq!(rows.len(), 128);
        assert!(rows[0].content().text().contains("artifact-72"));
        assert!(rows[127].content().text().contains("artifact-199"));
        assert!(rows.iter().all(
            |row| row.message_actions() == gpui_ai::prelude::MessageActions::none().copy(true)
        ));
        // At a saved-page edge only the canonical cursor governs paging;
        // the live omission flag must not loop back to another tail.
        assert!(view.older(true).is_none());
        view.live();
        assert_eq!(
            view.older(true),
            Some(DesktopHistoryRequest::Latest { before: None })
        );
    }

    #[test]
    fn stale_history_completion_cannot_replace_live_or_another_session() {
        let mut view = HistoryView::new("first".into(), 10, 20);
        let (session, generation) = view.begin();
        view.live();
        assert!(!view.finish(&session, generation, Ok(page("first", 0, 10))));
        let (session, generation) = view.begin();
        view.reset("second".into(), 20, 30);
        assert!(!view.finish(&session, generation, Err("late error".into())));
        let (session, generation) = view.begin();
        assert!(view.finish(&session, generation, Ok(page("first", 0, 10))));
        assert!(view.saved().is_none());
        assert!(view.label().contains("another Conversation"));
    }

    #[test]
    fn superseded_page_and_clear_reject_late_success_without_erasing_a_loaded_page_on_error() {
        let mut view = HistoryView::new("session".into(), 400, 500);
        let (session, old) = view.begin();
        let (_, current) = view.begin();
        assert!(!view.finish(&session, old, Ok(page("session", 0, 100))));
        assert!(view.loading());
        assert!(view.finish(&session, current, Ok(page("session", 300, 400))));
        let (_, failed) = view.begin();
        assert!(view.finish(&session, failed, Err("history changed".into())));
        assert_eq!(
            view.saved()
                .expect("prior page remains readable after an error")
                .page
                .start,
            300
        );
        let (_, cleared) = view.begin();
        view.clear(0, 0);
        assert!(!view.finish(&session, cleared, Ok(page("session", 0, 100))));
        assert!(!view.is_browsing());
        assert!(view.older(false).is_none());
    }
}
