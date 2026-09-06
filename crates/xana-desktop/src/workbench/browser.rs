//! Same-owner browser controls; dispatch never blocks the UI or starts a model turn.
use super::*;
use xana::desktop::{DesktopAuthority, DesktopBrowserControl};

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
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                if enabled { "Controls the current Xana-owned browser. Resume requires a new reviewed browser action." }
                else { "Browser control requires a native Xana conversation and controller authority." }
            ))
            .into_any_element()
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
