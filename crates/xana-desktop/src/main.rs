//! Native process composition for Xana Desktop.

mod catalog;
mod component_inventory;
mod design_system;
mod localization;
mod projection;
mod workbench;

use catalog::ComponentCatalog;
use gpui::{App, AppContext as _, Styled as _, WindowOptions};
use gpui_component::{ActiveTheme as _, Root};
use std::{env, ffi::OsStr, process::ExitCode};
use workbench::Workbench;
use xana::desktop::{DesktopClient, DesktopLaunch};

const APPLICATION_ID: &str = "com.labcoder.xana";
const APPLICATION_NAME: &str = "Xana";

fn main() -> ExitCode {
    let surface = if catalog_requested(env::args_os().skip(1)) {
        LaunchSurface::Catalog
    } else {
        match DesktopLaunch::from_process().and_then(DesktopClient::launch) {
            Ok(runtime) => LaunchSurface::Workbench(Box::new(runtime)),
            Err(error) => {
                eprintln!("Xana Desktop could not start: {error}");
                return ExitCode::FAILURE;
            }
        }
    };
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(|cx: &mut App| {
            cx.set_app_identity(APPLICATION_ID, APPLICATION_NAME);
            gpui_ai::init(cx);
            design_system::install(cx);

            cx.open_window(
                WindowOptions {
                    app_id: Some(APPLICATION_ID.to_owned()),
                    ..WindowOptions::default()
                },
                move |window, cx| match surface {
                    LaunchSurface::Catalog => {
                        window.set_window_title("Xana Component Catalog");
                        let catalog = cx.new(|cx| ComponentCatalog::new(window, cx));
                        cx.new(|cx| Root::new(catalog, window, cx).bg(cx.theme().background))
                    }
                    LaunchSurface::Workbench(runtime) => {
                        window.set_window_title(APPLICATION_NAME);
                        let workbench = cx.new(|cx| Workbench::new(*runtime, window, cx));
                        cx.new(|cx| Root::new(workbench, window, cx).bg(cx.theme().background))
                    }
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

enum LaunchSurface {
    Catalog,
    Workbench(Box<DesktopClient>),
}

fn catalog_requested(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> bool {
    args.into_iter()
        .any(|argument| argument.as_ref() == OsStr::new("--catalog"))
}

#[cfg(test)]
mod tests {
    use super::catalog_requested;

    #[test]
    fn catalog_mode_is_explicit_and_order_independent() {
        assert!(catalog_requested(["--catalog"]));
        assert!(catalog_requested(["--trace", "--catalog"]));
        assert!(!catalog_requested(["catalog"]));
        assert!(!catalog_requested(std::iter::empty::<&str>()));
    }
}
