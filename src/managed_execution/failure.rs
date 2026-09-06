//! Managed owner failures remain typed without inventing vendor HTTP detail.

use super::*;
use crate::failure::{
    FailureCategory, FailureDetails, FailureOrigin, FailureStage, TerminalDiagnostic,
    TerminalOutcome,
};

pub(super) fn diagnostic(
    error: &CodexError,
    operation: Option<crate::identity::OperationId>,
    conversation: ConversationId,
    config: &ManagedChatConfig,
) -> TerminalDiagnostic {
    let (category, stage, outcome) = match error {
        CodexError::Timeout(_) => (
            FailureCategory::ReadTimeout,
            FailureStage::Execution,
            TerminalOutcome::Failed,
        ),
        CodexError::RequestCancelled(_) => (
            FailureCategory::Cancelled,
            FailureStage::Execution,
            TerminalOutcome::Cancelled,
        ),
        CodexError::TurnInterrupted { .. } => (
            FailureCategory::Interrupted,
            FailureStage::Execution,
            TerminalOutcome::Interrupted,
        ),
        CodexError::Protocol(_) | CodexError::FrameTooLarge => (
            FailureCategory::InvalidResponse,
            FailureStage::ProviderStream,
            TerminalOutcome::Failed,
        ),
        CodexError::Io(_) | CodexError::Spawn(_) => (
            FailureCategory::Transport,
            FailureStage::Execution,
            TerminalOutcome::Failed,
        ),
        _ => (
            FailureCategory::ManagedRuntime,
            FailureStage::Execution,
            TerminalOutcome::Failed,
        ),
    };
    TerminalDiagnostic::new(
        operation,
        Some(conversation),
        FailureOrigin::Managed,
        outcome,
        FailureDetails::new(category, stage),
    )
    .route(Some(&config.connection), Some(&config.model))
}
