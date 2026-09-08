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
        web_provider: None,
        public_web: None,
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
fn search_setup_retains_chat_and_other_search_connections_without_network() {
    let (_root, paths) = fixture();
    let before = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    let mut args = empty_args();
    args.integration = Some(ConnectIntegration::Web);
    args.web_provider = Some(crate::cli::WebProviderChoice::ExaMcp);
    args.yes = true;
    run_web(
        &args,
        &paths,
        false,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    )
    .unwrap();
    args.web_provider = Some(crate::cli::WebProviderChoice::Brave);
    args.credential_env = Some("XANA_TEST_UNSET_BRAVE_KEY".into());
    run_web(
        &args,
        &paths,
        false,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    )
    .unwrap();
    let after = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    assert_eq!(after.connections, before.connections);
    assert_eq!(after.default_profile, before.default_profile);
    let profile = &after.profiles[&after.default_profile];
    let mut original = before.profiles[&before.default_profile].clone();
    original.egress_policy = profile.egress_policy.clone();
    assert_eq!(
        profile, &original,
        "only the reviewed disclosure policy changes"
    );
    assert_eq!(
        after.egress_policies[profile.egress_policy.as_ref().unwrap()].allowed,
        vec![crate::config::OutboundDataClass::PromptText]
    );
    assert_eq!(after.web.connections.len(), 2);
    assert_eq!(after.web.default_connection.as_deref(), Some("brave"));
    args.web_provider = Some(crate::cli::WebProviderChoice::Disabled);
    run_web(
        &args,
        &paths,
        false,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    )
    .unwrap();
    let after = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    assert_eq!(after.web.connections.len(), 2);
    assert!(after.web.selected().is_none());
}

#[test]
fn public_web_preference_is_explicit_and_does_not_select_search() {
    let (_root, paths) = fixture();
    let mut args = empty_args();
    args.integration = Some(ConnectIntegration::Web);
    args.public_web = Some(crate::cli::PublicWebChoice::Allow);
    args.yes = true;
    run_web(
        &args,
        &paths,
        false,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    )
    .unwrap();
    let config = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    assert_eq!(config.web.public_web, crate::web::PublicWebConsent::Allow);
    assert!(config.web.selected().is_none());
}

#[test]
fn narrowing_web_settings_preserves_restrictive_profiles_and_cancel_is_atomic() {
    let (_root, paths) = fixture();
    let original = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    let mut args = empty_args();
    args.integration = Some(ConnectIntegration::Web);
    args.yes = true;
    args.public_web = Some(crate::cli::PublicWebChoice::Ask);
    run_web(
        &args,
        &paths,
        false,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    )
    .unwrap();
    args.public_web = None;
    args.web_provider = Some(crate::cli::WebProviderChoice::Disabled);
    run_web(
        &args,
        &paths,
        false,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
    )
    .unwrap();
    let narrowed = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    assert_eq!(narrowed.profiles, original.profiles);
    assert_eq!(narrowed.egress_policies, original.egress_policies);
    let before = std::fs::read(paths.config_file()).unwrap();
    args.yes = false;
    args.web_provider = Some(crate::cli::WebProviderChoice::PagesOnly);
    let mut output = Vec::new();
    run_web(&args, &paths, true, &mut Cursor::new(b"n\n"), &mut output).unwrap();
    assert_eq!(std::fs::read(paths.config_file()).unwrap(), before);
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Profile default"));
    assert!(output.contains("add prompt_text"));
    assert!(output.contains("No changes made"));
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
