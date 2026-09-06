//! Preserve originating failure facts before cleanup and legacy prose rendering.

use super::*;
use crate::failure::{
    FailureCategory, FailureDetails, FailureOrigin, FailureStage, TerminalDiagnostic,
    TerminalOutcome,
};

pub(super) struct OperationFailure {
    pub(super) reason: String,
    pub(super) failure: FailureDetails,
}

impl OperationFailure {
    pub(super) fn from_error(error: anyhow::Error) -> Self {
        let failure = error
            .downcast_ref::<crate::provider::ProviderError>()
            .map(crate::provider::ProviderError::failure)
            .unwrap_or_else(|| {
                let storage = error.downcast_ref::<std::io::Error>().is_some()
                    || error
                        .downcast_ref::<crate::failure::PersistenceFailure>()
                        .is_some();
                FailureDetails::new(
                    if storage {
                        FailureCategory::Storage
                    } else {
                        FailureCategory::Unknown
                    },
                    if storage {
                        FailureStage::Persistence
                    } else {
                        FailureStage::Execution
                    },
                )
            });
        Self {
            reason: format!("{error:#}"),
            failure,
        }
    }

    pub(super) fn panicked() -> Self {
        Self {
            reason: "native operation task panicked".into(),
            failure: FailureDetails::new(FailureCategory::HostPanic, FailureStage::Execution),
        }
    }
}

impl Runtime {
    pub(super) fn terminal_diagnostic(
        &self,
        operation: Option<OperationId>,
        outcome: TerminalOutcome,
        failure: FailureDetails,
    ) {
        let conversation = self
            .session
            .as_ref()
            .map(|session| crate::identity::ConversationId::for_native(session.session_id()));
        let (route, model) = self.agent.diagnostic_route();
        let origin = if outcome == TerminalOutcome::HostShutdown {
            FailureOrigin::Host
        } else {
            FailureOrigin::Native
        };
        let diagnostic = TerminalDiagnostic::new(operation, conversation, origin, outcome, failure)
            .route(route, model);
        self.agent.record_terminal(diagnostic.clone());
        self.emit(AgentEvent::TerminalDiagnostic { diagnostic });
    }

    pub(super) fn storage_diagnostic(&self, operation: OperationId) {
        self.terminal_diagnostic(
            Some(operation),
            TerminalOutcome::Failed,
            FailureDetails::new(FailureCategory::Storage, FailureStage::Persistence),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejected_writer_ack_retains_typed_persistence_origin_without_recording_prose() {
        let (sender, mut receiver) = crate::operation::DurableOperationSender::channel();
        let operation = OperationId::new();
        let writer = tokio::spawn(async move {
            let Some(crate::operation::DurableOperationCommand::Append { acknowledged, .. }) =
                receiver.recv().await
            else {
                panic!("append request");
            };
            acknowledged
                .send(Err("C:/private/canary-path: disk failure".into()))
                .unwrap();
        });
        let error = sender
            .append(
                SessionRecord::OperationStateChanged {
                    operation_id: operation,
                    state: OperationState::Running,
                },
                None,
            )
            .await
            .unwrap_err();
        writer.await.unwrap();
        let error = OperationFailure::from_error(error.context("outer execution context"));
        assert_eq!(error.failure.category, FailureCategory::Storage);
        assert_eq!(error.failure.stage, FailureStage::Persistence);
        let record = TerminalDiagnostic::new(
            Some(operation),
            None,
            FailureOrigin::Native,
            TerminalOutcome::Failed,
            error.failure,
        );
        let text = serde_json::to_string(&record).unwrap();
        assert!(!text.contains("canary"));
        assert_eq!(record.operation_id, Some(operation));
    }
}
