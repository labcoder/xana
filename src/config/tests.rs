use super::*;
use std::fs;
use tempfile::tempdir;

const MINIMAL: &str = r#"
version = 1
default_profile = "default"
permission_mode = "allow"

[providers.local]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[profiles.default]
provider = "local"
model = "qwen3:1.7b"
"#;

fn parse_ok(input: &str) -> XanaConfig {
    XanaConfig::parse(input).unwrap_or_else(|error| panic!("valid fixture failed: {error}"))
}

fn parse_error(input: &str) -> ConfigError {
    XanaConfig::parse(input).expect_err("fixture should fail")
}

#[test]
fn minimal_v1_resolves_default_profile_and_default_round_limit() {
    let config = parse_ok(MINIMAL);

    assert_eq!(config.provider_name, "local");
    assert_eq!(config.provider_kind, ProviderKind::OpenAiCompat);
    assert_eq!(config.base_url, "http://localhost:11434/v1");
    assert_eq!(config.model, "qwen3:1.7b");
    assert_eq!(config.permission_mode, PermissionMode::Allow);
    assert_eq!(config.max_tool_rounds, 8);
}

#[test]
fn explicit_round_limit_overrides_the_default() {
    let input = MINIMAL.replace(
        "model = \"qwen3:1.7b\"",
        "model = \"qwen3:1.7b\"\nmax_tool_rounds = 12",
    );

    let config = parse_ok(&input);

    assert_eq!(config.max_tool_rounds, 12);
}

#[test]
fn missing_required_field_is_a_decode_error() {
    let input = MINIMAL.replace("permission_mode = \"allow\"\n", "");

    let error = parse_error(&input);

    assert!(matches!(error, ConfigError::Decode(_)));
}

#[test]
fn unknown_top_level_field_is_rejected() {
    let input = MINIMAL.replace(
        "permission_mode = \"allow\"",
        "permission_mode = \"allow\"\nfuture_toggle = true",
    );

    let error = parse_error(&input);

    assert!(matches!(error, ConfigError::Decode(_)));
}

#[test]
fn unknown_nested_field_and_plaintext_api_key_are_rejected() {
    let input = MINIMAL.replace(
        "base_url = \"http://localhost:11434/v1\"",
        "base_url = \"http://localhost:11434/v1\"\napi_key = \"secret\"",
    );

    let error = parse_error(&input);

    assert!(matches!(error, ConfigError::Decode(_)));
}

#[test]
fn future_version_is_reported_before_future_fields() {
    let input = MINIMAL.replace("version = 1", "version = 9\nfuture_schema_field = true");

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::UnsupportedVersion { found: 9 }
    ));
}

#[test]
fn permission_modes_other_than_allow_are_rejected() {
    let input = MINIMAL.replace("permission_mode = \"allow\"", "permission_mode = \"ask\"");

    let error = parse_error(&input);

    assert!(matches!(error, ConfigError::Decode(_)));
}

#[test]
fn invalid_provider_name_is_rejected() {
    let input = MINIMAL.replace("[providers.local]", "[providers.Bad]");

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::InvalidName {
            section: "provider",
            name,
        } if name == "Bad"
    ));
}

#[test]
fn invalid_profile_name_is_rejected() {
    let input = MINIMAL.replace("[profiles.default]", "[profiles.Bad]");

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::InvalidName {
            section: "profile",
            name,
        } if name == "Bad"
    ));
}

#[test]
fn missing_default_profile_carries_its_name() {
    let input = MINIMAL.replace(
        "default_profile = \"default\"",
        "default_profile = \"missing\"",
    );

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::MissingDefaultProfile { name } if name == "missing"
    ));
}

#[test]
fn every_profile_must_reference_a_known_provider() {
    let input =
        format!("{MINIMAL}\n[profiles.broken]\nprovider = \"missing\"\nmodel = \"other\"\n");

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::UnknownProvider { profile, provider }
            if profile == "broken" && provider == "missing"
    ));
}

#[test]
fn blank_model_is_rejected() {
    let input = MINIMAL.replace("model = \"qwen3:1.7b\"", "model = \"   \"");

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::EmptyModel { profile } if profile == "default"
    ));
}

#[test]
fn zero_tool_rounds_is_rejected() {
    let input = MINIMAL.replace(
        "model = \"qwen3:1.7b\"",
        "model = \"qwen3:1.7b\"\nmax_tool_rounds = 0",
    );

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::InvalidToolRoundLimit {
            profile,
            value: 0,
        } if profile == "default"
    ));
}

#[test]
fn tool_rounds_above_the_limit_are_rejected() {
    let input = MINIMAL.replace(
        "model = \"qwen3:1.7b\"",
        "model = \"qwen3:1.7b\"\nmax_tool_rounds = 65",
    );

    let error = parse_error(&input);

    assert!(matches!(
        error,
        ConfigError::InvalidToolRoundLimit {
            profile,
            value: 65,
        } if profile == "default"
    ));
}

#[test]
fn invalid_url_forms_are_rejected() {
    let invalid_urls = [
        "/v1",
        "ftp://localhost/v1",
        "http://user:secret@localhost/v1",
        "http://localhost/v1?token=secret",
        "http://localhost/v1#fragment",
    ];

    for invalid_url in invalid_urls {
        let input = MINIMAL.replace("http://localhost:11434/v1", invalid_url);

        let error = parse_error(&input);

        assert!(matches!(
            error,
            ConfigError::InvalidBaseUrl { provider, .. } if provider == "local"
        ));
    }
}

#[test]
fn valid_non_default_entries_are_checked_but_only_default_is_resolved() {
    let input = r#"
version = 1
default_profile = "default"
permission_mode = "allow"

[providers.local]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[providers.remote]
kind = "openai_compat"
base_url = "https://example.test/v1"

[profiles.default]
provider = "local"
model = "main-model"

[profiles.worker]
provider = "remote"
model = "worker-model"
max_tool_rounds = 4
"#;

    let config = parse_ok(input);

    assert_eq!(config.provider_name, "local");
    assert_eq!(config.model, "main-model");
    assert_eq!(config.max_tool_rounds, 8);
}

#[test]
fn load_from_reads_config_toml() {
    let directory = tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, MINIMAL).expect("write config");

    let config = XanaConfig::load_from(&path).expect("load config");

    assert_eq!(config.provider_name, "local");
}

#[test]
fn legacy_file_is_detected_only_when_toml_is_missing() {
    let directory = tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    let legacy_path = directory.path().join("config.kv");
    fs::write(
        &legacy_path,
        "model=qwen3:1.7b\nbase_url=http://localhost:11434/v1\n",
    )
    .expect("write legacy config");

    let error = XanaConfig::load_from(&config_path).expect_err("legacy file should fail");

    assert!(matches!(
        error,
        ConfigError::LegacyConfigFound {
            legacy_path: actual_legacy,
            config_path: actual_config,
        } if actual_legacy == legacy_path && actual_config == config_path
    ));
}

#[test]
fn config_toml_wins_when_both_files_exist() {
    let directory = tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    let legacy_path = directory.path().join("config.kv");
    fs::write(&config_path, MINIMAL).expect("write TOML config");
    fs::write(&legacy_path, "not even valid legacy syntax").expect("write legacy config");

    let config = XanaConfig::load_from(&config_path).expect("TOML wins");

    assert_eq!(config.model, "qwen3:1.7b");
}

#[test]
fn malformed_toml_preserves_the_decode_source() {
    let directory = tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, "version = [").expect("write malformed config");

    let error = XanaConfig::load_from(&path).expect_err("malformed TOML should fail");

    assert!(matches!(&error, ConfigError::Decode(_)));
    assert!(error.source().is_some());
}

#[test]
fn missing_configuration_reports_the_config_toml_path() {
    let directory = tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");

    let error = XanaConfig::load_from(&path).expect_err("missing config should fail");

    assert!(matches!(
        error,
        ConfigError::Io {
            path: actual_path,
            source,
        } if actual_path == path && source.kind() == io::ErrorKind::NotFound
    ));
}

fn initial_config() -> InitialConfig {
    InitialConfig {
        provider_name: "ollama".to_owned(),
        base_url: "http://localhost:11434/v1".to_owned(),
        model: "qwen3:1.7b".to_owned(),
        max_tool_rounds: 12,
    }
}

#[test]
fn rendered_initial_config_round_trips_through_the_real_loader() {
    let rendered = XanaConfig::render_initial(initial_config()).expect("render config");
    let parsed = XanaConfig::parse(&rendered).expect("parse rendered config");

    assert_eq!(
        parsed,
        XanaConfig {
            provider_name: "ollama".to_owned(),
            provider_kind: ProviderKind::OpenAiCompat,
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "qwen3:1.7b".to_owned(),
            permission_mode: PermissionMode::Allow,
            max_tool_rounds: 12,
        }
    );
    assert!(rendered.contains("permission_mode = \"allow\""));
}

#[test]
fn rendered_initial_config_escapes_model_text_as_toml() {
    let mut input = initial_config();
    input.model = "model \\\"quoted\\\"\\nnext".to_owned();

    let rendered = XanaConfig::render_initial(input.clone()).expect("render escaped config");
    let parsed = XanaConfig::parse(&rendered).expect("parse escaped config");

    assert_eq!(parsed.model, input.model);
}

#[test]
fn invalid_initial_provider_name_uses_the_existing_validation_error() {
    let mut input = initial_config();
    input.provider_name = "Bad Provider".to_owned();

    let error = XanaConfig::render_initial(input).expect_err("invalid provider should fail");

    assert!(matches!(
        error,
        ConfigError::InvalidName {
            section: "provider",
            name,
        } if name == "Bad Provider"
    ));
}

#[test]
fn invalid_initial_url_uses_the_existing_validation_error() {
    let mut input = initial_config();
    input.base_url = "/v1".to_owned();

    let error = XanaConfig::render_initial(input).expect_err("invalid URL should fail");

    assert!(matches!(
        error,
        ConfigError::InvalidBaseUrl { provider, .. } if provider == "ollama"
    ));
}

#[test]
fn invalid_initial_round_limit_uses_the_existing_validation_error() {
    let mut input = initial_config();
    input.max_tool_rounds = 0;

    let error = XanaConfig::render_initial(input).expect_err("invalid rounds should fail");

    assert!(matches!(
        error,
        ConfigError::InvalidToolRoundLimit {
            profile,
            value: 0,
        } if profile == "default"
    ));
}
