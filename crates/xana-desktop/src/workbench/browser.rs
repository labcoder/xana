//! Same-owner browser controls; dispatch never blocks the UI or starts a model turn.
use super::*;
use xana::desktop::{
    DesktopAuthority, DesktopBrowserControl, DesktopBrowserResolution, DesktopBrowserReview,
    DesktopFactSource,
};

impl Workbench {
    pub(super) fn render_browser_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let enabled = browser_controls_enabled(
            self.projection.authority(),
            self.projection.execution_owner(),
        );
        v_flex()
            .gap_2()
            .child(div().text_sm().child("Disposable browser"))
            .child(h_flex().gap_2().flex_wrap().children([
                ("browser-status", "Status", DesktopBrowserControl::Status),
                ("browser-takeover", "Take over", DesktopBrowserControl::Takeover),
                ("browser-close", "Close browser", DesktopBrowserControl::Close),
            ].into_iter().map(|(id, label, action)| {
                Button::new(id).label(label).disabled(!enabled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        match this.runtime.browser_control(action) {
                            Ok(_) => this.projection.set_activity("Requesting browser control…"),
                            Err(error) => this.projection.fail(error.message),
                        }
                        this.sync_components(window, cx);
                    }))
            })))
            .child(Button::new("browser-review-outcome").label("Review uncertain outcome")
                .disabled(!enabled).on_click(cx.listener(|this, _, window, cx| {
                    this.review_browser_outcome(window, cx);
                })))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                if enabled { "Controls the current Xana-owned browser. Resume requires a new reviewed browser action." }
                else { "Browser control requires a native Xana conversation and controller authority." }
            ))
            .into_any_element()
    }

    fn review_browser_outcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let review = self
            .projection
            .conversation_facts()
            .activity
            .iter()
            .find(|item| item.id == "browser:status" && item.source == DesktopFactSource::Runtime)
            .and_then(|item| item.disclosed_text.as_deref())
            .and_then(DesktopBrowserReview::from_status_detail);
        let Some(review) = review else {
            self.projection.set_activity("Request browser Status first. If no pending review appears, there is no uncertain effect to resolve.");
            self.sync_components(window, cx);
            return;
        };
        let confirmation = window.prompt(
            PromptLevel::Info,
            "What happened to this exact browser action?",
            Some(&format!(
                "{}\n\nInspect the recipient and evidence before choosing. Record Applied only if you verified the effect occurred, or Not applied only if you verified it did not. If still uncertain, keep it unresolved. This records your finding; it never retries the action.",
                review.summary
            )),
            &["Keep unresolved", "Verified applied", "Verified not applied"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match confirmation.await {
                Ok(1) => DesktopBrowserResolution::Applied,
                Ok(2) => DesktopBrowserResolution::NotApplied,
                _ => return,
            };
            _ = this.update_in(cx, |this, window, cx| {
                if !browser_controls_enabled(
                    this.projection.authority(),
                    this.projection.execution_owner(),
                ) {
                    this.projection
                        .fail("Browser controller authority changed; outcome was not recorded.");
                } else {
                    match this.runtime.browser_control(review.resolve(outcome)) {
                        Ok(_) => this
                            .projection
                            .set_activity("Recording the exact reviewed browser outcome…"),
                        Err(error) => this.projection.fail(error.message),
                    }
                }
                this.sync_components(window, cx);
            });
        })
        .detach();
    }
}

fn browser_controls_enabled(authority: DesktopAuthority, owner: &str) -> bool {
    matches!(
        authority,
        DesktopAuthority::Owner | DesktopAuthority::Controller
    ) && owner == "native"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_buttons_require_native_controller_authority() {
        assert!(browser_controls_enabled(DesktopAuthority::Owner, "native"));
        assert!(browser_controls_enabled(
            DesktopAuthority::Controller,
            "native"
        ));
        assert!(!browser_controls_enabled(
            DesktopAuthority::Observer,
            "native"
        ));
        assert!(!browser_controls_enabled(
            DesktopAuthority::Owner,
            "managed_codex"
        ));
        assert!(!browser_controls_enabled(DesktopAuthority::Owner, ""));
    }
}
