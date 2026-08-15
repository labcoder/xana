//! Bounded local command execution behind the shared permission planner.

use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, workspace_path,
};
use crate::{
    permission::PermissionScope,
    process_capture,
    shell::{ProcessPlan, Shell, display_argv},
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};
use tokio::process::Command;

const MAX_STREAM_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCommandArgs {
    command: String,
    #[serde(default = "default_cwd")]
    cwd: String,
}

struct RunCommandPlan {
    args: RunCommandArgs,
    process: ProcessPlan,
    canonical_cwd: PathBuf,
    cwd_identity: workspace_path::FileIdentity,
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

    fn plan_inner(
        &self,
        arguments: &Value,
        workspace_root: &std::path::Path,
    ) -> Result<RunCommandPlan, RunCommandError> {
        let args: RunCommandArgs =
            serde_json::from_value(arguments.clone()).map_err(RunCommandError::InvalidArguments)?;

        if args.command.trim().is_empty() {
            return Err(RunCommandError::BlankCommand);
        }

        let resolved_cwd = if Path::new(&args.cwd).is_absolute() {
            let (resolved, location) =
                workspace_path::resolve_existing_for_read(args.cwd.clone(), workspace_root)
                    .map_err(|source| RunCommandError::InvalidCwd {
                        requested: args.cwd.clone(),
                        source,
                    })?;
            if location == workspace_path::ResolvedPathLocation::External {
                return Err(RunCommandError::InvalidCwd {
                    requested: args.cwd.clone(),
                    source: workspace_path::WorkspacePathError::OutsideWorkspace {
                        requested_path: args.cwd.clone(),
                    },
                });
            }
            resolved
        } else {
            workspace_path::resolve_existing(args.cwd.clone(), workspace_root).map_err(
                |source| RunCommandError::InvalidCwd {
                    requested: args.cwd.clone(),
                    source,
                },
            )?
        };
        if !resolved_cwd.canonical_path.is_dir() {
            return Err(RunCommandError::CwdNotDirectory {
                requested: resolved_cwd.requested_path,
                resolved: resolved_cwd.canonical_path,
            });
        }

        let process = self.shell.plan(&args.command);
        let cwd_identity = resolved_cwd.identity;
        Ok(RunCommandPlan {
            args,
            process,
            canonical_cwd: resolved_cwd.canonical_path,
            cwd_identity,
        })
    }

    async fn execute_inner(&self, plan: &RunCommandPlan) -> Result<CommandResult, RunCommandError> {
        workspace_path::revalidate_path(&plan.args.cwd, &plan.canonical_cwd, &plan.cwd_identity)
            .map_err(|source| RunCommandError::InvalidCwd {
                requested: plan.args.cwd.clone(),
                source,
            })?;
        let mut command = Command::new(&plan.process.program);
        command
            .args(&plan.process.args)
            .current_dir(&plan.canonical_cwd);
        let output = process_capture::run(&mut command, MAX_STREAM_BYTES)
            .await
            .map_err(|source| RunCommandError::Process {
                plan: plan.process.clone(),
                source,
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        Ok(CommandResult {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout,
            stderr,
            stdout_truncated: output.stdout_truncated,
            stderr_truncated: output.stderr_truncated,
        })
    }
}

fn default_cwd() -> String {
    ".".to_owned()
}

impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Run one command through Xana's configured local shell when runtime permission allows it. The process uses Xana's ordinary host permissions and is not sandboxed.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command string interpreted by the configured shell."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Existing directory inside the launch workspace. Use '.' for the workspace root; workspace-relative paths are preferred, and absolute paths are accepted only when they resolve inside the workspace. To operate on an approved external file, keep cwd='.' and reference the absolute file in command.",
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

    fn plan(
        &self,
        arguments: &Value,
        workspace_root: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        let plan = self
            .plan_inner(arguments, workspace_root)
            .map_err(|error| error.to_string())?;
        let final_arguments =
            serde_json::to_value(&plan.args).map_err(|error| error.to_string())?;
        let scope = PermissionScope::Command {
            shell: self.shell.prompt_description(),
            canonical_cwd: plan.canonical_cwd.clone(),
            command: plan.args.command.clone(),
        };
        Ok(PlannedToolInvocation::new(final_arguments, scope, plan))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _context: super::ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<RunCommandPlan>("run_command")?;
            let result = self
                .execute_inner(plan)
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
                write!(
                    f,
                    "run_command cwd {requested:?} is invalid or unavailable; use '.' or a directory inside the launch workspace"
                )
            }
            Self::CwdNotDirectory {
                requested,
                resolved,
            } => write!(
                f,
                "run_command cwd {requested:?} resolves to non-directory {}",
                resolved.display()
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
            Self::Process { source, .. } => Some(source),
            Self::BlankCommand | Self::CwdNotDirectory { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
