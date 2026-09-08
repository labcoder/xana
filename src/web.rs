//! Bounded public-web retrieval policy shared by native search and page reading.
//! This module does not select a conversational model or synthesize answers.

mod config;
mod turn;

pub(crate) use config::{PublicWebConsent, SearchConnection, SearchProvider, WebConfig, WebLimits};
pub(crate) use turn::{WebFailure, WebRuntime, WebTurn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum WebStage {
    Searching,
    Reading,
    Redirecting,
    Extracting,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WebProgress {
    pub(crate) operation_id: crate::identity::OperationId,
    pub(crate) stage: WebStage,
    pub(crate) elapsed_ms: u64,
}

impl WebProgress {
    pub(crate) fn label(&self) -> String {
        let stage = match self.stage {
            WebStage::Searching => "searching public sources",
            WebStage::Reading => "reading a public page",
            WebStage::Redirecting => "checking a redirect",
            WebStage::Extracting => "extracting page text",
            WebStage::Complete => "web evidence ready",
            WebStage::Failed => "web request stopped; see tool result",
        };
        format!(
            "{stage} ({:.1}s into web work)",
            self.elapsed_ms as f64 / 1000.
        )
    }
}

pub(crate) fn progress(
    events: Option<&crate::native_runtime::AgentEventSender>,
    operation_id: crate::identity::OperationId,
    stage: WebStage,
    turn: &WebTurn,
) {
    if let Some(events) = events {
        let _ = events.send(crate::native_runtime::AgentEvent::WebProgress {
            progress: WebProgress {
                operation_id,
                stage,
                elapsed_ms: turn.elapsed_ms(),
            },
        });
    }
}
