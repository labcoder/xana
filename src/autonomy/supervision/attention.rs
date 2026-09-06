//! Read-only attention over active jobs and new immutable receipts. Historical
//! terminal jobs never enter the polling loop; client attachment is not authority.
use crate::{
    autonomy::{Job, JobState, QUEUE_LIMIT, RunOutcome},
    storage::ProtectedStore,
};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundAttentionKind {
    NeedsYou,
    Completed,
}

/// Only opaque identities and fixed lifecycle labels cross the observer seam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackgroundAttention {
    pub task: String,
    pub conversation: String,
    pub occurrence: Option<String>,
    pub revision: u64,
    pub kind: BackgroundAttentionKind,
}
impl BackgroundAttention {
    pub(crate) fn signal(&self) -> crate::host_lifecycle::AttentionSignal {
        use crate::{
            host_lifecycle::{AttentionKind, AttentionSignal},
            workspace_host::ConversationRef,
        };
        let key = self.occurrence.clone().unwrap_or_else(|| {
            Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("xana/background-attention/{}/{}", self.task, self.revision).as_bytes(),
            )
            .to_string()
        });
        AttentionSignal {
            kind: match self.kind {
                BackgroundAttentionKind::NeedsYou => AttentionKind::BackgroundNeedsYou,
                BackgroundAttentionKind::Completed => AttentionKind::BackgroundCompleted,
            },
            conversation: Some(ConversationRef::Native {
                session_id: self
                    .conversation
                    .parse()
                    .expect("typed job Conversation UUID"),
            }),
            operation_id: Some(key.parse().expect("typed receipt or derived UUID")),
        }
    }
}

#[derive(Default)]
pub(crate) struct AttentionObserver {
    home: Option<Uuid>,
    seen_bounds: (u64, u64),
    after_job: u64,
    after_receipt: Option<u64>,
    initial_job_sequence: u64,
    seen: BTreeMap<Uuid, u64>,
    order: VecDeque<Uuid>,
    keys: BTreeSet<String>,
    key_order: VecDeque<String>,
}
impl AttentionObserver {
    /// The caller invokes this off the UI thread. Each poll reads at most 16
    /// active jobs and 16 new receipts; unchanged state never creates a notice.
    pub(crate) fn poll(&mut self, store: &ProtectedStore) -> Result<Vec<BackgroundAttention>> {
        let bounds = store.autonomy_attention_baseline()?;
        // A replaced/selected home must not inherit another generation's
        // cursors. Indexed high-water reads also detect a restored older image.
        if self.home != Some(store.id())
            || self.seen_bounds.0 > bounds.0
            || self.seen_bounds.1 > bounds.1
        {
            *self = Self::default();
            self.home = Some(store.id());
        }
        let baseline = if self.after_receipt.is_none() {
            Some(bounds)
        } else {
            None
        };
        let receipt_cursor =
            baseline.map_or(self.after_receipt.unwrap_or(0), |(_, receipt)| receipt);
        let rows = store.autonomy_attention_jobs(self.after_job)?;
        let receipts = store.autonomy_attention_receipts(receipt_cursor)?;
        // Only advance observer state after every bounded database read succeeds.
        self.seen_bounds = bounds;
        if let Some((job, receipt)) = baseline {
            self.initial_job_sequence = job;
            self.after_receipt = Some(receipt);
        }
        self.after_job = if rows.len() == 16 {
            rows.last().map_or(0, |(sequence, _)| *sequence)
        } else {
            0
        };
        let mut notes = Vec::new();
        for (sequence, job) in rows {
            let previous = self.seen.insert(job.id, job.revision);
            if previous.is_none() {
                self.order.push_back(job.id);
                if self.order.len() > QUEUE_LIMIT
                    && let Some(expired) = self.order.pop_front()
                {
                    self.seen.remove(&expired);
                }
            }
            // Existing tasks are a quiet reconnect baseline. New tasks that
            // failed before their first observation still need an attention edge.
            if (previous.is_some_and(|revision| revision != job.revision)
                || previous.is_none() && sequence > self.initial_job_sequence)
                && job.state == JobState::NeedsYou
            {
                self.emit(
                    &mut notes,
                    &job,
                    BackgroundAttentionKind::NeedsYou,
                    job.last_receipt.as_ref().map(|receipt| receipt.occurrence),
                );
            }
        }
        for (sequence, job, receipt) in receipts {
            self.after_receipt = Some(sequence);
            if receipt.outcome == RunOutcome::Completed {
                self.emit(
                    &mut notes,
                    &job,
                    BackgroundAttentionKind::Completed,
                    Some(receipt.occurrence),
                );
            } else if matches!(receipt.outcome, RunOutcome::NeedsYou | RunOutcome::Unknown)
                && job.state == JobState::NeedsYou
                && job
                    .last_receipt
                    .as_ref()
                    .is_some_and(|latest| latest.occurrence == receipt.occurrence)
            {
                // A job may finish while its first active-page baseline is
                // still rotating. The immutable receipt cursor preserves that
                // edge without notifying about an already resumed old failure.
                self.emit(
                    &mut notes,
                    &job,
                    BackgroundAttentionKind::NeedsYou,
                    Some(receipt.occurrence),
                );
            }
        }
        Ok(notes)
    }
    fn emit(
        &mut self,
        notes: &mut Vec<BackgroundAttention>,
        job: &Job,
        kind: BackgroundAttentionKind,
        occurrence: Option<Uuid>,
    ) {
        let key = format!(
            "{}:{kind:?}:{}",
            job.id,
            occurrence.map_or_else(|| job.revision.to_string(), |id| id.to_string())
        );
        if !self.keys.insert(key.clone()) {
            return;
        }
        self.key_order.push_back(key);
        if self.key_order.len() > 256
            && let Some(expired) = self.key_order.pop_front()
        {
            self.keys.remove(&expired);
        }
        notes.push(BackgroundAttention {
            task: job.id.to_string(),
            conversation: job.conversation.to_string(),
            occurrence: occurrence.map(|id| id.to_string()),
            revision: job.revision,
            kind,
        });
    }
}

#[cfg(test)]
mod tests;
