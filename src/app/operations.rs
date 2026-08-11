//! Route inspection and explicit interrupted-operation recovery commands.

use super::load_config;
use crate::{
    cli::{OperationCommand, RouteCommand},
    config::XanaConfig,
    model::ModelManager,
    operation::{RecoveryAction, execute_recovery, plan_recovery},
    orchestration::{ExecutionOwner, ResolvedAgentConfig, RouteResolver},
    paths::XanaPaths,
    permission::{PermissionBroker, PermissionPolicy},
    runtime::RuntimeCommand,
    session::{DurableSession, RestoredOperation},
    shell::Shell,
    terminal,
    tool::ToolRegistry,
};
use anyhow::{Context, Result};
use std::io::Write;

pub(super) fn run_route<W: Write>(
    command: RouteCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load route registry")?;
    let manager = ModelManager::new(
        registry.clone(),
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    );
    let resolver = RouteResolver::new(&registry, &manager);
    match command {
        RouteCommand::List => {
            if registry.routes.is_empty() {
                writeln!(output, "no child task routes configured")?;
                return Ok(());
            }
            for route in registry.routes.keys() {
                let marker = if registry.default_child_route.as_deref() == Some(route.as_str()) {
                    "*"
                } else {
                    " "
                };
                match resolver.resolve(Some(route)) {
                    Ok(resolved) => writeln!(
                        output,
                        "{marker} {}\t{}\t{}/{}\tprofile {}",
                        route,
                        resolved.owner.as_str(),
                        resolved.connection,
                        resolved.model.id,
                        resolved.profile
                    )?,
                    Err(error) => writeln!(output, "{marker} {route}\tunavailable\t{error}")?,
                }
            }
            Ok(())
        }
        RouteCommand::Check { route } => {
            let resolved = resolver.resolve(Some(&route))?;
            write_resolved_route(output, &resolved)
        }
    }
}

fn write_resolved_route<W: Write>(output: &mut W, route: &ResolvedAgentConfig) -> Result<()> {
    writeln!(output, "route: {}", route.route)?;
    writeln!(output, "profile: {}", route.profile)?;
    writeln!(output, "execution: {}", route.owner.as_str())?;
    writeln!(output, "connection: {}", route.connection)?;
    writeln!(output, "kind: {}", route.provider_kind.as_str())?;
    writeln!(output, "model: {}", route.model.id)?;
    if route.owner == ExecutionOwner::Codex {
        writeln!(
            output,
            "reasoning effort: {}",
            route.reasoning_effort.as_deref().unwrap_or("model default")
        )?;
        writeln!(
            output,
            "reasoning summary: {}",
            route
                .reasoning_summary
                .map_or_else(|| "provider default".to_owned(), |value| value.to_string())
        )?;
    }
    let capabilities = route
        .capabilities
        .capabilities()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    writeln!(
        output,
        "capabilities: {}",
        if capabilities.is_empty() {
            "none".to_owned()
        } else {
            capabilities.join(",")
        }
    )?;
    writeln!(
        output,
        "permission ceiling: {}",
        route.permission_mode.as_str()
    )?;
    writeln!(output, "maximum tool rounds: {}", route.max_tool_rounds)?;
    writeln!(
        output,
        "orchestration: fan-out {}, descendants {}, concurrency {}, deadline {}s, context {} tokens, report {} bytes, artifacts {} bytes",
        route.orchestration.max_fan_out,
        route.orchestration.max_descendants,
        route.orchestration.max_concurrency,
        route.orchestration.deadline_seconds,
        route.orchestration.max_context_tokens,
        route.orchestration.max_report_bytes,
        route.orchestration.max_artifact_bytes
    )?;
    Ok(())
}

pub(super) async fn run_operation<W: Write>(
    command: OperationCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let config = load_config(paths)?;
    let shell =
        Shell::resolve(config.shell.clone()).context("could not resolve configured shell")?;
    let tools = ToolRegistry::builtins(shell).context("could not build tool registry")?;

    match command {
        OperationCommand::Plan {
            session,
            operation_id,
        } => {
            let (_, restored) = DurableSession::inspect_restored(paths.data_dir(), session)?;
            let operation = restored
                .operation_details
                .get(&operation_id)
                .with_context(|| format!("operation {operation_id} is not in session {session}"))?;
            let actions = plan_recovery(operation, &tools)?;
            write_recovery_plan(output, session, operation, &actions)
        }
        OperationCommand::Resume {
            session,
            operation_id,
        } => {
            execute_recovery_command(
                RuntimeCommand::ResumeOperation {
                    session_id: session,
                    operation_id,
                },
                paths,
                &config,
                &tools,
                output,
            )
            .await
        }
    }
}

async fn execute_recovery_command<W: Write>(
    command: RuntimeCommand,
    paths: &XanaPaths,
    config: &XanaConfig,
    tools: &ToolRegistry,
    output: &mut W,
) -> Result<()> {
    match command {
        RuntimeCommand::ResumeOperation {
            session_id,
            operation_id,
        } => {
            let (mut durable, _) = DurableSession::resume(paths.data_dir(), session_id)?;
            let operation = durable.restored_operation(operation_id).with_context(|| {
                format!("operation {operation_id} is not in session {session_id}")
            })?;
            let policy = PermissionPolicy::new(
                config.permission_mode.into(),
                config.permission_rules.clone(),
                durable.workspace_root(),
            )
            .context("could not resolve current recovery permission policy")?;
            let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
            let (permissions, broker) =
                PermissionBroker::spawn_for_durable_runtime(policy, true, events);
            let actions = execute_recovery(
                &mut durable,
                operation_id,
                tools,
                &permissions,
                &mut event_receiver,
                |request| terminal::prompt_permission_decision(request).map_err(Into::into),
            )
            .await?;
            permissions.shutdown();
            let _ = broker.await;
            write_recovery_plan(output, session_id, &operation, &actions)
        }
        RuntimeCommand::SubmitTurn { .. }
        | RuntimeCommand::SubmitTurnWithImages { .. }
        | RuntimeCommand::InterruptOperation { .. }
        | RuntimeCommand::SteerOperation { .. }
        | RuntimeCommand::ClearConversation
        | RuntimeCommand::DecidePermission { .. }
        | RuntimeCommand::DecideChildPermission { .. }
        | RuntimeCommand::ListChildren
        | RuntimeCommand::InspectChild { .. }
        | RuntimeCommand::CancelChild { .. }
        | RuntimeCommand::Shutdown => {
            anyhow::bail!("the explicit recovery controller accepts only ResumeOperation")
        }
    }
}

fn write_recovery_plan<W: Write>(
    output: &mut W,
    session_id: crate::identity::SessionId,
    operation: &RestoredOperation,
    actions: &[RecoveryAction],
) -> Result<()> {
    writeln!(output, "session: {session_id}")?;
    writeln!(output, "operation: {}", operation.operation_id)?;
    writeln!(output, "thread: {}", operation.thread_id)?;
    writeln!(output, "input entry: {}", operation.input_entry_id)?;
    if operation.step_order.is_empty() {
        writeln!(output, "steps: none committed")?;
    } else {
        let steps = operation
            .step_order
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "steps: {steps}")?;
    }
    for action in actions {
        match action {
            RecoveryAction::AlreadyCompleted { result_id } => {
                writeln!(output, "already completed: result {result_id}")?
            }
            RecoveryAction::ReplayExactInvocation { invocation_id } => {
                writeln!(output, "replay after current permission: {invocation_id}")?
            }
            RecoveryAction::RecordInterruption {
                invocation_id,
                result_id,
                reason,
            } => writeln!(
                output,
                "record interruption: invocation {invocation_id}, result {result_id}, reason {reason:?}"
            )?,
            RecoveryAction::ContinueWithNextInvocation => {
                writeln!(output, "continue in original call order")?
            }
            RecoveryAction::FinishOperation => writeln!(output, "finish operation")?,
        }
    }
    Ok(())
}
