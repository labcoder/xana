//! Native process composition for Xana Desktop.

mod accounting_view;
mod catalog;
mod commands;
mod component_inventory;
mod composer;
mod connection_actions;
mod connection_manager;
mod design_system;
mod espejo;
mod localization;
mod maintenance_view;
mod management_view;
mod model_filter;
mod permission_view;
mod projection;
mod resource_policy_view;
mod settings_view;
mod setup_view;
mod shell;
mod storage_lock;
mod workbench;
mod workbench_preferences_view;

use catalog::ComponentCatalog;
use gpui::{App, AppContext as _, Styled as _, WindowOptions};
use gpui_component::{ActiveTheme as _, Root};
use shell::{DesktopShell, DesktopShellLaunch};
use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};
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
                                    workbench.control,
                                    workbench.instance,
                                    workbench.native_paths,
                                    workbench.initial_intent,
                                    window,
                                    cx,
                                )
                            });
                            cx.new(|cx| Root::new(workbench, window, cx).bg(cx.theme().background))
                        }
                        LaunchSurface::Launcher(launcher) => {
                            window.set_window_title(APPLICATION_NAME);
                            let shell = cx.new(|cx| DesktopShell::new(*launcher, window, cx));
                            cx.new(|cx| Root::new(shell, window, cx).bg(cx.theme().background))
                        }
                    },
                )
                .expect("could not open the Xana Desktop window");

            let notification_window = window;
            cx.on_system_notification_response(move |response, cx| {
                _ = notification_window.update(cx, |_, window, cx| {
                    window.activate_window();
                    match response.tag.as_ref() {
                        "xana-desktop-activity" => {
                            window.dispatch_action(Box::new(commands::ShowActivity), cx);
                        }
                        "xana-desktop-diagnostics" => {
                            window.dispatch_action(Box::new(commands::ShowEspejo), cx);
                        }
                        _ => {}
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
    Launcher(Box<DesktopShellLaunch>),
    Workbench(Box<WorkbenchLaunch>),
}

struct WorkbenchLaunch {
    runtime: DesktopClient,
    control: xana::desktop::DesktopControlPlane,
    instance: DesktopInstanceLease,
    native_paths: DesktopNativePaths,
    initial_intent: DesktopLaunchIntent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchRequest {
    Catalog,
    Launcher,
    Workbench {
        workspace: Option<PathBuf>,
        intent: DesktopLaunchIntent,
    },
}

fn prepare_surface(
    request: LaunchRequest,
) -> Result<Option<LaunchSurface>, xana::desktop::DesktopError> {
    if request == LaunchRequest::Catalog {
        return Ok(Some(LaunchSurface::Catalog));
    }
    let (launch, initial_intent, cold) = match request {
        LaunchRequest::Catalog => unreachable!("catalog handled above"),
        LaunchRequest::Launcher => (
            DesktopLaunch::for_launcher_from_process(),
            DesktopLaunchIntent::Focus,
            true,
        ),
        LaunchRequest::Workbench { workspace, intent } => (
            workspace.map_or_else(DesktopLaunch::from_process, |workspace| {
                Ok(DesktopLaunch::new(workspace, env::var_os("XANA_HOME")))
            })?,
            intent,
            false,
        ),
    };
    let native_paths = launch.native_paths()?;
    let instance = match launch.claim_instance(initial_intent)? {
        DesktopInstanceClaim::Primary(instance) => instance,
        DesktopInstanceClaim::Forwarded => return Ok(None),
    };
    if cold {
        let catalog = launch.launch_catalog()?;
        let setup_snapshot = launch.control_plane()?.setup_snapshot()?;
        return Ok(Some(LaunchSurface::Launcher(Box::new(
            DesktopShellLaunch {
                launch,
                instance,
                native_paths,
                catalog,
                setup_snapshot,
                initial_intent,
            },
        ))));
    }
    let control = launch.control_plane()?;
    let runtime = DesktopClient::launch(launch)?;
    Ok(Some(LaunchSurface::Workbench(Box::new(WorkbenchLaunch {
        runtime,
        control,
        instance,
        native_paths,
        initial_intent,
    }))))
}

fn parse_launch_request(args: impl IntoIterator<Item = OsString>) -> Result<LaunchRequest, String> {
    let mut args = args.into_iter().peekable();
    if args.peek().is_none() {
        return Ok(LaunchRequest::Launcher);
    }
    let mut workspace = None;
    let mut intent = DesktopLaunchIntent::Focus;
    let mut has_workbench_option = false;
    while let Some(argument) = args.next() {
        let display = argument.to_string_lossy();
        if display == "--catalog" {
            if has_workbench_option || args.peek().is_some() {
                return Err("--catalog cannot be combined with other launch options".to_owned());
            }
            return Ok(LaunchRequest::Catalog);
        }
        if display == "--workspace" {
            let path = args
                .next()
                .ok_or_else(|| "--workspace requires a directory path".to_owned())?;
            workspace = Some(PathBuf::from(path));
            has_workbench_option = true;
            continue;
        }
        if let Some(path) = display.strip_prefix("--workspace=") {
            if path.is_empty() {
                return Err("--workspace requires a non-empty directory path".to_owned());
            }
            workspace = Some(PathBuf::from(path));
            has_workbench_option = true;
            continue;
        }
        let target = if display == "--open" {
            args.next()
                .ok_or_else(|| {
                    "--open requires one of: conversation, activity, diagnostics, settings, espejo"
                        .to_owned()
                })?
                .into_string()
                .map_err(|_| "the --open destination must be valid Unicode".to_owned())?
        } else if let Some(target) = display.strip_prefix("--open=") {
            target.to_owned()
        } else {
            return Err(format!(
                "unknown launch argument {display:?}; use --catalog, --workspace PATH, or --open DESTINATION"
            ));
        };
        intent = DesktopLaunchIntent::Navigate(parse_navigation(&target)?);
        has_workbench_option = true;
    }
    Ok(LaunchRequest::Workbench { workspace, intent })
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
    use std::path::PathBuf;
    use xana::desktop::{DesktopLaunchIntent, DesktopNavigationTarget};

    #[test]
    fn launch_arguments_are_closed_and_typed() {
        assert_eq!(
            parse_launch_request(std::iter::empty()).expect("default launch"),
            LaunchRequest::Launcher
        );
        assert_eq!(
            parse_launch_request([OsString::from("--catalog")]).expect("catalog launch"),
            LaunchRequest::Catalog
        );
        assert_eq!(
            parse_launch_request([OsString::from("--open"), OsString::from("diagnostics")])
                .expect("diagnostics launch"),
            LaunchRequest::Workbench {
                workspace: None,
                intent: DesktopLaunchIntent::Navigate(DesktopNavigationTarget::Diagnostics),
            }
        );
        assert_eq!(
            parse_launch_request([OsString::from("--open=settings")]).expect("settings launch"),
            LaunchRequest::Workbench {
                workspace: None,
                intent: DesktopLaunchIntent::Navigate(DesktopNavigationTarget::Settings),
            }
        );
        assert_eq!(
            parse_launch_request([OsString::from("--workspace"), OsString::from("."),])
                .expect("workspace launch"),
            LaunchRequest::Workbench {
                workspace: Some(PathBuf::from(".")),
                intent: DesktopLaunchIntent::Focus,
            }
        );
        assert!(parse_launch_request([OsString::from("--open=../secrets")]).is_err());
        assert!(parse_launch_request([OsString::from("--unknown")]).is_err());
    }
}
