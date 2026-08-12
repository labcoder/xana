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
fn v3_registry_preserves_complete_profiles_and_exact_routes() {
    let input = r#"
version = 3
default_profile = "default"
default_child_route = "worker"
permission_mode = "ask"

[providers.local]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[providers.remote]
kind = "openai_compat"
base_url = "https://example.test/v1"

[profiles.default]
connection = "local"
model = "root-model"

[profiles.worker]
connection = "remote"
model = "worker-model"
reasoning_effort = "high"
reasoning_summary = "concise"
capabilities = ["fs.read", "xana.docs.read"]
permission_mode = "deny"
max_tool_rounds = 4

[profiles.worker.orchestration]
max_fan_out = 3
max_descendants = 5
max_concurrency = 2
deadline_seconds = 90
max_context_tokens = 4096
max_report_bytes = 8192
max_artifact_bytes = 65536

[routes.worker]
profile = "worker"
"#;
    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, input).expect("write config");

    let registry = XanaConfig::load_registry_from(&path).expect("load registry");

    assert_eq!(registry.default_child_route.as_deref(), Some("worker"));
    assert_eq!(registry.routes["worker"].profile, "worker");
    let worker = &registry.profiles["worker"];
    assert_eq!(worker.connection, "remote");
    assert_eq!(worker.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(worker.reasoning_summary.as_deref(), Some("concise"));
    assert_eq!(
        worker.capabilities.as_deref(),
        Some(["fs.read".to_owned(), "xana.docs.read".to_owned()].as_slice())
    );
    assert_eq!(worker.permission_mode, Some(PermissionMode::Deny));
    assert_eq!(worker.orchestration.max_fan_out, 3);
    assert_eq!(worker.orchestration.max_descendants, 5);
    assert_eq!(worker.orchestration.max_concurrency, 2);
    assert_eq!(worker.orchestration.deadline_seconds, 90);
    assert_eq!(worker.orchestration.max_context_tokens, 4096);
    assert_eq!(worker.orchestration.max_report_bytes, 8192);
    assert_eq!(worker.orchestration.max_artifact_bytes, 65536);
}

#[test]
fn v4_registry_models_interoperable_declarations_without_secret_values() {
    let input = r#"
version = 4
default_profile = "default"
permission_mode = "ask"

[providers.local]
kind = "ollama"

[profiles.default]
connection = "local"
model = "qwen"
identity = "Help with this project."
skills = ["repo.review"]
plugins = ["reviewer"]
mcp_servers = ["files"]
external_agents = ["research"]
service_routes = ["illustrate"]
egress_policy = "selected"
applies_to = ["primary"]

[plugins.reviewer]
source = { kind = "git", url = "https://example.test/reviewer.git", revision = "0123456789abcdef" }
enabled = true

[mcp_servers.files]
transport = "stdio"
command = "files-server"
args = ["--read-only"]
enabled = true
egress_policy = "selected"

[external_agents.research]
endpoint = "https://agents.example.test/a2a"
credential = { source = "stored", id = "research" }
enabled = true
egress_policy = "selected"

[service_connections.images]
adapter = "openai.images"
credential = { source = "environment", variable = "OPENAI_API_KEY" }

[service_routes.illustrate]
operation = "image.generate"
connection = "images"
model = "image-model"
egress_policy = "selected"

[egress_policies.selected]
allowed = ["prompt_text", "selected_artifacts"]
"#;

    let registry = XanaConfig::parse_registry(input).expect("v4 registry");

    let profile = &registry.profiles["default"];
    assert_eq!(profile.identity.as_deref(), Some("Help with this project."));
    assert_eq!(profile.applies_to, vec![ProfileUse::Primary]);
    assert_eq!(profile.skills, ["repo.review"]);
    assert!(registry.plugins["reviewer"].enabled);
    assert!(matches!(
        registry.mcp_servers["files"],
        McpServerDeclaration::Stdio { enabled: true, .. }
    ));
    assert!(registry.external_agents["research"].enabled);
    assert_eq!(
        registry.service_routes["illustrate"].operation,
        "image.generate"
    );
    assert_eq!(
        registry.egress_policies["selected"].allowed,
        vec![
            interoperable::OutboundDataClass::PromptText,
            interoperable::OutboundDataClass::SelectedArtifacts
        ]
    );
    assert!(!input.contains("api_key ="));
}

#[test]
fn interoperable_references_and_endpoint_credentials_fail_closed() {
    let unknown = MINIMAL.replace(
        "model = \"qwen3:1.7b\"",
        "model = \"qwen3:1.7b\"\nplugins = [\"missing\"]",
    );
    assert!(matches!(
        parse_error(&unknown),
        ConfigError::InvalidInteroperableConfig {
            section: "profile plugins",
            ..
        }
    ));

    let embedded_secret = format!(
        "{}\n[external_agents.remote]\nendpoint = \"https://token@example.test/a2a\"\n",
        MINIMAL.replace("version = 1", "version = 4")
    );
    assert!(matches!(
        parse_error(&embedded_secret),
        ConfigError::InvalidInteroperableConfig {
            section: "external agent",
            ..
        }
    ));
}

#[test]
fn profile_connection_migration_rejects_conflicting_keys() {
    let input = MINIMAL.replace(
        "provider = \"local\"",
        "provider = \"local\"\nconnection = \"local\"",
    );

    assert!(matches!(parse_error(&input), ConfigError::Decode(_)));
}

#[test]
fn routes_and_orchestration_limits_are_validated_before_resolution() {
    let unknown_profile = format!(
        "{}\n[routes.worker]\nprofile = \"missing\"\n",
        MINIMAL.replace("version = 1", "version = 3").replace(
            "default_profile = \"default\"",
            "default_profile = \"default\"\ndefault_child_route = \"worker\""
        )
    );
    assert!(matches!(
        parse_error(&unknown_profile),
        ConfigError::UnknownProfile { route, profile }
            if route == "worker" && profile == "missing"
    ));

    let invalid_limit = MINIMAL.replace(
        "model = \"qwen3:1.7b\"",
        "model = \"qwen3:1.7b\"\n[profiles.default.orchestration]\nmax_concurrency = 0",
    );
    assert!(matches!(
        parse_error(&invalid_limit),
        ConfigError::InvalidOrchestrationLimit {
            profile,
            field: "max_concurrency",
            value: 0,
            ..
        } if profile == "default"
    ));
}

#[test]
fn oversized_configuration_is_rejected_before_toml_decoding() {
    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, vec![b'x'; MAX_CONFIG_BYTES + 1]).expect("oversized config");

    assert!(matches!(
        XanaConfig::load_from(&path),
        Err(ConfigError::TooLarge { .. })
    ));
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
fn codex_connection_delegates_credentials_and_requires_local_runtime_fields() {
    let valid = MINIMAL.replace(
        "kind = \"openai_compat\"\nbase_url = \"http://localhost:11434/v1\"",
        "kind = \"codex\"\ncodex_program = \"codex\"",
    );
    assert_eq!(parse_ok(&valid).provider_kind, ProviderKind::Codex);

    let with_credential = valid.replace(
        "codex_program = \"codex\"",
        "codex_program = \"codex\"\ncredential = { source = \"stored\", id = \"codex\" }",
    );
    assert!(matches!(
        parse_error(&with_credential),
        ConfigError::InvalidCredential { .. }
    ));

    let with_relative_home = valid.replace(
        "codex_program = \"codex\"",
        "codex_program = \"codex\"\ncodex_home = \"relative/home\"",
    );
    assert!(matches!(
        parse_error(&with_relative_home),
        ConfigError::InvalidCodexHome { .. }
    ));
}

#[test]
fn api_connections_require_explicit_non_secret_credential_references() {
    let missing = MINIMAL.replace(
        "kind = \"openai_compat\"\nbase_url = \"http://localhost:11434/v1\"",
        "kind = \"openrouter\"",
    );
    assert!(matches!(
        parse_error(&missing),
        ConfigError::InvalidCredential { .. }
    ));

    let stored = missing.replace(
        "kind = \"openrouter\"",
        "kind = \"openrouter\"\ncredential = { source = \"stored\", id = \"openrouter\" }",
    );
    let parsed = parse_ok(&stored);
    assert_eq!(
        parsed.credential,
        Some(CredentialReference::Stored {
            id: "openrouter".into()
        })
    );
    assert!(!stored.contains("api_key"));
}

#[test]
fn connection_edits_preserve_comments_and_validate_the_complete_document() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(
        &path,
        MINIMAL.replace("version = 1", "# keep me\nversion = 1"),
    )
    .unwrap();

    XanaConfig::add_connection(
        &path,
        NewConnection {
            id: "codex".into(),
            kind: ProviderKind::Codex,
            base_url: None,
            credential: None,
            model: "gpt-5.3-codex".into(),
            codex_program: Some("codex".into()),
            codex_home: None,
        },
    )
    .unwrap();

    let edited = fs::read_to_string(&path).unwrap();
    assert!(edited.contains("# keep me"));
    assert!(edited.contains("version = 4"));
    let registry = XanaConfig::load_registry_from(&path).unwrap();
    assert_eq!(registry.connections["codex"].kind, ProviderKind::Codex);
}

#[test]
fn structured_writes_migrate_legacy_profile_keys_to_connection() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(
        &path,
        MINIMAL.replace("version = 1", "# preserve this\nversion = 2"),
    )
    .unwrap();

    XanaConfig::add_connection(
        &path,
        NewConnection {
            id: "codex".into(),
            kind: ProviderKind::Codex,
            base_url: None,
            credential: None,
            model: "gpt-5.6-sol".into(),
            codex_program: Some("codex".into()),
            codex_home: None,
        },
    )
    .unwrap();

    let edited = fs::read_to_string(&path).unwrap();
    assert!(edited.contains("# preserve this"));
    assert!(edited.contains("version = 4"));
    assert!(edited.contains("connection = \"local\""));
    assert!(!edited.contains("provider = \"local\""));
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
fn deny_ask_and_allow_permission_modes_are_accepted() {
    for (name, expected) in [
        ("deny", PermissionMode::Deny),
        ("ask", PermissionMode::Ask),
        ("allow", PermissionMode::Allow),
    ] {
        let input = MINIMAL.replace(
            "permission_mode = \"allow\"",
            &format!("permission_mode = {name:?}"),
        );
        assert_eq!(parse_ok(&input).permission_mode, expected);
    }
}

#[test]
fn permission_rules_decode_and_retain_lexical_workspace_paths() {
    let input = format!(
        "{MINIMAL}\n[[permission_rules]]\nid = \"allow-reads\"\ndecision = \"allow\"\neffect = \"read\"\nworkspace = \".\"\n"
    );
    let config = parse_ok(&input);

    assert_eq!(config.permission_rules.len(), 1);
    assert_eq!(config.permission_rules[0].id, "allow-reads");
    assert_eq!(
        config.permission_rules[0].workspace.as_deref(),
        Some(std::path::Path::new("."))
    );
}

#[test]
fn invalid_permission_rules_use_a_structured_config_error() {
    let duplicate = format!(
        "{MINIMAL}\n[[permission_rules]]\nid = \"same\"\ndecision = \"ask\"\ntool = \"read_file\"\n\n[[permission_rules]]\nid = \"same\"\ndecision = \"deny\"\neffect = \"write\"\n"
    );
    let error = parse_error(&duplicate);

    assert!(matches!(
        error,
        ConfigError::InvalidPermissionPolicy(PolicyError::DuplicateRuleId(id)) if id == "same"
    ));
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
        connection: InitialConnection::Ollama {
            name: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
        },
        model: "qwen3:1.7b".to_owned(),
        max_tool_rounds: 12,
        shell: crate::shell::ShellConfig::default(),
        permission_mode: PermissionMode::Ask,
        reasoning_effort: None,
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
            provider_kind: ProviderKind::Ollama,
            base_url: "http://localhost:11434/v1".to_owned(),
            credential: None,
            codex_program: None,
            codex_home: None,
            model: "qwen3:1.7b".to_owned(),
            permission_mode: PermissionMode::Ask,
            permission_rules: Vec::new(),
            shell: crate::shell::ShellConfig::default(),
            max_tool_rounds: 12,
        }
    );
    assert!(rendered.contains("permission_mode = \"ask\""));
    assert!(rendered.contains("version = 4"));
    assert!(rendered.contains("default_child_route = \"default\""));
    assert!(rendered.contains("connection = \"ollama\""));
    assert!(!rendered.contains("provider = \"ollama\""));
    assert!(rendered.contains("[routes.default]"));
    assert!(rendered.contains("[shell]"));
    assert!(rendered.contains("kind = \"platform\""));
}

#[test]
fn missing_shell_table_uses_the_platform_default() {
    let parsed = parse_ok(MINIMAL);

    assert_eq!(parsed.shell, crate::shell::ShellConfig::default());
}

#[test]
fn incompatible_shell_kind_is_rejected_during_config_validation() {
    #[cfg(windows)]
    let kind = "posix";
    #[cfg(unix)]
    let kind = "powershell";
    let input = format!("{MINIMAL}\n[shell]\nkind = {kind:?}\n");

    let error = XanaConfig::parse(&input).expect_err("incompatible shell should fail");

    assert!(matches!(error, ConfigError::InvalidShell(_)));
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
    let InitialConnection::Ollama { name, .. } = &mut input.connection else {
        unreachable!("fixture uses Ollama")
    };
    *name = "Bad Provider".to_owned();

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
    let InitialConnection::Ollama { base_url, .. } = &mut input.connection else {
        unreachable!("fixture uses Ollama")
    };
    *base_url = "/v1".to_owned();

    let error = XanaConfig::render_initial(input).expect_err("invalid URL should fail");

    assert!(matches!(
        error,
        ConfigError::InvalidBaseUrl { provider, .. } if provider == "ollama"
    ));
}

#[test]
fn rendered_initial_codex_config_has_no_http_or_credential_fields() {
    let mut input = initial_config();
    input.connection = InitialConnection::Codex {
        name: "codex".to_owned(),
        program: "codex-preview".to_owned(),
        home: None,
    };
    input.model = "gpt-5.6-sol".to_owned();

    let rendered = XanaConfig::render_initial(input).expect("render Codex config");
    let parsed = XanaConfig::parse(&rendered).expect("parse Codex config");

    assert_eq!(parsed.provider_kind, ProviderKind::Codex);
    assert_eq!(parsed.codex_program.as_deref(), Some("codex-preview"));
    assert_eq!(parsed.base_url, "codex-app-server://stdio");
    assert!(parsed.credential.is_none());
    assert!(!rendered.contains("base_url"));
    assert!(!rendered.contains("credential"));
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
