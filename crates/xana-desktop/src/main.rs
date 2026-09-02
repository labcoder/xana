//! Native process composition for Xana Desktop.

mod catalog;
mod commands;
mod component_inventory;
mod design_system;
mod localization;
mod projection;
mod workbench;

use catalog::ComponentCatalog;
use gpui::{App, AppContext as _, Styled as _, WindowOptions};
use gpui_component::{ActiveTheme as _, Root};
use std::{env, ffi::OsString, process::ExitCode};
use workbench::Workbench;
use xana::desktop::{
    DesktopClient, DesktopInstanceClaim, DesktopInstanceLease, DesktopLaunch, DesktopLaunchIntent,
    DesktopNativePaths, DesktopNavigationTarget,
};

const APPLICATION_ID: &str = "com.labcoder.xana";
const APPLICATION_NAME: &str = "Xana";

fn main() -> ExitCode {
    let request = match parse_launch_request(env::args_os().skip(1)) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("Xana Desktop could not start: {error}");
            return ExitCode::FAILURE;
        }
    };
    let surface = match prepare_surface(request) {
        Ok(Some(surface)) => surface,
        Ok(None) => return ExitCode::SUCCESS,
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
            design_system::install(cx);
            commands::install(cx);

            let window = cx
                .open_window(
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
                        LaunchSurface::Workbench(workbench) => {
                            window.set_window_title(APPLICATION_NAME);
                            let workbench = cx.new(|cx| {
                                Workbench::new(
                                    workbench.runtime,
                                    workbench.instance,
                                    workbench.native_paths,
                                    workbench.initial_intent,
                                    window,
                                    cx,
                                )
                            });
                            cx.new(|cx| Root::new(workbench, window, cx).bg(cx.theme().background))
                        }
                    },
                )
                .expect("could not open the Xana Desktop window");

            let notification_window = window;
            cx.on_system_notification_response(move |response, cx| {
                _ = notification_window.update(cx, |_, window, cx| {
                    window.activate_window();
                    if response.tag.as_ref() != "xana-desktop-conversation" {
                        window.dispatch_action(Box::new(commands::ShowActivity), cx);
                    }
                });
            });

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
    Workbench(Box<WorkbenchLaunch>),
}

struct WorkbenchLaunch {
    runtime: DesktopClient,
    instance: DesktopInstanceLease,
    native_paths: DesktopNativePaths,
    initial_intent: DesktopLaunchIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchRequest {
    Catalog,
    Workbench(DesktopLaunchIntent),
}

fn prepare_surface(
    request: LaunchRequest,
) -> Result<Option<LaunchSurface>, xana::desktop::DesktopError> {
    let LaunchRequest::Workbench(initial_intent) = request else {
        return Ok(Some(LaunchSurface::Catalog));
    };
    let launch = DesktopLaunch::from_process()?;
    let native_paths = launch.native_paths()?;
    let instance = match launch.claim_instance(initial_intent)? {
        DesktopInstanceClaim::Primary(instance) => instance,
        DesktopInstanceClaim::Forwarded => return Ok(None),
    };
    let runtime = DesktopClient::launch(launch)?;
    Ok(Some(LaunchSurface::Workbench(Box::new(WorkbenchLaunch {
        runtime,
        instance,
        native_paths,
        initial_intent,
    }))))
}

fn parse_launch_request(args: impl IntoIterator<Item = OsString>) -> Result<LaunchRequest, String> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(LaunchRequest::Workbench(DesktopLaunchIntent::Focus));
    };
    let first = first
        .into_string()
        .map_err(|_| "launch arguments must be valid Unicode".to_owned())?;
    let request = if first == "--catalog" {
        LaunchRequest::Catalog
    } else if let Some(target) = first.strip_prefix("--open=") {
        LaunchRequest::Workbench(DesktopLaunchIntent::Navigate(parse_navigation(target)?))
    } else if first == "--open" {
        let target = args
            .next()
            .ok_or_else(|| {
                "--open requires one of: conversation, activity, diagnostics, settings, espejo"
                    .to_owned()
            })?
            .into_string()
            .map_err(|_| "the --open destination must be valid Unicode".to_owned())?;
        LaunchRequest::Workbench(DesktopLaunchIntent::Navigate(parse_navigation(&target)?))
    } else {
        return Err(format!(
            "unknown launch argument {first:?}; use --catalog or --open DESTINATION"
        ));
    };
    if let Some(extra) = args.next() {
        return Err(format!("unexpected extra launch argument {extra:?}"));
    }
    Ok(request)
}

fn parse_navigation(value: &str) -> Result<DesktopNavigationTarget, String> {
    DesktopNavigationTarget::parse(value).ok_or_else(|| {
        format!(
            "unknown Desktop destination {value:?}; expected conversation, activity, diagnostics, settings, or espejo"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{LaunchRequest, parse_launch_request};
    use std::ffi::OsString;
    use xana::desktop::{DesktopLaunchIntent, DesktopNavigationTarget};

    #[test]
    fn launch_arguments_are_closed_and_typed() {
        assert_eq!(
            parse_launch_request(std::iter::empty()).expect("default launch"),
            LaunchRequest::Workbench(DesktopLaunchIntent::Focus)
        );
        assert_eq!(
            parse_launch_request([OsString::from("--catalog")]).expect("catalog launch"),
            LaunchRequest::Catalog
        );
        assert_eq!(
            parse_launch_request([OsString::from("--open"), OsString::from("diagnostics")])
                .expect("diagnostics launch"),
            LaunchRequest::Workbench(DesktopLaunchIntent::Navigate(
                DesktopNavigationTarget::Diagnostics
            ))
        );
        assert_eq!(
            parse_launch_request([OsString::from("--open=settings")]).expect("settings launch"),
            LaunchRequest::Workbench(DesktopLaunchIntent::Navigate(
                DesktopNavigationTarget::Settings
            ))
        );
        assert!(parse_launch_request([OsString::from("--open=../secrets")]).is_err());
        assert!(parse_launch_request([OsString::from("--unknown")]).is_err());
    }
}
