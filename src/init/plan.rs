//! Pure and scripted initialization planning.

use crate::{
    cli::{InitArgs, InitConnectionKindChoice},
    config::{InitialConfig, InitialConnection, PermissionMode},
    shell::{ShellConfig, ShellKind},
};
use std::{
    error::Error,
    fmt,
    io::{self, BufRead, Write},
};

const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;
const MAX_PROMPT_ATTEMPTS: usize = 3;
const OLLAMA_CONNECTION_NAME: &str = "ollama";
const OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";
const CODEX_CONNECTION_NAME: &str = "codex";
const DEFAULT_CODEX_PROGRAM: &str = "codex";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InitPlan {
    Create(InitialConfig),
    Cancelled,
}

#[derive(Debug)]
pub(crate) enum InitError {
    Io(io::Error),
    InteractiveTerminalRequired,
    InteractiveFlagsRequireNonInteractive,
    MissingNonInteractiveValues { fields: Vec<&'static str> },
    IncompatibleNonInteractiveValues { reason: &'static str },
    InvalidPermissionMode { value: String },
    InvalidChoice { value: String },
    InvalidShellChoice { value: String },
    InvalidRoundLimit { value: String },
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "could not read or write setup input: {source}"),
            Self::InteractiveTerminalRequired => write!(
                f,
                "interactive setup requires a terminal; use --non-interactive with all required flags"
            ),
            Self::InteractiveFlagsRequireNonInteractive => write!(
                f,
                "connection, model, round-limit, shell, and permission flags require --non-interactive"
            ),
            Self::MissingNonInteractiveValues { fields } => write!(
                f,
                "noninteractive setup is missing required flags: {}",
                fields.join(", ")
            ),
            Self::IncompatibleNonInteractiveValues { reason } => {
                write!(f, "invalid noninteractive setup: {reason}")
            }
            Self::InvalidPermissionMode { value } => write!(
                f,
                "invalid permission mode {value:?}; expected deny, ask, or allow"
            ),
            Self::InvalidChoice { value } => {
                write!(
                    f,
                    "invalid connection choice {value:?}; expected 1, 2, or 3"
                )
            }
            Self::InvalidShellChoice { value } => write!(
                f,
                "invalid shell choice {value:?}; expected one of the choices shown"
            ),
            Self::InvalidRoundLimit { value } => write!(
                f,
                "invalid maximum tool rounds {value:?}; expected a whole number"
            ),
        }
    }
}

impl Error for InitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::InteractiveTerminalRequired
            | Self::InteractiveFlagsRequireNonInteractive
            | Self::MissingNonInteractiveValues { .. }
            | Self::IncompatibleNonInteractiveValues { .. }
            | Self::InvalidPermissionMode { .. }
            | Self::InvalidChoice { .. }
            | Self::InvalidShellChoice { .. }
            | Self::InvalidRoundLimit { .. } => None,
        }
    }
}

impl From<io::Error> for InitError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

pub(crate) fn plan<R: BufRead, W: Write>(
    args: &InitArgs,
    is_terminal: bool,
    input: &mut R,
    output: &mut W,
) -> Result<InitPlan, InitError> {
    if args.non_interactive {
        return plan_noninteractive(args);
    }

    if has_noninteractive_values(args) {
        return Err(InitError::InteractiveFlagsRequireNonInteractive);
    }

    if !is_terminal {
        return Err(InitError::InteractiveTerminalRequired);
    }

    plan_interactive(input, output)
}

fn has_noninteractive_values(args: &InitArgs) -> bool {
    args.kind.is_some()
        || args.provider_name.is_some()
        || args.base_url.is_some()
        || args.codex_program.is_some()
        || args.codex_home.is_some()
        || args.model.is_some()
        || args.max_tool_rounds.is_some()
        || args.shell.is_some()
        || args.shell_program.is_some()
        || args.permission_mode.is_some()
}

fn plan_noninteractive(args: &InitArgs) -> Result<InitPlan, InitError> {
    let kind = args.kind.unwrap_or(InitConnectionKindChoice::OpenAiCompat);
    let mut fields = Vec::new();

    if args.provider_name.is_none() {
        fields.push("--provider-name");
    }
    if kind == InitConnectionKindChoice::OpenAiCompat && args.base_url.is_none() {
        fields.push("--base-url");
    }
    if args.model.is_none() {
        fields.push("--model");
    }
    if args.permission_mode.is_none() {
        fields.push("--permission-mode");
    }

    if !fields.is_empty() {
        return Err(InitError::MissingNonInteractiveValues { fields });
    }

    if kind == InitConnectionKindChoice::Codex && args.base_url.is_some() {
        return Err(InitError::IncompatibleNonInteractiveValues {
            reason: "Codex app-server uses stdio and does not accept --base-url",
        });
    }
    if kind != InitConnectionKindChoice::Codex
        && (args.codex_program.is_some() || args.codex_home.is_some())
    {
        return Err(InitError::IncompatibleNonInteractiveValues {
            reason: "--codex-program and --codex-home require --kind codex",
        });
    }

    let provider_name = args
        .provider_name
        .clone()
        .expect("provider name presence was checked");
    let connection = match kind {
        InitConnectionKindChoice::Ollama => InitialConnection::Ollama {
            name: provider_name,
            base_url: args
                .base_url
                .clone()
                .unwrap_or_else(|| OLLAMA_BASE_URL.to_owned()),
        },
        InitConnectionKindChoice::OpenAiCompat => InitialConnection::OpenAiCompatible {
            name: provider_name,
            base_url: args
                .base_url
                .clone()
                .expect("base URL presence was checked"),
        },
        InitConnectionKindChoice::Codex => InitialConnection::Codex {
            name: provider_name,
            program: args
                .codex_program
                .clone()
                .unwrap_or_else(|| DEFAULT_CODEX_PROGRAM.to_owned()),
            home: args.codex_home.clone(),
        },
    };

    Ok(InitPlan::Create(InitialConfig {
        connection,
        model: args.model.clone().expect("model presence was checked"),
        max_tool_rounds: args.max_tool_rounds.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS),
        shell: ShellConfig {
            kind: args
                .shell
                .unwrap_or(crate::cli::ShellChoice::Platform)
                .into(),
            program: args.shell_program.clone(),
        },
        permission_mode: args
            .permission_mode
            .expect("permission mode presence was checked")
            .into(),
    }))
}

fn plan_interactive<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<InitPlan, InitError> {
    writeln!(output, "First-time setup")?;
    writeln!(output)?;
    writeln!(output, "[1/4] Connection")?;
    writeln!(output, "  1. Local Ollama")?;
    writeln!(output, "  2. ChatGPT subscription through Codex")?;
    writeln!(output, "  3. Custom OpenAI-compatible endpoint")?;

    let Some(choice) = prompt_connection_choice(input, output)? else {
        return Ok(InitPlan::Cancelled);
    };

    let connection = match choice.as_str() {
        "1" => InitialConnection::Ollama {
            name: OLLAMA_CONNECTION_NAME.to_owned(),
            base_url: OLLAMA_BASE_URL.to_owned(),
        },
        "2" => {
            let Some(program) =
                prompt_with_default(input, output, "Codex executable", DEFAULT_CODEX_PROGRAM)?
            else {
                return Ok(InitPlan::Cancelled);
            };
            let Some(home) = prompt(
                input,
                output,
                "Codex home (blank to share the Codex default): ",
            )?
            else {
                return Ok(InitPlan::Cancelled);
            };
            InitialConnection::Codex {
                name: CODEX_CONNECTION_NAME.to_owned(),
                program,
                home: (!home.trim().is_empty()).then(|| home.into()),
            }
        }
        "3" => {
            let Some(provider_name) = prompt(input, output, "Connection name: ")? else {
                return Ok(InitPlan::Cancelled);
            };
            let Some(base_url) = prompt(input, output, "Base URL: ")? else {
                return Ok(InitPlan::Cancelled);
            };
            InitialConnection::OpenAiCompatible {
                name: provider_name,
                base_url,
            }
        }
        _ => unreachable!("connection choices are validated before planning"),
    };

    let model_prompt = if connection.is_codex() {
        "Model (for example gpt-5.6-sol): "
    } else {
        "Model (for example qwen3:1.7b): "
    };
    let Some(model) = prompt(input, output, model_prompt)? else {
        return Ok(InitPlan::Cancelled);
    };
    let Some(max_tool_rounds) = prompt_round_limit(input, output)? else {
        return Ok(InitPlan::Cancelled);
    };

    writeln!(output)?;
    writeln!(output, "[2/4] Shell")?;
    write_shell_choices(output)?;
    let Some(shell_kind) = prompt_shell_kind(input, output)? else {
        return Ok(InitPlan::Cancelled);
    };
    let Some(shell_program) = prompt(
        input,
        output,
        "Custom shell program path (blank for the default): ",
    )?
    else {
        return Ok(InitPlan::Cancelled);
    };
    let shell = ShellConfig {
        kind: shell_kind,
        program: (!shell_program.trim().is_empty()).then(|| shell_program.into()),
    };

    writeln!(output)?;
    writeln!(output, "[3/4] Authority")?;
    writeln!(output, "  deny: reject tool effects")?;
    writeln!(
        output,
        "  ask: request a decision for each new tool scope (recommended)"
    )?;
    writeln!(output, "  allow: run tool effects automatically")?;
    writeln!(
        output,
        "All modes use your ordinary host permissions; none is containment."
    )?;
    let Some(permission_mode) = prompt_permission_mode(input, output)? else {
        return Ok(InitPlan::Cancelled);
    };

    writeln!(output)?;
    writeln!(output, "[4/4] Review")?;
    writeln!(output, "  Connection:          {}", connection.name())?;
    match &connection {
        InitialConnection::Ollama { base_url, .. } => {
            writeln!(output, "  Kind:                Ollama")?;
            writeln!(output, "  Base URL:            {base_url}")?;
        }
        InitialConnection::OpenAiCompatible { base_url, .. } => {
            writeln!(output, "  Kind:                OpenAI-compatible")?;
            writeln!(output, "  Base URL:            {base_url}")?;
        }
        InitialConnection::Codex { program, home, .. } => {
            writeln!(output, "  Kind:                managed Codex")?;
            writeln!(output, "  Codex executable:    {program}")?;
            if let Some(home) = home {
                writeln!(output, "  Codex home:          {}", home.display())?;
            }
        }
    }
    writeln!(output, "  Model:               {model}")?;
    writeln!(output, "  Maximum tool rounds: {max_tool_rounds}")?;
    writeln!(
        output,
        "  Shell:               {}",
        shell.kind.config_name()
    )?;
    if let Some(program) = &shell.program {
        writeln!(output, "  Shell program:       {}", program.display())?;
    }
    writeln!(
        output,
        "  Permission mode:     {}",
        permission_mode_name(permission_mode)
    )?;
    let Some(create) = prompt(input, output, "Create this configuration? [y/N]: ")? else {
        return Ok(InitPlan::Cancelled);
    };
    if !confirmed(&create) {
        return Ok(InitPlan::Cancelled);
    }

    Ok(InitPlan::Create(InitialConfig {
        connection,
        model,
        max_tool_rounds,
        shell,
        permission_mode,
    }))
}

fn prompt_permission_mode<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<Option<PermissionMode>, InitError> {
    for attempt in 0..MAX_PROMPT_ATTEMPTS {
        let Some(value) = prompt_with_default(input, output, "Permission mode", "ask")? else {
            return Ok(None);
        };
        if let Some(mode) = parse_permission_mode(&value) {
            return Ok(Some(mode));
        }
        if attempt + 1 < MAX_PROMPT_ATTEMPTS {
            writeln!(output, "Choose deny, ask, or allow.")?;
        } else {
            return Err(InitError::InvalidPermissionMode { value });
        }
    }
    unreachable!("the permission-mode loop always returns")
}

fn parse_permission_mode(value: &str) -> Option<PermissionMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "deny" => Some(PermissionMode::Deny),
        "ask" => Some(PermissionMode::Ask),
        "allow" => Some(PermissionMode::Allow),
        _ => None,
    }
}

fn permission_mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Deny => "deny",
        PermissionMode::Ask => "ask",
        PermissionMode::Allow => "allow",
    }
}

fn write_shell_choices<W: Write>(output: &mut W) -> Result<(), io::Error> {
    writeln!(
        output,
        "  platform: use Xana's documented default for this operating system"
    )?;
    #[cfg(unix)]
    writeln!(output, "  posix: use sh with -lc")?;
    #[cfg(windows)]
    {
        writeln!(output, "  git_bash: use bash.exe with -lc")?;
        writeln!(output, "  powershell: use powershell.exe without profiles")?;
        writeln!(output, "  cmd: use cmd.exe with AutoRun disabled")?;
    }
    Ok(())
}

fn prompt_shell_kind<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<Option<ShellKind>, InitError> {
    for attempt in 0..MAX_PROMPT_ATTEMPTS {
        let Some(value) = prompt_with_default(input, output, "Shell", "platform")? else {
            return Ok(None);
        };

        if let Some(kind) = parse_shell_kind(&value) {
            return Ok(Some(kind));
        }

        if attempt + 1 < MAX_PROMPT_ATTEMPTS {
            writeln!(output, "Choose one of the shell names shown above.")?;
        } else {
            return Err(InitError::InvalidShellChoice { value });
        }
    }

    unreachable!("the shell-choice loop always returns")
}

fn parse_shell_kind(value: &str) -> Option<ShellKind> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "platform" => Some(ShellKind::Platform),
        #[cfg(unix)]
        "posix" => Some(ShellKind::Posix),
        #[cfg(windows)]
        "git_bash" => Some(ShellKind::GitBash),
        #[cfg(windows)]
        "powershell" => Some(ShellKind::PowerShell),
        #[cfg(windows)]
        "cmd" => Some(ShellKind::Cmd),
        _ => None,
    }
}

fn prompt_connection_choice<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<Option<String>, InitError> {
    for attempt in 0..MAX_PROMPT_ATTEMPTS {
        let Some(choice) = prompt_with_default(input, output, "Choice", "1")? else {
            return Ok(None);
        };

        if matches!(choice.as_str(), "1" | "2" | "3") {
            return Ok(Some(choice));
        }

        if attempt + 1 < MAX_PROMPT_ATTEMPTS {
            writeln!(output, "Enter 1, 2, or 3.")?;
        } else {
            return Err(InitError::InvalidChoice { value: choice });
        }
    }

    unreachable!("the connection-choice loop always returns")
}

fn prompt_round_limit<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<Option<usize>, InitError> {
    for attempt in 0..MAX_PROMPT_ATTEMPTS {
        let Some(value) = prompt_with_default(
            input,
            output,
            "Maximum tool rounds",
            &DEFAULT_MAX_TOOL_ROUNDS.to_string(),
        )?
        else {
            return Ok(None);
        };

        match parse_round_limit(&value) {
            Ok(rounds) => return Ok(Some(rounds)),
            Err(error) if attempt + 1 == MAX_PROMPT_ATTEMPTS => return Err(error),
            Err(_) => writeln!(output, "Enter a whole number.")?,
        }
    }

    unreachable!("the round-limit loop always returns")
}

fn prompt<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    label: &str,
) -> Result<Option<String>, io::Error> {
    write!(output, "{label}")?;
    output.flush()?;

    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }

    Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()))
}

fn prompt_with_default<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    label: &str,
    default: &str,
) -> Result<Option<String>, io::Error> {
    let Some(value) = prompt(input, output, &format!("{label} [{default}]: "))? else {
        return Ok(None);
    };

    if value.trim().is_empty() {
        Ok(Some(default.to_owned()))
    } else {
        Ok(Some(value))
    }
}

fn confirmed(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn parse_round_limit(value: &str) -> Result<usize, InitError> {
    value
        .trim()
        .parse()
        .map_err(|_| InitError::InvalidRoundLimit {
            value: value.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn interactive_args() -> InitArgs {
        InitArgs {
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
        }
    }

    fn noninteractive_args() -> InitArgs {
        InitArgs {
            non_interactive: true,
            kind: Some(InitConnectionKindChoice::Ollama),
            provider_name: Some("ollama".to_owned()),
            base_url: Some(OLLAMA_BASE_URL.to_owned()),
            codex_program: None,
            codex_home: None,
            model: Some("qwen3:1.7b".to_owned()),
            max_tool_rounds: None,
            shell: None,
            shell_program: None,
            permission_mode: Some(crate::cli::PermissionChoice::Ask),
            dry_run: false,
        }
    }

    fn scripted_plan(input: &str) -> Result<(InitPlan, String), InitError> {
        let mut input = Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        let plan = plan(&interactive_args(), true, &mut input, &mut output)?;
        Ok((plan, String::from_utf8(output).expect("UTF-8 transcript")))
    }

    #[test]
    fn interactive_ollama_flow_uses_only_honest_defaults() {
        let (plan, transcript) = scripted_plan("\nqwen3:1.7b\n\n\n\n\ny\n").expect("setup plan");

        assert_eq!(
            plan,
            InitPlan::Create(InitialConfig {
                connection: InitialConnection::Ollama {
                    name: "ollama".to_owned(),
                    base_url: OLLAMA_BASE_URL.to_owned(),
                },
                model: "qwen3:1.7b".to_owned(),
                max_tool_rounds: 8,
                shell: ShellConfig::default(),
                permission_mode: PermissionMode::Ask,
            })
        );
        assert!(transcript.contains("[1/4] Connection"));
        assert!(transcript.contains("Choice [1]:"));
        assert!(transcript.contains("Maximum tool rounds [8]:"));
        assert!(transcript.contains("Shell [platform]:"));
        assert!(transcript.contains("none is containment"));
        assert!(transcript.contains("Permission mode [ask]:"));
    }

    #[test]
    fn interactive_custom_flow_carries_every_answer() {
        let (plan, _) =
            scripted_plan("3\nlocal_test\nhttps://example.test/v1\nmodel-x\n4\nplatform\ncustom-shell\nallow\nyes\n")
                .expect("custom setup plan");

        assert_eq!(
            plan,
            InitPlan::Create(InitialConfig {
                connection: InitialConnection::OpenAiCompatible {
                    name: "local_test".to_owned(),
                    base_url: "https://example.test/v1".to_owned(),
                },
                model: "model-x".to_owned(),
                max_tool_rounds: 4,
                shell: ShellConfig {
                    kind: ShellKind::Platform,
                    program: Some("custom-shell".into()),
                },
                permission_mode: PermissionMode::Allow,
            })
        );
    }

    #[test]
    fn interactive_codex_flow_builds_a_managed_connection() {
        let (plan, transcript) =
            scripted_plan("2\n\n\ngpt-5.6-sol\n\n\n\n\ny\n").expect("Codex setup plan");

        assert_eq!(
            plan,
            InitPlan::Create(InitialConfig {
                connection: InitialConnection::Codex {
                    name: "codex".to_owned(),
                    program: "codex".to_owned(),
                    home: None,
                },
                model: "gpt-5.6-sol".to_owned(),
                max_tool_rounds: 8,
                shell: ShellConfig::default(),
                permission_mode: PermissionMode::Ask,
            })
        );
        assert!(transcript.contains("ChatGPT subscription through Codex"));
        assert!(transcript.contains("Codex executable [codex]:"));
        assert!(transcript.contains("Kind:                managed Codex"));
    }

    #[test]
    fn deny_is_a_valid_explicit_permission_selection() {
        let (plan, _) = scripted_plan("\nmodel\n\n\n\ndeny\ny\n").expect("deny setup");

        assert!(matches!(
            plan,
            InitPlan::Create(InitialConfig {
                permission_mode: PermissionMode::Deny,
                ..
            })
        ));
    }

    #[test]
    fn declining_final_review_cancels() {
        let (plan, _) = scripted_plan("\nmodel\n\n\n\n\nn\n").expect("declined review");

        assert_eq!(plan, InitPlan::Cancelled);
    }

    #[test]
    fn eof_cancels_without_inventing_answers() {
        let (plan, _) = scripted_plan("").expect("EOF cancellation");

        assert_eq!(plan, InitPlan::Cancelled);
    }

    #[test]
    fn noninteractive_mode_requires_every_value_and_permission_mode() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let missing = plan(
            &InitArgs {
                non_interactive: true,
                ..interactive_args()
            },
            false,
            &mut input,
            &mut output,
        )
        .expect_err("missing values should fail");
        assert!(matches!(
            missing,
            InitError::MissingNonInteractiveValues { fields }
                if fields == ["--provider-name", "--base-url", "--model", "--permission-mode"]
        ));
    }

    #[test]
    fn noninteractive_mode_never_reads_or_prompts() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let planned = plan(&noninteractive_args(), false, &mut input, &mut output)
            .expect("noninteractive plan");

        assert!(matches!(planned, InitPlan::Create(_)));
        assert!(output.is_empty());
        assert_eq!(input.position(), 0);
    }

    #[test]
    fn noninteractive_codex_defaults_to_the_standard_executable() {
        let mut args = noninteractive_args();
        args.kind = Some(InitConnectionKindChoice::Codex);
        args.provider_name = Some("codex".to_owned());
        args.base_url = None;
        args.model = Some("gpt-5.6-sol".to_owned());
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let planned =
            plan(&args, false, &mut input, &mut output).expect("noninteractive Codex plan");

        assert!(matches!(
            planned,
            InitPlan::Create(InitialConfig {
                connection: InitialConnection::Codex { program, home: None, .. },
                ..
            }) if program == "codex"
        ));
        assert!(output.is_empty());
        assert_eq!(input.position(), 0);
    }

    #[test]
    fn noninteractive_codex_rejects_an_http_base_url() {
        let mut args = noninteractive_args();
        args.kind = Some(InitConnectionKindChoice::Codex);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = plan(&args, false, &mut input, &mut output)
            .expect_err("Codex base URL should be rejected");

        assert!(matches!(
            error,
            InitError::IncompatibleNonInteractiveValues { reason }
                if reason.contains("does not accept --base-url")
        ));
    }

    #[test]
    fn piped_interactive_mode_is_rejected() {
        let mut input = Cursor::new(b"\nmodel\n\n\n\ny\ny\n");
        let mut output = Vec::new();

        let error = plan(&interactive_args(), false, &mut input, &mut output)
            .expect_err("piped interactive mode should fail");

        assert!(matches!(error, InitError::InteractiveTerminalRequired));
        assert_eq!(input.position(), 0);
        assert!(output.is_empty());
    }

    #[test]
    fn value_flags_without_noninteractive_mode_are_rejected() {
        let mut args = interactive_args();
        args.model = Some("model".to_owned());
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = plan(&args, true, &mut input, &mut output)
            .expect_err("value flags should require noninteractive mode");

        assert!(matches!(
            error,
            InitError::InteractiveFlagsRequireNonInteractive
        ));
        assert_eq!(input.position(), 0);
        assert!(output.is_empty());
    }

    #[test]
    fn invalid_choice_and_round_limit_retry_with_a_complete_transcript() {
        let (plan, transcript) = scripted_plan(
            "9\n3\ncustom\nhttps://example.test/v1\nmodel\nnot-a-number\n5\nplatform\n\n\ny\n",
        )
        .expect("retrying setup plan");

        assert!(matches!(
            plan,
            InitPlan::Create(InitialConfig {
                max_tool_rounds: 5,
                ..
            })
        ));
        assert!(transcript.contains("Enter 1, 2, or 3."));
        assert!(transcript.contains("Enter a whole number."));
    }

    #[test]
    fn invalid_choice_retries_are_bounded() {
        let error =
            scripted_plan("bad\nstill-bad\nlast-bad\n").expect_err("bounded retries should fail");

        assert!(matches!(
            error,
            InitError::InvalidChoice { value } if value == "last-bad"
        ));
    }

    #[test]
    fn invalid_round_limit_retries_are_bounded() {
        let error =
            scripted_plan("\nmodel\none\ntwo\nthree\n").expect_err("bounded retries should fail");

        assert!(matches!(
            error,
            InitError::InvalidRoundLimit { value } if value == "three"
        ));
    }
}
