//! Bounded local command execution behind the temporary approval protocol.

use super::{EffectClass, ReplaySafety, Tool, ToolContext, ToolDefinition, workspace_path};
use crate::{
    approval::{ApprovalError, RequestedAction},
    shell::{ProcessPlan, Shell, display_argv},
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{error::Error, fmt, io, path::PathBuf};
use tokio::process::Command;

const MAX_STREAM_BYTES: usize = 32 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCommandArgs {
    command: String,
    #[serde(default = "default_cwd")]
    cwd: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct CommandResult {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

pub(super) struct RunCommand {
    shell: Shell,
}

#[derive(Debug)]
enum RunCommandError {
    InvalidArguments(serde_json::Error),
    BlankCommand,
    InvalidCwd {
        requested: String,
        source: workspace_path::WorkspacePathError,
    },
    CwdNotDirectory {
        requested: String,
        resolved: PathBuf,
    },
    Approval(ApprovalError),
    Declined {
        command: String,
        cwd: PathBuf,
    },
    Process {
        plan: ProcessPlan,
        source: io::Error,
    },
    Serialize(serde_json::Error),
}

impl RunCommand {
    pub(super) fn new(shell: Shell) -> Self {
        Self { shell }
    }

    async fn execute_inner(
        &self,
        arguments: &Value,
        context: ToolContext<'_>,
    ) -> Result<CommandResult, RunCommandError> {
        let args: RunCommandArgs =
            serde_json::from_value(arguments.clone()).map_err(RunCommandError::InvalidArguments)?;

        if args.command.trim().is_empty() {
            return Err(RunCommandError::BlankCommand);
        }

        let resolved_cwd =
            workspace_path::resolve_existing(args.cwd.clone(), context.workspace_root).map_err(
                |source| RunCommandError::InvalidCwd {
                    requested: args.cwd,
                    source,
                },
            )?;
        if !resolved_cwd.canonical_path.is_dir() {
            return Err(RunCommandError::CwdNotDirectory {
                requested: resolved_cwd.requested_path,
                resolved: resolved_cwd.canonical_path,
            });
        }

        let plan = self.shell.plan(&args.command);
        let action = RequestedAction {
            tool_name: "run_command",
            shell: self.shell.display_name(),
            command: args.command,
            argv: display_argv(&plan),
            cwd: resolved_cwd.canonical_path,
        };

        let approved = context
            .approvals
            .request(context.operation_id, context.invocation_id, &action)
            .await
            .map_err(RunCommandError::Approval)?;
        if !approved {
            return Err(RunCommandError::Declined {
                command: action.command,
                cwd: action.cwd,
            });
        }

        let mut command = Command::new(&plan.program);
        command
            .args(&plan.args)
            .current_dir(&action.cwd)
            .kill_on_drop(true);
        let output = command
            .output()
            .await
            .map_err(|source| RunCommandError::Process {
                plan: plan.clone(),
                source,
            })?;
        let (stdout, stdout_truncated) = bounded_text(&output.stdout, MAX_STREAM_BYTES);
        let (stderr, stderr_truncated) = bounded_text(&output.stderr, MAX_STREAM_BYTES);

        Ok(CommandResult {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        })
    }
}

fn default_cwd() -> String {
    ".".to_owned()
}

fn bounded_text(bytes: &[u8], limit: usize) -> (String, bool) {
    let truncated = bytes.len() > limit;
    let selected = &bytes[..bytes.len().min(limit)];
    (String::from_utf8_lossy(selected).into_owned(), truncated)
}

impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command",
            description: "Run one command through Xana's configured local shell after explicit per-call approval. The process uses Xana's ordinary host permissions and is not sandboxed.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command string interpreted by the configured shell."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Existing directory relative to the launch workspace.",
                        "default": "."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            effect_class: EffectClass::Execute,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn execute<'a>(
        &'a self,
        arguments: &'a Value,
        context: ToolContext<'a>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let result = self
                .execute_inner(arguments, context)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&result)
                .map_err(|source| RunCommandError::Serialize(source).to_string())
        })
    }
}

impl fmt::Display for RunCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(_) => write!(f, "invalid run_command arguments"),
            Self::BlankCommand => write!(f, "run_command requires a non-blank command"),
            Self::InvalidCwd { requested, .. } => {
                write!(f, "run_command cwd {requested:?} is invalid or unavailable")
            }
            Self::CwdNotDirectory {
                requested,
                resolved,
            } => write!(
                f,
                "run_command cwd {requested:?} resolves to non-directory {}",
                resolved.display()
            ),
            Self::Approval(_) => write!(f, "run_command approval could not be completed"),
            Self::Declined { command, cwd } => write!(
                f,
                "run_command was declined for {command:?} in {}",
                cwd.display()
            ),
            Self::Process { plan, source } => {
                write!(f, "could not execute {}: {source}", display_argv(plan))
            }
            Self::Serialize(_) => write!(f, "could not encode run_command result"),
        }
    }
}

impl Error for RunCommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidArguments(source) | Self::Serialize(source) => Some(source),
            Self::InvalidCwd { source, .. } => Some(source),
            Self::Approval(source) => Some(source),
            Self::Process { source, .. } => Some(source),
            Self::BlankCommand | Self::CwdNotDirectory { .. } | Self::Declined { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
