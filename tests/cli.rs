//! Package-level smoke tests for the compiled Xana executable.

use std::{path::Path, process::Command};
use tempfile::tempdir;

fn xana(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xana"));
    command.env("XANA_HOME", home).env("NO_COLOR", "1");
    command
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "Xana failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn help_runs_without_initializing_xana() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("unused-home");
    let output = xana(&home).arg("--help").output().expect("run Xana help");

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    assert!(!home.exists());
}

#[test]
fn config_path_honors_an_absolute_xana_home() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let output = xana(&home)
        .args(["config", "path"])
        .output()
        .expect("run config path");

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        home.join("config.toml").display().to_string()
    );
    assert!(!home.exists());
}

#[test]
fn noninteractive_init_creates_once_and_config_check_loads_it() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let init_args = [
        "init",
        "--non-interactive",
        "--provider-name",
        "ollama",
        "--base-url",
        "http://localhost:11434/v1",
        "--model",
        "qwen3:1.7b",
        "--permission-mode",
        "ask",
    ];

    let first = xana(&home)
        .args(init_args)
        .output()
        .expect("initialize Xana");
    assert_success(&first);
    let config_path = home.join("config.toml");
    let original = std::fs::read(&config_path).expect("read created configuration");

    let second = xana(&home)
        .args(init_args)
        .output()
        .expect("repeat initialization");
    assert_success(&second);
    assert_eq!(
        std::fs::read(&config_path).expect("read unchanged configuration"),
        original
    );

    let check = xana(&home)
        .args(["config", "check"])
        .output()
        .expect("check configuration");
    assert_success(&check);
    assert!(String::from_utf8_lossy(&check.stdout).starts_with("configuration is valid:"));
}

#[test]
fn noninteractive_codex_init_creates_a_valid_managed_connection() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let output = xana(&home)
        .args([
            "init",
            "--non-interactive",
            "--kind",
            "codex",
            "--provider-name",
            "codex",
            "--model",
            "gpt-5.6-sol",
            "--permission-mode",
            "ask",
        ])
        .output()
        .expect("initialize managed Codex connection");

    assert_success(&output);
    let config = std::fs::read_to_string(home.join("config.toml"))
        .expect("read managed Codex configuration");
    assert!(config.contains("kind = \"codex\""));
    assert!(config.contains("codex_program = \"codex\""));
    assert!(!config.contains("base_url"));

    let check = xana(&home)
        .args(["config", "check"])
        .output()
        .expect("check managed Codex configuration");
    assert_success(&check);
}
