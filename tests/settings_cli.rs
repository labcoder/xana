//! Executable smoke coverage for Xana's scriptable and non-TTY settings adapters.

use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

const CONFIG: &str = r#"
version = 1
default_profile = "default"
permission_mode = "ask"

[providers.local]
kind = "ollama"
base_url = "http://localhost:11434/v1"

[profiles.default]
provider = "local"
model = "qwen3:1.7b"
"#;

fn xana(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xana"));
    command.env("XANA_HOME", home).env("NO_COLOR", "1");
    command
}

fn fixture() -> tempfile::TempDir {
    let directory = tempdir().expect("temporary Xana home");
    fs::write(directory.path().join("config.toml"), CONFIG).expect("write config");
    directory
}

fn run(home: &Path, arguments: &[&str]) -> std::process::Output {
    let output = xana(home).args(arguments).output().expect("run Xana");
    assert!(
        output.status.success(),
        "Xana failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn config_inspection_is_discoverable_in_text_and_stable_json() {
    let directory = fixture();
    let output = run(
        directory.path(),
        &["config", "list", "--section", "appearance"],
    );
    let text = String::from_utf8(output.stdout).expect("UTF-8 settings list");
    assert!(text.contains("Xana settings"));
    assert!(text.contains("appearance.theme"));
    assert!(!text.contains("permissions.default"));

    let output = run(
        directory.path(),
        &["config", "explain", "appearance.theme", "--json"],
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("settings JSON");
    assert_eq!(document["key"], "appearance.theme");
    assert_eq!(document["target"], "machine_presentation");
    assert_eq!(document["effect"], "immediate");
}

#[test]
fn config_mutation_supports_no_write_preview_and_atomic_commit() {
    let directory = fixture();
    let config_path = directory.path().join("config.toml");
    let before = fs::read(&config_path).expect("read config");

    let preview = run(
        directory.path(),
        &["config", "set", "permissions.default", "deny", "--dry-run"],
    );
    assert!(String::from_utf8_lossy(&preview.stdout).contains("no files changed"));
    assert_eq!(fs::read(&config_path).expect("read config"), before);

    run(
        directory.path(),
        &["config", "set", "permissions.default", "deny"],
    );
    let value = run(directory.path(), &["config", "get", "permissions.default"]);
    assert_eq!(
        String::from_utf8(value.stdout).expect("UTF-8 value"),
        "deny\n"
    );
    assert!(config_path.with_extension("toml.bak").exists());
}

#[test]
fn redirected_settings_command_degrades_to_the_same_browseable_catalog() {
    let directory = fixture();

    let output = run(
        directory.path(),
        &[
            "settings",
            "--section",
            "diagnostics",
            "--search",
            "retention",
        ],
    );
    let text = String::from_utf8(output.stdout).expect("UTF-8 settings fallback");

    assert!(text.contains("Xana settings"));
    assert!(text.contains("diagnostics.retention_days"));
    assert!(!text.contains("diagnostics.max_files"));
}

#[test]
fn read_only_rows_fail_with_the_exact_focused_manager() {
    let directory = fixture();
    let output = xana(directory.path())
        .args(["config", "set", "connections.manage", "anything"])
        .output()
        .expect("run Xana");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("xana connection list"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
