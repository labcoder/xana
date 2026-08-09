//! Stable one-shot result and process-exit contract.

use crate::identity::SessionId;
use serde::Serialize;
use std::{error::Error, fmt, io::Write, process::ExitCode};

pub(crate) const ONE_SHOT_RESULT_VERSION: u16 = 1;
const MAX_DIAGNOSTIC_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OneShotOutput {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExitCategory {
    InvalidInput,
    Configuration,
    Connection,
    Approval,
    Runtime,
    Interrupted,
}

impl ExitCategory {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::InvalidInput => 2,
            Self::Configuration => 3,
            Self::Connection => 4,
            Self::Approval => 5,
            Self::Runtime => 6,
            Self::Interrupted => 130,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OneShotSuccess {
    pub(crate) text: String,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) execution_owner: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OneShotFailure {
    pub(crate) category: ExitCategory,
    pub(crate) message: String,
    pub(crate) rendered: bool,
}

impl OneShotFailure {
    pub(crate) fn new(category: ExitCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: bounded_diagnostic(message.into()),
            rendered: false,
        }
    }

    pub(crate) fn rendered(mut self) -> Self {
        self.rendered = true;
        self
    }

    pub(crate) fn exit_code(&self) -> ExitCode {
        ExitCode::from(self.category.code())
    }
}

impl fmt::Display for OneShotFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for OneShotFailure {}

#[derive(Serialize)]
struct ResultEnvelope<'a> {
    version: u16,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<ResultBody<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody<'a>>,
}

#[derive(Serialize)]
struct ResultBody<'a> {
    text: &'a str,
    execution_owner: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<SessionId>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    category: ExitCategory,
    message: &'a str,
}

pub(crate) fn write_success(
    output: OneShotOutput,
    success: &OneShotSuccess,
    stdout: &mut impl Write,
) -> Result<(), OneShotFailure> {
    match output {
        OneShotOutput::Text => {
            stdout
                .write_all(success.text.as_bytes())
                .and_then(|()| stdout.write_all(b"\n"))
                .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
        }
        OneShotOutput::Json => {
            serde_json::to_writer(
                &mut *stdout,
                &ResultEnvelope {
                    version: ONE_SHOT_RESULT_VERSION,
                    status: "success",
                    result: Some(ResultBody {
                        text: &success.text,
                        execution_owner: success.execution_owner,
                        session_id: success.session_id,
                    }),
                    error: None,
                },
            )
            .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
            stdout
                .write_all(b"\n")
                .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
        }
    }
    Ok(())
}

pub(crate) fn write_failure(
    output: OneShotOutput,
    failure: &OneShotFailure,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::io::Result<()> {
    if output == OneShotOutput::Json {
        serde_json::to_writer(
            &mut *stdout,
            &ResultEnvelope {
                version: ONE_SHOT_RESULT_VERSION,
                status: "error",
                result: None,
                error: Some(ErrorBody {
                    category: failure.category,
                    message: &failure.message,
                }),
            },
        )?;
        stdout.write_all(b"\n")?;
    }
    writeln!(stderr, "xana: {}", failure.message)
}

fn bounded_diagnostic(mut message: String) -> String {
    if message.len() <= MAX_DIAGNOSTIC_BYTES {
        return message;
    }
    let mut boundary = MAX_DIAGNOSTIC_BYTES - 3;
    while !message.is_char_boundary(boundary) {
        boundary -= 1;
    }
    message.truncate(boundary);
    message.push_str("...");
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_success_and_failure_have_one_versioned_envelope() {
        let success = OneShotSuccess {
            text: "hello".to_owned(),
            session_id: None,
            execution_owner: "managed_codex",
        };
        let mut stdout = Vec::new();
        write_success(OneShotOutput::Json, &success, &mut stdout).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["version"], ONE_SHOT_RESULT_VERSION);
        assert_eq!(value["status"], "success");
        assert_eq!(value["result"]["text"], "hello");

        let failure = OneShotFailure::new(ExitCategory::Approval, "approval required");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        write_failure(OneShotOutput::Json, &failure, &mut stdout, &mut stderr).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["status"], "error");
        assert_eq!(value["error"]["category"], "approval");
        assert_eq!(failure.exit_code(), ExitCode::from(5));
    }
}
