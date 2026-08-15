//! Managed Codex child execution behind the provider-neutral supervisor seam.

use super::{
    ChildActivity, ChildExecution, ChildExecutionContext, ChildExecutionOutcome,
    ChildExecutionOutput, ChildUsage,
};
use crate::{
    config::PermissionMode,
    identity::ToolInvocationId,
    managed::codex::{
        AccountStatus, ApprovalDecision, ApprovalRequest, CodexAppServer, CodexError,
        CodexLaunchConfig, ManagedApprovalPolicy, ManagedEventHandler, ManagedNotification,
        ManagedSandbox, ManagedThreadPolicy, ManagedTurnInput, ManagedTurnOptions,
    },
    native_runtime::{AgentEvent, AgentEventSender},
    permission::{Authorization, PermissionBrokerHandle, PermissionRequest, PermissionScope},
    tool::EffectClass,
};
use futures::future::BoxFuture;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Clone)]
pub(crate) struct ManagedCodexChildSpec {
    pub(crate) launch: CodexLaunchConfig,
    pub(crate) workspace: PathBuf,
    pub(crate) model: String,
    pub(crate) options: ManagedTurnOptions,
    pub(crate) task: String,
    pub(crate) policy: ManagedThreadPolicy,
}

pub(crate) trait ManagedCodexRunner: Send + Sync {
    fn run(
        &self,
        spec: ManagedCodexChildSpec,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome>;
}

pub(crate) struct ManagedCodexChildExecution {
    runner: Arc<dyn ManagedCodexRunner>,
    spec: ManagedCodexChildSpec,
}

impl ManagedCodexChildExecution {
    pub(crate) fn new(runner: Arc<dyn ManagedCodexRunner>, spec: ManagedCodexChildSpec) -> Self {
        Self { runner, spec }
    }
}

impl ChildExecution for ManagedCodexChildExecution {
    fn handles_cancellation(&self) -> bool {
        true
    }

    fn run(
        self: Box<Self>,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        self.runner.run(self.spec, context)
    }
}

#[derive(Default)]
pub(crate) struct AppServerCodexRunner;

impl ManagedCodexRunner for AppServerCodexRunner {
    fn run(
        &self,
        spec: ManagedCodexChildSpec,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async move { run_app_server(spec, context).await })
    }
}

async fn run_app_server(
    spec: ManagedCodexChildSpec,
    context: ChildExecutionContext,
) -> ChildExecutionOutcome {
    if context.cancellation.is_cancelled() {
        return ChildExecutionOutcome::Cancelled(
            "managed cancellation was observed before Codex startup".to_owned(),
        );
    }
    let mut server = tokio::select! {
        result = CodexAppServer::spawn(&spec.launch) => match result {
            Ok(server) => server,
            Err(error) => return ChildExecutionOutcome::Failed(error.to_string()),
        },
        _ = context.cancellation.cancelled() => {
            return ChildExecutionOutcome::Cancelled(
                "managed cancellation was observed during Codex startup".to_owned()
            );
        }
    };
    let mut handler = ChildManagedHandler::new(&context, spec.workspace.clone());
    let account = tokio::select! {
        result = server.account_status() => result,
        _ = context.cancellation.cancelled() => {
            let _ = server.shutdown().await;
            return ChildExecutionOutcome::Cancelled(
                "managed cancellation was observed before account validation".to_owned()
            );
        }
    };
    let account = match account {
        Ok(account) => account,
        Err(error) => {
            let _ = server.shutdown().await;
            return ChildExecutionOutcome::Failed(error.to_string());
        }
    };
    if matches!(account, AccountStatus::LoggedOut) {
        let _ = server.shutdown().await;
        return ChildExecutionOutcome::Failed(
            CodexError::LoginFailed("managed child connection is logged out".to_owned())
                .to_string(),
        );
    }
    let thread = tokio::select! {
        result = server.start_thread_with_policy(
                &spec.model,
                &spec.workspace,
                crate::prompt::xana_identity(),
                spec.policy,
                &mut handler,
            ) => result,
        _ = context.cancellation.cancelled() => {
            let _ = server.shutdown().await;
            return ChildExecutionOutcome::Cancelled(
                "managed cancellation was observed before thread creation".to_owned()
            );
        }
    };
    let thread_id = match thread {
        Ok(thread_id) => thread_id,
        Err(error) => {
            let _ = server.shutdown().await;
            return ChildExecutionOutcome::Failed(error.to_string());
        }
    };
    if let Err(error) = handler.ensure_thread_visible(&thread_id) {
        let _ = server.shutdown().await;
        return ChildExecutionOutcome::Failed(error.to_string());
    }
    if context.cancellation.is_cancelled() {
        let _ = server.shutdown().await;
        return ChildExecutionOutcome::Cancelled(
            "managed cancellation was observed before turn start".to_owned(),
        );
    }
    let turn = server
        .run_turn_cancellable(
            &thread_id,
            &spec.model,
            &spec.options,
            ManagedTurnInput {
                text: spec.task,
                local_images: Vec::new(),
            },
            &context.cancellation,
            &mut handler,
        )
        .await;
    let shutdown = server.shutdown().await;
    match turn {
        Ok(result) => match shutdown {
            Ok(()) => ChildExecutionOutcome::Completed(ChildExecutionOutput {
                text: result.final_text,
                usage: result.usage.map_or(
                    ChildUsage::Measured {
                        input_tokens: None,
                        output_tokens: None,
                        total_tokens: None,
                        requests: 1,
                        spend_microusd: None,
                    },
                    |usage| ChildUsage::Measured {
                        input_tokens: Some(usage.input_tokens),
                        output_tokens: Some(usage.output_tokens),
                        total_tokens: Some(usage.total_tokens),
                        requests: 1,
                        spend_microusd: None,
                    },
                ),
            }),
            Err(error) => ChildExecutionOutcome::Failed(error.to_string()),
        },
        Err(CodexError::TurnInterrupted { .. }) if context.cancellation.is_cancelled() => {
            ChildExecutionOutcome::Cancelled("Codex acknowledged turn interruption".to_owned())
        }
        Err(CodexError::RequestCancelled(_)) if context.cancellation.is_cancelled() => {
            ChildExecutionOutcome::Cancelled(
                "managed cancellation was observed while starting the Codex turn".to_owned(),
            )
        }
        Err(error) => ChildExecutionOutcome::Failed(error.to_string()),
    }
}

struct ChildManagedHandler {
    attribution: super::ChildAttribution,
    operation_id: crate::identity::OperationId,
    permissions: PermissionBrokerHandle,
    events: AgentEventSender,
    workspace: PathBuf,
    observed_thread: Option<String>,
}

impl ChildManagedHandler {
    fn new(context: &ChildExecutionContext, workspace: PathBuf) -> Self {
        Self {
            attribution: context.attribution.clone(),
            operation_id: context.operation_id,
            permissions: context.permissions.clone(),
            events: context.events.clone(),
            workspace,
            observed_thread: None,
        }
    }

    fn ensure_thread_visible(&mut self, thread_id: &str) -> Result<(), CodexError> {
        if self.observed_thread.as_deref() != Some(thread_id) {
            self.notification(ManagedNotification::ThreadStarted {
                thread_id: thread_id.to_owned(),
            })?;
        }
        Ok(())
    }

    fn rejection(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, CodexError> {
        if request.available_decisions.contains("decline") {
            Ok(ApprovalDecision::Decline)
        } else if request.available_decisions.contains("cancel") {
            Ok(ApprovalDecision::Cancel)
        } else {
            Err(CodexError::Protocol(
                "managed approval offered no supported fail-closed decision".to_owned(),
            ))
        }
    }

    fn scope(&self, request: &ApprovalRequest) -> PermissionScope {
        let (Some(command), Some(cwd)) = (&request.command, &request.cwd) else {
            return PermissionScope::Unscoped;
        };
        let Ok(canonical_cwd) = PathBuf::from(cwd).canonicalize() else {
            return PermissionScope::Unscoped;
        };
        if !canonical_cwd.starts_with(&self.workspace) {
            return PermissionScope::Unscoped;
        }
        PermissionScope::Command {
            shell: "codex-managed".to_owned(),
            canonical_cwd,
            command: command.clone(),
        }
    }
}

impl ManagedEventHandler for ChildManagedHandler {
    fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError> {
        if let ManagedNotification::ThreadStarted { thread_id } = &notification {
            self.observed_thread = Some(thread_id.clone());
        }
        let _ = self.events.send(AgentEvent::ChildActivity {
            attribution: self.attribution.clone(),
            activity: ChildActivity::ManagedRuntime { notification },
        });
        Ok(())
    }

    fn approve<'a>(
        &'a mut self,
        request: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        Box::pin(async move {
            let scope = self.scope(&request);
            let authorization = self
                .permissions
                .authorize(PermissionRequest {
                    operation_id: self.operation_id,
                    invocation_id: ToolInvocationId::new(),
                    tool_name: "codex_managed_approval".to_owned(),
                    effect_class: if request.method.contains("fileChange") {
                        EffectClass::Write
                    } else {
                        EffectClass::Execute
                    },
                    final_arguments: json!({
                        "method": request.method,
                        "item_id": request.item_id,
                        "reason": request.reason,
                        "command": request.command,
                        "cwd": request.cwd,
                    }),
                    scope: scope.clone(),
                    outbound_review: None,
                })
                .await
                .map_err(|_| CodexError::Protocol("child permission broker closed".to_owned()))?;
            let Authorization::Allowed(_) = authorization else {
                return self.rejection(&request);
            };
            if request.available_decisions.contains("accept") {
                Ok(ApprovalDecision::AcceptOnce)
            } else {
                self.rejection(&request)
            }
        })
    }
}

pub(crate) fn child_policy(mode: PermissionMode) -> Result<ManagedThreadPolicy, &'static str> {
    match mode {
        PermissionMode::Deny => Err(
            "the current Codex app-server contract cannot prove that all inner tool effects are disabled",
        ),
        PermissionMode::Ask => Ok(ManagedThreadPolicy {
            approval: ManagedApprovalPolicy::OnRequest,
            sandbox: ManagedSandbox::WorkspaceWrite,
            ephemeral: true,
        }),
        PermissionMode::Allow => Ok(ManagedThreadPolicy {
            approval: ManagedApprovalPolicy::Never,
            sandbox: ManagedSandbox::WorkspaceWrite,
            ephemeral: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{AgentId, OperationId, ThreadId},
        orchestration::{ChildAttribution, ExecutionOwner},
        permission::{ControllerDecision, PermissionBroker, PermissionPolicy, PolicyDecision},
    };
    use std::collections::BTreeSet;
    use tempfile::tempdir;

    fn attribution(operation_id: OperationId) -> ChildAttribution {
        ChildAttribution {
            agent_id: AgentId::new(),
            parent_agent_id: AgentId::new(),
            operation_id,
            parent_operation_id: OperationId::new(),
            thread_id: ThreadId::new(),
            route: "codex-review".to_owned(),
            profile: "codex-review".to_owned(),
            owner: ExecutionOwner::Codex,
            connection: "codex".to_owned(),
            model: "gpt-test".to_owned(),
        }
    }

    #[tokio::test]
    async fn approval_waits_on_the_child_permission_owner_and_preserves_scope() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().canonicalize().expect("workspace");
        let operation_id = OperationId::new();
        let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let policy = PermissionPolicy::new(PolicyDecision::Ask, Vec::new(), &workspace)
            .expect("permission policy");
        let (permissions, broker) = PermissionBroker::spawn(policy, true, events.clone());
        let mut handler = ChildManagedHandler {
            attribution: attribution(operation_id),
            operation_id,
            permissions: permissions.clone(),
            events: events.into(),
            workspace: workspace.clone(),
            observed_thread: None,
        };
        let request = ApprovalRequest {
            item_id: Some("command-1".to_owned()),
            method: "item/commandExecution/requestApproval".to_owned(),
            available_decisions: BTreeSet::from([
                "accept".to_owned(),
                "acceptForSession".to_owned(),
                "decline".to_owned(),
            ]),
            reason: Some("run tests".to_owned()),
            command: Some("cargo test".to_owned()),
            cwd: Some(workspace.display().to_string()),
        };
        let mut approval = handler.approve(request);
        let pending = loop {
            tokio::select! {
                result = &mut approval => panic!("approval resolved before controller: {result:?}"),
                event = event_receiver.recv() => {
                    if let Some(AgentEvent::PermissionRequested { request }) = event {
                        break request;
                    }
                }
            }
        };
        assert!(matches!(pending.scope, PermissionScope::Command { .. }));
        permissions
            .decide(
                pending.operation_id,
                pending.invocation_id,
                ControllerDecision::AllowSession {
                    scope: pending.scope.clone(),
                },
            )
            .await
            .expect("controller decision");

        assert_eq!(
            approval.await.expect("approval response"),
            ApprovalDecision::AcceptOnce
        );

        let session_only = ApprovalRequest {
            item_id: Some("command-2".to_owned()),
            method: "item/commandExecution/requestApproval".to_owned(),
            available_decisions: BTreeSet::from([
                "acceptForSession".to_owned(),
                "decline".to_owned(),
            ]),
            reason: Some("run tests again".to_owned()),
            command: Some("cargo test".to_owned()),
            cwd: Some(workspace.display().to_string()),
        };
        assert_eq!(
            handler
                .approve(session_only)
                .await
                .expect("fail-closed approval response"),
            ApprovalDecision::Decline
        );
        permissions.shutdown();
        broker.await.expect("broker task");
    }

    #[tokio::test]
    async fn denied_managed_approval_fails_closed_without_controller_input() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().canonicalize().expect("workspace");
        let operation_id = OperationId::new();
        let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), &workspace)
            .expect("permission policy");
        let (permissions, broker) = PermissionBroker::spawn(policy, true, events.clone());
        let mut handler = ChildManagedHandler {
            attribution: attribution(operation_id),
            operation_id,
            permissions: permissions.clone(),
            events: events.into(),
            workspace,
            observed_thread: None,
        };

        let decision = handler
            .approve(ApprovalRequest {
                item_id: Some("command-1".to_owned()),
                method: "item/commandExecution/requestApproval".to_owned(),
                available_decisions: BTreeSet::from(["accept".to_owned(), "decline".to_owned()]),
                reason: Some("run tests".to_owned()),
                command: Some("cargo test".to_owned()),
                cwd: None,
            })
            .await
            .expect("fail-closed decision");

        assert_eq!(decision, ApprovalDecision::Decline);
        permissions.shutdown();
        broker.await.expect("broker task");
    }

    #[tokio::test]
    async fn cancellation_before_start_never_spawns_the_managed_process() {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().canonicalize().expect("workspace");
        let operation_id = OperationId::new();
        let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), &workspace)
            .expect("permission policy");
        let (permissions, broker) = PermissionBroker::spawn(policy, true, events.clone());
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let outcome = AppServerCodexRunner
            .run(
                ManagedCodexChildSpec {
                    launch: CodexLaunchConfig {
                        program: "definitely-not-a-real-codex-executable".to_owned(),
                        home: None,
                    },
                    workspace,
                    model: "gpt-test".to_owned(),
                    options: ManagedTurnOptions {
                        reasoning_effort: None,
                        reasoning_summary: None,
                    },
                    task: "must not start".to_owned(),
                    policy: ManagedThreadPolicy {
                        approval: ManagedApprovalPolicy::Never,
                        sandbox: ManagedSandbox::WorkspaceWrite,
                        ephemeral: true,
                    },
                },
                ChildExecutionContext {
                    attribution: attribution(operation_id),
                    operation_id,
                    permissions: permissions.clone(),
                    events: events.into(),
                    cancellation,
                },
            )
            .await;

        assert!(matches!(outcome, ChildExecutionOutcome::Cancelled(_)));
        permissions.shutdown();
        broker.await.expect("broker task");
    }

    #[test]
    fn managed_permission_modes_never_select_full_host_access() {
        assert!(child_policy(PermissionMode::Deny).is_err());
        assert_eq!(
            child_policy(PermissionMode::Ask)
                .expect("ask policy")
                .approval,
            ManagedApprovalPolicy::OnRequest
        );
        assert_eq!(
            child_policy(PermissionMode::Allow)
                .expect("allow policy")
                .sandbox,
            ManagedSandbox::WorkspaceWrite
        );
        assert!(
            child_policy(PermissionMode::Allow)
                .expect("allow policy")
                .ephemeral
        );
    }

    #[tokio::test]
    #[ignore = "requires an authenticated local Codex CLI and XANA_LIVE_CODEX_MODEL"]
    async fn live_codex_child_completes_with_visible_activity() {
        let model = std::env::var("XANA_LIVE_CODEX_MODEL")
            .expect("set XANA_LIVE_CODEX_MODEL to an advertised Codex model");
        let program =
            std::env::var("XANA_LIVE_CODEX_PROGRAM").unwrap_or_else(|_| "codex".to_owned());
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().canonicalize().expect("workspace");
        let operation_id = OperationId::new();
        let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), &workspace)
            .expect("permission policy");
        let (permissions, broker) = PermissionBroker::spawn(policy, true, events.clone());
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(180),
            AppServerCodexRunner.run(
                ManagedCodexChildSpec {
                    launch: CodexLaunchConfig {
                        program,
                        home: None,
                    },
                    workspace,
                    model,
                    options: ManagedTurnOptions {
                        reasoning_effort: None,
                        reasoning_summary: None,
                    },
                    task: "Reply with exactly: phase 4 live smoke complete. Do not use tools."
                        .to_owned(),
                    policy: child_policy(PermissionMode::Ask).expect("ask policy"),
                },
                ChildExecutionContext {
                    attribution: attribution(operation_id),
                    operation_id,
                    permissions: permissions.clone(),
                    events: events.into(),
                    cancellation: tokio_util::sync::CancellationToken::new(),
                },
            ),
        )
        .await
        .expect("live Codex child timeout");

        let ChildExecutionOutcome::Completed(output) = outcome else {
            panic!("live Codex child did not complete: {outcome:?}");
        };
        assert!(output.text.contains("phase 4 live smoke complete"));
        assert!(
            std::iter::from_fn(|| event_receiver.try_recv().ok()).any(|event| {
                matches!(
                    event,
                    AgentEvent::ChildActivity {
                        activity: ChildActivity::ManagedRuntime { .. },
                        ..
                    }
                )
            })
        );
        permissions.shutdown();
        broker.await.expect("broker task");
    }

    #[tokio::test]
    #[ignore = "requires an authenticated local Codex CLI and XANA_LIVE_CODEX_MODEL"]
    async fn live_codex_child_cancellation_reaches_a_terminal_report() {
        let model = std::env::var("XANA_LIVE_CODEX_MODEL")
            .expect("set XANA_LIVE_CODEX_MODEL to an advertised Codex model");
        let program =
            std::env::var("XANA_LIVE_CODEX_PROGRAM").unwrap_or_else(|_| "codex".to_owned());
        let directory = tempdir().expect("temporary directory");
        let workspace = directory.path().canonicalize().expect("workspace");
        let operation_id = OperationId::new();
        let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), &workspace)
            .expect("permission policy");
        let (permissions, broker) = PermissionBroker::spawn(policy, true, events.clone());
        let cancellation = tokio_util::sync::CancellationToken::new();
        let mut run = AppServerCodexRunner.run(
            ManagedCodexChildSpec {
                launch: CodexLaunchConfig {
                    program,
                    home: None,
                },
                workspace,
                model,
                options: ManagedTurnOptions {
                    reasoning_effort: None,
                    reasoning_summary: None,
                },
                task:
                    "Write a detailed 5,000-word explanation of Rust ownership without using tools."
                        .to_owned(),
                policy: child_policy(PermissionMode::Ask).expect("ask policy"),
            },
            ChildExecutionContext {
                attribution: attribution(operation_id),
                operation_id,
                permissions: permissions.clone(),
                events: events.into(),
                cancellation: cancellation.clone(),
            },
        );

        tokio::time::timeout(std::time::Duration::from_secs(180), async {
            loop {
                tokio::select! {
                    outcome = &mut run => panic!(
                        "live Codex child finished before cancellation could be requested: {outcome:?}"
                    ),
                    event = event_receiver.recv() => {
                        if matches!(
                            event,
                            Some(AgentEvent::ChildActivity {
                                activity: ChildActivity::ManagedRuntime {
                                    notification: ManagedNotification::ThreadStarted { .. }
                                },
                                ..
                            })
                        ) {
                            continue;
                        }
                        if matches!(
                            event,
                            Some(AgentEvent::ChildActivity {
                                activity: ChildActivity::ManagedRuntime { .. },
                                ..
                            })
                        ) {
                            break;
                        }
                    }
                }
            }
        })
        .await
        .expect("live Codex child never emitted turn activity");
        cancellation.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), run)
            .await
            .expect("live Codex cancellation did not terminate");
        assert!(matches!(outcome, ChildExecutionOutcome::Cancelled(_)));
        permissions.shutdown();
        broker.await.expect("broker task");
    }
}
