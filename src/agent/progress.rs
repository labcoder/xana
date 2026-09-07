//! Bound repeated failed attempts, including errors rejected before execution.
//! Keep only hashes of a small recent window; successful polling is not failure.
use crate::message::{ToolCall, ToolResult, ToolResultStatus};
use std::collections::VecDeque;

const WINDOW: usize = 32;
const REPEAT_LIMIT: usize = 3;
pub(super) const STOP_REASON: &str = "Stopped after repeated tool failures with unchanged arguments. No further tools in this batch were executed. Review the tool error and correct the request before retrying; repeating the same call will not fix it.";

#[derive(Default)]
pub(super) struct ProgressGuard {
    failed: VecDeque<blake3::Hash>,
    stopped: bool,
}

impl ProgressGuard {
    pub(super) fn stopped(&self) -> bool {
        self.stopped
    }

    pub(super) fn observe(&mut self, call: &ToolCall, result: &ToolResult) {
        let pattern = call.pattern_fingerprint();
        if result.status == ToolResultStatus::Success {
            self.failed.retain(|value| *value != pattern);
            return;
        }
        if self.failed.len() == WINDOW {
            self.failed.pop_front();
        }
        self.failed.push_back(pattern);
        self.stopped |= self
            .failed
            .iter()
            .filter(|value| **value == pattern)
            .count()
            >= REPEAT_LIMIT;
    }
}
