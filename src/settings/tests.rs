use super::*;
use crate::{
    config::{InitialConfig, InitialConnection, PermissionMode},
    shell::ShellConfig,
};
use std::{ffi::OsString, fs};
use tempfile::TempDir;

fn fixture() -> (TempDir, XanaPaths) {
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
    .expect("render initial config");
    fs::write(paths.config_file(), rendered).expect("write initial config");
    (directory, paths)
}

#[test]
fn snapshot_exposes_stable_keys_without_configuration_secrets() {
    let (_directory, paths) = fixture();
    let manager = SettingsManager::new(&paths);

    let snapshot = manager.snapshot().expect("load settings snapshot");
    let encoded = serde_json::to_string(&snapshot).expect("encode snapshot");

    assert!(snapshot.entry(APPEARANCE_THEME).is_some());
    assert!(snapshot.entry(PERMISSIONS_DEFAULT).is_some());
    assert!(snapshot.entry("connections.manage").is_some());
    assert!(encoded.contains("qwen3:1.7b"));
    assert!(!encoded.contains("credential"));
    assert_eq!(snapshot.revision.len(), 16);
}

#[test]
fn draft_previews_and_commits_changes_across_both_owners() {
    let (_directory, paths) = fixture();
    let manager = SettingsManager::new(&paths);
    let original_config = fs::read(paths.config_file()).expect("read original config");
    let mut draft = manager.begin().expect("begin settings draft");

    draft
        .set(APPEARANCE_THEME, "dark")
        .expect("stage appearance change");
    draft
        .set(PERMISSIONS_DEFAULT, "deny")
        .expect("stage permission change");

    let preview = draft.preview().expect("preview settings");
    assert_eq!(
        preview
            .entry(APPEARANCE_THEME)
            .expect("theme entry")
            .value
            .raw,
        Some("dark".to_owned())
    );
    assert!(preview.entry(APPEARANCE_THEME).expect("theme entry").staged);

    let receipt = manager.commit(draft, false).expect("commit settings");

    assert_eq!(receipt.changes.len(), 2);
    assert!(receipt.requires_new_conversation());
    assert_ne!(receipt.revision_before, receipt.revision_after);
    assert_eq!(
        fs::read(paths.config_file().with_extension("toml.bak")).expect("read backup"),
        original_config
    );
    let persisted = manager.snapshot().expect("reload settings");
    assert_eq!(
        persisted
            .entry(APPEARANCE_THEME)
            .expect("theme entry")
            .value
            .raw,
        Some("dark".to_owned())
    );
    assert_eq!(
        persisted
            .entry(PERMISSIONS_DEFAULT)
            .expect("permission entry")
            .value
            .raw,
        Some("deny".to_owned())
    );
}

#[test]
fn dry_run_returns_an_exact_receipt_without_writing() {
    let (_directory, paths) = fixture();
    let manager = SettingsManager::new(&paths);
    let config_before = fs::read(paths.config_file()).expect("read config");
    let presentation_before = fs::read(paths.presentation_file()).ok();
    let mut draft = manager.begin().expect("begin settings draft");
    draft
        .set(DIAGNOSTICS_MAX_TOTAL_BYTES, "64 MiB")
        .expect("stage byte value");

    let receipt = manager.commit(draft, true).expect("preview commit");

    assert!(receipt.dry_run);
    assert_eq!(receipt.changes.len(), 1);
    assert_eq!(receipt.changes[0].after.raw.as_deref(), Some("67108864"));
    assert_eq!(
        fs::read(paths.config_file()).expect("read config"),
        config_before
    );
    assert_eq!(
        fs::read(paths.presentation_file()).ok(),
        presentation_before
    );
    assert!(!paths.config_file().with_extension("toml.bak").exists());
}

#[test]
fn commit_rejects_a_concurrent_configuration_change() {
    let (_directory, paths) = fixture();
    let manager = SettingsManager::new(&paths);
    let mut draft = manager.begin().expect("begin settings draft");
    draft
        .set(APPEARANCE_GLYPHS, "ascii")
        .expect("stage glyph change");
    let mut external = fs::read_to_string(paths.config_file()).expect("read config");
    external.push_str("\n# external edit\n");
    fs::write(paths.config_file(), &external).expect("write external edit");

    let error = manager
        .commit(draft, false)
        .expect_err("concurrent edit must fail");

    assert!(matches!(
        error,
        SettingsError::ConcurrentChange {
            owner: "config.toml"
        }
    ));
    assert_eq!(
        fs::read_to_string(paths.config_file()).expect("read current config"),
        external
    );
    assert!(!paths.presentation_file().exists());
}

#[test]
fn read_only_manager_rows_link_to_the_focused_workflow() {
    let (_directory, paths) = fixture();
    let manager = SettingsManager::new(&paths);
    let mut draft = manager.begin().expect("begin settings draft");

    let error = draft
        .set("connections.manage", "anything")
        .expect_err("manager summary must be read-only");

    assert!(matches!(
        error,
        SettingsError::ReadOnly { key, action: Some(action) }
            if key == "connections.manage" && action == "xana connection list"
    ));
}

#[test]
fn complete_config_validation_rejects_incompatible_limits() {
    let (_directory, paths) = fixture();
    let manager = SettingsManager::new(&paths);
    let mut draft = manager.begin().expect("begin settings draft");

    let error = draft
        .set(DIAGNOSTICS_MAX_FILE_BYTES, "64 MiB")
        .expect_err("one file cannot exceed total storage");

    assert!(matches!(error, SettingsError::Config(_)));
    assert!(
        !draft
            .preview()
            .expect("preview recovered draft")
            .entry(DIAGNOSTICS_MAX_FILE_BYTES)
            .expect("file limit entry")
            .staged
    );
}

#[test]
fn reset_removes_optional_shell_program() {
    let (_directory, paths) = fixture();
    let manager = SettingsManager::new(&paths);
    let mut draft = manager.begin().expect("begin settings draft");
    draft
        .set(EXECUTION_SHELL_PROGRAM, "C:\\tools\\shell.exe")
        .expect("stage shell program");
    manager.commit(draft, false).expect("commit shell program");

    let mut reset = manager.begin().expect("begin reset draft");
    reset.reset(EXECUTION_SHELL_PROGRAM).expect("stage reset");
    manager.commit(reset, false).expect("commit reset");

    let snapshot = manager.snapshot().expect("reload settings");
    assert_eq!(
        snapshot
            .entry(EXECUTION_SHELL_PROGRAM)
            .expect("shell program entry")
            .value
            .raw,
        None
    );
}

#[test]
fn parser_accepts_human_units_and_rejects_ambiguous_values() {
    assert_eq!(parse_bytes("4 MiB"), Ok(4 * 1024 * 1024));
    assert_eq!(parse_bytes("32MB"), Ok(32_000_000));
    assert!(parse_bytes("about four megs").is_err());
    assert_eq!(parse_days("30d"), Ok(30));
    assert_eq!(parse_boolean("ON"), Ok("true"));
}
