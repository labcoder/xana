//! Passive bounded supervision. No model work, controller claims, or frame loop.
use super::*;
use xana::desktop::{DesktopAutonomyObserver, NotificationPolicy};

fn notification_context_unchanged(
    observed: &NotificationPolicy,
    current: &NotificationPolicy,
    focused: bool,
) -> bool {
    !focused && observed == current
}

impl Workbench {
    pub(super) fn observe_background_work(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let control = self.control.clone();
        self.background_driver = Some(cx.spawn_in(window, async move |this, cx| {
            let mut observer = DesktopAutonomyObserver::default();
            loop {
                let Ok((policy, focus)) = this.update_in(cx, |this, window, _| {
                    let focus = if window.is_window_active() {
                        ClientFocus::Focused
                    } else {
                        ClientFocus::Unfocused
                    };
                    (this.projection.notification_policy().clone(), focus)
                }) else {
                    break;
                };
                let observed_policy = policy.clone();
                let owner = control.clone();
                let (next, result) = cx
                    .background_executor()
                    .spawn(async move {
                        let result = observer.poll(&owner, &policy, focus);
                        (observer, result)
                    })
                    .await;
                observer = next;
                let failed = result.is_err();
                if let Ok(update) = result
                    && this
                        .update_in(cx, |this, window, cx| {
                            if !update.attention.is_empty() {
                                this.espejo.update(cx, |view, cx| {
                                    view.background_attention(update.attention, cx)
                                });
                            }
                            // Focus or settings may change during the store read.
                            // Keep in-app attention, not alerts from stale preferences.
                            let show = notification_context_unchanged(
                                &observed_policy,
                                this.projection.notification_policy(),
                                window.is_window_active(),
                            );
                            for notice in update.notifications.into_iter().filter(|_| show) {
                                let destination = notification_destination(notice.destination);
                                cx.show_system_notification(SystemNotification {
                                    tag: format!("xana-desktop-{destination}").into(),
                                    title: notice.title.into(),
                                    body: notice.body.into(),
                                    actions: Vec::new(),
                                });
                            }
                        })
                        .is_err()
                {
                    break;
                }
                // Unconfigured/locked homes stay quiet and cheap. No store/key
                // handle is held by this timer, and unchanged polls don't redraw.
                cx.background_executor()
                    .timer(Duration::from_secs(if failed { 30 } else { 2 }))
                    .await;
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_delivery_rechecks_focus_and_changed_preferences() {
        let observed = NotificationPolicy::default();
        assert!(notification_context_unchanged(&observed, &observed, false));
        assert!(!notification_context_unchanged(&observed, &observed, true));
        let mut current = observed.clone();
        current.enabled = false;
        assert!(!notification_context_unchanged(&observed, &current, false));
        current.enabled = true;
        current.completions = false;
        assert!(!notification_context_unchanged(&observed, &current, false));
    }
}
