use super::*;
use crate::{
    config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
    shell::ShellConfig,
};
use std::{ffi::OsString, fs};
use tempfile::TempDir;

fn fixture() -> (TempDir, XanaPaths, SettingsManager) {
    let directory = tempfile::tempdir().expect("temporary Xana home");
    let paths = XanaPaths::resolve(Some(OsString::from(directory.path())))
        .expect("absolute temporary Xana home");
    let rendered = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Ollama {
            name: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
        },
        model: "qwen3:1.7b".to_owned(),
        max_tool_rounds: 12,
        shell: ShellConfig::default(),
        permission_mode: PermissionMode::Ask,
        reasoning_effort: None,
    })
    .expect("render config");
    fs::write(paths.config_file(), rendered).expect("write config");
    let manager = SettingsManager::new(&paths);
    (directory, paths, manager)
}

fn state_in(section: SettingsSection) -> (TempDir, XanaPaths, SettingsManager, SettingsState) {
    let (directory, paths, manager) = fixture();
    let state = SettingsState::new(
        manager.begin().expect("begin settings draft"),
        Some(section),
        None,
    )
    .expect("create settings state");
    (directory, paths, manager, state)
}

fn select_key(state: &mut SettingsState, key: &str) {
    state.selected = state
        .visible_entries()
        .iter()
        .position(|entry| entry.key == key)
        .expect("setting must be visible");
}

#[test]
fn choice_editor_stages_without_writing_and_review_requires_confirmation() {
    let (_directory, paths, _manager, mut state) = state_in(SettingsSection::Appearance);
    select_key(&mut state, "appearance.theme");
    let presentation_path = paths.presentation_file();

    assert_eq!(state.update(Action::Activate), UpdateEffect::None);
    assert!(matches!(state.overlay, Some(Overlay::Choice { .. })));
    assert_eq!(state.update(Action::Down), UpdateEffect::None);
    assert_eq!(state.update(Action::Activate), UpdateEffect::None);

    assert_eq!(state.pending_count(), 1);
    assert!(
        state
            .snapshot
            .entry("appearance.theme")
            .expect("theme entry")
            .staged
    );
    assert!(!presentation_path.exists());
    assert_eq!(state.update(Action::Apply), UpdateEffect::None);
    assert!(matches!(state.overlay, Some(Overlay::Review { .. })));
    assert_eq!(state.update(Action::Activate), UpdateEffect::Apply);
    assert!(!presentation_path.exists());
}

#[test]
fn invalid_text_edit_stays_open_with_an_actionable_error() {
    let (_directory, _paths, _manager, mut state) = state_in(SettingsSection::Diagnostics);
    select_key(&mut state, "diagnostics.max_total_bytes");
    state.update(Action::Activate);
    let Some(Overlay::Text { input, .. }) = &mut state.overlay else {
        panic!("byte setting should open text editor");
    };
    *input = "lots".to_owned();

    assert_eq!(state.update(Action::Activate), UpdateEffect::None);

    assert!(matches!(state.overlay, Some(Overlay::Text { .. })));
    assert_eq!(state.status.tone, state::StatusTone::Error);
    assert!(state.status.message.contains("expected bytes"));
    assert_eq!(state.pending_count(), 0);
}

#[test]
fn search_is_global_live_and_escape_is_one_level_back() {
    let (_directory, _paths, _manager, mut state) = state_in(SettingsSection::Overview);
    state.update(Action::Search);
    for character in "retention".chars() {
        state.update(Action::Input(character));
    }

    assert_eq!(state.visible_entries().len(), 1);
    assert_eq!(
        state.selected_entry().expect("search result").key,
        "diagnostics.retention_days"
    );
    assert_eq!(state.update(Action::Back), UpdateEffect::None);
    assert!(state.overlay.is_none());
    assert_eq!(state.search, "retention");
    assert_eq!(state.update(Action::Back), UpdateEffect::None);
    assert!(state.search.is_empty());
    assert_eq!(state.update(Action::Back), UpdateEffect::Exit);
}

#[test]
fn pending_exit_never_discards_silently() {
    let (_directory, _paths, manager, mut state) = state_in(SettingsSection::Permissions);
    select_key(&mut state, "permissions.default");
    state.update(Action::Activate);
    state.update(Action::Down);
    state.update(Action::Activate);
    assert_eq!(state.pending_count(), 1);

    assert_eq!(state.update(Action::Back), UpdateEffect::None);
    assert!(matches!(state.overlay, Some(Overlay::ConfirmDiscard)));
    assert_eq!(state.update(Action::Activate), UpdateEffect::Discard);

    state.discard_succeeded(manager.begin().expect("reload clean draft"));
    assert_eq!(state.pending_count(), 0);
    assert_eq!(state.update(Action::Back), UpdateEffect::Exit);
}

#[test]
fn applying_refreshes_the_draft_and_tracks_new_conversation_effects() {
    let (_directory, _paths, manager, mut state) = state_in(SettingsSection::Permissions);
    select_key(&mut state, "permissions.default");
    state.update(Action::Activate);
    state.update(Action::Down);
    state.update(Action::Activate);
    let receipt = manager
        .commit(state.draft(), false)
        .expect("commit staged permission");
    let replacement = manager.begin().expect("reload settings");

    state.apply_succeeded(&receipt, replacement);

    assert_eq!(state.pending_count(), 0);
    assert!(state.requires_new_conversation());
    assert_eq!(state.applied_transactions, 1);
    assert_eq!(state.status.tone, state::StatusTone::Success);
}

#[test]
fn keyboard_mapping_respects_editors_and_global_shortcuts() {
    let (_directory, _paths, _manager, mut state) = state_in(SettingsSection::Appearance);
    let plain = KeyModifiers::NONE;
    assert_eq!(
        action_for_key(KeyEvent::new(KeyCode::Char('/'), plain), &state),
        Some(Action::Search)
    );
    assert_eq!(
        action_for_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &state
        ),
        Some(Action::Apply)
    );
    state.update(Action::Search);
    assert_eq!(
        action_for_key(KeyEvent::new(KeyCode::Char('r'), plain), &state),
        Some(Action::Input('r'))
    );
    assert_eq!(
        action_for_key(KeyEvent::new(KeyCode::Esc, plain), &state),
        Some(Action::Back)
    );
}

#[test]
fn pasted_controls_are_dropped_before_they_reach_search() {
    let (_directory, _paths, _manager, mut state) = state_in(SettingsSection::Overview);
    state.update(Action::Search);
    let actions = actions_for_event(Event::Paste("diag\u{1b}[31m\nnostics".to_owned()), &state);

    assert!(!actions.is_empty());
    assert!(
        actions
            .iter()
            .all(|action| matches!(action, Action::Input(_)))
    );
    for action in actions {
        state.update(action);
    }
    assert_eq!(state.search, "diag[31mnostics");
}
