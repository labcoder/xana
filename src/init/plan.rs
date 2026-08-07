//! Pure and scripted initialization planning.

use crate::{cli::InitArgs, config::InitialConfig};
use std::{
    error::Error,
    fmt,
    io::{self, BufRead, Write},
};

const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;
const MAX_PROMPT_ATTEMPTS: usize = 3;
const OLLAMA_PROVIDER_NAME: &str = "ollama";
const OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

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
    AutomaticToolsNotAccepted,
    InvalidChoice { value: String },
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
                "provider, model, round-limit, and acknowledgement flags require --non-interactive"
            ),
            Self::MissingNonInteractiveValues { fields } => write!(
                f,
                "noninteractive setup is missing required flags: {}",
                fields.join(", ")
            ),
            Self::AutomaticToolsNotAccepted => {
                write!(f, "noninteractive setup requires --accept-automatic-tools")
            }
            Self::InvalidChoice { value } => {
                write!(f, "invalid connection choice {value:?}; expected 1 or 2")
            }
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
            | Self::AutomaticToolsNotAccepted
            | Self::InvalidChoice { .. }
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
    args.provider_name.is_some()
        || args.base_url.is_some()
        || args.model.is_some()
        || args.max_tool_rounds.is_some()
        || args.accept_automatic_tools
}

fn plan_noninteractive(args: &InitArgs) -> Result<InitPlan, InitError> {
    let mut fields = Vec::new();

    if args.provider_name.is_none() {
        fields.push("--provider-name");
    }
    if args.base_url.is_none() {
        fields.push("--base-url");
    }
    if args.model.is_none() {
        fields.push("--model");
    }

    if !fields.is_empty() {
        return Err(InitError::MissingNonInteractiveValues { fields });
    }

    if !args.accept_automatic_tools {
        return Err(InitError::AutomaticToolsNotAccepted);
    }

    Ok(InitPlan::Create(InitialConfig {
        provider_name: args
            .provider_name
            .clone()
            .expect("provider name presence was checked"),
        base_url: args
            .base_url
            .clone()
            .expect("base URL presence was checked"),
        model: args.model.clone().expect("model presence was checked"),
        max_tool_rounds: args.max_tool_rounds.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS),
    }))
}

fn plan_interactive<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<InitPlan, InitError> {
    writeln!(output, "First-time setup")?;
    writeln!(output)?;
    writeln!(output, "[1/3] Connection")?;
    writeln!(output, "  1. Local Ollama (recommended)")?;
    writeln!(output, "  2. Custom OpenAI-compatible endpoint")?;

    let Some(choice) = prompt_connection_choice(input, output)? else {
        return Ok(InitPlan::Cancelled);
    };

    let (provider_name, base_url) = if choice == "1" {
        (OLLAMA_PROVIDER_NAME.to_owned(), OLLAMA_BASE_URL.to_owned())
    } else {
        let Some(provider_name) = prompt(input, output, "Provider name: ")? else {
            return Ok(InitPlan::Cancelled);
        };
        let Some(base_url) = prompt(input, output, "Base URL: ")? else {
            return Ok(InitPlan::Cancelled);
        };
        (provider_name, base_url)
    };

    let Some(model) = prompt(input, output, "Model (for example qwen3:1.7b): ")? else {
        return Ok(InitPlan::Cancelled);
    };
    let Some(max_tool_rounds) = prompt_round_limit(input, output)? else {
        return Ok(InitPlan::Cancelled);
    };

    writeln!(output)?;
    writeln!(output, "[2/3] Authority")?;
    writeln!(
        output,
        "Xana currently runs requested tools with your user permissions."
    )?;
    writeln!(output, "This is permission, not OS-level containment.")?;
    let Some(automatic_tools) = prompt(
        input,
        output,
        "Accept automatic tool execution for this configuration? [y/N]: ",
    )?
    else {
        return Ok(InitPlan::Cancelled);
    };
    if !confirmed(&automatic_tools) {
        return Ok(InitPlan::Cancelled);
    }

    writeln!(output)?;
    writeln!(output, "[3/3] Review")?;
    writeln!(output, "  Provider:            {provider_name}")?;
    writeln!(output, "  Base URL:            {base_url}")?;
    writeln!(output, "  Model:               {model}")?;
    writeln!(output, "  Maximum tool rounds: {max_tool_rounds}")?;
    let Some(create) = prompt(input, output, "Create this configuration? [y/N]: ")? else {
        return Ok(InitPlan::Cancelled);
    };
    if !confirmed(&create) {
        return Ok(InitPlan::Cancelled);
    }

    Ok(InitPlan::Create(InitialConfig {
        provider_name,
        base_url,
        model,
        max_tool_rounds,
    }))
}

fn prompt_connection_choice<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<Option<String>, InitError> {
    for attempt in 0..MAX_PROMPT_ATTEMPTS {
        let Some(choice) = prompt_with_default(input, output, "Choice", "1")? else {
            return Ok(None);
        };

        if matches!(choice.as_str(), "1" | "2") {
            return Ok(Some(choice));
        }

        if attempt + 1 < MAX_PROMPT_ATTEMPTS {
            writeln!(output, "Enter 1 or 2.")?;
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
            provider_name: None,
            base_url: None,
            model: None,
            max_tool_rounds: None,
            accept_automatic_tools: false,
            dry_run: false,
        }
    }

    fn noninteractive_args() -> InitArgs {
        InitArgs {
            non_interactive: true,
            provider_name: Some("ollama".to_owned()),
            base_url: Some(OLLAMA_BASE_URL.to_owned()),
            model: Some("qwen3:1.7b".to_owned()),
            max_tool_rounds: None,
            accept_automatic_tools: true,
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
        let (plan, transcript) = scripted_plan("\nqwen3:1.7b\n\ny\ny\n").expect("setup plan");

        assert_eq!(
            plan,
            InitPlan::Create(InitialConfig {
                provider_name: "ollama".to_owned(),
                base_url: OLLAMA_BASE_URL.to_owned(),
                model: "qwen3:1.7b".to_owned(),
                max_tool_rounds: 8,
            })
        );
        assert!(transcript.contains("[1/3] Connection"));
        assert!(transcript.contains("Choice [1]:"));
        assert!(transcript.contains("Maximum tool rounds [8]:"));
        assert!(transcript.contains("permission, not OS-level containment"));
    }

    #[test]
    fn interactive_custom_flow_carries_every_answer() {
        let (plan, _) =
            scripted_plan("2\nlocal_test\nhttps://example.test/v1\nmodel-x\n4\nyes\nyes\n")
                .expect("custom setup plan");

        assert_eq!(
            plan,
            InitPlan::Create(InitialConfig {
                provider_name: "local_test".to_owned(),
                base_url: "https://example.test/v1".to_owned(),
                model: "model-x".to_owned(),
                max_tool_rounds: 4,
            })
        );
    }

    #[test]
    fn declining_automatic_tools_cancels() {
        let (plan, _) = scripted_plan("\nmodel\n\nn\n").expect("declined setup");

        assert_eq!(plan, InitPlan::Cancelled);
    }

    #[test]
    fn declining_final_review_cancels() {
        let (plan, _) = scripted_plan("\nmodel\n\ny\nn\n").expect("declined review");

        assert_eq!(plan, InitPlan::Cancelled);
    }

    #[test]
    fn eof_cancels_without_inventing_answers() {
        let (plan, _) = scripted_plan("").expect("EOF cancellation");

        assert_eq!(plan, InitPlan::Cancelled);
    }

    #[test]
    fn noninteractive_mode_requires_every_value_and_acknowledgement() {
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
        let mut no_acknowledgement = noninteractive_args();
        no_acknowledgement.accept_automatic_tools = false;
        let acknowledgement = plan(&no_acknowledgement, false, &mut input, &mut output)
            .expect_err("missing acknowledgement should fail");

        assert!(matches!(
            missing,
            InitError::MissingNonInteractiveValues { fields }
                if fields == ["--provider-name", "--base-url", "--model"]
        ));
        assert!(matches!(
            acknowledgement,
            InitError::AutomaticToolsNotAccepted
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
    fn piped_interactive_mode_is_rejected() {
        let mut input = Cursor::new(b"\nmodel\n\ny\ny\n");
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
        let (plan, transcript) =
            scripted_plan("9\n2\ncustom\nhttps://example.test/v1\nmodel\nnot-a-number\n5\ny\ny\n")
                .expect("retrying setup plan");

        assert!(matches!(
            plan,
            InitPlan::Create(InitialConfig {
                max_tool_rounds: 5,
                ..
            })
        ));
        assert!(transcript.contains("Enter 1 or 2."));
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
