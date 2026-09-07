//! Turn-owned adapter from the managed callback into Xana's shared tools and
//! permission broker. No vendor output constructs owner provenance or authority.

use super::ManagedChatConfig;
use crate::{
    identity::{ConversationId, OperationId, ToolInvocationId},
    managed::codex::{
        ApprovalDecision, ApprovalRequest, CodexError, ManagedEventHandler, ManagedNotification,
        ManagedToolCall, ManagedToolResult,
    },
    message::{ToolCall, ToolResultStatus},
    native_runtime::AgentEvent,
    permission::{
        ControllerDecision, PermissionAuditFact, PermissionBroker, PermissionBrokerHandle,
        PermissionPolicy, PermissionRequest,
    },
    tool::{DeferredCleanup, OwnerTurnInput, ToolContext, ToolDefinition, ToolRegistry},
};
use futures::future::BoxFuture;
use std::path::PathBuf;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// Xana-owned exact review, never translated to a Codex sandbox request.
pub(super) struct MemoryReview {
    pub(super) request:
        Box<dyn Fn(PermissionRequest) -> BoxFuture<'static, ControllerDecision> + Send + Sync>,
    pub(super) audit: Box<dyn Fn(PermissionAuditFact) -> BoxFuture<'static, ()> + Send + Sync>,
}

impl MemoryReview {
    pub(super) fn new(
        request: impl Fn(PermissionRequest) -> BoxFuture<'static, ControllerDecision>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            request: Box::new(request),
            audit: Box::new(|_| Box::pin(async {})),
        }
    }
}

pub(super) struct MemoryManagedHandler<'a, H> {
    inner: &'a mut H,
    registry: ToolRegistry,
    owner_input: OwnerTurnInput,
    workspace: PathBuf,
    permissions: PermissionBrokerHandle,
    events: mpsc::UnboundedReceiver<AgentEvent>,
    broker: Option<JoinHandle<()>>,
    review: MemoryReview,
    cleanup: DeferredCleanup,
}

impl<'a, H: ManagedEventHandler> MemoryManagedHandler<'a, H> {
    pub(super) fn new(
        config: &ManagedChatConfig,
        conversation: ConversationId,
        owner_input: OwnerTurnInput,
        inner: &'a mut H,
        review: MemoryReview,
    ) -> Result<Self, CodexError> {
        let owner = super::memory_context::for_conversation(config.memory.as_ref(), conversation);
        let mut registry = ToolRegistry::new();
        crate::memory::tools::register(&mut registry, owner)
            .map_err(|error| CodexError::Protocol(error.to_string()))?;
        let policy = PermissionPolicy::new(
            config.permission_default,
            config.permission_rules.clone(),
            &config.workspace,
        )
        .map_err(|error| CodexError::Protocol(error.to_string()))?;
        let (sender, events) = mpsc::unbounded_channel();
        let (permissions, broker) = PermissionBroker::spawn(policy, true, sender);
        Ok(Self {
            inner,
            registry,
            owner_input,
            workspace: config.workspace.clone(),
            permissions,
            events,
            broker: Some(broker),
            review,
            cleanup: DeferredCleanup::default(),
        })
    }

    pub(super) async fn finish(mut self) -> Result<(), CodexError> {
        self.cleanup.drain().await;
        self.permissions.shutdown();
        if let Some(broker) = self.broker.take() {
            broker.await.map_err(|_| {
                CodexError::Io("memory permission broker stopped unexpectedly".into())
            })?;
        }
        while let Ok(event) = self.events.try_recv() {
            if let AgentEvent::PermissionAudited { fact } = event {
                (self.review.audit)(fact).await;
            }
        }
        Ok(())
    }
}

impl<H> Drop for MemoryManagedHandler<'_, H> {
    fn drop(&mut self) {
        self.permissions.controller_lost();
        self.permissions.shutdown();
        // No storage work lives in this task. Execution itself is always joined
        // inside dynamic_tool before the adapter can be dropped normally.
        if let Some(broker) = self.broker.take() {
            broker.abort();
        }
    }
}

impl<H: ManagedEventHandler> ManagedEventHandler for MemoryManagedHandler<'_, H> {
    fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError> {
        self.inner.notification(notification)
    }

    fn approve<'a>(
        &'a mut self,
        request: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        self.inner.approve(request)
    }

    fn memory_tool_definitions(&self) -> Vec<ToolDefinition> {
        crate::memory::tools::definitions()
    }

    fn dynamic_tool<'a>(
        &'a mut self,
        call: ManagedToolCall,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<ManagedToolResult, CodexError>> {
        let Self {
            registry,
            owner_input,
            workspace,
            permissions,
            events,
            review,
            cleanup,
            ..
        } = self;
        Box::pin(async move {
            let call = ToolCall {
                id: call.call_id,
                name: call.name,
                arguments: call.arguments,
            };
            let invocation_id = ToolInvocationId::new();
            let context = ToolContext {
                workspace_root: workspace,
                operation_id: owner_input.operation_id,
                invocation_id,
                permissions,
                events: None,
                cleanup: cleanup.clone(),
            };
            let invocation = registry.invoke_in_turn(&call, context, Some(owner_input));
            tokio::pin!(invocation);
            let result = loop {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        owner_input.cancellation.cancel();
                        permissions.controller_lost();
                        // Cancels pending approvals but never drops a started
                        // bounded memory transaction; return its known outcome.
                        break invocation.await;
                    }
                    result = &mut invocation => break result,
                    event = events.recv() => {
                        if let Some(AgentEvent::PermissionRequested {request}) = event {
                            let decision = tokio::select! {
                                biased;
                                _ = cancellation.cancelled() => ControllerDecision::Deny,
                                result = (review.request)(request.clone()) => result,
                            };
                            // Cancellation may have removed the pending request.
                            let _ = permissions.decide(request.operation_id, request.invocation_id, decision).await;
                        } else if let Some(AgentEvent::PermissionAudited {fact}) = event {
                            (review.audit)(fact).await;
                        } else if event.is_none() {
                            permissions.controller_lost();
                            break invocation.await;
                        }
                    }
                }
            };
            while let Ok(event) = events.try_recv() {
                if let AgentEvent::PermissionAudited { fact } = event {
                    (review.audit)(fact).await;
                }
            }
            Ok(ManagedToolResult {
                text: result.output,
                success: result.status == ToolResultStatus::Success,
            })
        })
    }
}

pub(super) fn owner_input(
    operation_id: OperationId,
    text: &str,
    cancellation: CancellationToken,
) -> OwnerTurnInput {
    OwnerTurnInput {
        operation_id,
        // Managed Codex owns its transcript. This is Xana's owner-input request
        // identity, not a fabricated local transcript EntryId or vendor call id.
        source_id: uuid::Uuid::new_v4(),
        text: std::sync::Arc::from(text),
        cancellation,
    }
}

#[cfg(test)]
mod tests;
