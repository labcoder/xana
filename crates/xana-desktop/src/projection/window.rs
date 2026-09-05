//! One retained window feeds Chat's existing stable-ID virtualized list.
//! Bounds apply on ingress; an unchanged snapshot reuses its Arc allocation.

use super::ConversationProjection;
use std::collections::HashSet;

const MAX_MESSAGES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_WINDOW_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESOURCES: usize = 128;
const MAX_ACTIVITY: usize = 128;

pub(super) fn append_text(text: &mut String, delta: &str) {
    let available = MAX_MESSAGE_BYTES.saturating_sub(text.len());
    text.push_str(&delta[..delta.floor_char_boundary(delta.len().min(available))]);
}

impl ConversationProjection {
    pub(super) fn bound_history(&mut self) {
        let mut bytes = 0_usize;
        let mut resources = 0_usize;
        for message in &mut self.messages {
            if message.text.len() > MAX_MESSAGE_BYTES {
                message
                    .text
                    .truncate(message.text.floor_char_boundary(MAX_MESSAGE_BYTES - 32));
                message.text.push_str("\n[display preview truncated]");
                self.cached_messages.take();
                self.history_omitted = true;
            }
            if message.resources.len() > MAX_RESOURCES {
                message.resources.truncate(MAX_RESOURCES);
                self.cached_messages.take();
                self.history_omitted = true;
            }
            bytes += message.text.len();
            resources += message.resources.len();
        }
        let mut evicted = false;
        while self.messages.len() > MAX_MESSAGES
            || bytes > MAX_WINDOW_BYTES
            || resources > MAX_RESOURCES
        {
            let Some(message) = self.messages.pop_front() else {
                break;
            };
            bytes -= message.text.len();
            resources -= message.resources.len();
            evicted = true;
        }
        if evicted {
            self.cached_messages.take();
            self.history_omitted = true;
        }
        // Indexes and decoded resources may not keep evicted messages alive.
        // Skip the allocations on the overwhelmingly common unchanged window.
        if evicted
            || self.streams.len() > MAX_MESSAGES
            || self.message_operations.len() > MAX_MESSAGES
        {
            let ids: HashSet<_> = self
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect();
            self.streams.retain(|_, id| ids.contains(id.as_str()));
            self.message_operations
                .retain(|_, id| ids.contains(id.as_str()));
        }
        if !self.image_previews.is_empty() {
            let admitted: HashSet<String> = self
                .preview_window_ids()
                .into_iter()
                .map(str::to_owned)
                .collect();
            let before = self.image_previews.len();
            self.image_previews.retain(|id, _| admitted.contains(id));
            if before != self.image_previews.len() {
                self.cached_messages.take();
            }
        }
        if self.conversation_facts.activity.len() > MAX_ACTIVITY {
            let excess = self.conversation_facts.activity.len() - MAX_ACTIVITY;
            self.conversation_facts.activity.drain(..excess);
        }
    }
}
