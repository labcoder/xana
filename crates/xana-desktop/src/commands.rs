//! Native command projection for Xana Desktop.
//!
//! The shared command catalog owns semantic identity. This module owns only
//! Desktop labels, shortcuts, and the small set of handlers implemented by the
//! current workbench slice.

use crate::settings_view::SettingsRoute;

#[cfg(target_os = "macos")]
use gpui::SystemMenuType;
use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, actions};
use gpui_ai::prelude::CommandSearchItem;
use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use xana::desktop::{
    DesktopAuthority, DesktopCommandDescriptor, DesktopSettingsSection, desktop_commands,
};

pub(crate) const WORKBENCH_KEY_CONTEXT: &str = "XanaWorkbench";

pub(crate) const COMMAND_PALETTE_ID: &str = "application.command_palette.v1";
pub(crate) const QUIT_ID: &str = "application.quit.v1";
pub(crate) const LOCK_ID: &str = "storage.lock_and_close.v1";
pub(crate) const MINIMIZE_ID: &str = "application.window.minimize.v1";
pub(crate) const DOCUMENTATION_ID: &str = "help.documentation.open.v1";
pub(crate) const CONFIGURATION_FILE_ID: &str = "configuration.file.open.v1";
pub(crate) const LOGS_ID: &str = "diagnostics.logs.v1";
pub(crate) const CLEAR_ID: &str = "conversation.clear.v1";
pub(crate) const INTERRUPT_ID: &str = "run.interrupt.v1";
pub(crate) const ACTIVITY_ID: &str = "presentation.activity.show.v1";
pub(crate) const ESPEJO_ID: &str = "espejo.open.v1";
pub(crate) const SETTINGS_ID: &str = "settings.open.v1";
pub(crate) const FRAME_PERFORMANCE_TOGGLE_ID: &str = "debug.frame_performance.toggle.v1";
pub(crate) const FRAME_PERFORMANCE_RESET_ID: &str = "debug.frame_performance.reset.v1";
pub(crate) const FRAME_PERFORMANCE_COPY_ID: &str = "debug.frame_performance.copy.v1";

actions!(
    xana_desktop,
    [
        ShowCommandPalette,
        QuitXana,
        LockStorage,
        MinimizeWindow,
        OpenDocumentation,
        OpenConfigurationFile,
        RevealLogs,
        ClearConversation,
        InterruptRun,
        ShowActivity,
        ShowEspejo,
        ShowSettings,
        ToggleFramePerformance,
        ResetFramePerformance,
        CopyFramePerformance,
        NewConversation,
        RenameSelectedProject,
        ArchiveSelectedProject,
        RestoreSelectedProject,
        ArchiveSelectedConversation,
        MoveSelectedConversation,
        UngroupSelectedConversation,
        BranchSelectedConversation,
        OlderHistory,
        NewerHistory,
        LiveHistory,
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkbenchCommand {
    ShowCommandPalette,
    Quit,
    LockStorage,
    Minimize,
    OpenDocumentation,
    OpenConfigurationFile,
    RevealLogs,
    ClearConversation,
    InterruptRun,
    ShowActivity,
    ShowEspejo,
    ShowSettings,
    ToggleFramePerformance,
    ResetFramePerformance,
    CopyFramePerformance,
    NewConversation,
}

impl WorkbenchCommand {
    pub(crate) const fn stable_id(self) -> &'static str {
        match self {
            Self::ShowCommandPalette => COMMAND_PALETTE_ID,
            Self::Quit => QUIT_ID,
            Self::LockStorage => LOCK_ID,
            Self::Minimize => MINIMIZE_ID,
            Self::OpenDocumentation => DOCUMENTATION_ID,
            Self::OpenConfigurationFile => CONFIGURATION_FILE_ID,
            Self::RevealLogs => LOGS_ID,
            Self::ClearConversation => CLEAR_ID,
            Self::InterruptRun => INTERRUPT_ID,
            Self::ShowActivity => ACTIVITY_ID,
            Self::ShowEspejo => ESPEJO_ID,
            Self::ShowSettings => SETTINGS_ID,
            Self::ToggleFramePerformance => FRAME_PERFORMANCE_TOGGLE_ID,
            Self::ResetFramePerformance => FRAME_PERFORMANCE_RESET_ID,
            Self::CopyFramePerformance => FRAME_PERFORMANCE_COPY_ID,
            Self::NewConversation => "conversation.new.v1",
        }
    }

    pub(crate) fn from_stable_id(id: &str) -> Option<Self> {
        Some(match id {
            COMMAND_PALETTE_ID => Self::ShowCommandPalette,
            QUIT_ID => Self::Quit,
            LOCK_ID => Self::LockStorage,
            MINIMIZE_ID => Self::Minimize,
            DOCUMENTATION_ID => Self::OpenDocumentation,
            CONFIGURATION_FILE_ID => Self::OpenConfigurationFile,
            LOGS_ID => Self::RevealLogs,
            CLEAR_ID => Self::ClearConversation,
            INTERRUPT_ID => Self::InterruptRun,
            ACTIVITY_ID => Self::ShowActivity,
            ESPEJO_ID => Self::ShowEspejo,
            SETTINGS_ID => Self::ShowSettings,
            FRAME_PERFORMANCE_TOGGLE_ID => Self::ToggleFramePerformance,
            FRAME_PERFORMANCE_RESET_ID => Self::ResetFramePerformance,
            FRAME_PERFORMANCE_COPY_ID => Self::CopyFramePerformance,
            "conversation.new.v1" => Self::NewConversation,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteDestination {
    Conversation,
    Activity,
    Usage,
    Memory,
    Schedules,
    Settings(SettingsRoute),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteSelection {
    Dispatch(WorkbenchCommand),
    Navigate(PaletteDestination),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandExposure {
    Select(PaletteSelection),
    Contextual(&'static str),
}

pub(crate) fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-alt-up", OlderHistory, Some(WORKBENCH_KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-down", NewerHistory, Some(WORKBENCH_KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-end", LiveHistory, Some(WORKBENCH_KEY_CONTEXT)),
        KeyBinding::new(
            "cmd-shift-p",
            ShowCommandPalette,
            Some(WORKBENCH_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "cmd-shift-k",
            ClearConversation,
            Some(WORKBENCH_KEY_CONTEXT),
        ),
        KeyBinding::new("cmd-.", InterruptRun, Some(WORKBENCH_KEY_CONTEXT)),
        KeyBinding::new("cmd-m", MinimizeWindow, Some(WORKBENCH_KEY_CONTEXT)),
        KeyBinding::new("cmd-,", ShowSettings, Some(WORKBENCH_KEY_CONTEXT)),
        KeyBinding::new("cmd-q", QuitXana, Some(WORKBENCH_KEY_CONTEXT)),
    ]);
    cx.set_menus(native_menus());
}

fn native_menus() -> Vec<Menu> {
    let mut application_items = vec![
        MenuItem::action("Open Configuration", OpenConfigurationFile),
        MenuItem::separator(),
    ];
    #[cfg(target_os = "macos")]
    application_items.push(MenuItem::os_submenu("Services", SystemMenuType::Services));
    #[cfg(target_os = "macos")]
    application_items.push(MenuItem::separator());
    application_items.push(MenuItem::action("Quit Xana", QuitXana));
    application_items.push(MenuItem::action("Stop Work and Lock Storage", LockStorage));

    vec![
        Menu::new("Xana").items(application_items),
        Menu::new("File").items([
            MenuItem::action("New Conversation", NewConversation),
            MenuItem::separator(),
            MenuItem::action("Quit Xana", QuitXana),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette…", ShowCommandPalette),
            MenuItem::action("Activity", ShowActivity),
            MenuItem::action("Espejo", ShowEspejo),
            MenuItem::action("Settings…", ShowSettings),
            MenuItem::separator(),
            MenuItem::action("Cycle Frame Performance HUD", ToggleFramePerformance),
            MenuItem::action("Copy Frame Performance Snapshot", CopyFramePerformance),
            MenuItem::action("Reset Frame Performance Statistics", ResetFramePerformance),
        ]),
        Menu::new("Conversation").items([
            MenuItem::action("New Conversation", NewConversation),
            MenuItem::action("Clear Conversation", ClearConversation),
            MenuItem::action("Interrupt Run", InterruptRun),
        ]),
        Menu::new("Window").items([MenuItem::action("Minimize", MinimizeWindow)]),
        Menu::new("Help").items([
            MenuItem::action("Xana Documentation", OpenDocumentation),
            MenuItem::action("Reveal Logs", RevealLogs),
        ]),
    ]
}

pub(crate) fn palette_items(
    authority: DesktopAuthority,
    attached_to_foreground_host: bool,
    run_active: bool,
) -> Vec<CommandSearchItem> {
    let mut items = desktop_commands(authority, true)
        .into_iter()
        .map(|descriptor| palette_item(descriptor, attached_to_foreground_host, run_active))
        .collect::<Vec<_>>();
    items.extend([
        CommandSearchItem::new(LOCK_ID, "Stop work and lock storage")
            .subtitle(
                "Close all Conversation views and stop work before locking this protected home",
            )
            .keywords(["privacy", "lock", "storage"]),
        CommandSearchItem::new(FRAME_PERFORMANCE_TOGGLE_ID, "debug frame performance")
            .subtitle("Cycle the GPUI frame-time HUD through hidden, current, and detailed modes")
            .keywords(["fps", "frame", "performance", "debug"]),
        CommandSearchItem::new(FRAME_PERFORMANCE_COPY_ID, "debug frame performance copy")
            .subtitle("Copy bounded draw, presentation, and effective FPS statistics")
            .keywords(["fps", "frame", "performance", "copy"]),
        CommandSearchItem::new(FRAME_PERFORMANCE_RESET_ID, "debug frame performance reset")
            .subtitle("Clear retained frame-performance samples before a new observation")
            .keywords(["fps", "frame", "performance", "reset"]),
    ]);
    items
}

fn palette_item(
    descriptor: DesktopCommandDescriptor,
    attached_to_foreground_host: bool,
    run_active: bool,
) -> CommandSearchItem {
    let exposure = command_exposure(descriptor.id);
    let runtime_disabled = descriptor.id == INTERRUPT_ID && !run_active;
    let attached_lifecycle_disabled =
        attached_to_foreground_host && descriptor.id == "conversation.new.v1";
    let unavailable_reason = descriptor
        .unavailable_reason
        .map(str::to_owned)
        .or_else(|| {
            exposure.and_then(|exposure| match exposure {
                CommandExposure::Select(_) => None,
                CommandExposure::Contextual(reason) => Some(reason.to_owned()),
            })
        })
        .or_else(|| {
            attached_lifecycle_disabled.then(|| {
                "The foreground host owns Conversation creation; return to its owning surface."
                    .to_owned()
            })
        })
        .or_else(|| {
            exposure
                .is_none()
                .then(|| "No Desktop exposure is registered.".to_owned())
        })
        .or_else(|| runtime_disabled.then(|| "No Run is active.".to_owned()));
    let title = command_title(&descriptor);
    let subtitle = unavailable_reason.map_or_else(
        || {
            command_exposure_note(descriptor.id).map_or_else(
                || descriptor.summary.to_owned(),
                |note| format!("{} — {note}", descriptor.summary),
            )
        },
        |reason| format!("{} — {reason}", descriptor.summary),
    );
    let keywords = descriptor
        .aliases
        .iter()
        .copied()
        .chain([descriptor.family, descriptor.mode, descriptor.id])
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let disabled = !descriptor.available
        || !matches!(exposure, Some(CommandExposure::Select(_)))
        || runtime_disabled
        || attached_lifecycle_disabled;
    let mut item = CommandSearchItem::new(descriptor.id, title)
        .subtitle(subtitle)
        .keywords(keywords)
        .disabled(disabled);
    if let Some(shortcut) = shortcut_for(descriptor.id) {
        item = item.shortcut(shortcut);
    }
    item
}

fn command_exposure_note(stable_id: &str) -> Option<&'static str> {
    match stable_id {
        "skill.manage.v1"
        | "plugin.manage.v1"
        | "mcp.manage.v1"
        | "external_agent.manage.v1"
        | "image.manage.v1"
        | "vision.manage.v1"
        | "outbound.manage.v1"
        | "operation.reconcile.v1" => Some(
            "Desktop shows the relevant state here; lifecycle changes remain in Xana's typed terminal management flow in this build.",
        ),
        _ => None,
    }
}

pub(crate) fn palette_selection(stable_id: &str) -> Option<PaletteSelection> {
    match command_exposure(stable_id) {
        Some(CommandExposure::Select(selection)) => Some(selection),
        Some(CommandExposure::Contextual(_)) | None => None,
    }
}

fn command_exposure(stable_id: &str) -> Option<CommandExposure> {
    use CommandExposure::{Contextual, Select};
    use PaletteDestination::{Activity, Conversation, Settings};
    use PaletteSelection::{Dispatch, Navigate};

    Some(match stable_id {
        COMMAND_PALETTE_ID => Select(Dispatch(WorkbenchCommand::ShowCommandPalette)),
        QUIT_ID => Select(Dispatch(WorkbenchCommand::Quit)),
        LOCK_ID => Select(Dispatch(WorkbenchCommand::LockStorage)),
        MINIMIZE_ID => Select(Dispatch(WorkbenchCommand::Minimize)),
        DOCUMENTATION_ID => Select(Dispatch(WorkbenchCommand::OpenDocumentation)),
        CONFIGURATION_FILE_ID => Select(Dispatch(WorkbenchCommand::OpenConfigurationFile)),
        LOGS_ID => Select(Dispatch(WorkbenchCommand::RevealLogs)),
        CLEAR_ID => Select(Dispatch(WorkbenchCommand::ClearConversation)),
        INTERRUPT_ID => Select(Dispatch(WorkbenchCommand::InterruptRun)),
        ACTIVITY_ID => Select(Dispatch(WorkbenchCommand::ShowActivity)),
        ESPEJO_ID => Select(Dispatch(WorkbenchCommand::ShowEspejo)),
        SETTINGS_ID => Select(Dispatch(WorkbenchCommand::ShowSettings)),
        FRAME_PERFORMANCE_TOGGLE_ID => Select(Dispatch(WorkbenchCommand::ToggleFramePerformance)),
        FRAME_PERFORMANCE_RESET_ID => Select(Dispatch(WorkbenchCommand::ResetFramePerformance)),
        FRAME_PERFORMANCE_COPY_ID => Select(Dispatch(WorkbenchCommand::CopyFramePerformance)),
        "conversation.new.v1" => Select(Dispatch(WorkbenchCommand::NewConversation)),

        "presentation.activity.auto.v1" | "presentation.activity.hide.v1" => {
            Select(Navigate(Activity))
        }
        "capability.report.v1" => Select(Navigate(Settings(SettingsRoute::Capabilities))),
        "diagnostics.doctor.v1" => Select(Navigate(Settings(SettingsRoute::Doctor))),
        "presentation.layout.manage.v1" => {
            Select(Navigate(Settings(SettingsRoute::WorkbenchPreferences)))
        }
        "model.select.v1" | "connection.manage.v1" | "integration.connect.v1" => {
            Select(Navigate(Settings(SettingsRoute::Connections)))
        }
        "profile.manage.v1" | "route.inspect.v1" => {
            Select(Navigate(Settings(SettingsRoute::Profiles)))
        }
        "project.manage.v1" => Select(Navigate(Settings(SettingsRoute::Projects))),
        "skill.manage.v1"
        | "plugin.manage.v1"
        | "mcp.manage.v1"
        | "external_agent.manage.v1"
        | "image.manage.v1" => Select(Navigate(Settings(SettingsRoute::Capabilities))),
        "vision.manage.v1" => Select(Navigate(Settings(SettingsRoute::Section(
            DesktopSettingsSection::AttachmentsMedia,
        )))),
        "setup.run.v1" => Select(Navigate(Settings(SettingsRoute::Section(
            DesktopSettingsSection::Overview,
        )))),
        "configuration.reset.v1" => Select(Navigate(Settings(SettingsRoute::Reset))),
        "configuration.inspect.v1" => Select(Navigate(Settings(SettingsRoute::Section(
            DesktopSettingsSection::Overview,
        )))),
        "outbound.manage.v1" => Select(Navigate(Settings(SettingsRoute::Permissions))),
        "operation.reconcile.v1" => Select(Navigate(Settings(SettingsRoute::Doctor))),

        "artifact.inspect.v1" => Contextual(
            "Use Inspect, Save, Reveal, or Open on the exact artifact card in Conversation.",
        ),
        "approval.decide.v1" => {
            Contextual("Use the exact pending approval card in Conversation or Activity.")
        }
        "turn.attachment.stage.v1" => {
            Contextual("Use Add resources or the attachment tray beside the composer.")
        }
        "conversation.compact.v1" => Contextual(
            "Compaction is runtime-directed; inspect the prompt ledger in Conversation details.",
        ),
        "child.list.v1" | "child.inspect.v1" | "child.cancel.v1" => Select(Navigate(Activity)),
        "run.continue.v1" => {
            Contextual("Use Continue on the exact suspended Run card in Conversation.")
        }
        "presentation.composer.newline.v1"
        | "presentation.composer.submit.v1"
        | "presentation.composer.insert_newline.v1" => Contextual(
            "Use the composer control or Settings → Appearance for submit/newline behavior.",
        ),
        "presentation.header.hide.v1" | "presentation.header.show.v1" => {
            Contextual("Desktop uses native window chrome rather than the terminal header panel.")
        }
        "help.contextual.v1" => Select(Navigate(Conversation)),
        "followup.list.v1" | "followup.edit.v1" | "followup.remove.v1" => {
            Contextual("Use the follow-up queue attached to the Conversation composer.")
        }
        "model.reasoning.select.v1" => {
            Contextual("Use the model and reasoning controls in the Conversation composer.")
        }
        "turn.submit.v1" => Contextual("Type in the Conversation composer and choose Send."),
        "conversation.list.v1"
        | "conversation.search.v1"
        | "conversation.continue.v1"
        | "conversation.preview.v1"
        | "conversation.attach.v1"
        | "presentation.conversation_list.hide.v1"
        | "presentation.conversation_list.show.v1" => Select(Navigate(Conversation)),
        "conversation.archive.v1" => {
            Contextual("Use the selected managed Conversation's context menu.")
        }
        "usage.inspect.v1" => Select(Navigate(Activity)),
        "budget.manage.v1" | "usage.ledger.v1" => Select(Navigate(PaletteDestination::Usage)),
        "memory.manage.v1" => Select(Navigate(PaletteDestination::Memory)),
        "autonomy.manage.v1" | "worker.manage.v1" => {
            Select(Navigate(PaletteDestination::Schedules))
        }
        "browser.control.v1" => Select(Navigate(Activity)),
        "run.steer.v1" => Contextual("Use Send now or Queue on the active Run's composer."),
        "run.stop.v1" | "run.resume.v1" => {
            Contextual("Use the exact active, interrupted, or suspended Run card.")
        }
        "application.shutdown.v1" => Select(Dispatch(WorkbenchCommand::Quit)),
        _ => return None,
    })
}

fn command_title(descriptor: &DesktopCommandDescriptor) -> String {
    let family = descriptor.family.replace('-', " ");
    if descriptor.mode.is_empty() {
        family
    } else {
        format!("{family} {}", descriptor.mode)
    }
}

pub(crate) fn shortcut_for(stable_id: &str) -> Option<&'static str> {
    match stable_id {
        COMMAND_PALETTE_ID => Some("⌘⇧P"),
        CLEAR_ID => Some("⌘⇧K"),
        INTERRUPT_ID => Some("⌘."),
        MINIMIZE_ID => Some("⌘M"),
        SETTINGS_ID => Some("⌘,"),
        QUIT_ID => Some("⌘Q"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_desktop_command_has_one_palette_row() {
        let descriptors = desktop_commands(DesktopAuthority::Owner, true);
        let items = palette_items(DesktopAuthority::Owner, false, false);
        assert_eq!(items.len(), descriptors.len() + 4);
        assert_eq!(
            items
                .iter()
                .map(|item| item.id().as_ref())
                .collect::<HashSet<_>>()
                .len(),
            items.len()
        );
    }

    #[test]
    fn dispatchable_commands_round_trip_through_stable_ids() {
        for command in [
            WorkbenchCommand::ShowCommandPalette,
            WorkbenchCommand::Quit,
            WorkbenchCommand::LockStorage,
            WorkbenchCommand::Minimize,
            WorkbenchCommand::OpenDocumentation,
            WorkbenchCommand::OpenConfigurationFile,
            WorkbenchCommand::RevealLogs,
            WorkbenchCommand::ClearConversation,
            WorkbenchCommand::InterruptRun,
            WorkbenchCommand::ShowActivity,
            WorkbenchCommand::ShowEspejo,
            WorkbenchCommand::ShowSettings,
            WorkbenchCommand::ToggleFramePerformance,
            WorkbenchCommand::ResetFramePerformance,
            WorkbenchCommand::CopyFramePerformance,
            WorkbenchCommand::NewConversation,
        ] {
            assert_eq!(
                WorkbenchCommand::from_stable_id(command.stable_id()),
                Some(command)
            );
        }
    }

    #[test]
    fn essential_shortcuts_are_unique_and_bounded() {
        let shortcuts = [
            COMMAND_PALETTE_ID,
            CLEAR_ID,
            INTERRUPT_ID,
            MINIMIZE_ID,
            SETTINGS_ID,
            QUIT_ID,
        ]
        .into_iter()
        .filter_map(shortcut_for)
        .collect::<Vec<_>>();
        assert!(shortcuts.len() <= 8);
        assert_eq!(
            shortcuts.iter().copied().collect::<HashSet<_>>().len(),
            shortcuts.len()
        );
    }

    #[test]
    fn every_desktop_command_has_an_explicit_exposure() {
        for descriptor in desktop_commands(DesktopAuthority::Owner, true) {
            assert!(
                command_exposure(descriptor.id).is_some(),
                "{} has no Desktop exposure",
                descriptor.id
            );
        }
    }

    #[test]
    fn storage_lock_is_an_explicit_stop_work_command() {
        assert!(matches!(
            palette_selection(LOCK_ID),
            Some(PaletteSelection::Dispatch(WorkbenchCommand::LockStorage))
        ));
        let item = palette_items(DesktopAuthority::Owner, true, true)
            .into_iter()
            .find(|item| item.id().as_ref() == LOCK_ID)
            .expect("lock remains reachable during work");
        assert!(!item.is_disabled());
    }

    #[test]
    fn contextual_commands_name_the_real_control_instead_of_future_work() {
        let items = palette_items(DesktopAuthority::Owner, false, false);
        let attachment = items
            .iter()
            .find(|item| item.id().as_ref() == "turn.attachment.stage.v1")
            .expect("attachment command");
        assert!(attachment.is_disabled());
        assert!(
            attachment
                .subtitle_text()
                .is_some_and(|subtitle| subtitle.contains("Add resources"))
        );
        assert!(items.iter().all(|item| {
            item.subtitle_text()
                .is_none_or(|subtitle| !subtitle.contains("later Milestone 4"))
        }));

        let new_conversation = items
            .iter()
            .find(|item| item.id().as_ref() == "conversation.new.v1")
            .expect("conversation command");
        assert!(!new_conversation.is_disabled());

        let espejo = items
            .iter()
            .find(|item| item.id().as_ref() == ESPEJO_ID)
            .expect("Espejo command");
        assert!(!espejo.is_disabled());
    }

    #[test]
    fn management_commands_route_to_typed_settings_destinations() {
        assert_eq!(
            palette_selection("connection.manage.v1"),
            Some(PaletteSelection::Navigate(PaletteDestination::Settings(
                SettingsRoute::Connections
            )))
        );
        assert_eq!(
            palette_selection("project.manage.v1"),
            Some(PaletteSelection::Navigate(PaletteDestination::Settings(
                SettingsRoute::Projects
            )))
        );
        assert_eq!(
            palette_selection("configuration.reset.v1"),
            Some(PaletteSelection::Navigate(PaletteDestination::Settings(
                SettingsRoute::Reset
            )))
        );
    }

    #[test]
    fn status_only_management_routes_disclose_the_terminal_boundary() {
        for stable_id in [
            "skill.manage.v1",
            "plugin.manage.v1",
            "mcp.manage.v1",
            "external_agent.manage.v1",
            "image.manage.v1",
            "vision.manage.v1",
            "outbound.manage.v1",
            "operation.reconcile.v1",
        ] {
            let item = palette_items(DesktopAuthority::Owner, false, false)
                .into_iter()
                .find(|item| item.id().as_ref() == stable_id)
                .unwrap_or_else(|| panic!("missing palette row for {stable_id}"));
            assert!(
                !item.is_disabled(),
                "{stable_id} should open its status view"
            );
            assert!(
                item.subtitle_text().is_some_and(|subtitle| {
                    subtitle.contains("typed terminal management flow")
                }),
                "{stable_id} must disclose its status-only Desktop scope"
            );
        }
    }
}
