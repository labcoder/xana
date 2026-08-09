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
    permission::{
        Authorization, ControllerDecision, PermissionBrokerHandle, PermissionRequest,
        PermissionScope,
    },
    runtime::AgentEvent,
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
    let mut server = match CodexAppServer::spawn(&spec.launch).await {
        Ok(server) => server,
        Err(error) => return ChildExecutionOutcome::Failed(error.to_string()),
    };
    let mut handler = ChildManagedHandler::new(&context, spec.workspace.clone());
    let turn = async {
        if matches!(server.account_status().await?, AccountStatus::LoggedOut) {
            return Err(CodexError::LoginFailed(
                "managed child connection is logged out".to_owned(),
            ));
        }
        let thread_id = server
            .start_thread_with_policy(
                &spec.model,
                &spec.workspace,
                crate::prompt::xana_identity(),
                spec.policy,
                &mut handler,
            )
            .await?;
        handler.ensure_thread_visible(&thread_id)?;
        server
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
            .await
    }
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
        Err(error) if context.cancellation.is_cancelled() => ChildExecutionOutcome::Cancelled(
            format!("managed cancellation closed Codex after interrupt failed: {error}"),
        ),
        Err(error) => ChildExecutionOutcome::Failed(error.to_string()),
    }
}

struct ChildManagedHandler {
    attribution: super::ChildAttribution,
    operation_id: crate::identity::OperationId,
    permissions: PermissionBrokerHandle,
    events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
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
                })
                .await
                .map_err(|_| CodexError::Protocol("child permission broker closed".to_owned()))?;
            let Authorization::Allowed(fact) = authorization else {
                return self.rejection(&request);
            };
            if matches!(
                fact.controller_decision,
                Some(ControllerDecision::AllowSession { .. })
            ) && request.available_decisions.contains("acceptForSession")
            {
                return Ok(ApprovalDecision::AcceptForSession);
            }
            if request.available_decisions.contains("accept") {
                Ok(ApprovalDecision::AcceptOnce)
            } else {
                self.rejection(&request)
            }
        })
    }
}

pub(crate) fn child_policy(mode: PermissionMode) -> ManagedThreadPolicy {
    match mode {
        PermissionMode::Deny => ManagedThreadPolicy {
            approval: ManagedApprovalPolicy::Never,
            sandbox: ManagedSandbox::ReadOnly,
            ephemeral: true,
        },
        PermissionMode::Ask => ManagedThreadPolicy {
            approval: ManagedApprovalPolicy::OnRequest,
            sandbox: ManagedSandbox::WorkspaceWrite,
            ephemeral: true,
        },
        PermissionMode::Allow => ManagedThreadPolicy {
            approval: ManagedApprovalPolicy::Never,
            sandbox: ManagedSandbox::WorkspaceWrite,
            ephemeral: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{AgentId, OperationId, ThreadId},
        orchestration::{ChildAttribution, ExecutionOwner},
        permission::{PermissionBroker, PermissionPolicy, PolicyDecision},
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
            events,
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
            ApprovalDecision::AcceptForSession
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
            events,
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

    #[test]
    fn managed_permission_modes_never_select_full_host_access() {
        assert_eq!(
            child_policy(PermissionMode::Deny).sandbox,
            ManagedSandbox::ReadOnly
        );
        assert_eq!(
            child_policy(PermissionMode::Ask).approval,
            ManagedApprovalPolicy::OnRequest
        );
        assert_eq!(
            child_policy(PermissionMode::Allow).sandbox,
            ManagedSandbox::WorkspaceWrite
        );
        assert!(child_policy(PermissionMode::Allow).ephemeral);
    }
}
