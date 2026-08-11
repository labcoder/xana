use super::*;
use crate::cli::RouteCommand;
use crate::config::{InitialConfig, InitialConnection, ProviderKind};
use std::{fs, io::Cursor};
use tempfile::tempdir;

#[test]
fn config_commands_use_the_injected_paths_and_writer() {
    let directory = tempdir().expect("temporary Xana home");
    let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned()))
        .expect("absolute Xana home");
    let rendered = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Ollama {
            name: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
        },
        model: "model".to_owned(),
        max_tool_rounds: 8,
        shell: crate::shell::ShellConfig::default(),
        permission_mode: crate::config::PermissionMode::Ask,
        reasoning_effort: None,
    })
    .expect("render config");
    fs::write(paths.config_file(), rendered).expect("write config");

    let mut path_output = Vec::new();
    run_config_command(ConfigCommand::Path, &paths, &mut path_output).expect("print config path");
    let mut check_output = Vec::new();
    run_config_command(ConfigCommand::Check, &paths, &mut check_output).expect("check config");

    assert_eq!(
        String::from_utf8(path_output).expect("path output"),
        format!("{}\n", paths.config_file().display())
    );
    assert!(
        String::from_utf8(check_output)
            .expect("check output")
            .starts_with("configuration is valid:")
    );
}

#[test]
fn route_commands_report_exact_local_resolution_without_starting_a_provider() {
    let directory = tempdir().expect("temporary Xana home");
    let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned()))
        .expect("absolute Xana home");
    let rendered = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Ollama {
            name: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
        },
        model: "qwen".to_owned(),
        max_tool_rounds: 8,
        shell: crate::shell::ShellConfig::default(),
        permission_mode: crate::config::PermissionMode::Ask,
        reasoning_effort: None,
    })
    .expect("render config");
    fs::write(paths.config_file(), rendered).expect("write config");

    let mut listed = Vec::new();
    operations::run_route(RouteCommand::List, &paths, &mut listed).expect("list routes");
    let mut checked = Vec::new();
    operations::run_route(
        RouteCommand::Check {
            route: "default".into(),
        },
        &paths,
        &mut checked,
    )
    .expect("check route");

    assert_eq!(
        String::from_utf8(listed).expect("utf8 list"),
        "* default\tnative\tollama/qwen\tprofile default\n"
    );
    let checked = String::from_utf8(checked).expect("utf8 check");
    assert!(checked.contains("route: default\n"));
    assert!(checked.contains("execution: native\n"));
    assert!(checked.contains("connection: ollama\n"));
    assert!(checked.contains("model: qwen\n"));
    assert!(!checked.to_ascii_lowercase().contains("secret"));
}

#[test]
fn reset_confirmation_can_decline_without_removing_configuration() {
    let directory = tempdir().expect("temporary Xana home");
    let root = directory.path().join("xana-home");
    let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
    fs::create_dir_all(paths.config_file().parent().expect("config parent"))
        .expect("create config parent");
    fs::write(paths.config_file(), b"existing").expect("existing config");
    let mut input = Cursor::new(b"\nn\n");
    let mut output = Vec::new();

    run_reset_with_io(
        &cli::ResetArgs {
            yes: false,
            ..cli::ResetArgs::default()
        },
        &paths,
        true,
        &mut input,
        &mut output,
    )
    .expect("decline reset");

    assert!(paths.config_file().is_file());
    assert!(
        String::from_utf8(output)
            .expect("reset output")
            .contains("No changes made.")
    );
}

#[test]
fn confirmed_reset_does_not_read_input_and_allows_initialization_again() {
    let directory = tempdir().expect("temporary Xana home");
    let root = directory.path().join("xana-home");
    let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
    fs::create_dir_all(paths.config_file().parent().expect("config parent"))
        .expect("create config parent");
    fs::write(paths.config_file(), b"existing").expect("existing config");
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();

    run_reset_with_io(
        &cli::ResetArgs {
            yes: true,
            ..cli::ResetArgs::default()
        },
        &paths,
        false,
        &mut input,
        &mut output,
    )
    .expect("confirmed reset");

    assert_eq!(input.position(), 0);
    assert!(!paths.config_file().exists());
    assert!(
        String::from_utf8(output)
            .expect("reset output")
            .contains("cargo run -- setup")
    );
}

#[test]
fn credential_reset_dry_run_never_opens_the_secret_store() {
    let directory = tempdir().expect("temporary Xana home");
    let root = directory.path().join("xana-home");
    let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
    let rendered = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Native {
            name: "remote".into(),
            kind: ProviderKind::OpenRouter,
            base_url: None,
            credential: Some(CredentialReference::Stored {
                id: "remote-key".into(),
            }),
        },
        model: "provider/model".into(),
        permission_mode: crate::config::PermissionMode::Ask,
        reasoning_effort: None,
        shell: crate::shell::ShellConfig::default(),
        max_tool_rounds: 8,
    })
    .unwrap();
    fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    fs::write(paths.config_file(), rendered).unwrap();
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();

    run_reset_with_io(
        &cli::ResetArgs {
            scope: vec![cli::ResetScopeChoice::Credentials],
            dry_run: true,
            ..cli::ResetArgs::default()
        },
        &paths,
        false,
        &mut input,
        &mut output,
    )
    .unwrap();

    assert!(paths.config_file().is_file());
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("referenced OS credential: remote-key"));
    assert!(output.contains("Dry run only"));
}

#[test]
fn noninteractive_init_routes_without_process_environment_or_terminal() {
    let directory = tempdir().expect("temporary Xana home");
    let root = directory.path().join("xana-home");
    let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
    let args = cli::InitArgs {
        non_interactive: true,
        kind: Some(crate::cli::InitConnectionKindChoice::Ollama),
        provider_name: Some("ollama".to_owned()),
        base_url: Some("http://localhost:11434/v1".to_owned()),
        codex_program: None,
        codex_home: None,
        model: Some("model".to_owned()),
        max_tool_rounds: Some(8),
        shell: None,
        shell_program: None,
        permission_mode: Some(crate::cli::PermissionChoice::Ask),
        dry_run: false,
    };
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();

    run_init_with_io(
        &args,
        &paths,
        false,
        &mut input,
        &mut output,
        BannerMode::test_hidden(),
    )
    .expect("noninteractive init");

    assert!(paths.config_file().is_file());
    assert_eq!(input.position(), 0);
    assert!(
        String::from_utf8(output)
            .expect("init output")
            .contains("configuration created:")
    );
}

#[test]
fn interactive_codex_init_writes_managed_runtime_and_next_steps() {
    let directory = tempdir().expect("temporary Xana home");
    let root = directory.path().join("xana-home");
    let paths = XanaPaths::resolve(Some(root.into_os_string())).expect("absolute Xana home");
    let args = cli::InitArgs {
        non_interactive: false,
        kind: None,
        provider_name: None,
        base_url: None,
        codex_program: None,
        codex_home: None,
        model: None,
        max_tool_rounds: None,
        shell: None,
        shell_program: None,
        permission_mode: None,
        dry_run: false,
    };
    let mut input = Cursor::new(b"2\n\n\ngpt-5.6-sol\n\n\n\n\ny\n");
    let mut output = Vec::new();

    run_init_with_io(
        &args,
        &paths,
        true,
        &mut input,
        &mut output,
        BannerMode::test_hidden(),
    )
    .expect("interactive Codex init");

    let rendered = fs::read_to_string(paths.config_file()).expect("created configuration");
    assert!(rendered.contains("kind = \"codex\""));
    assert!(rendered.contains("codex_program = \"codex\""));
    assert!(!rendered.contains("base_url"));
    let transcript = String::from_utf8(output).expect("init output");
    assert!(transcript.contains("xana connection status codex"));
    assert!(transcript.contains("xana connection login codex"));
    assert!(transcript.contains("xana model list --connection codex"));
    assert!(transcript.contains("xana model use codex/MODEL"));
    assert!(transcript.contains("cargo run -- connection status codex"));
}

#[test]
fn dry_run_routes_without_creating_the_home() {
    let directory = tempdir().expect("temporary Xana home");
    let root = directory.path().join("xana-home");
    let paths =
        XanaPaths::resolve(Some(root.clone().into_os_string())).expect("absolute Xana home");
    let args = cli::InitArgs {
        dry_run: true,
        ..cli::InitArgs {
            non_interactive: true,
            kind: Some(crate::cli::InitConnectionKindChoice::Ollama),
            provider_name: Some("ollama".to_owned()),
            base_url: Some("http://localhost:11434/v1".to_owned()),
            codex_program: None,
            codex_home: None,
            model: Some("model".to_owned()),
            max_tool_rounds: None,
            shell: None,
            shell_program: None,
            permission_mode: Some(crate::cli::PermissionChoice::Ask),
            dry_run: false,
        }
    };
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();

    run_init_with_io(
        &args,
        &paths,
        false,
        &mut input,
        &mut output,
        BannerMode::test_hidden(),
    )
    .expect("dry-run init");

    assert!(!root.exists());
    assert!(
        String::from_utf8(output)
            .expect("dry-run output")
            .contains("permission_mode = \"ask\"")
    );
}
