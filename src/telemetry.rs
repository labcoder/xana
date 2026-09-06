//! Explicit, metadata-only telemetry seam for headless runtime components.
//!
//! The agent and tool registry report typed facts through an injected sink.
//! They never discover or mutate process-global diagnostic state themselves.

use crate::identity::OperationId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeTelemetryKind {
    ProviderFailed,
    ToolDenied,
    ToolFailed,
    StorageFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeTelemetryEvent {
    pub(crate) operation_id: OperationId,
    pub(crate) kind: RuntimeTelemetryKind,
    pub(crate) subject: String,
}

pub(crate) trait RuntimeTelemetry: Send + Sync {
    fn record(&self, event: RuntimeTelemetryEvent);

    fn provider_failure(&self, _operation: OperationId, _failure: crate::failure::FailureDetails) {}

    fn terminal(&self, _diagnostic: crate::failure::TerminalDiagnostic) {}

    fn context_phase(&self, _event: ContextPhaseEvent) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextPhase {
    SourceAdmission,
    SourcePreparation,
    HelperGeneration,
    CheckpointCommit,
    PromptPreparation,
}

#[derive(Debug, Clone)]
pub(crate) struct ContextPhaseEvent {
    pub(crate) operation_id: OperationId,
    pub(crate) phase: ContextPhase,
    pub(crate) elapsed: std::time::Duration,
}

#[derive(Debug, Default)]
pub(crate) struct NoopRuntimeTelemetry;

impl RuntimeTelemetry for NoopRuntimeTelemetry {
    fn record(&self, _event: RuntimeTelemetryEvent) {}
}
