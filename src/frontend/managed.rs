//! Provider-neutral, bounded projection of managed-runtime activity.

use crate::managed::codex::{ManagedItem, ManagedNotification, ManagedPlanStep};
use serde::{Deserialize, Serialize};

const MAX_LABEL_BYTES: usize = 160;
const MAX_DELTA_BYTES: usize = 64 * 1024;
const MAX_DIFF_BYTES: usize = 256 * 1024;
const MAX_STEPS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedClientItem {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) status: Option<String>,
    pub(crate) label: String,
    pub(crate) details: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedClientPlanStep {
    pub(crate) step: String,
    pub(crate) status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ManagedClientEvent {
    ThreadReady,
    AssistantDelta(String),
    ReasoningSummaryDelta(String),
    ReasoningSummaryPartAdded,
    ReasoningDelta(String),
    PlanDelta(String),
    PlanUpdated {
        explanation: Option<String>,
        steps: Vec<ManagedClientPlanStep>,
        truncated: bool,
    },
    CommandOutputDelta(String),
    DiffUpdated {
        preview: String,
        truncated: bool,
    },
    ItemStarted(ManagedClientItem),
    ItemCompleted(ManagedClientItem),
    ModelRerouted {
        from_model: String,
        to_model: String,
        reason: String,
    },
    TurnCompleted {
        status: String,
        error: Option<String>,
    },
    TokenUsageUpdated {
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
    Warning(String),
}

impl ManagedClientEvent {
    /// Converts Xana's normalized managed-runtime notification into the
    /// provider-neutral frontend vocabulary. Vendor thread/turn ids, RPC
    /// methods, and login protocol details deliberately do not cross it.
    pub(crate) fn from_notification(notification: ManagedNotification) -> Option<Self> {
        Some(match notification {
            ManagedNotification::ThreadStarted { .. } => Self::ThreadReady,
            ManagedNotification::AssistantDelta { delta, .. } => {
                Self::AssistantDelta(bounded_text(delta, MAX_DELTA_BYTES))
            }
            ManagedNotification::ReasoningSummaryDelta { delta, .. } => {
                Self::ReasoningSummaryDelta(bounded_text(delta, MAX_DELTA_BYTES))
            }
            ManagedNotification::ReasoningSummaryPartAdded { .. } => {
                Self::ReasoningSummaryPartAdded
            }
            ManagedNotification::ReasoningDelta { delta, .. } => {
                Self::ReasoningDelta(bounded_text(delta, MAX_DELTA_BYTES))
            }
            ManagedNotification::PlanDelta { delta, .. } => {
                Self::PlanDelta(bounded_text(delta, MAX_DELTA_BYTES))
            }
            ManagedNotification::PlanUpdated { explanation, steps } => {
                let truncated = steps.len() > MAX_STEPS;
                Self::PlanUpdated {
                    explanation: explanation.map(|value| bounded_text(value, MAX_DELTA_BYTES)),
                    steps: steps
                        .into_iter()
                        .take(MAX_STEPS)
                        .map(ManagedClientPlanStep::from)
                        .collect(),
                    truncated,
                }
            }
            ManagedNotification::CommandOutputDelta { delta, .. } => {
                Self::CommandOutputDelta(bounded_text(delta, MAX_DELTA_BYTES))
            }
            ManagedNotification::DiffUpdated(diff) => {
                let truncated = diff.len() > MAX_DIFF_BYTES;
                Self::DiffUpdated {
                    preview: bounded_text(diff, MAX_DIFF_BYTES),
                    truncated,
                }
            }
            ManagedNotification::ItemStarted(item) => {
                Self::ItemStarted(ManagedClientItem::from(item))
            }
            ManagedNotification::ItemCompleted(item) => {
                Self::ItemCompleted(ManagedClientItem::from(item))
            }
            ManagedNotification::ModelRerouted {
                from_model,
                to_model,
                reason,
            } => Self::ModelRerouted {
                from_model: bounded_text(from_model, MAX_LABEL_BYTES),
                to_model: bounded_text(to_model, MAX_LABEL_BYTES),
                reason: bounded_text(reason, MAX_DELTA_BYTES),
            },
            ManagedNotification::TurnCompleted { status, error, .. } => Self::TurnCompleted {
                status: bounded_text(status, MAX_LABEL_BYTES),
                error: error.map(|value| bounded_text(value, MAX_DELTA_BYTES)),
            },
            ManagedNotification::TokenUsageUpdated {
                input_tokens,
                output_tokens,
                total_tokens,
                ..
            } => Self::TokenUsageUpdated {
                input_tokens,
                output_tokens,
                total_tokens,
            },
            ManagedNotification::Warning(message) => {
                Self::Warning(bounded_text(message, MAX_DELTA_BYTES))
            }
            ManagedNotification::LoginCompleted { .. } | ManagedNotification::Other { .. } => {
                return None;
            }
        })
    }
}

impl From<ManagedItem> for ManagedClientItem {
    fn from(item: ManagedItem) -> Self {
        Self {
            id: bounded_text(item.id, MAX_LABEL_BYTES),
            kind: bounded_text(item.kind, MAX_LABEL_BYTES),
            status: item
                .status
                .map(|value| bounded_text(value, MAX_LABEL_BYTES)),
            label: bounded_text(item.label, MAX_DELTA_BYTES),
            details: bounded_text(item.details, MAX_DELTA_BYTES),
        }
    }
}

impl From<ManagedPlanStep> for ManagedClientPlanStep {
    fn from(step: ManagedPlanStep) -> Self {
        Self {
            step: bounded_text(step.step, MAX_DELTA_BYTES),
            status: bounded_text(step.status, MAX_LABEL_BYTES),
        }
    }
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}
