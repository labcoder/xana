use super::*;
use crate::{
    identity::{OperationId, ToolInvocationId},
    runtime::{AgentEvent, ProvisionalApprovalCoordinator},
    shell::{ShellConfig, ShellKind},
};
use tempfile::tempdir;
use tokio::sync::mpsc;

fn platform_shell() -> Shell {
    Shell::resolve(ShellConfig::default()).expect("platform shell")
}

fn tool() -> RunCommand {
    RunCommand::new(platform_shell())
}

async fn execute_with_decision(
    command: &RunCommand,
    arguments: &Value,
    workspace_root: &std::path::Path,
    approved: bool,
) -> (Result<CommandResult, RunCommandError>, Vec<AgentEvent>) {
    let (events, mut receiver) = mpsc::unbounded_channel();
    let approvals = ProvisionalApprovalCoordinator::new(events);
    let operation_id = OperationId::new();
    let invocation_id = ToolInvocationId::new();
    let execution = command.execute_inner(
        arguments,
        ToolContext {
            workspace_root,
            operation_id,
            invocation_id,
            approvals: &approvals,
        },
    );
    tokio::pin!(execution);
    let mut observed = Vec::new();

    loop {
        tokio::select! {
            result = &mut execution => return (result, observed),
            event = receiver.recv() => {
                let Some(event) = event else {
                    return (execution.await, observed);
                };
                let is_request = matches!(event, AgentEvent::ProvisionalApprovalRequested { .. });
                observed.push(event);
                if is_request {
                    approvals
                        .decide(operation_id, invocation_id, approved)
                        .expect("matching approval decision");
                }
            }
        }
    }
}

#[test]
fn definition_declares_execute_and_never() {
    let definition = tool().definition();

    assert_eq!(definition.name, "run_command");
    assert_eq!(definition.effect_class, EffectClass::Execute);
    assert_eq!(definition.replay_safety, ReplaySafety::Never);
    assert_eq!(definition.parameters["additionalProperties"], false);
}

#[tokio::test]
async fn unknown_fields_fail_before_workspace_io() {
    let unavailable_workspace = tempdir().expect("temporary parent").path().join("missing");
    let (result, events) = execute_with_decision(
        &tool(),
        &serde_json::json!({"command": "echo no", "unexpected": true}),
        &unavailable_workspace,
        true,
    )
    .await;

    assert!(matches!(result, Err(RunCommandError::InvalidArguments(_))));
    assert!(events.is_empty());
}

#[tokio::test]
async fn blank_command_fails_before_approval() {
    let workspace = tempdir().expect("temporary workspace");
    let (result, events) = execute_with_decision(
        &tool(),
        &serde_json::json!({"command": "  "}),
        workspace.path(),
        true,
    )
    .await;

    assert!(matches!(result, Err(RunCommandError::BlankCommand)));
    assert!(events.is_empty());
}

#[tokio::test]
async fn outside_workspace_cwd_is_rejected() {
    let workspace = tempdir().expect("temporary workspace");
    let (result, events) = execute_with_decision(
        &tool(),
        &serde_json::json!({"command": "echo no", "cwd": "../outside"}),
        workspace.path(),
        true,
    )
    .await;

    assert!(matches!(result, Err(RunCommandError::InvalidCwd { .. })));
    assert!(events.is_empty());
}

#[tokio::test]
async fn denial_prevents_even_an_invalid_program_from_spawning() {
    let workspace = tempdir().expect("temporary workspace");
    let shell = Shell::resolve(ShellConfig {
        kind: ShellKind::Platform,
        program: Some(PathBuf::from("definitely-not-a-real-xana-shell")),
    })
    .expect("custom shell plan");
    let command = RunCommand::new(shell);
    let (result, events) = execute_with_decision(
        &command,
        &serde_json::json!({"command": "echo should-not-run"}),
        workspace.path(),
        false,
    )
    .await;

    assert!(matches!(result, Err(RunCommandError::Declined { .. })));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ProvisionalApprovalRequested { .. }))
    );
}

#[tokio::test]
async fn request_contains_exact_shell_command_and_canonical_cwd() {
    let workspace = tempdir().expect("temporary workspace");
    let nested = workspace.path().join("nested");
    std::fs::create_dir(&nested).expect("nested cwd");
    let (result, events) = execute_with_decision(
        &tool(),
        &serde_json::json!({"command": "cargo test", "cwd": "nested"}),
        workspace.path(),
        false,
    )
    .await;

    assert!(matches!(result, Err(RunCommandError::Declined { .. })));
    let action = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ProvisionalApprovalRequested { action, .. } => Some(action),
            _ => None,
        })
        .expect("approval request");
    assert!(action.contains("command: cargo test"));
    assert!(action.contains(&format!(
        "cwd: {}",
        nested.canonicalize().unwrap().display()
    )));
    assert!(action.contains("argv:"));
}

#[cfg(unix)]
#[tokio::test]
async fn nonzero_exit_preserves_status_stdout_and_stderr() {
    assert_nonzero_result("printf output; printf error >&2; exit 7").await;
}

#[cfg(windows)]
#[tokio::test]
async fn nonzero_exit_preserves_status_stdout_and_stderr() {
    assert_nonzero_result(
        "[Console]::Out.Write('output'); [Console]::Error.Write('error'); exit 7",
    )
    .await;
}

async fn assert_nonzero_result(command: &str) {
    let workspace = tempdir().expect("temporary workspace");
    let (result, _) = execute_with_decision(
        &tool(),
        &serde_json::json!({"command": command}),
        workspace.path(),
        true,
    )
    .await;
    let result = result.expect("executed command");

    assert!(!result.success);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.stdout, "output");
    assert_eq!(result.stderr, "error");
    assert!(!result.stdout_truncated);
    assert!(!result.stderr_truncated);
}

#[cfg(unix)]
#[tokio::test]
async fn stdout_and_stderr_are_bounded_independently() {
    assert_bounded_result(
        "head -c 40000 /dev/zero | tr '\\0' o; head -c 40001 /dev/zero | tr '\\0' e >&2",
    )
    .await;
}

#[cfg(windows)]
#[tokio::test]
async fn stdout_and_stderr_are_bounded_independently() {
    assert_bounded_result("[Console]::Out.Write('o' * 40000); [Console]::Error.Write('e' * 40001)")
        .await;
}

async fn assert_bounded_result(command: &str) {
    let workspace = tempdir().expect("temporary workspace");
    let (result, _) = execute_with_decision(
        &tool(),
        &serde_json::json!({"command": command}),
        workspace.path(),
        true,
    )
    .await;
    let result = result.expect("executed command");

    assert_eq!(result.stdout.len(), MAX_STREAM_BYTES);
    assert_eq!(result.stderr.len(), MAX_STREAM_BYTES);
    assert!(result.stdout.bytes().all(|byte| byte == b'o'));
    assert!(result.stderr.bytes().all(|byte| byte == b'e'));
    assert!(result.stdout_truncated);
    assert!(result.stderr_truncated);
}

#[cfg(unix)]
#[tokio::test]
async fn platform_shell_execution_smoke_test() {
    assert_smoke_result("printf xana-shell", "xana-shell").await;
}

#[cfg(windows)]
#[tokio::test]
async fn platform_shell_execution_smoke_test() {
    assert_smoke_result("[Console]::Out.Write('xana-shell')", "xana-shell").await;
}

async fn assert_smoke_result(command: &str, expected: &str) {
    let workspace = tempdir().expect("temporary workspace");
    let (result, _) = execute_with_decision(
        &tool(),
        &serde_json::json!({"command": command}),
        workspace.path(),
        true,
    )
    .await;
    let result = result.expect("executed command");

    assert!(result.success);
    assert_eq!(result.stdout, expected);
    assert!(result.stderr.is_empty());
}

#[tokio::test]
async fn closed_controller_fails_closed_and_removes_pending_request() {
    let workspace = tempdir().expect("temporary workspace");
    let (events, receiver) = mpsc::unbounded_channel();
    drop(receiver);
    let approvals = ProvisionalApprovalCoordinator::new(events);
    let result = tool()
        .execute_inner(
            &serde_json::json!({"command": "echo never"}),
            ToolContext {
                workspace_root: workspace.path(),
                operation_id: OperationId::new(),
                invocation_id: ToolInvocationId::new(),
                approvals: &approvals,
            },
        )
        .await;

    assert!(matches!(result, Err(RunCommandError::Approval(_))));
    assert_eq!(approvals.pending_count(), 0);
}
