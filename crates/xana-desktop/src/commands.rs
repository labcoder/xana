//! Native command projection for Xana Desktop.
//!
//! The shared command catalog owns semantic identity. This module owns only
//! Desktop labels, shortcuts, and the small set of handlers implemented by the
//! current workbench slice.

#[cfg(target_os = "macos")]
use gpui::SystemMenuType;
use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, actions};
use gpui_ai::prelude::CommandSearchItem;
use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use xana::desktop::{DesktopAuthority, DesktopCommandDescriptor, desktop_commands};

pub(crate) const WORKBENCH_KEY_CONTEXT: &str = "XanaWorkbench";

pub(crate) const COMMAND_PALETTE_ID: &str = "application.command_palette.v1";
pub(crate) const QUIT_ID: &str = "application.quit.v1";
pub(crate) const MINIMIZE_ID: &str = "application.window.minimize.v1";
pub(crate) const DOCUMENTATION_ID: &str = "help.documentation.open.v1";
pub(crate) const CONFIGURATION_FILE_ID: &str = "configuration.file.open.v1";
pub(crate) const LOGS_ID: &str = "diagnostics.logs.v1";
pub(crate) const CLEAR_ID: &str = "conversation.clear.v1";
pub(crate) const INTERRUPT_ID: &str = "run.interrupt.v1";
pub(crate) const ACTIVITY_ID: &str = "presentation.activity.show.v1";

actions!(
    xana_desktop,
    [
        ShowCommandPalette,
        QuitXana,
        MinimizeWindow,
        OpenDocumentation,
        OpenConfigurationFile,
        RevealLogs,
        ClearConversation,
        InterruptRun,
        ShowActivity,
        NewConversation,
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkbenchCommand {
    ShowCommandPalette,
    Quit,
    Minimize,
    OpenDocumentation,
    OpenConfigurationFile,
    RevealLogs,
    ClearConversation,
    InterruptRun,
    ShowActivity,
}

impl WorkbenchCommand {
    pub(crate) const fn stable_id(self) -> &'static str {
        match self {
            Self::ShowCommandPalette => COMMAND_PALETTE_ID,
            Self::Quit => QUIT_ID,
            Self::Minimize => MINIMIZE_ID,
            Self::OpenDocumentation => DOCUMENTATION_ID,
            Self::OpenConfigurationFile => CONFIGURATION_FILE_ID,
            Self::RevealLogs => LOGS_ID,
            Self::ClearConversation => CLEAR_ID,
            Self::InterruptRun => INTERRUPT_ID,
            Self::ShowActivity => ACTIVITY_ID,
        }
    }

    pub(crate) fn from_stable_id(id: &str) -> Option<Self> {
        Some(match id {
            COMMAND_PALETTE_ID => Self::ShowCommandPalette,
            QUIT_ID => Self::Quit,
            MINIMIZE_ID => Self::Minimize,
            DOCUMENTATION_ID => Self::OpenDocumentation,
            CONFIGURATION_FILE_ID => Self::OpenConfigurationFile,
            LOGS_ID => Self::RevealLogs,
            CLEAR_ID => Self::ClearConversation,
            INTERRUPT_ID => Self::InterruptRun,
            ACTIVITY_ID => Self::ShowActivity,
            _ => return None,
        })
    }
}

pub(crate) fn install(cx: &mut App) {
    cx.bind_keys([
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

    vec![
        Menu::new("Xana").items(application_items),
        Menu::new("File").items([
            MenuItem::action("New Conversation", NewConversation).disabled(true),
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
        ]),
        Menu::new("Conversation").items([
            MenuItem::action("New Conversation", NewConversation).disabled(true),
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

pub(crate) fn palette_items(run_active: bool) -> Vec<CommandSearchItem> {
    desktop_commands(DesktopAuthority::Owner, true)
        .into_iter()
        .map(|descriptor| palette_item(descriptor, run_active))
        .collect()
}

fn palette_item(descriptor: DesktopCommandDescriptor, run_active: bool) -> CommandSearchItem {
    let implementation = WorkbenchCommand::from_stable_id(descriptor.id);
    let runtime_disabled = descriptor.id == INTERRUPT_ID && !run_active;
    let unavailable_reason = descriptor
        .unavailable_reason
        .map(str::to_owned)
        .or_else(|| {
            implementation
                .is_none()
                .then(|| "Available in a later Milestone 4 Desktop slice.".to_owned())
        })
        .or_else(|| runtime_disabled.then(|| "No Run is active.".to_owned()));
    let title = command_title(&descriptor);
    let subtitle = unavailable_reason.map_or_else(
        || descriptor.summary.to_owned(),
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
    let disabled = !descriptor.available || implementation.is_none() || runtime_disabled;
    let mut item = CommandSearchItem::new(descriptor.id, title)
        .subtitle(subtitle)
        .keywords(keywords)
        .disabled(disabled);
    if let Some(shortcut) = shortcut_for(descriptor.id) {
        item = item.shortcut(shortcut);
    }
    item
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
        let items = palette_items(false);
        assert_eq!(items.len(), descriptors.len());
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
            WorkbenchCommand::Minimize,
            WorkbenchCommand::OpenDocumentation,
            WorkbenchCommand::OpenConfigurationFile,
            WorkbenchCommand::RevealLogs,
            WorkbenchCommand::ClearConversation,
            WorkbenchCommand::InterruptRun,
            WorkbenchCommand::ShowActivity,
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
    fn future_commands_are_visible_but_disabled() {
        let items = palette_items(false);
        let new_conversation = items
            .iter()
            .find(|item| item.id().as_ref() == "conversation.new.v1")
            .expect("conversation command");
        assert!(new_conversation.is_disabled());
        assert!(
            new_conversation
                .subtitle_text()
                .is_some_and(|subtitle| subtitle.contains("later Milestone 4"))
        );
    }
}
