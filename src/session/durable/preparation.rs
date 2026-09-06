//! Owned compaction preparation; only complete original-source proofs become candidates.

use super::*;
use crate::{
    session::{
        CompactionCandidate,
        compaction::{self, CompactionSourceProof, CompactionSourceProofBuilder},
    },
    storage::{ActivePrefixProofCursor, CompactionDisclosureGuard},
};

pub(crate) struct CompactionPreparation {
    plan: Plan,
    source: Source,
}

struct Plan {
    session: SessionId,
    operation: OperationId,
    previous: Option<CompactionId>,
    reason: CompactionReason,
    start: ConversationEntryId,
    end: ConversationEntryId,
    tail: ConversationEntryId,
    count: usize,
    summary: CompactionSummary,
    budget: PromptBudgetPlan,
    helper_messages: Option<Vec<Message>>,
    privacy_generation: Option<u64>,
    source_guard: Option<CompactionDisclosureGuard>,
}

enum Source {
    Ready(CompactionSourceProof),
    Pending(ActivePrefixProofCursor),
}

impl CompactionPreparation {
    pub(super) fn begin(
        session: &DurableSession,
        operation: OperationId,
        reason: CompactionReason,
        budget: &PromptBudgetPlan,
    ) -> Result<Self> {
        let privacy_generation = if let Some(home) = session.store.protected_home() {
            anyhow::ensure!(
                home.source_eligible(session.session_id().to_string().parse()?)?,
                "compaction source is excluded by forgetting or restore policy; raw history remains inspectable"
            );
            Some(home.privacy_generation()?)
        } else {
            None
        };
        // The execution projection is already bounded. Only evicted originals
        // require a paged proof; do not clone/hydrate their bodies at admission.
        let path = session
            .restored
            .conversation_entry_path()
            .context("could not restore compaction source path")?;
        let messages = path.iter().map(|entry| &entry.message).collect::<Vec<_>>();
        let retained_start =
            compaction::select_retained_start(&messages, budget.retained_tail_tokens);
        if retained_start == 0 || retained_start >= path.len() {
            return Err(CompactionError::NothingToCompact.into());
        }
        let previous = session
            .restored
            .active_compaction()
            .context("could not resolve prior compaction checkpoint")?;
        let offset = session.restored.retained_offset();
        let incremental_start = previous
            .map(|checkpoint| {
                checkpoint
                    .source_entry_count
                    .checked_sub(offset)
                    .context("prior compaction precedes retained history")
            })
            .transpose()?
            .unwrap_or(0);
        if retained_start <= incremental_start {
            return Err(CompactionError::NothingToCompact.into());
        }
        let count = offset
            .checked_add(retained_start)
            .context("compaction source count exceeds supported history size")?;
        let summary = CompactionSummary::derive(
            previous.map(|checkpoint| &checkpoint.summary),
            messages[incremental_start..retained_start].iter().copied(),
            budget.summary_max_bytes,
        );
        let source_guard = session
            .store
            .protected_home()
            .map(|home| {
                home.compaction_disclosure_guard(
                    session.session_id(),
                    count,
                    session
                        .store
                        .protected_revision()
                        .context("protected history has no revision")?,
                    session.restored.head,
                    privacy_generation.context("protected history has no privacy generation")?,
                )
            })
            .transpose()?;
        let cached = session.compaction_prefix.as_ref().filter(|prefix| {
            previous.is_some_and(|checkpoint| prefix.matches(session.session_id(), checkpoint))
        });
        let source = if let Some(prefix) = cached {
            let mut builder = CompactionSourceProofBuilder::from_prefix(prefix, count)?;
            for entry in &path[incremental_start..=retained_start] {
                builder.push(entry.id, &entry.message)?;
            }
            Source::Ready(builder.finish()?)
        } else if offset > 0 {
            Source::Pending(
                source_guard
                    .as_ref()
                    .context("archived compaction sources require protected history")?
                    .begin_proof(),
            )
        } else {
            let mut builder = CompactionSourceProofBuilder::new(session.session_id(), count);
            for entry in &path[..=retained_start] {
                builder.push(entry.id, &entry.message)?;
            }
            Source::Ready(builder.finish()?)
        };
        let start = if offset == 0 {
            path[0].id
        } else {
            previous
                .context("archived compaction has no prior checkpoint")?
                .source_start
        };
        let helper_messages = compaction::semantic::source_messages_with_limits(
            previous.map(|checkpoint| &checkpoint.summary),
            &messages[incremental_start..retained_start],
            compaction::semantic::HelperLimits::from_plan(budget),
        );
        Ok(Self {
            plan: Plan {
                session: session.session_id(),
                operation,
                previous: previous.map(|checkpoint| checkpoint.id),
                reason,
                start,
                end: path[retained_start - 1].id,
                tail: path[retained_start].id,
                count,
                summary,
                budget: budget.clone(),
                helper_messages,
                privacy_generation,
                source_guard,
            },
            source,
        })
    }

    pub(crate) fn is_ready(&self) -> bool {
        match &self.source {
            Source::Ready(_) => true,
            Source::Pending(cursor) => cursor.is_ready(),
        }
    }

    #[cfg(test)]
    pub(crate) fn progress(&self) -> (usize, usize) {
        match &self.source {
            Source::Ready(_) => (self.plan.count + 1, self.plan.count + 1),
            Source::Pending(cursor) => cursor.progress(),
        }
    }

    pub(crate) fn advance(self) -> Result<Self> {
        let Source::Pending(cursor) = self.source else {
            bail!("compaction source proof is already complete");
        };
        Ok(Self {
            plan: self.plan,
            source: Source::Pending(cursor.advance()?),
        })
    }

    pub(crate) fn finish(self) -> Result<CompactionCandidate> {
        let proof = match self.source {
            Source::Ready(proof) => {
                if let Some(guard) = &self.plan.source_guard {
                    guard.recheck()?;
                }
                proof
            }
            Source::Pending(cursor) => cursor.finish()?,
        };
        let plan = self.plan;
        let checkpoint = CompactionCheckpoint {
            version: COMPACTION_CHECKPOINT_VERSION,
            id: CompactionId::new(),
            operation_id: plan.operation,
            previous_checkpoint: plan.previous,
            reason: plan.reason,
            source_start: plan.start,
            source_end: plan.end,
            source_entry_count: plan.count,
            retained_tail_start: plan.tail,
            source_digest: proof.digest().to_owned(),
            summary: plan.summary,
            budget: plan.budget,
            semantic: None,
        };
        anyhow::ensure!(
            proof.matches(plan.session, &checkpoint),
            "compaction proof differs from its prepared source range"
        );
        let mut helper_messages = plan.helper_messages;
        if let Some(messages) = &mut helper_messages {
            // Verified recovery coordinates are still quoted task DATA, not a
            // System instruction or permission to inspect another Conversation.
            // The final helper admission check includes this bounded overhead.
            messages.push(Message::text(crate::message::Role::User, format!(
                "<verified-original-source-range>Conversation {}; entries {} through {}; count {}; original-source digest {}.</verified-original-source-range>",
                plan.session, checkpoint.source_start, checkpoint.source_end,
                checkpoint.source_entry_count, checkpoint.source_digest,
            )));
        }
        Ok(CompactionCandidate {
            checkpoint,
            helper_messages,
            privacy_generation: plan.privacy_generation,
            source_proof: Some(proof),
            source_guard: plan.source_guard,
        })
    }
}

#[cfg(test)]
mod tests;
