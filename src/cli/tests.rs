use super::*;
use clap::{CommandFactory as _, error::ErrorKind};

#[test]
fn visible_cli_families_have_shared_catalog_projections() {
    let catalog = crate::command_catalog::commands_for(crate::command_catalog::CommandSurface::Cli)
        .map(|command| command.name)
        .collect::<std::collections::HashSet<_>>();
    let cli = Cli::command();
    let missing = cli
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(clap::Command::get_name)
        .filter(|name| *name != "help")
        .filter(|name| !catalog.contains(name))
        .collect::<Vec<_>>();
    assert!(missing.is_empty(), "missing catalog families: {missing:?}");
}

#[test]
fn conversation_is_canonical_and_session_remains_compatible() {
    let canonical = Cli::try_parse_from(["xana", "conversation", "list"]).unwrap();
    let compatibility = Cli::try_parse_from(["xana", "session", "list"]).unwrap();
    assert_eq!(canonical.command, compatibility.command);
    assert!(matches!(
        canonical.command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::List
        }))
    ));
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

    let stream = Cli::try_parse_from(["xana", "--output", "stream-json", "-p", "hello"])
        .expect("private JSONL stream");
    assert_eq!(stream.output, Some(OutputChoice::StreamJson));

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
            json: false,
            command: ConnectionCommand::Add {
                id: "codex".into(),
                kind: ConnectionKindChoice::Codex,
                base_url: None,
                env: None,
                credential_id: None,
                key_from_stdin: false,
                model: "gpt-5.3-codex".into(),
                codex_program: None,
                codex_home: None,
                yes: false,
                dry_run: false,
            }
        }))
    );
    for action in ["test", "repair", "refresh"] {
        assert!(matches!(
            Cli::try_parse_from(["xana", "connection", action, "codex"])
                .unwrap()
                .command,
            Some(Command::Connection(_))
        ));
    }
    assert!(Cli::try_parse_from(["xana", "connection", "delete-key", "openai"]).is_ok());
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
fn parses_provider_neutral_usage_query() {
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "usage",
            "--connection",
            "openrouter",
            "--model",
            "openai/gpt-5",
            "--refresh",
            "--json",
        ])
        .unwrap()
        .command,
        Some(Command::Usage(UsageArgs {
            connection: Some("openrouter".into()),
            model: Some("openai/gpt-5".into()),
            refresh: true,
            json: true,
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
fn parses_explicit_quick_setup_without_the_path_menu() {
    assert!(matches!(
        Cli::try_parse_from(["xana", "setup", "--quick"])
            .unwrap()
            .command,
        Some(Command::Setup(args)) if args.quick
    ));
}

#[test]
fn parses_explicit_blank_setup_without_provider_fiction() {
    assert!(matches!(
        Cli::try_parse_from(["xana", "setup", "--blank", "--non-interactive", "--yes"])
            .unwrap()
            .command,
        Some(Command::Setup(args)) if args.blank && args.non_interactive && args.yes
    ));
    assert!(Cli::try_parse_from(["xana", "setup", "--blank", "--quick"]).is_err());
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
fn parses_config_edit_and_migrate() {
    let edit = Cli::try_parse_from(["xana", "config", "edit", "--editor", "code"])
        .expect("config edit command");
    let migrate = Cli::try_parse_from(["xana", "config", "migrate", "--apply"])
        .expect("config migrate command");

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
fn parses_bounded_local_diagnostic_commands() {
    assert!(matches!(
        Cli::try_parse_from(["xana", "logs", "path"])
            .unwrap()
            .command,
        Some(Command::Logs(LogsArgs {
            command: LogsCommand::Path
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from(["xana", "logs", "show", "xana-1.jsonl", "--lines", "50"])
            .unwrap()
            .command,
        Some(Command::Logs(LogsArgs {
            command: LogsCommand::Show { lines: 50, .. }
        }))
    ));
    assert!(
        Cli::try_parse_from(["xana", "logs", "show", "xana-1.jsonl", "--lines", "1001"]).is_err()
    );
    assert!(matches!(
        Cli::try_parse_from(["xana", "outbound", "list"])
            .unwrap()
            .command,
        Some(Command::Outbound(OutboundArgs {
            command: OutboundCommand::List
        }))
    ));
    let digest = "a".repeat(64);
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "outbound",
            "revoke",
            &digest,
            "selected-artifacts",
            "--yes"
        ])
        .unwrap()
        .command,
        Some(Command::Outbound(OutboundArgs {
            command: OutboundCommand::Revoke {
                class: OutboundClassChoice::SelectedArtifacts,
                yes: true,
                ..
            }
        }))
    ));
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
        Cli::try_parse_from(["xana", "conversation", "continue"])
            .unwrap()
            .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::Continue
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from(["xana", "conversation", "attach", &id.to_string()])
            .unwrap()
            .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::Attach { conversation }
        })) if conversation == id.to_string()
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "conversation",
            "preview",
            &id.to_string(),
            "--limit",
            "32",
            "--json"
        ])
        .unwrap()
        .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::Preview {
                conversation,
                limit: 32,
                json: true,
            }
        })) if conversation == id.to_string()
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "conversation",
            "search",
            "needle",
            "--conversation",
            &id.to_string(),
            "--limit",
            "7",
            "--json"
        ])
        .unwrap()
        .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::Search {
                query,
                conversation: Some(selector),
                limit: 7,
                json: true,
            }
        })) if query == "needle" && selector == id.to_string()
    ));
    let conversation = crate::identity::ConversationId::new();
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "session",
            "branch",
            &conversation.to_string(),
            "--at",
            "current"
        ])
        .unwrap()
        .command,
        Some(Command::Session(SessionArgs {
            command: SessionCommand::Branch {
                conversation_id,
                at,
            }
        })) if conversation_id == conversation && at == "current"
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
fn parses_optional_project_lifecycle_and_placement_commands() {
    let project = ProjectId::new();
    assert!(matches!(
        Cli::try_parse_from(["xana", "project", "create", "Xana"])
            .unwrap()
            .command,
        Some(Command::Project(ProjectArgs {
            command: ProjectCommand::Create {
                workspace: None,
                ..
            }
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "project",
            "continue",
            &project.to_string(),
            "conversation-1",
            "--owner",
            "codex",
        ])
        .unwrap()
        .command,
        Some(Command::Project(ProjectArgs {
            command: ProjectCommand::Continue {
                project_id,
                owner: ProjectOwnerChoice::ManagedCodex,
                ..
            }
        })) if project_id == project
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "project",
            "forget",
            &project.to_string(),
            "--yes",
        ])
        .unwrap()
        .command,
        Some(Command::Project(ProjectArgs {
            command: ProjectCommand::Forget {
                project_id,
                yes: true,
            }
        })) if project_id == project
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "project",
            "bind",
            &project.to_string(),
            "chat",
            "ollama",
        ])
        .unwrap()
        .command,
        Some(Command::Project(ProjectArgs {
            command: ProjectCommand::Bind {
                project_id,
                logical,
                local,
            }
        })) if project_id == project && logical == "chat" && local == "ollama"
    ));
}

#[test]
fn parses_profile_lifecycle_and_immutable_continuation_commands() {
    let project = ProjectId::new();
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "profile",
            "create",
            "review",
            "--connection",
            "chat",
            "--model",
            "qwen",
            "--project",
            &project.to_string(),
            "--authority-profile",
            "default",
        ])
        .unwrap()
        .command,
        Some(Command::Profile(ProfileArgs {
            command: ProfileCommand::Create {
                name,
                project: Some(project_id),
                authority_profile: Some(authority),
                ..
            }
        })) if name == "review" && project_id == project && authority == "default"
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "profile",
            "continue",
            "review",
            "source-conversation",
        ])
        .unwrap()
        .command,
        Some(Command::Profile(ProfileArgs {
            command: ProfileCommand::Continue { name, conversation, project: None }
        })) if name == "review" && conversation == "source-conversation"
    ));
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
fn parses_plugin_review_and_exact_git_install() {
    assert!(matches!(
        Cli::try_parse_from(["xana", "plugin", "review", ".", "--linked"])
            .unwrap()
            .command,
        Some(Command::Plugin(PluginArgs {
            command: PluginCommand::Review { linked: true, .. }
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "plugin",
            "install",
            "https://example.com/plugin.git",
            "--git",
            "--revision",
            "0123456789012345678901234567890123456789",
            "--yes",
        ])
        .unwrap()
        .command,
        Some(Command::Plugin(PluginArgs {
            command: PluginCommand::Install {
                git: true,
                yes: true,
                ..
            }
        }))
    ));
}

#[test]
fn parses_plugin_lifecycle_and_scopes() {
    let project = ProjectId::new();
    for arguments in [
        vec!["xana", "plugin", "inspect", "quality"],
        vec![
            "xana",
            "plugin",
            "enable",
            "quality",
            "--profile",
            "default",
        ],
        vec![
            "xana",
            "plugin",
            "disable",
            "quality",
            "--project",
            &project.to_string(),
        ],
        vec![
            "xana",
            "plugin",
            "update-check",
            "quality",
            "--revision",
            "0123456789012345678901234567890123456789",
        ],
        vec!["xana", "plugin", "rollback", "quality", "--yes"],
        vec!["xana", "plugin", "remove", "quality", "--yes"],
        vec!["xana", "plugin", "gc", "--yes"],
    ] {
        assert!(matches!(
            Cli::try_parse_from(arguments).unwrap().command,
            Some(Command::Plugin(_))
        ));
    }
}

#[test]
fn parses_mcp_discovery_read_and_prompt_commands() {
    assert_eq!(
        Cli::try_parse_from(["xana", "mcp", "tools", "docs", "--query", "read"])
            .unwrap()
            .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::Tools {
                server: "docs".into(),
                query: "read".into(),
            },
        }))
    );
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "mcp",
            "prompt",
            "docs",
            "review",
            "--arg",
            "text=hello"
        ])
        .unwrap()
        .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::Prompt { .. }
        }))
    ));
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "mcp",
            "serve",
            "--workspace",
            ".",
            "--profile",
            "default",
            "--allow",
            "xana_docs"
        ])
        .unwrap()
        .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::Serve {
                workspace: PathBuf::from("."),
                profile: "default".into(),
                allow: vec!["xana_docs".into()],
            },
        }))
    );
}

#[test]
fn parses_exact_mcp_configuration_commands() {
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "mcp",
            "add-stdio",
            "docs",
            "--command",
            "docs-server",
            "--arg",
            "--stdio",
            "--allow-tool",
            "read",
            "--yes",
        ])
        .unwrap()
        .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::AddStdio { server, tools, yes: true, .. }
        })) if server == "docs" && tools == ["read"]
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "mcp",
            "add-http",
            "remote",
            "--url",
            "https://mcp.example.test/rpc",
            "--credential-env",
            "MCP_TOKEN",
            "--yes",
        ])
        .unwrap()
        .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::AddHttp { server, yes: true, .. }
        })) if server == "remote"
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "xana",
            "mcp",
            "add-http",
            "oauth",
            "--url",
            "https://mcp.example.test/rpc",
            "--oauth-credential-id",
            "mcp-oauth",
            "--oauth-issuer",
            "https://issuer.example.test/",
            "--oauth-client-id",
            "xana-local",
            "--oauth-scope",
            "tools.read",
            "--yes",
        ])
        .unwrap()
        .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::AddHttp {
                oauth_client_id: Some(client_id),
                oauth_scopes,
                ..
            }
        })) if client_id == "xana-local" && oauth_scopes == ["tools.read"]
    ));
    assert!(matches!(
        Cli::try_parse_from(["xana", "mcp", "login", "oauth"])
            .unwrap()
            .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::Login { server }
        })) if server == "oauth"
    ));
    assert!(matches!(
        Cli::try_parse_from(["xana", "mcp", "logout", "oauth", "--yes"])
            .unwrap()
            .command,
        Some(Command::Mcp(McpArgs {
            command: McpCommand::Logout { server, yes: true }
        })) if server == "oauth"
    ));
}

#[test]
fn parses_external_agent_discovery_and_trust_lifecycle() {
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "external-agent",
            "add",
            "research",
            "--endpoint",
            "https://agent.example/.well-known/agent-card.json",
            "--credential-id",
            "a2a-research"
        ])
        .unwrap()
        .command,
        Some(Command::ExternalAgent(ExternalAgentArgs {
            command: ExternalAgentCommand::Add {
                name: "research".into(),
                endpoint: "https://agent.example/.well-known/agent-card.json".into(),
                env: None,
                credential_id: Some("a2a-research".into()),
                egress_policy: None,
            },
        }))
    );
    assert!(matches!(
        Cli::try_parse_from(["xana", "external-agent", "trust", "research", "--yes"])
            .unwrap()
            .command,
        Some(Command::ExternalAgent(ExternalAgentArgs {
            command: ExternalAgentCommand::Trust { yes: true, .. }
        }))
    ));
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "external-agent",
            "cancel",
            "research",
            "task-1",
            "--yes"
        ])
        .unwrap()
        .command,
        Some(Command::ExternalAgent(ExternalAgentArgs {
            command: ExternalAgentCommand::Cancel {
                name: "research".into(),
                task_id: "task-1".into(),
                yes: true,
            },
        }))
    );
}

#[test]
fn parses_focused_image_commands() {
    assert_eq!(
        Cli::try_parse_from(["xana", "image", "list", "--json"])
            .unwrap()
            .command,
        Some(Command::Image(ImageArgs {
            command: ImageCommand::List { json: true },
        }))
    );
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "image",
            "generate",
            "a safe diagram",
            "--route",
            "illustrate",
            "--yes",
        ])
        .unwrap()
        .command,
        Some(Command::Image(ImageArgs {
            command: ImageCommand::Generate {
                prompt: Some("a safe diagram".into()),
                route: Some("illustrate".into()),
                yes: true,
                json: false,
            },
        }))
    );
}

#[test]
fn parses_focused_vision_commands() {
    assert_eq!(
        Cli::try_parse_from(["xana", "vision", "list", "--json"])
            .unwrap()
            .command,
        Some(Command::Vision(VisionArgs {
            command: VisionCommand::List { json: true },
        }))
    );
    assert_eq!(
        Cli::try_parse_from([
            "xana",
            "vision",
            "analyze",
            "first.png",
            "second.jpg",
            "--question",
            "Compare these images",
            "--route",
            "describe",
            "--yes",
        ])
        .unwrap()
        .command,
        Some(Command::Vision(VisionArgs {
            command: VisionCommand::Analyze {
                images: vec!["first.png".into(), "second.jpg".into()],
                question: Some("Compare these images".into()),
                route: Some("describe".into()),
                yes: true,
                json: false,
            },
        }))
    );
}

#[test]
fn parses_exact_noninteractive_vision_connection_setup() {
    let cli = Cli::try_parse_from([
        "xana",
        "connect",
        "vision",
        "--route",
        "describe",
        "--service-connection",
        "openai-vision",
        "--service-provider",
        "openai",
        "--model",
        "gpt-vision",
        "--credential-env",
        "OPENAI_API_KEY",
        "--make-default",
        "--yes",
    ])
    .unwrap();

    assert_eq!(
        cli.command,
        Some(Command::Connect(ConnectArgs {
            integration: Some(ConnectIntegration::Vision),
            route: Some("describe".into()),
            service_connection: Some("openai-vision".into()),
            service_provider: Some(FocusedServiceProviderChoice::OpenAi),
            model: Some("gpt-vision".into()),
            credential_env: Some("OPENAI_API_KEY".into()),
            base_url: None,
            profile: None,
            make_default: true,
            remove: false,
            yes: true,
        }))
    );
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
