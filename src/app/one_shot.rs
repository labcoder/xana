//! Bounded one-shot input, rendering, and process-status adaptation.

use super::{banner_mode, chat};
use crate::{
    completion_evidence::{AcceptanceCondition, CompletionContract},
    identity::SessionId,
    oneshot::{
        ExitCategory, OneShotFailure, OneShotOutput, StreamSequence, write_failure_with_sequence,
        write_success_with_sequence,
    },
    paths::XanaPaths,
};
use anyhow::{Context, Result};
use std::io::{self, IsTerminal, Read};

const MAX_ONE_SHOT_INPUT_BYTES: u64 = 1024 * 1024;

pub(crate) fn preflight(cli: &mut crate::cli::Cli) -> Result<()> {
    let Some(argument) = cli.print.take() else {
        return Ok(());
    };
    let result = acceptance_contract(cli.accept_command.clone(), cli.accept_cwd.clone())
        .map_err(|error| OneShotFailure::new(ExitCategory::InvalidInput, error.to_string()))
        .and_then(|_| resolve_one_shot_input(argument));
    match result {
        Ok(input) => {
            cli.print = Some(Some(input));
            Ok(())
        }
        Err(failure) => {
            let output = if cli.json {
                OneShotOutput::Json
            } else {
                match cli.output.unwrap_or_default() {
                    crate::cli::OutputChoice::Text => OneShotOutput::Text,
                    crate::cli::OutputChoice::Json => OneShotOutput::Json,
                    crate::cli::OutputChoice::StreamJson => OneShotOutput::StreamJson,
                }
            };
            let stdout = io::stdout();
            let stderr = io::stderr();
            write_failure_with_sequence(
                output,
                &failure,
                &mut stdout.lock(),
                &mut stderr.lock(),
                None,
            )
            .context("could not write one-shot preflight failure")?;
            Err(anyhow::Error::new(failure.rendered()))
        }
    }
}

pub(super) async fn run_and_render(
    paths: &XanaPaths,
    resume: Option<SessionId>,
    continue_chat: bool,
    argument: Option<String>,
    output: OneShotOutput,
    contract: CompletionContract,
) -> Result<()> {
    let stream_sequence = (output == OneShotOutput::StreamJson).then(StreamSequence::default);
    let result = resolve_one_shot_input(argument);
    let result = match result {
        Ok(input) => chat::run(
            paths,
            chat::ChatSurface::Plain(banner_mode(paths, false, false, false, true)),
            resume,
            continue_chat,
            false,
            Some(chat::OneShotInput { input, contract }),
            stream_sequence.clone(),
        )
        .await
        .and_then(|success| success.context("one-shot launch returned no result"))
        .map_err(classify_one_shot_error),
        Err(error) => Err(error),
    };

    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();
    match result {
        Ok(success) => {
            write_success_with_sequence(output, &success, &mut stdout, stream_sequence.as_ref())
                .map_err(anyhow::Error::new)
        }
        Err(failure) => {
            write_failure_with_sequence(
                output,
                &failure,
                &mut stdout,
                &mut stderr,
                stream_sequence.as_ref(),
            )
            .context("could not write one-shot failure")?;
            Err(anyhow::Error::new(failure.rendered()))
        }
    }
}

pub(super) fn acceptance_contract(
    command: Option<String>,
    cwd: Option<String>,
) -> Result<CompletionContract> {
    let contract = CompletionContract {
        conditions: command
            .map(|command| AcceptanceCondition::CommandSucceeded {
                command,
                cwd: cwd.unwrap_or_else(|| ".".to_owned()),
            })
            .into_iter()
            .collect(),
    };
    contract.validate()?;
    Ok(contract)
}

fn resolve_one_shot_input(argument: Option<String>) -> Result<String, OneShotFailure> {
    let stdin = io::stdin();
    let piped = !stdin.is_terminal();
    let mut stdin_text = String::new();
    if piped {
        let mut bounded = stdin.lock().take(MAX_ONE_SHOT_INPUT_BYTES + 1);
        bounded.read_to_string(&mut stdin_text).map_err(|error| {
            OneShotFailure::new(
                ExitCategory::InvalidInput,
                format!("could not read stdin: {error}"),
            )
        })?;
        if stdin_text.len() as u64 > MAX_ONE_SHOT_INPUT_BYTES {
            return Err(OneShotFailure::new(
                ExitCategory::InvalidInput,
                format!("one-shot stdin exceeds the {MAX_ONE_SHOT_INPUT_BYTES}-byte limit"),
            ));
        }
    }
    resolve_sources(argument, piped.then_some(stdin_text))
}

fn resolve_sources(
    argument: Option<String>,
    stdin_text: Option<String>,
) -> Result<String, OneShotFailure> {
    let stdin_text = stdin_text.unwrap_or_default();
    let stdin_has_input = !stdin_text.trim().is_empty();
    match (argument, stdin_has_input) {
        (Some(_), true) => Err(OneShotFailure::new(
            ExitCategory::InvalidInput,
            "provide the one-shot prompt either as `-p PROMPT` or through stdin, not both",
        )),
        (Some(argument), false) if argument.trim().is_empty() => Err(OneShotFailure::new(
            ExitCategory::InvalidInput,
            "one-shot prompt must not be blank",
        )),
        (Some(argument), false) => Ok(argument),
        (None, true) => Ok(stdin_text.trim_end_matches(['\r', '\n']).to_owned()),
        (None, false) => Err(OneShotFailure::new(
            ExitCategory::InvalidInput,
            "one-shot mode requires `xana -p PROMPT` or a prompt on stdin",
        )),
    }
}

fn classify_one_shot_error(error: anyhow::Error) -> OneShotFailure {
    if let Some(failure) = error.downcast_ref::<OneShotFailure>() {
        return failure.clone();
    }
    let message = format!("{error:#}");
    let lower = message.to_ascii_lowercase();
    let category = if lower.contains("config")
        || lower.contains("not initialized")
        || lower.contains("xana_home")
    {
        ExitCategory::Configuration
    } else if lower.contains("connection")
        || lower.contains("credential")
        || lower.contains("logged out")
        || lower.contains("model")
        || lower.contains("codex")
        || lower.contains("provider")
    {
        ExitCategory::Connection
    } else if lower.contains("interrupt") || lower.contains("cancelled") {
        ExitCategory::Interrupted
    } else {
        ExitCategory::Runtime
    };
    OneShotFailure::new(category, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_shot_acceptance_is_opt_in_and_validated_before_launch() {
        assert!(
            acceptance_contract(None, None)
                .unwrap()
                .conditions
                .is_empty()
        );
        let contract = acceptance_contract(Some("cargo test --offline".to_owned()), None).unwrap();
        assert_eq!(
            contract.conditions,
            vec![AcceptanceCondition::CommandSucceeded {
                command: "cargo test --offline".to_owned(),
                cwd: ".".to_owned(),
            }]
        );
        assert!(acceptance_contract(Some(" ".to_owned()), None).is_err());
        assert!(acceptance_contract(Some("ok".to_owned()), Some("\0".to_owned())).is_err());
    }

    #[test]
    fn accepts_exactly_one_nonblank_input_source() {
        assert_eq!(
            resolve_sources(Some("argument".to_owned()), None).expect("argument"),
            "argument"
        );
        assert_eq!(
            resolve_sources(None, Some("stdin\r\n".to_owned())).expect("stdin"),
            "stdin"
        );

        for result in [
            resolve_sources(Some("argument".to_owned()), Some("stdin".to_owned())),
            resolve_sources(None, None),
            resolve_sources(Some("  ".to_owned()), None),
            resolve_sources(None, Some("\n\t".to_owned())),
        ] {
            let failure = result.expect_err("ambiguous or empty input must fail");
            assert_eq!(failure.category, ExitCategory::InvalidInput);
        }
    }
}
