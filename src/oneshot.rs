//! Stable one-shot result and process-exit contract.

use crate::{
    frontend::{ClientObservation, ManagedClientEvent},
    identity::{ConversationId, SessionId},
};
use serde::Serialize;
use std::{
    error::Error,
    fmt,
    io::Write,
    process::ExitCode,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub(crate) const ONE_SHOT_RESULT_VERSION: u16 = 2;
const MAX_DIAGNOSTIC_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OneShotOutput {
    Text,
    Json,
    StreamJson,
}

pub(crate) const ONE_SHOT_STREAM_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy)]
struct StreamContext {
    execution_owner: &'static str,
    conversation_id: ConversationId,
}

#[derive(Debug, Default)]
struct StreamState {
    sequence: AtomicU64,
    context: OnceLock<StreamContext>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StreamSequence(Arc<StreamState>);

impl StreamSequence {
    fn next(&self) -> u64 {
        self.0
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    fn bind(&self, execution_owner: &'static str, conversation_id: ConversationId) {
        let _ = self.0.context.set(StreamContext {
            execution_owner,
            conversation_id,
        });
    }

    fn context(&self) -> Option<StreamContext> {
        self.0.context.get().copied()
    }
}

pub(crate) enum OneShotReporter<'a> {
    Text(&'a mut dyn Write),
    StreamJson {
        output: &'a mut dyn Write,
        sequence: StreamSequence,
        execution_owner: &'static str,
        conversation_id: ConversationId,
    },
}

impl<'a> OneShotReporter<'a> {
    pub(crate) fn text(output: &'a mut dyn Write) -> Self {
        Self::Text(output)
    }

    pub(crate) fn stream_json(
        output: &'a mut dyn Write,
        sequence: StreamSequence,
        execution_owner: &'static str,
        conversation_id: ConversationId,
    ) -> Self {
        sequence.bind(execution_owner, conversation_id);
        Self::StreamJson {
            output,
            sequence,
            execution_owner,
            conversation_id,
        }
    }

    pub(crate) fn activity(&mut self, code: &'static str, message: &str) -> std::io::Result<()> {
        match self {
            Self::Text(output) => writeln!(output, "{message}"),
            Self::StreamJson {
                output,
                sequence,
                execution_owner,
                conversation_id,
            } => write_stream_frame(
                output,
                sequence,
                "activity",
                execution_owner,
                Some(*conversation_id),
                &ActivityPayload {
                    code,
                    message: &bounded_diagnostic(message.to_owned()),
                },
            ),
        }
    }

    pub(crate) fn native_observation(
        &mut self,
        observation: &ClientObservation,
    ) -> std::io::Result<()> {
        if let Self::StreamJson {
            output,
            sequence,
            execution_owner,
            conversation_id,
        } = self
        {
            write_stream_frame(
                output,
                sequence,
                "observation",
                execution_owner,
                Some(*conversation_id),
                observation,
            )?;
        }
        Ok(())
    }

    pub(crate) fn managed_observation(
        &mut self,
        observation: &ManagedClientEvent,
    ) -> std::io::Result<()> {
        if let Self::StreamJson {
            output,
            sequence,
            execution_owner,
            conversation_id,
        } = self
        {
            write_stream_frame(
                output,
                sequence,
                "observation",
                execution_owner,
                Some(*conversation_id),
                observation,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExitCategory {
    InvalidInput,
    Configuration,
    Connection,
    Approval,
    Runtime,
    Incomplete,
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
            Self::Incomplete => 7,
            Self::Interrupted => 130,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OneShotSuccess {
    pub(crate) text: String,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) conversation_id: ConversationId,
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
    conversation_id: ConversationId,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<SessionId>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    category: ExitCategory,
    message: &'a str,
}

pub(crate) fn write_success_with_sequence(
    output: OneShotOutput,
    success: &OneShotSuccess,
    stdout: &mut impl Write,
    sequence: Option<&StreamSequence>,
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
                        conversation_id: success.conversation_id,
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
        OneShotOutput::StreamJson => {
            let sequence = sequence.cloned().unwrap_or_default();
            write_stream_frame(
                stdout,
                &sequence,
                "result",
                success.execution_owner,
                Some(success.conversation_id),
                &ResultEnvelope {
                    version: ONE_SHOT_RESULT_VERSION,
                    status: "success",
                    result: Some(ResultBody {
                        text: &success.text,
                        execution_owner: success.execution_owner,
                        conversation_id: success.conversation_id,
                        session_id: success.session_id,
                    }),
                    error: None,
                },
            )
            .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
        }
    }
    Ok(())
}

pub(crate) fn write_failure_with_sequence(
    output: OneShotOutput,
    failure: &OneShotFailure,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    sequence: Option<&StreamSequence>,
) -> std::io::Result<()> {
    if output == OneShotOutput::Json {
        serde_json::to_writer(
            &mut *stdout,
            &ResultEnvelope {
                version: ONE_SHOT_RESULT_VERSION,
                status: if failure.category == ExitCategory::Incomplete {
                    "incomplete"
                } else {
                    "error"
                },
                result: None,
                error: Some(ErrorBody {
                    category: failure.category,
                    message: &failure.message,
                }),
            },
        )?;
        stdout.write_all(b"\n")?;
    } else if output == OneShotOutput::StreamJson {
        let sequence = sequence.cloned().unwrap_or_default();
        let context = sequence.context();
        write_stream_frame(
            stdout,
            &sequence,
            "result",
            context
                .map(|context| context.execution_owner)
                .unwrap_or("unknown"),
            context.map(|context| context.conversation_id),
            &ResultEnvelope {
                version: ONE_SHOT_RESULT_VERSION,
                status: if failure.category == ExitCategory::Incomplete {
                    "incomplete"
                } else {
                    "error"
                },
                result: None,
                error: Some(ErrorBody {
                    category: failure.category,
                    message: &failure.message,
                }),
            },
        )?;
    }
    writeln!(stderr, "xana: {}", failure.message)
}

#[derive(Serialize)]
struct StreamFrame<'a, T> {
    version: u16,
    sequence: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    execution_owner: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<ConversationId>,
    payload: &'a T,
}

#[derive(Serialize)]
struct ActivityPayload<'a> {
    code: &'static str,
    message: &'a str,
}

fn write_stream_frame<T: Serialize>(
    output: &mut dyn Write,
    sequence: &StreamSequence,
    kind: &'static str,
    execution_owner: &'static str,
    conversation_id: Option<ConversationId>,
    payload: &T,
) -> std::io::Result<()> {
    serde_json::to_writer(
        &mut *output,
        &StreamFrame {
            version: ONE_SHOT_STREAM_VERSION,
            sequence: sequence.next(),
            kind,
            execution_owner,
            conversation_id,
            payload,
        },
    )?;
    output.write_all(b"\n")?;
    output.flush()
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
            conversation_id: ConversationId::new(),
            execution_owner: "managed_codex",
        };
        let mut stdout = Vec::new();
        write_success_with_sequence(OneShotOutput::Json, &success, &mut stdout, None).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["version"], ONE_SHOT_RESULT_VERSION);
        assert_eq!(value["status"], "success");
        assert_eq!(value["result"]["text"], "hello");

        let failure = OneShotFailure::new(ExitCategory::Approval, "approval required");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        write_failure_with_sequence(
            OneShotOutput::Json,
            &failure,
            &mut stdout,
            &mut stderr,
            None,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["status"], "error");
        assert_eq!(value["error"]["category"], "approval");

        let incomplete = OneShotFailure::new(ExitCategory::Incomplete, "resume interactively");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        write_failure_with_sequence(
            OneShotOutput::Json,
            &incomplete,
            &mut stdout,
            &mut stderr,
            None,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["status"], "incomplete");
        assert_eq!(value["error"]["category"], "incomplete");
        assert_eq!(incomplete.category.code(), 7);
        assert_eq!(failure.exit_code(), ExitCode::from(5));
    }

    #[test]
    fn stream_json_is_ordered_jsonl_with_one_authoritative_result() {
        let conversation_id = ConversationId::new();
        let sequence = StreamSequence::default();
        let mut stdout = Vec::new();
        {
            let mut reporter = OneShotReporter::stream_json(
                &mut stdout,
                sequence.clone(),
                "native",
                conversation_id,
            );
            reporter
                .activity("run.started", "run started")
                .expect("activity frame");
        }
        write_success_with_sequence(
            OneShotOutput::StreamJson,
            &OneShotSuccess {
                text: "done".to_owned(),
                session_id: None,
                conversation_id,
                execution_owner: "native",
            },
            &mut stdout,
            Some(&sequence),
        )
        .expect("result frame");

        let lines = String::from_utf8(stdout).unwrap();
        let frames = lines
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["sequence"], 1);
        assert_eq!(frames[0]["type"], "activity");
        assert_eq!(frames[1]["sequence"], 2);
        assert_eq!(frames[1]["type"], "result");
        assert_eq!(frames[1]["payload"]["status"], "success");
        assert_eq!(frames[1]["payload"]["result"]["text"], "done");
    }

    #[test]
    fn stream_json_failure_reuses_bound_execution_context() {
        let conversation_id = ConversationId::new();
        let sequence = StreamSequence::default();
        let mut stdout = Vec::new();
        {
            let mut reporter = OneShotReporter::stream_json(
                &mut stdout,
                sequence.clone(),
                "managed_codex",
                conversation_id,
            );
            reporter
                .activity("run.started", "run started")
                .expect("activity frame");
        }
        let mut stderr = Vec::new();
        write_failure_with_sequence(
            OneShotOutput::StreamJson,
            &OneShotFailure::new(ExitCategory::Runtime, "provider stopped"),
            &mut stdout,
            &mut stderr,
            Some(&sequence),
        )
        .expect("failure frame");

        let final_frame = String::from_utf8(stdout)
            .unwrap()
            .lines()
            .last()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .expect("result frame");
        assert_eq!(final_frame["execution_owner"], "managed_codex");
        assert_eq!(final_frame["conversation_id"], conversation_id.to_string());
        assert_eq!(final_frame["payload"]["status"], "error");
    }
}
