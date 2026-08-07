use super::*;
use crate::{
    permission::PermissionScope,
    shell::{ShellConfig, ShellKind},
};
use tempfile::tempdir;

fn platform_shell() -> Shell {
    Shell::resolve(ShellConfig::default()).expect("platform shell")
}

fn tool() -> RunCommand {
    RunCommand::new(platform_shell())
}

#[test]
fn definition_declares_execute_and_never() {
    let definition = tool().definition();

    assert_eq!(definition.name, "run_command");
    assert_eq!(definition.effect_class, EffectClass::Execute);
    assert_eq!(definition.replay_safety, ReplaySafety::Never);
    assert_eq!(definition.parameters["additionalProperties"], false);
}

#[test]
fn unknown_fields_fail_before_workspace_io() {
    let unavailable_workspace = tempdir().expect("temporary parent").path().join("missing");
    let result = tool().plan_inner(
        &serde_json::json!({"command": "echo no", "unexpected": true}),
        &unavailable_workspace,
    );

    assert!(matches!(result, Err(RunCommandError::InvalidArguments(_))));
}

#[test]
fn blank_command_fails_before_scope_or_process_planning() {
    let workspace = tempdir().expect("temporary workspace");
    let result = tool().plan_inner(&serde_json::json!({"command": "  "}), workspace.path());

    assert!(matches!(result, Err(RunCommandError::BlankCommand)));
}

#[test]
fn outside_workspace_cwd_is_rejected() {
    let workspace = tempdir().expect("temporary workspace");
    let result = tool().plan_inner(
        &serde_json::json!({"command": "echo no", "cwd": "../outside"}),
        workspace.path(),
    );

    assert!(matches!(result, Err(RunCommandError::InvalidCwd { .. })));
}

#[test]
fn plan_binds_normalized_arguments_shell_command_and_canonical_cwd() {
    let workspace = tempdir().expect("temporary workspace");
    let nested = workspace.path().join("nested");
    std::fs::create_dir(&nested).expect("nested cwd");
    let planned = tool()
        .plan(
            &serde_json::json!({"command": "cargo test", "cwd": "nested"}),
            workspace.path(),
        )
        .expect("command plan");

    assert_eq!(
        planned.final_arguments,
        serde_json::json!({"command": "cargo test", "cwd": "nested"})
    );
    assert!(matches!(
        planned.scope,
        PermissionScope::Command { canonical_cwd, command, .. }
            if canonical_cwd == nested.canonicalize().expect("canonical cwd")
                && command == "cargo test"
    ));
}

#[test]
fn planning_does_not_spawn_the_configured_program() {
    let workspace = tempdir().expect("temporary workspace");
    let shell = Shell::resolve(ShellConfig {
        kind: ShellKind::Platform,
        program: Some(PathBuf::from("definitely-not-a-real-xana-shell")),
    })
    .expect("custom shell plan");
    let command = RunCommand::new(shell);

    command
        .plan_inner(
            &serde_json::json!({"command": "echo should-not-run"}),
            workspace.path(),
        )
        .expect("pure command plan");
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
    let tool = tool();
    let plan = tool
        .plan_inner(&serde_json::json!({"command": command}), workspace.path())
        .expect("command plan");
    let result = tool.execute_inner(&plan).await.expect("executed command");

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
        "head -c 40000 /dev/zero | tr '\0' o; head -c 40001 /dev/zero | tr '\0' e >&2",
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
    let tool = tool();
    let plan = tool
        .plan_inner(&serde_json::json!({"command": command}), workspace.path())
        .expect("command plan");
    let result = tool.execute_inner(&plan).await.expect("executed command");

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
    let tool = tool();
    let plan = tool
        .plan_inner(&serde_json::json!({"command": command}), workspace.path())
        .expect("command plan");
    let result = tool.execute_inner(&plan).await.expect("executed command");

    assert!(result.success);
    assert_eq!(result.stdout, expected);
    assert!(result.stderr.is_empty());
}
