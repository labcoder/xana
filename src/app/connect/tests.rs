use super::*;
use crate::{
    config::{InitialConfig, InitialConnection, PermissionMode},
    shell::ShellConfig,
};
use std::io::Cursor;

fn fixture() -> (tempfile::TempDir, XanaPaths) {
    let directory = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
    std::fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    let config = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Ollama {
            name: "local".into(),
            base_url: "http://localhost:11434/v1".into(),
        },
        model: "qwen".into(),
        max_tool_rounds: 8,
        shell: ShellConfig::default(),
        permission_mode: PermissionMode::Ask,
        reasoning_effort: None,
    })
    .unwrap();
    std::fs::write(paths.config_file(), config).unwrap();
    (directory, paths)
}

fn empty_args() -> ConnectArgs {
    ConnectArgs {
        integration: Some(ConnectIntegration::Vision),
        route: None,
        service_connection: None,
        service_provider: None,
        model: None,
        credential_env: None,
        base_url: None,
        profile: None,
        make_default: false,
        remove: false,
        yes: false,
    }
}

#[test]
fn guided_vision_setup_commits_one_reviewed_route_and_backup() {
    let (_directory, paths) = fixture();
    let before = std::fs::read(paths.config_file()).unwrap();
    let mut input = Cursor::new(b"1\n\n\nvision-model\n\n\nY\n".to_vec());
    let mut output = Vec::new();

    run_focused_service(
        &empty_args(),
        FocusedServiceKind::Vision,
        &paths,
        true,
        &mut input,
        &mut output,
    )
    .unwrap();

    let registry = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    assert_eq!(registry.service_routes["describe"].model, "vision-model");
    assert_eq!(
        registry.service_routes["describe"].operation,
        "vision.analyze"
    );
    assert_eq!(
        std::fs::read(paths.config_file().with_extension("toml.bak")).unwrap(),
        before
    );
    let receipt = String::from_utf8(output).unwrap();
    assert!(receipt.contains("Review focused-service setup"));
    assert!(receipt.contains("prompt_text + selected_artifacts"));
}

#[test]
fn cancelled_guided_setup_preserves_config_and_creates_no_backup() {
    let (_directory, paths) = fixture();
    let before = std::fs::read(paths.config_file()).unwrap();
    let mut input = Cursor::new(b"1\n\n\nvision-model\n\n\nn\n".to_vec());
    let mut output = Vec::new();

    run_focused_service(
        &empty_args(),
        FocusedServiceKind::Vision,
        &paths,
        true,
        &mut input,
        &mut output,
    )
    .unwrap();

    assert_eq!(std::fs::read(paths.config_file()).unwrap(), before);
    assert!(!paths.config_file().with_extension("toml.bak").exists());
}

#[test]
fn integration_hub_reports_readiness_and_ordered_mcp_prerequisites() {
    let (_directory, paths) = fixture();
    let mut args = empty_args();
    args.integration = Some(ConnectIntegration::Mcp);
    let mut output = Vec::new();

    write_hub(&args, args.integration, &paths, &mut output).unwrap();

    let rendered = String::from_utf8(output).unwrap();
    assert!(rendered.contains("Current readiness"));
    assert!(rendered.contains("MCP clients     configured=0 exposed_by_profiles=0"));
    assert!(rendered.contains("1. xana mcp add-stdio"));
    assert!(rendered.contains("2. xana mcp refresh <SERVER>"));
    assert!(rendered.contains("Transport, profile exposure, egress, and primitive allowlists"));
    assert!(!paths.package_state_file().exists());
    assert!(!paths.external_agent_state_file().exists());
}

#[test]
fn integration_hub_reports_corrupt_private_state_instead_of_zero_counts() {
    let (_directory, paths) = fixture();
    std::fs::create_dir_all(paths.package_state_file().parent().unwrap()).unwrap();
    std::fs::write(paths.package_state_file(), b"not-json").unwrap();
    let mut args = empty_args();
    args.integration = Some(ConnectIntegration::Plugin);
    let mut output = Vec::new();

    write_hub(&args, args.integration, &paths, &mut output).unwrap();

    let rendered = String::from_utf8(output).unwrap();
    assert!(rendered.contains("plugins         unavailable"));
    assert!(rendered.contains("xana doctor"));
    assert!(!rendered.contains("installed=0 healthy=0"));
}
