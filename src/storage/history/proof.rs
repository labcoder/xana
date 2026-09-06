//! Cancellable original-byte proof construction, one bounded transaction per page.

use super::*;
use crate::{
    identity::ConversationEntryId,
    session::compaction::{CompactionSourceProof, CompactionSourceProofBuilder},
};

const MAX_PROOF_PAGE_ROWS: usize = 128;
const MAX_PROOF_PAGE_BYTES: usize = 2 * 1024 * 1024;

/// An in-memory continuation, not a serializable certificate or a database lock.
/// Its builder only advances after reading exact original records. Dropping it
/// cancels preparation without writing a checkpoint or retaining a trusted hash.
pub(crate) struct ActivePrefixProofCursor {
    guard: CompactionDisclosureGuard,
    next: usize,
    previous: Option<ConversationEntryId>,
    builder: CompactionSourceProofBuilder,
}

/// Exact live source authority, not a persisted proof or a transferable grant.
/// Clones share the same revocable protected home. Rechecking never rehashes
/// original history; it fences provider disclosure after an asynchronous wait.
#[derive(Clone)]
pub(crate) struct CompactionDisclosureGuard {
    home: ProtectedStore,
    session: SessionId,
    revision: usize,
    head: Option<ConversationEntryId>,
    privacy_generation: u64,
    count: usize,
}

impl ProtectedStore {
    #[cfg(test)]
    pub(crate) fn begin_active_prefix_proof(
        &self,
        session: SessionId,
        count: usize,
        revision: usize,
        head: Option<ConversationEntryId>,
        privacy_generation: u64,
    ) -> Result<ActivePrefixProofCursor> {
        Ok(self
            .compaction_disclosure_guard(session, count, revision, head, privacy_generation)?
            .begin_proof())
    }

    pub(crate) fn compaction_disclosure_guard(
        &self,
        session: SessionId,
        count: usize,
        revision: usize,
        head: Option<ConversationEntryId>,
        privacy_generation: u64,
    ) -> Result<CompactionDisclosureGuard> {
        ensure!(
            count > 0 && count < MAX_PROTECTED_RECORDS,
            "compaction prefix exceeds its source bound"
        );
        let guard = CompactionDisclosureGuard {
            home: self.clone(),
            session,
            revision,
            head,
            privacy_generation,
            count,
        };
        guard.recheck()?;
        Ok(guard)
    }
}

impl ActivePrefixProofCursor {
    pub(crate) fn is_ready(&self) -> bool {
        self.next == self.guard.count + 1
    }

    #[cfg(test)]
    pub(crate) fn progress(&self) -> (usize, usize) {
        (self.next, self.guard.count + 1)
    }

    /// The caller schedules one page off the foreground executor, then regains
    /// control to service cancellation. No connection guard survives this call.
    pub(crate) fn advance(mut self) -> Result<Self> {
        ensure!(
            !self.is_ready(),
            "compaction source proof is already complete"
        );
        let home = self.guard.home.clone();
        home.with_database(|db| {
            let tx = db.connection.transaction()?;
            self.guard.validate_snapshot(&tx)?;
            let end = (self.next + MAX_PROOF_PAGE_ROWS).min(self.guard.count + 1);
            let mut query = tx.prepare("SELECT p.position,p.entry,p.sequence,r.id,r.body FROM native_path p JOIN native_records r ON r.session=p.session AND r.sequence=p.sequence WHERE p.session=?1 AND p.position>=?2 AND p.position<?3 ORDER BY p.position LIMIT 128")?;
            let mut rows = query.query(params![self.guard.session.to_string(), i64::try_from(self.next)?, i64::try_from(end)?])?;
            let start = self.next;
            let mut bytes = 0usize;
            let mut byte_limited = false;
            while let Some(row) = rows.next()? {
                ensure!(read_usize(row, 0)? == self.next, "compaction source positions are discontinuous");
                ensure!(read_usize(row, 2)? < self.guard.revision, "compaction source is ahead of its journal");
                let body = row.get_ref(4)?.as_blob()?;
                ensure!(body.len() <= MAX_RECORD_BYTES, "compaction source exceeds its record bound");
                if bytes + body.len() > MAX_PROOF_PAGE_BYTES {
                    byte_limited = true;
                    break;
                }
                bytes += body.len();
                let record: RecordEnvelope = serde_json::from_slice(body)?;
                ensure!(record.session_id == self.guard.session && record.version == SESSION_RECORD_VERSION && record.record_id.to_string() == row.get::<_, String>(3)?, "compaction source identity differs");
                let SessionRecord::ConversationEntryAppended { entry } = record.record else {
                    anyhow::bail!("compaction source is not a Conversation entry");
                };
                ensure!(entry.id.to_string() == row.get::<_, String>(1)? && entry.parent == self.previous, "compaction source ancestry differs");
                self.builder.push(entry.id, &entry.message)?;
                self.previous = Some(entry.id);
                self.next += 1;
            }
            ensure!(self.next > start, "compaction source page made no progress");
            ensure!(self.next == end || byte_limited, "compaction source page is incomplete");
            Ok(())
        })?;
        Ok(self)
    }

    pub(crate) fn finish(self) -> Result<CompactionSourceProof> {
        ensure!(self.is_ready(), "compaction source proof is incomplete");
        self.guard.recheck()?;
        self.builder.finish()
    }
}

impl CompactionDisclosureGuard {
    pub(crate) fn recheck(&self) -> Result<()> {
        self.home.with_database(|db| {
            let tx = db.connection.transaction()?;
            self.validate_snapshot(&tx)
        })
    }

    pub(crate) fn begin_proof(&self) -> ActivePrefixProofCursor {
        ActivePrefixProofCursor {
            guard: self.clone(),
            next: 0,
            previous: None,
            builder: CompactionSourceProofBuilder::new(self.session, self.count),
        }
    }

    fn validate_snapshot(&self, tx: &Transaction<'_>) -> Result<()> {
        let metadata = super::subjects::metadata(tx, self.session)?;
        ensure!(
            metadata.revision == self.revision && metadata.head == self.head,
            "history changed during compaction source preparation"
        );
        ensure!(
            self.count < metadata.active_entries,
            "compaction prefix is not an active source range"
        );
        let generation = tx.query_row(
            "SELECT revision FROM privacy_generation WHERE singleton=1",
            [],
            |row| read_u64(row, 0),
        )?;
        ensure!(
            generation == self.privacy_generation
                && !super::super::forgetting::memory_review_required(tx)?
                && super::super::forgetting::source_allowed(tx, self.session.to_string().parse()?)?,
            "compaction source eligibility changed; retry after reviewing current privacy controls"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
