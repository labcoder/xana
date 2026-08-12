use super::*;
use clap::error::ErrorKind;

#[test]
fn no_subcommand_means_normal_chat() {
    let cli = Cli::try_parse_from(["xana"]).expect("bare invocation");

    assert!(!cli.no_banner);
    assert_eq!(cli.resume, None);
    assert_eq!(cli.command, None);
}

#[test]
fn parses_plain_tui_and_one_shot_surface_contracts() {
    let plain = Cli::try_parse_from(["xana", "--plain"]).expect("plain surface");
    assert!(plain.plain);
    assert!(!plain.tui);

    let tui = Cli::try_parse_from(["xana", "--tui"]).expect("TUI surface");
    assert!(tui.tui);
    assert!(!tui.plain);

    let argument = Cli::try_parse_from(["xana", "-p", "hello"]).expect("one-shot argument surface");
    assert_eq!(argument.print, Some(Some("hello".to_owned())));

    let stdin = Cli::try_parse_from(["xana", "--print"]).expect("one-shot stdin surface");
    assert_eq!(stdin.print, Some(None));

    let json = Cli::try_parse_from(["xana", "--json", "-p", "hello"]).expect("JSON alias");
    assert!(json.json);
    assert_eq!(json.output, None);

    let continued = Cli::try_parse_from(["xana", "--continue"]).expect("continuation");
    assert!(continued.continue_chat);
    let compatibility =
        Cli::try_parse_from(["xana", "--continue-chat"]).expect("continuation alias");
    assert!(compatibility.continue_chat);

    assert!(Cli::try_parse_from(["xana", "--plain", "--tui"]).is_err());
    let conflict = Cli::try_parse_from([
        "xana",
        "--resume",
        "9eb8cfe0-2b3a-4c7b-9dc9-0a34f6490bf3",
        "--continue",
    ])
    .expect_err("resume and continue conflict");
    assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn parses_auth_lifecycle_commands() {
    assert_eq!(
        Cli::try_parse_from(["xana", "auth", "status", "codex"])
            .expect("auth status")
            .command,
        Some(Command::Auth(AuthArgs {
            command: AuthCommand::Status {
                provider: "codex".to_owned(),
            },
        }))
    );
}

#[test]
fn parses_connection_and_model_control_plane() {
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "connection",
            "add",
            "codex",
            "--kind",
            "codex",
            "--model",
            "gpt-5.3-codex",
        ])
        .unwrap()
        .command,
        Some(Command::Connection(ConnectionArgs {
            command: ConnectionCommand::Add {
                id: "codex".into(),
                kind: ConnectionKindChoice::Codex,
                base_url: None,
                env: None,
                credential_id: None,
                model: "gpt-5.3-codex".into(),
                codex_program: None,
                codex_home: None,
            }
        }))
    );
    assert_eq!(
        Cli::try_parse_from(["xana", "model", "use", "openrouter/openai/gpt-4.1"])
            .unwrap()
            .command,
        Some(Command::Model(ModelArgs {
            command: Some(ModelCommand::Use {
                selection: "openrouter/openai/gpt-4.1".into(),
                effort: None,
                summary: None,
            })
        }))
    );
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "model",
            "use",
            "codex/gpt-5.6-sol",
            "--effort",
            "xhigh",
            "--summary",
            "detailed",
        ])
        .unwrap()
        .command,
        Some(Command::Model(ModelArgs {
            command: Some(ModelCommand::Use {
                selection: "codex/gpt-5.6-sol".into(),
                effort: Some("xhigh".into()),
                summary: Some("detailed".into()),
            })
        }))
    );
}

#[test]
fn parses_read_only_route_diagnostics() {
    assert_eq!(
        Cli::try_parse_from(["xana", "route", "list"])
            .expect("route list")
            .command,
        Some(Command::Route(RouteArgs {
            command: RouteCommand::List,
        }))
    );
    assert_eq!(
        Cli::try_parse_from(["xana", "route", "check", "worker"])
            .expect("route check")
            .command,
        Some(Command::Route(RouteArgs {
            command: RouteCommand::Check {
                route: "worker".into(),
            },
        }))
    );
}

#[test]
fn parses_interactive_init() {
    let cli =
        Cli::try_parse_from(["xana", "init", "--dry-run"]).expect("interactive initialization");

    assert_eq!(
        cli.command,
        Some(Command::Init(InitArgs {
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
            dry_run: true,
        }))
    );
}

#[test]
fn parses_guarded_reset_and_clean_alias() {
    let reset = Cli::try_parse_from(["xana", "reset", "--yes"]).expect("reset command");
    let clean = Cli::try_parse_from(["xana", "clean"]).expect("clean alias");

    assert_eq!(
        reset.command,
        Some(Command::Reset(ResetArgs {
            yes: true,
            ..ResetArgs::default()
        }))
    );
    assert_eq!(
        clean.command,
        Some(Command::Reset(ResetArgs {
            yes: false,
            ..ResetArgs::default()
        }))
    );

    let scoped = Cli::try_parse_from([
        "xana",
        "reset",
        "--scope",
        "sessions",
        "--scope",
        "credentials",
        "--yes",
        "--credentials-yes",
    ])
    .expect("scoped reset");
    assert_eq!(
        scoped.command,
        Some(Command::Reset(ResetArgs {
            scope: vec![ResetScopeChoice::Sessions, ResetScopeChoice::Credentials],
            yes: true,
            credentials_yes: true,
            dry_run: false,
        }))
    );

    let doctor =
        Cli::try_parse_from(["xana", "doctor", "--output", "json"]).expect("doctor command");
    assert_eq!(
        doctor.command,
        Some(Command::Doctor(DoctorArgs {
            output: OutputChoice::Json,
            ..DoctorArgs::default()
        }))
    );
}

#[test]
fn parses_loopback_serve_and_workspace_attach() {
    let serve = Cli::try_parse_from(["xana", "serve", "--bind", "::1", "--port", "43123"])
        .expect("serve command");
    let attach = Cli::try_parse_from(["xana", "attach"]).expect("attach command");

    assert_eq!(
        serve.command,
        Some(Command::Serve(ServeArgs {
            bind: "::1".parse().unwrap(),
            port: 43123,
        }))
    );

    let artifact_id = ArtifactId::new();
    let artifact = Cli::try_parse_from(["xana", "attach", "--artifact", &artifact_id.to_string()])
        .expect("artifact attachment");
    assert!(matches!(
        artifact.command,
        Some(Command::Attach(AttachArgs {
            artifact: Some(actual),
            ..
        })) if actual == artifact_id
    ));
    assert_eq!(
        attach.command,
        Some(Command::Attach(AttachArgs {
            control: false,
            takeover: false,
            prompt: None,
            artifact: None,
        }))
    );

    let controller = Cli::try_parse_from([
        "xana",
        "attach",
        "--control",
        "--takeover",
        "--prompt",
        "continue",
    ])
    .expect("controller attach");
    assert_eq!(
        controller.command,
        Some(Command::Attach(AttachArgs {
            control: true,
            takeover: true,
            prompt: Some("continue".into()),
            artifact: None,
        }))
    );
}

#[test]
fn parses_complete_noninteractive_init() {
    let cli = Cli::try_parse_from([
        "xana",
        "init",
        "--non-interactive",
        "--kind",
        "ollama",
        "--provider-name",
        "ollama",
        "--base-url",
        "http://localhost:11434/v1",
        "--model",
        "qwen3:1.7b",
        "--max-tool-rounds",
        "12",
        "--shell",
        "powershell",
        "--shell-program",
        "pwsh.exe",
        "--permission-mode",
        "ask",
    ])
    .expect("complete noninteractive initialization");

    assert_eq!(
        cli.command,
        Some(Command::Init(InitArgs {
            non_interactive: true,
            kind: Some(InitConnectionKindChoice::Ollama),
            provider_name: Some("ollama".to_owned()),
            base_url: Some("http://localhost:11434/v1".to_owned()),
            codex_program: None,
            codex_home: None,
            model: Some("qwen3:1.7b".to_owned()),
            max_tool_rounds: Some(12),
            shell: Some(ShellChoice::PowerShell),
            shell_program: Some(PathBuf::from("pwsh.exe")),
            permission_mode: Some(PermissionChoice::Ask),
            dry_run: false,
        }))
    );
}

#[test]
fn parses_provider_neutral_noninteractive_setup_without_secret_argv() {
    let cli = Cli::try_parse_from([
        "xana",
        "setup",
        "--non-interactive",
        "--kind",
        "open-router",
        "--connection",
        "openrouter",
        "--credential-env",
        "OPENROUTER_API_KEY",
        "--model",
        "openai/gpt-4.1",
        "--permission-mode",
        "ask",
        "--yes",
    ])
    .expect("complete setup");

    assert_eq!(
        cli.command,
        Some(Command::Setup(Box::new(SetupArgs {
            non_interactive: true,
            kind: Some(ConnectionKindChoice::OpenRouter),
            connection: Some("openrouter".into()),
            base_url: None,
            codex_program: None,
            codex_home: None,
            credential_env: Some("OPENROUTER_API_KEY".into()),
            key_from_stdin: false,
            model: Some("openai/gpt-4.1".into()),
            reasoning_effort: None,
            permission_mode: Some(PermissionChoice::Ask),
            yes: true,
            dry_run: false,
            ..SetupArgs::default()
        })))
    );
}

#[test]
fn canonical_cli_values_are_human_spelled_and_legacy_values_remain_compatible() {
    for kind in ["openrouter", "open-router"] {
        let parsed = Cli::try_parse_from([
            "xana",
            "setup",
            "--kind",
            kind,
            "--shell",
            "git-bash",
            "--activity",
            "show",
        ])
        .expect("canonical or compatible setup spelling");
        assert!(matches!(
            parsed.command,
            Some(Command::Setup(args))
                if args.kind == Some(ConnectionKindChoice::OpenRouter)
                    && args.shell == Some(ShellChoice::GitBash)
                    && args.activity == Some(ActivityChoice::Open)
        ));
    }

    let legacy = Cli::try_parse_from([
        "xana",
        "setup",
        "--kind",
        "openai_compat",
        "--shell",
        "git_bash",
        "--activity",
        "hidden",
    ])
    .expect("legacy setup spelling");
    assert!(matches!(
        legacy.command,
        Some(Command::Setup(args))
            if args.kind == Some(ConnectionKindChoice::OpenAiCompat)
                && args.shell == Some(ShellChoice::GitBash)
                && args.activity == Some(ActivityChoice::Hidden)
    ));
}

#[test]
fn parses_installer_owned_setup_readiness_handoff() {
    let cli = Cli::try_parse_from(["xana", "setup", "--if-needed"])
        .expect("parse setup readiness handoff");
    assert!(matches!(
        cli.command,
        Some(Command::Setup(args)) if *args == SetupArgs {
            if_needed: true,
            ..SetupArgs::default()
        }
    ));
}

#[test]
fn parses_explicit_quick_setup_without_the_path_menu() {
    assert!(matches!(
        Cli::try_parse_from(["xana", "setup", "--quick"])
            .unwrap()
            .command,
        Some(Command::Setup(args)) if args.quick
    ));
}

#[test]
fn setup_readiness_handoff_rejects_setup_choices() {
    let cli = Cli::try_parse_from(["xana", "setup", "--if-needed", "--model", "llama3.2"])
        .expect("parse before application validation");
    let Some(Command::Setup(args)) = cli.command else {
        panic!("expected setup command");
    };
    assert!(args.if_needed);
    assert_ne!(
        *args,
        SetupArgs {
            if_needed: true,
            ..SetupArgs::default()
        }
    );
}

#[test]
fn parses_exact_sectional_setup_operations() {
    let appearance = Cli::try_parse_from([
        "xana",
        "setup",
        "--non-interactive",
        "--section",
        "appearance",
        "--theme",
        "monochrome",
        "--motion",
        "reduced",
        "--yes",
    ])
    .unwrap();
    assert!(matches!(
        appearance.command,
        Some(Command::Setup(args)) if matches!(*args, SetupArgs {
            section: Some(SetupSectionChoice::Appearance),
            theme: Some(ThemeChoice::Monochrome),
            motion: Some(MotionChoice::Reduced),
            ..
        })
    ));

    let routes = Cli::try_parse_from([
        "xana",
        "setup",
        "--non-interactive",
        "--section",
        "profiles-routes",
        "--profile",
        "reviewer",
        "--profile-connection",
        "ollama",
        "--profile-model",
        "qwen",
        "--route",
        "review",
        "--route-profile",
        "reviewer",
        "--max-concurrency",
        "2",
        "--yes",
    ])
    .unwrap();
    assert!(matches!(
        routes.command,
        Some(Command::Setup(args)) if matches!(*args, SetupArgs {
            section: Some(SetupSectionChoice::ProfilesRoutes),
            profile: Some(_),
            route: Some(_),
            max_concurrency: Some(2),
            ..
        })
    ));
}

#[test]
fn parses_managed_codex_init() {
    let cli = Cli::try_parse_from([
        "xana",
        "init",
        "--non-interactive",
        "--kind",
        "codex",
        "--provider-name",
        "codex",
        "--codex-program",
        "codex-preview",
        "--model",
        "gpt-5.6-sol",
        "--permission-mode",
        "ask",
    ])
    .expect("managed Codex initialization");

    assert!(matches!(
        cli.command,
        Some(Command::Init(InitArgs {
            kind: Some(InitConnectionKindChoice::Codex),
            codex_program: Some(program),
            base_url: None,
            ..
        })) if program == "codex-preview"
    ));
}

#[test]
fn parses_config_path_check_and_edit() {
    let path = Cli::try_parse_from(["xana", "config", "path"]).expect("config path command");
    let check = Cli::try_parse_from(["xana", "config", "check"]).expect("config check command");
    let edit = Cli::try_parse_from(["xana", "config", "edit", "--editor", "code"])
        .expect("config edit command");
    let migrate = Cli::try_parse_from(["xana", "config", "migrate", "--apply"])
        .expect("config migrate command");

    assert_eq!(
        path.command,
        Some(Command::Config(ConfigArgs {
            command: ConfigCommand::Path,
        }))
    );
    assert_eq!(
        check.command,
        Some(Command::Config(ConfigArgs {
            command: ConfigCommand::Check,
        }))
    );
    assert_eq!(
        edit.command,
        Some(Command::Config(ConfigArgs {
            command: ConfigCommand::Edit {
                editor: Some(PathBuf::from("code")),
            },
        }))
    );
    assert_eq!(
        migrate.command,
        Some(Command::Config(ConfigArgs {
            command: ConfigCommand::Migrate { apply: true },
        }))
    );
}

#[test]
fn parses_explicit_resume_and_session_inspection() {
    let id = SessionId::new();
    let resume =
        Cli::try_parse_from(["xana", "--resume", &id.to_string()]).expect("resume argument");
    let inspect = Cli::try_parse_from(["xana", "session", "inspect", &id.to_string()])
        .expect("session inspect command");

    assert_eq!(resume.resume, Some(id));
    assert_eq!(resume.command, None);
    assert_eq!(
        inspect.command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::Inspect { session_id: id },
        }))
    );
    assert!(matches!(
        Cli::try_parse_from(["xana", "session", "list"])
            .unwrap()
            .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::List
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from(["xana", "session", "new"])
            .unwrap()
            .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::New
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from(["xana", "session", "select", "codex", "thread-1"])
            .unwrap()
            .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::SelectManaged { .. }
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from(["xana", "session", "archive", "codex", "thread-1"])
            .unwrap()
            .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::ArchiveManaged { .. }
        }))
    ));
    for legacy in ["select-managed", "archive-managed"] {
        assert!(Cli::try_parse_from(["xana", "session", legacy, "codex", "thread-1"]).is_ok());
    }
}

#[test]
fn parses_operation_plan_and_resume() {
    let session_id = SessionId::new();
    let operation_id = crate::identity::OperationId::new();
    for action in ["plan", "resume"] {
        let parsed = Cli::try_parse_from([
            "xana",
            "operation",
            action,
            "--session",
            &session_id.to_string(),
            &operation_id.to_string(),
        ])
        .expect("operation command");
        assert!(matches!(parsed.command, Some(Command::Operation(_))));
    }
}

#[test]
fn rejects_unknown_commands_and_invalid_round_counts() {
    let unknown =
        Cli::try_parse_from(["xana", "unknown"]).expect_err("unknown command should fail");
    let invalid_rounds = Cli::try_parse_from(["xana", "init", "--max-tool-rounds", "not-a-number"])
        .expect_err("invalid count should fail");

    assert_eq!(unknown.kind(), ErrorKind::InvalidSubcommand);
    assert_eq!(invalid_rounds.kind(), ErrorKind::ValueValidation);
}

#[test]
fn no_banner_is_global() {
    let bare = Cli::try_parse_from(["xana", "--no-banner"]).expect("bare no-banner");
    let init = Cli::try_parse_from(["xana", "init", "--no-banner"]).expect("init no-banner");

    assert!(bare.no_banner);
    assert!(init.no_banner);
}
