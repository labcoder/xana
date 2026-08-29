use super::*;
use crate::{
    config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
    paths::XanaPaths,
    settings::{SettingsManager, SettingsSection},
    shell::ShellConfig,
    tui::settings::state::{Action, SettingsState},
};
use ratatui::{Terminal, backend::TestBackend};
use std::{ffi::OsString, fs};
use tempfile::TempDir;

fn fixture_state(section: SettingsSection) -> (TempDir, SettingsState) {
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
    let state = SettingsState::new(
        manager.begin().expect("begin settings draft"),
        Some(section),
        None,
    )
    .expect("create settings state");
    (directory, state)
}

fn draw(width: u16, height: u16, state: &SettingsState) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, state, ResolvedPresentation::test_plain()))
        .expect("render settings");
    buffer_text(terminal.backend().buffer())
}

#[test]
fn wide_layout_keeps_navigation_list_details_and_status_visible() {
    let (_directory, state) = fixture_state(SettingsSection::Appearance);

    let rendered = draw(128, 30, &state);

    assert!(rendered.contains("XANA / SETTINGS"));
    assert!(rendered.contains("Browse"));
    assert!(rendered.contains("appearance.theme"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("Nothing is written until you review and apply"));
    assert!(rendered.contains("Ctrl+S review"));
    assert!(rendered.is_ascii(), "{rendered}");
}

#[test]
fn eighty_by_twenty_four_layout_preserves_every_primary_control() {
    let (_directory, state) = fixture_state(SettingsSection::Permissions);

    let rendered = draw(80, 24, &state);

    assert!(rendered.contains("XANA / SETTINGS"));
    assert!(rendered.contains("Browse"));
    assert!(rendered.contains("Default decision"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("Enter edit"));
    assert!(rendered.contains("/ search"));
    assert!(rendered.contains("Ctrl+S review"));
    assert!(rendered.contains("Esc back"), "{rendered}");
}

#[test]
fn narrow_layout_uses_a_section_carousel_and_stacked_detail() {
    let (_directory, state) = fixture_state(SettingsSection::Diagnostics);

    let rendered = draw(54, 24, &state);

    assert!(rendered.contains("Diagnostics  7/9  < > sections"));
    assert!(rendered.contains("Diagnostic logging"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("^S apply"));
}

#[test]
fn tiny_layout_fails_softly_with_a_resize_instruction() {
    let (_directory, state) = fixture_state(SettingsSection::Overview);

    let rendered = draw(35, 10, &state);

    assert!(rendered.contains("Resize to at least 36x12"));
    assert!(rendered.contains("Esc exits safely"));
}

#[test]
fn choice_and_review_overlays_make_staging_and_scope_obvious() {
    let (_directory, mut state) = fixture_state(SettingsSection::Appearance);
    state.selected = state
        .visible_entries()
        .iter()
        .position(|entry| entry.key == "appearance.theme")
        .expect("theme row");
    state.update(Action::Activate);

    let choice = draw(80, 24, &state);
    assert!(choice.contains("Theme"));
    assert!(choice.contains("monochrome"));
    assert!(choice.contains("Enter stage"));

    state.update(Action::Down);
    state.update(Action::Activate);
    state.update(Action::Apply);
    let review = draw(80, 24, &state);
    assert!(review.contains("Review 1 change(s)"));
    assert!(review.contains("auto -> dark"));
    assert!(review.contains("This terminal frontend"));
    assert!(review.contains("Applies immediately"));
}

#[test]
fn global_search_reports_empty_results_without_losing_navigation() {
    let (_directory, mut state) = fixture_state(SettingsSection::Overview);
    state.update(Action::Search);
    for character in "does-not-exist".chars() {
        state.update(Action::Input(character));
    }
    state.update(Action::Activate);

    let rendered = draw(80, 24, &state);

    assert!(rendered.contains("Search results"));
    assert!(rendered.contains("No settings matched"));
    assert!(rendered.contains("Ctrl+S review"));
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}
