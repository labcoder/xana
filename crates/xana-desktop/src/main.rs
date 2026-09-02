//! Native process composition for Xana Desktop.

mod workbench;

use gpui::{App, AppContext as _, Styled as _, WindowOptions};
use gpui_component::{ActiveTheme as _, Root};
use workbench::Workbench;

const APPLICATION_ID: &str = "com.labcoder.xana";
const APPLICATION_NAME: &str = "Xana";

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(|cx: &mut App| {
            cx.set_app_identity(APPLICATION_ID, APPLICATION_NAME);
            gpui_ai::init(cx);

            cx.open_window(
                WindowOptions {
                    app_id: Some(APPLICATION_ID.to_owned()),
                    ..WindowOptions::default()
                },
                move |window, cx| {
                    window.set_window_title(APPLICATION_NAME);
                    let workbench = cx.new(|cx| Workbench::new(window, cx));
                    cx.new(|cx| Root::new(workbench, window, cx).bg(cx.theme().background))
                },
            )
            .expect("could not open the Xana Desktop window");

            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
