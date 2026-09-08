use super::*;
use crate::{
    cli::{Cli, Command, ConfigCommand},
    config::{
        CredentialReference, InitialConfig, InitialConnection, PermissionMode, ProviderKind,
        XanaConfig,
    },
    shell::ShellConfig,
};
use clap::Parser as _;
use std::{ffi::OsString, fs};
use tempfile::TempDir;

fn fixture() -> (TempDir, XanaPaths) {
    let directory = tempfile::tempdir().expect("temporary Xana home");
    let paths = XanaPaths::resolve(Some(OsString::from(directory.path())))
        .expect("absolute temporary Xana home");
    let rendered = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Native {
            name: "openrouter".to_owned(),
            kind: ProviderKind::OpenRouter,
            base_url: None,
            credential: Some(CredentialReference::Stored {
                id: "private-secret-reference".to_owned(),
            }),
        },
        model: "qwen3:1.7b".to_owned(),
        max_tool_rounds: 12,
        shell: ShellConfig::default(),
        permission_mode: PermissionMode::Ask,
        reasoning_effort: None,
    })
    .expect("render initial config");
    fs::write(paths.config_file(), rendered).expect("write config");
    (directory, paths)
}

#[test]
fn list_groups_human_output_and_supports_section_search() {
    let (_directory, paths) = fixture();
    let mut output = Vec::new();

    list(
        &paths,
        Some("appearance"),
        Some("theme"),
        false,
        &mut output,
    )
    .expect("list settings");
    let output = String::from_utf8(output).expect("UTF-8 output");

    assert!(output.contains("Xana settings"));
    assert!(output.contains("Appearance"));
    assert!(output.contains("appearance.theme"));
    assert!(output.contains("Applies immediately"));
    assert!(!output.contains("permissions.default"));
}

#[test]
fn list_json_has_a_version_and_no_credential_material() {
    let (_directory, paths) = fixture();
    let mut output = Vec::new();

    list(&paths, None, None, true, &mut output).expect("list settings as JSON");
    let document: serde_json::Value = serde_json::from_slice(&output).expect("parse JSON");
    let encoded = String::from_utf8(output).expect("UTF-8 JSON");

    assert_eq!(document["version"], 1);
    assert!(document["revision"].as_str().is_some());
    assert!(
        document["settings"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(!encoded.contains("private-secret-reference"));
}

#[test]
fn get_is_script_friendly_and_explain_names_ownership() {
    let (_directory, paths) = fixture();
    let mut value = Vec::new();
    get(&paths, "appearance.theme", false, &mut value).expect("get setting");
    assert_eq!(String::from_utf8(value).expect("UTF-8 value"), "auto\n");

    let mut explanation = Vec::new();
    explain(&paths, "permissions.default", false, &mut explanation).expect("explain setting");
    let explanation = String::from_utf8(explanation).expect("UTF-8 explanation");
    assert!(explanation.contains("Source:"));
    assert!(explanation.contains("Global Xana configuration"));
    assert!(explanation.contains("Applies before the next new turn"));
    assert!(explanation.contains("deny, ask, allow"));
}

#[test]
fn dry_run_prints_receipt_without_mutating_config() {
    let (_directory, paths) = fixture();
    let before = fs::read(paths.config_file()).expect("read config");
    let mut output = Vec::new();

    set(
        &paths,
        "permissions.default",
        "deny",
        true,
        false,
        &mut output,
    )
    .expect("preview set");
    let output = String::from_utf8(output).expect("UTF-8 receipt");

    assert!(output.contains("Settings preview (no files changed)"));
    assert!(output.contains("ask -> deny"));
    assert!(output.contains("Global Xana configuration"));
    assert!(output.contains("Applies before the next new turn"));
    assert_eq!(fs::read(paths.config_file()).expect("read config"), before);
}

#[test]
fn set_and_reset_commit_through_the_shared_manager() {
    let (_directory, paths) = fixture();
    let mut output = Vec::new();
    set(
        &paths,
        "appearance.theme",
        "monochrome",
        false,
        true,
        &mut output,
    )
    .expect("set theme");
    let receipt: serde_json::Value = serde_json::from_slice(&output).expect("parse receipt");
    assert_eq!(receipt["changes"][0]["target"], "machine_presentation");
    assert_eq!(receipt["changes"][0]["effect"], "immediate");

    output.clear();
    reset(&paths, "appearance.theme", false, false, &mut output).expect("reset theme");
    let snapshot = SettingsManager::new(&paths)
        .snapshot()
        .expect("reload settings");
    assert_eq!(
        snapshot
            .entry("appearance.theme")
            .expect("theme entry")
            .value
            .raw
            .as_deref(),
        Some("auto")
    );
}

#[test]
fn unknown_section_and_read_only_mutation_are_actionable() {
    let (_directory, paths) = fixture();
    let mut output = Vec::new();
    let section_error = list(&paths, Some("colours"), None, false, &mut output)
        .expect_err("unknown section must fail");
    assert!(
        section_error
            .to_string()
            .contains("expected overview, appearance")
    );

    let mutation_error = set(
        &paths,
        "connections.manage",
        "anything",
        false,
        false,
        &mut output,
    )
    .expect_err("focused manager row must not flatten");
    assert!(mutation_error.to_string().contains("xana connection list"));
}

#[test]
fn clap_exposes_every_scriptable_settings_operation() {
    let cases = [
        vec![
            "xana",
            "config",
            "list",
            "--section",
            "appearance",
            "--json",
        ],
        vec!["xana", "config", "get", "appearance.theme", "--json"],
        vec!["xana", "config", "explain", "appearance.theme"],
        vec![
            "xana",
            "config",
            "set",
            "diagnostics.max_total_bytes",
            "64MiB",
            "--dry-run",
        ],
        vec!["xana", "config", "reset", "appearance.theme", "--json"],
    ];

    for arguments in cases {
        let parsed = Cli::try_parse_from(arguments).expect("settings command must parse");
        assert!(matches!(
            parsed.command,
            Some(Command::Config(crate::cli::ConfigArgs {
                command: ConfigCommand::List { .. }
                    | ConfigCommand::Get { .. }
                    | ConfigCommand::Explain { .. }
                    | ConfigCommand::Set { .. }
                    | ConfigCommand::Reset { .. }
            }))
        ));
    }
}
