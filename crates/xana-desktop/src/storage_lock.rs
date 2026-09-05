//! Content-free privacy screen. Replacing the root drops Conversation caches
//! before the background lock can claim success; no recovery secret enters GPUI.

use gpui::{Context, Render, Task, Window, prelude::*};
use gpui_component::{ActiveTheme as _, button::Button, v_flex};
use xana::desktop::DesktopControlPlane;

pub(crate) struct StorageLock {
    state: String,
    _task: Task<()>,
}

impl StorageLock {
    pub(crate) fn new(control: DesktopControlPlane, cx: &mut Context<Self>) -> Self {
        let task = cx.spawn(async move |this, cx| {
            // Entity release is deferred by GPUI. Retry for a bounded interval
            // while the replaced root releases its last reader, never forever.
            let result = cx.background_executor().spawn(async move {
                let mut result = control.lock_storage();
                for _ in 0..10 {
                    if result.is_ok() { break; }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    result = control.lock_storage();
                }
                result
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(()) => "Storage locked. Quit Xana, then explicitly unlock before reopening.".into(),
                    Err(error) => format!("Storage was not locked: {}. Close other Xana clients, then run xana storage lock.", error.message),
                };
                cx.notify();
            });
        });
        Self {
            state: "Conversation views closed. Locking storage…".into(),
            _task: task,
        }
    }
}

impl Render for StorageLock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child("Protected storage")
            .child(self.state.clone())
            .child(
                Button::new("storage-lock-quit")
                    .label("Quit Xana")
                    .on_click(|_, _, cx| cx.quit()),
            )
    }
}
