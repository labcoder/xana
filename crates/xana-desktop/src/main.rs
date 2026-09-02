//! Native process composition for Xana Desktop.

mod projection;
mod workbench;

use gpui::{App, AppContext as _, Styled as _, WindowOptions};
use gpui_component::{ActiveTheme as _, Root};
use std::process::ExitCode;
use workbench::Workbench;
use xana::desktop::{DesktopClient, DesktopLaunch};

const APPLICATION_ID: &str = "com.labcoder.xana";
const APPLICATION_NAME: &str = "Xana";

fn main() -> ExitCode {
    let runtime = match DesktopLaunch::from_process().and_then(DesktopClient::launch) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Xana Desktop could not start: {error}");
            return ExitCode::FAILURE;
        }
    };
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
                    let workbench = cx.new(|cx| Workbench::new(runtime, window, cx));
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
    ExitCode::SUCCESS
}
