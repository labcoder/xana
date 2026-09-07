//! Bound repeated failed attempts, including errors rejected before execution.
//! Keep only hashes of a small recent window; successful polling is not failure.
use crate::message::{
    ContentBlock, Message, Role, ToolCall, ToolFailure, ToolResult, ToolResultStatus,
};
use std::collections::VecDeque;

const WINDOW: usize = 32;
const REPEAT_LIMIT: usize = 3;
const CONSECUTIVE_ERROR_LIMIT: usize = 6;
pub(super) const STOP_REASON: &str = "Stopped after repeated tool failures without progress. Remaining tools in this batch were not executed. Review the tool errors and correct the request before retrying; repeating failed calls will not fix them.";

#[derive(Default)]
pub(super) struct ProgressGuard {
    failed: VecDeque<blake3::Hash>,
    denied: VecDeque<blake3::Hash>,
    unavailable: VecDeque<blake3::Hash>,
    consecutive_errors: usize,
    stopped: bool,
}

impl ProgressGuard {
    /// A tranche restart is not a new user turn. Rebuild from the current owner
    /// turn's committed call/result pairs, not old conversations or result prose.
    pub(super) fn from_history(messages: &[Message]) -> Self {
        let mut guard = Self::default();
        let start = messages
            .iter()
            .rposition(|message| message.role == Role::User)
            .map_or(0, |index| index + 1);
        let mut calls = &[][..];
        for message in &messages[start..] {
            if message.role == Role::Assistant {
                calls = message.content.as_slice();
            } else if message.role == Role::Tool {
                for block in &message.content {
                    if let ContentBlock::ToolResult(result) = block
                        && let Some(call) = calls.iter().find_map(|block| match block {
                            ContentBlock::ToolCall(call) if call.id == result.call_id => Some(call),
                            _ => None,
                        })
                    {
                        guard.observe(call, result);
                    }
                }
            }
        }
        guard
    }

    pub(super) fn blocked_result(&self, call: &ToolCall) -> Option<ToolResult> {
        if self.stopped {
            Some(ToolResult::error(call.id.clone(), STOP_REASON))
        } else if self
            .unavailable
            .contains(&blake3::hash(call.name.as_bytes()))
        {
            Some(ToolResult::unavailable(
                call.id.clone(),
                "This capability is unavailable for this turn. Retrying cannot enable it; no tool was executed.",
            ))
        } else if self.denied.contains(&call.pattern_fingerprint()) {
            Some(ToolResult::denied(
                call.id.clone(),
                "This exact tool request was already denied in this turn. Do not request it again; choose another approach or ask the user for direction.",
            ))
        } else {
            None
        }
    }

    pub(super) fn stopped(&self) -> bool {
        self.stopped
    }

    pub(super) fn observe(&mut self, call: &ToolCall, result: &ToolResult) {
        let pattern = call.pattern_fingerprint();
        if result.status == ToolResultStatus::Success {
            self.consecutive_errors = 0;
            self.failed.retain(|value| *value != pattern);
            return;
        }
        if result.failure == Some(ToolFailure::Unavailable) {
            // Permit an answer or useful alternative after the first rejection.
            // A retry of the same capability is permanent even with new arguments.
            let capability = blake3::hash(call.name.as_bytes());
            if self.unavailable.contains(&capability) {
                self.stopped = true;
            } else {
                if self.unavailable.len() == WINDOW {
                    self.unavailable.pop_front();
                }
                self.unavailable.push_back(capability);
            }
        }
        if result.failure == Some(ToolFailure::PermissionDenied) && !self.denied.contains(&pattern)
        {
            if self.denied.len() == WINDOW {
                self.denied.pop_front();
            }
            self.denied.push_back(pattern);
        }
        self.consecutive_errors = self.consecutive_errors.saturating_add(1);
        if self.failed.len() == WINDOW {
            self.failed.pop_front();
        }
        self.failed.push_back(pattern);
        self.stopped |= self
            .failed
            .iter()
            .filter(|value| **value == pattern)
            .count()
            >= REPEAT_LIMIT
            || self.consecutive_errors >= CONSECUTIVE_ERROR_LIMIT;
    }
}

#[cfg(test)]
mod tests;
