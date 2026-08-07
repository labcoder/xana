//! Bounded local command execution behind the temporary approval seam.

use super::{EffectClass, ReplaySafety, Tool, ToolDefinition, workspace_path};
use crate::{
    approval::{ApprovalError, ProvisionalApprover, RequestedAction},
    shell::{ProcessPlan, Shell, display_argv},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
};

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
    approver: Box<dyn ProvisionalApprover>,
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
    pub(super) fn new(shell: Shell, approver: Box<dyn ProvisionalApprover>) -> Self {
        Self { shell, approver }
    }

    fn execute_inner(
        &self,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Result<CommandResult, RunCommandError> {
        let args: RunCommandArgs =
            serde_json::from_value(arguments.clone()).map_err(RunCommandError::InvalidArguments)?;

        if args.command.trim().is_empty() {
            return Err(RunCommandError::BlankCommand);
        }

        let resolved_cwd = workspace_path::resolve_existing(args.cwd.clone(), workspace_root)
            .map_err(|source| RunCommandError::InvalidCwd {
                requested: args.cwd,
                source,
            })?;
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

        let approved = self
            .approver
            .confirm(&action)
            .map_err(RunCommandError::Approval)?;
        if !approved {
            return Err(RunCommandError::Declined {
                command: action.command,
                cwd: action.cwd,
            });
        }

        let output = Command::new(&plan.program)
            .args(&plan.args)
            .current_dir(&action.cwd)
            .output()
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

    fn execute(&self, arguments: &Value, workspace_root: &Path) -> Result<String, String> {
        let result = self
            .execute_inner(arguments, workspace_root)
            .map_err(|error| error.to_string())?;
        serde_json::to_string(&result)
            .map_err(|source| RunCommandError::Serialize(source).to_string())
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
mod tests {
    use super::*;
    use crate::{
        approval::RequestedAction,
        shell::{ShellConfig, ShellKind},
    };
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    struct FixedApprover {
        answer: bool,
    }

    impl ProvisionalApprover for FixedApprover {
        fn confirm(&self, _action: &RequestedAction) -> Result<bool, ApprovalError> {
            Ok(self.answer)
        }
    }

    struct RecordingApprover {
        action: Arc<Mutex<Option<RequestedAction>>>,
        answer: bool,
    }

    impl ProvisionalApprover for RecordingApprover {
        fn confirm(&self, action: &RequestedAction) -> Result<bool, ApprovalError> {
            *self.action.lock().expect("action lock") = Some(action.clone());
            Ok(self.answer)
        }
    }

    fn platform_shell() -> Shell {
        Shell::resolve(ShellConfig::default()).expect("platform shell")
    }

    fn tool(answer: bool) -> RunCommand {
        RunCommand::new(platform_shell(), Box::new(FixedApprover { answer }))
    }

    #[test]
    fn definition_declares_execute_and_never() {
        let definition = tool(false).definition();

        assert_eq!(definition.name, "run_command");
        assert_eq!(definition.effect_class, EffectClass::Execute);
        assert_eq!(definition.replay_safety, ReplaySafety::Never);
        assert_eq!(definition.parameters["additionalProperties"], false);
    }

    #[test]
    fn unknown_fields_fail_before_workspace_io() {
        let unavailable_workspace = tempdir().expect("temporary parent").path().join("missing");
        let result = tool(true).execute_inner(
            &serde_json::json!({"command": "echo no", "unexpected": true}),
            &unavailable_workspace,
        );

        assert!(matches!(result, Err(RunCommandError::InvalidArguments(_))));
    }

    #[test]
    fn blank_command_fails_before_approval() {
        let workspace = tempdir().expect("temporary workspace");
        let action = Arc::new(Mutex::new(None));
        let command = RunCommand::new(
            platform_shell(),
            Box::new(RecordingApprover {
                action: Arc::clone(&action),
                answer: true,
            }),
        );

        let result = command.execute_inner(&serde_json::json!({"command": "  "}), workspace.path());

        assert!(matches!(result, Err(RunCommandError::BlankCommand)));
        assert!(action.lock().expect("action lock").is_none());
    }

    #[test]
    fn outside_workspace_cwd_is_rejected() {
        let workspace = tempdir().expect("temporary workspace");
        let result = tool(true).execute_inner(
            &serde_json::json!({"command": "echo no", "cwd": "../outside"}),
            workspace.path(),
        );

        assert!(matches!(result, Err(RunCommandError::InvalidCwd { .. })));
    }

    #[test]
    fn denial_prevents_even_an_invalid_program_from_spawning() {
        let workspace = tempdir().expect("temporary workspace");
        let shell = Shell::resolve(ShellConfig {
            kind: ShellKind::Platform,
            program: Some(PathBuf::from("definitely-not-a-real-xana-shell")),
        })
        .expect("custom shell plan");
        let command = RunCommand::new(shell, Box::new(FixedApprover { answer: false }));

        let result = command.execute_inner(
            &serde_json::json!({"command": "echo should-not-run"}),
            workspace.path(),
        );

        assert!(matches!(result, Err(RunCommandError::Declined { .. })));
    }

    #[test]
    fn request_contains_exact_shell_command_and_canonical_cwd() {
        let workspace = tempdir().expect("temporary workspace");
        let nested = workspace.path().join("nested");
        std::fs::create_dir(&nested).expect("nested cwd");
        let action = Arc::new(Mutex::new(None));
        let command = RunCommand::new(
            platform_shell(),
            Box::new(RecordingApprover {
                action: Arc::clone(&action),
                answer: false,
            }),
        );

        command
            .execute_inner(
                &serde_json::json!({"command": "cargo test", "cwd": "nested"}),
                workspace.path(),
            )
            .expect_err("recorded denial");
        let action = action
            .lock()
            .expect("action lock")
            .clone()
            .expect("recorded action");

        assert_eq!(action.tool_name, "run_command");
        assert_eq!(action.command, "cargo test");
        assert_eq!(
            action.cwd,
            nested.canonicalize().expect("canonical nested cwd")
        );
        assert!(action.argv.contains("cargo test"));
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_exit_preserves_status_stdout_and_stderr() {
        assert_nonzero_result("printf output; printf error >&2; exit 7");
    }

    #[cfg(windows)]
    #[test]
    fn nonzero_exit_preserves_status_stdout_and_stderr() {
        assert_nonzero_result(
            "[Console]::Out.Write('output'); [Console]::Error.Write('error'); exit 7",
        );
    }

    fn assert_nonzero_result(command: &str) {
        let workspace = tempdir().expect("temporary workspace");
        let result = tool(true)
            .execute_inner(&serde_json::json!({"command": command}), workspace.path())
            .expect("executed command");

        assert!(!result.success);
        assert_eq!(result.exit_code, Some(7));
        assert_eq!(result.stdout, "output");
        assert_eq!(result.stderr, "error");
        assert!(!result.stdout_truncated);
        assert!(!result.stderr_truncated);
    }

    #[cfg(unix)]
    #[test]
    fn stdout_and_stderr_are_bounded_independently() {
        assert_bounded_result(
            "head -c 40000 /dev/zero | tr '\\0' o; head -c 40001 /dev/zero | tr '\\0' e >&2",
        );
    }

    #[cfg(windows)]
    #[test]
    fn stdout_and_stderr_are_bounded_independently() {
        assert_bounded_result(
            "[Console]::Out.Write('o' * 40000); [Console]::Error.Write('e' * 40001)",
        );
    }

    fn assert_bounded_result(command: &str) {
        let workspace = tempdir().expect("temporary workspace");
        let result = tool(true)
            .execute_inner(&serde_json::json!({"command": command}), workspace.path())
            .expect("executed command");

        assert_eq!(result.stdout.len(), MAX_STREAM_BYTES);
        assert_eq!(result.stderr.len(), MAX_STREAM_BYTES);
        assert!(result.stdout.bytes().all(|byte| byte == b'o'));
        assert!(result.stderr.bytes().all(|byte| byte == b'e'));
        assert!(result.stdout_truncated);
        assert!(result.stderr_truncated);
    }

    #[cfg(unix)]
    #[test]
    fn platform_shell_execution_smoke_test() {
        assert_smoke_result("printf xana-shell", "xana-shell");
    }

    #[cfg(windows)]
    #[test]
    fn platform_shell_execution_smoke_test() {
        assert_smoke_result("[Console]::Out.Write('xana-shell')", "xana-shell");
    }

    fn assert_smoke_result(command: &str, expected: &str) {
        let workspace = tempdir().expect("temporary workspace");
        let result = tool(true)
            .execute_inner(&serde_json::json!({"command": command}), workspace.path())
            .expect("executed command");

        assert!(result.success);
        assert_eq!(result.stdout, expected);
        assert!(result.stderr.is_empty());
    }
}
