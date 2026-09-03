//! Per-Conversation composer state owned by the Desktop application.

use std::collections::{HashMap, VecDeque};
use xana::desktop::DesktopAttachment;

const MAX_RETAINED_COMPOSERS: usize = 128;
const MAX_DRAFT_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_TURNS: usize = 32;

#[derive(Debug, Clone)]
pub(crate) struct QueuedSubmission {
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) attachments: Vec<DesktopAttachment>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ComposerState {
    pub(crate) draft: String,
    pub(crate) attachments: Vec<DesktopAttachment>,
    pub(crate) queue: VecDeque<QueuedSubmission>,
}

/// Bounded presentation state keyed by Xana's stable Conversation identity.
pub(crate) struct ComposerStore {
    active: String,
    states: HashMap<String, ComposerState>,
    recency: VecDeque<String>,
    next_queue_id: u64,
}

impl ComposerStore {
    pub(crate) fn new(active: impl Into<String>) -> Self {
        let active = active.into();
        let mut states = HashMap::new();
        states.insert(active.clone(), ComposerState::default());
        Self {
            recency: VecDeque::from([active.clone()]),
            active,
            states,
            next_queue_id: 1,
        }
    }

    pub(crate) fn active_key(&self) -> &str {
        &self.active
    }

    pub(crate) fn current(&self) -> &ComposerState {
        self.states
            .get(&self.active)
            .expect("active composer state is always retained")
    }

    pub(crate) fn switch_to(&mut self, conversation: impl Into<String>) -> &ComposerState {
        let conversation = conversation.into();
        self.active = conversation.clone();
        self.states.entry(conversation.clone()).or_default();
        self.touch(conversation);
        self.evict_inactive();
        self.current()
    }

    pub(crate) fn set_draft(&mut self, draft: impl Into<String>) -> Result<(), &'static str> {
        let draft = draft.into();
        if draft.len() > MAX_DRAFT_BYTES {
            return Err("draft exceeds the 1 MiB Desktop composer limit");
        }
        self.current_mut().draft = draft;
        Ok(())
    }

    pub(crate) fn clear_draft(&mut self) {
        self.current_mut().draft.clear();
    }

    pub(crate) fn stage_for(&mut self, conversation: &str, attachment: DesktopAttachment) -> bool {
        let Some(current) = self.states.get_mut(conversation) else {
            return false;
        };
        if current
            .attachments
            .iter()
            .any(|candidate| candidate.id == attachment.id)
        {
            return false;
        }
        current.attachments.push(attachment);
        true
    }

    pub(crate) fn remove_attachment(&mut self, id: &str) -> bool {
        let current = self.current_mut();
        let before = current.attachments.len();
        current.attachments.retain(|attachment| attachment.id != id);
        before != current.attachments.len()
    }

    pub(crate) fn take_attachments(&mut self) -> Vec<DesktopAttachment> {
        std::mem::take(&mut self.current_mut().attachments)
    }

    pub(crate) fn restore_submission(&mut self, submission: QueuedSubmission) {
        let active = self.active.clone();
        self.restore_submission_for(&active, submission);
    }

    pub(crate) fn restore_submission_for(
        &mut self,
        conversation: &str,
        submission: QueuedSubmission,
    ) {
        let Some(current) = self.states.get_mut(conversation) else {
            return;
        };
        if current.draft.is_empty() {
            current.draft = submission.text;
        } else if current.queue.len() < MAX_QUEUED_TURNS {
            current.queue.push_front(submission);
            return;
        }
        for attachment in submission.attachments {
            if !current
                .attachments
                .iter()
                .any(|candidate| candidate.id == attachment.id)
            {
                current.attachments.push(attachment);
            }
        }
    }

    pub(crate) fn submission(
        &mut self,
        text: impl Into<String>,
        attachments: Vec<DesktopAttachment>,
    ) -> QueuedSubmission {
        let id = format!("{}:queued:{}", self.active, self.next_queue_id);
        self.next_queue_id = self.next_queue_id.wrapping_add(1).max(1);
        QueuedSubmission {
            id,
            text: text.into(),
            attachments,
        }
    }

    pub(crate) fn queue(&mut self, submission: QueuedSubmission) -> Result<(), &'static str> {
        if self.current().queue.len() >= MAX_QUEUED_TURNS {
            return Err("this Conversation already has 32 queued follow-ups");
        }
        self.current_mut().queue.push_back(submission);
        Ok(())
    }

    pub(crate) fn pop_queued(&mut self) -> Option<QueuedSubmission> {
        self.current_mut().queue.pop_front()
    }

    pub(crate) fn remove_queued(&mut self, id: &str) -> bool {
        self.take_queued(id).is_some()
    }

    pub(crate) fn take_queued(&mut self, id: &str) -> Option<QueuedSubmission> {
        let current = self.current_mut();
        let index = current
            .queue
            .iter()
            .position(|submission| submission.id == id)?;
        current.queue.remove(index)
    }

    pub(crate) fn move_queued(&mut self, id: &str, earlier: bool) -> bool {
        let current = self.current_mut();
        let Some(index) = current.queue.iter().position(|item| item.id == id) else {
            return false;
        };
        let destination = if earlier {
            index.checked_sub(1)
        } else {
            (index + 1 < current.queue.len()).then_some(index + 1)
        };
        let Some(destination) = destination else {
            return false;
        };
        current.queue.swap(index, destination);
        true
    }

    pub(crate) fn clear_queue(&mut self) {
        self.current_mut().queue.clear();
    }

    fn current_mut(&mut self) -> &mut ComposerState {
        self.states
            .get_mut(&self.active)
            .expect("active composer state is always retained")
    }

    fn touch(&mut self, conversation: String) {
        self.recency.retain(|candidate| candidate != &conversation);
        self.recency.push_back(conversation);
    }

    fn evict_inactive(&mut self) {
        while self.states.len() > MAX_RETAINED_COMPOSERS {
            let Some(candidate) = self.recency.pop_front() else {
                return;
            };
            if candidate == self.active {
                self.recency.push_back(candidate);
                continue;
            }
            self.states.remove(&candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_and_queues_never_cross_conversations() {
        let mut store = ComposerStore::new("conversation-a");
        store.set_draft("draft a").expect("bounded fixture draft");
        let queued = store.submission("follow a", Vec::new());
        store.queue(queued).expect("bounded fixture queue");

        assert!(store.switch_to("conversation-b").draft.is_empty());
        store.set_draft("draft b").expect("bounded fixture draft");
        assert_eq!(store.switch_to("conversation-a").draft, "draft a");
        assert_eq!(
            store
                .current()
                .queue
                .front()
                .expect("queued fixture remains")
                .text,
            "follow a"
        );
        assert_eq!(store.switch_to("conversation-b").draft, "draft b");
        assert!(store.current().queue.is_empty());
    }

    #[test]
    fn queue_reordering_is_stable_and_bounded() {
        let mut store = ComposerStore::new("conversation");
        let first = store.submission("first", Vec::new());
        let first_id = first.id.clone();
        let second = store.submission("second", Vec::new());
        let second_id = second.id.clone();
        store.queue(first).expect("first bounded fixture");
        store.queue(second).expect("second bounded fixture");

        assert!(store.move_queued(&second_id, true));
        assert_eq!(
            store.pop_queued().expect("reordered fixture remains").id,
            second_id
        );
        assert!(store.remove_queued(&first_id));
        assert!(store.current().queue.is_empty());
    }

    #[test]
    fn retained_state_has_a_hard_lru_bound() {
        let mut store = ComposerStore::new("initial");
        for index in 0..MAX_RETAINED_COMPOSERS + 20 {
            store.switch_to(format!("conversation-{index}"));
        }

        assert_eq!(store.states.len(), MAX_RETAINED_COMPOSERS);
        assert!(store.states.contains_key(store.active_key()));
        assert!(!store.states.contains_key("initial"));
    }
}
