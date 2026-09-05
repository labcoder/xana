//! Retained Conversation-window limits. TuiState owns its cursor and selection;
//! rendering never decides which history to discard.

use super::{MAX_VISIBLE_MESSAGES, TuiState, VisibleMessage};
use std::collections::VecDeque;

const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESIDENT_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESOURCES: usize = 128;

pub(super) fn bound(messages: &mut VecDeque<VisibleMessage>, keep_oldest: bool) -> usize {
    let mut text = 0_usize;
    let mut bodies = 0_usize;
    let mut resources = 0_usize;
    for message in messages.iter() {
        text += message.text.len();
        bodies += message.text.len() + message.document.retained_bytes();
        resources += message.document.artifacts.len();
    }
    let mut removed = 0;
    while messages.len() > MAX_VISIBLE_MESSAGES
        || text > MAX_TEXT_BYTES
        || bodies > MAX_RESIDENT_BODY_BYTES
        || resources > MAX_RESOURCES
    {
        let Some(message) = (if keep_oldest {
            messages.pop_back()
        } else {
            messages.pop_front()
        }) else {
            break;
        };
        text -= message.text.len();
        bodies -= message.text.len() + message.document.retained_bytes();
        resources -= message.document.artifacts.len();
        removed += 1;
    }
    removed
}

impl TuiState {
    pub(in crate::tui) fn needs_history_snapshot(&self) -> bool {
        !self.history_preview
    }

    pub(in crate::tui) fn begin_saved_history_page(
        &mut self,
        page: crate::session::ConversationPage,
    ) {
        if self.viewed_conversation == self.runtime_conversation && !self.history_preview {
            self.background_messages = Some(std::mem::take(&mut self.messages));
        }
        self.history_preview = true;
        self.messages = page
            .messages
            .iter()
            .map(super::message_projection)
            .collect();
        self.history_start = page.start;
        self.history_end = page.total;
        self.history_has_older = page.has_older;
        self.bound_tail_window();
        self.conversation_selection = None;
    }

    pub(super) fn restore_live_tail(&mut self) {
        if self.viewed_conversation == self.runtime_conversation && self.history_preview {
            if let Some(messages) = self.background_messages.take() {
                self.messages = messages;
            }
            self.history_preview = false;
            self.history_start = 0;
            self.history_end = 0;
            // A subsequent older-page request refreshes its real source cursor.
            self.history_has_older = true;
            self.conversation_selection = None;
            self.scroll = 0;
        }
    }

    pub(super) fn bound_tail_window(&mut self) {
        let removed = bound(&mut self.messages, false);
        if removed > 0 {
            if self.history_preview {
                self.history_start = self.history_start.saturating_add(removed);
            }
            self.history_has_older = true;
            // Cell coordinates cannot select unrelated text after eviction.
            // Composer selection and drafts are independent and retained.
            self.conversation_selection = None;
        }
    }
}
