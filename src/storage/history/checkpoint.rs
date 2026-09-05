//! Revision-fenced bounded execution snapshots, distinct from lossy prompt summaries.

use super::*;
use crate::session::{RestoredSession, hydration};

impl ProtectedStore {
    pub(crate) fn save_execution_checkpoint(
        &self,
        id: SessionId,
        revision: usize,
        state: &RestoredSession,
    ) -> Result<usize> {
        ensure!(
            state.session_id == id,
            "execution checkpoint belongs to another Conversation"
        );
        let head = state.head.map(|head| head.to_string());
        let body = hydration::encode(state)?;
        let bytes = body.len();
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (actual, current_head): (usize, Option<String>) = tx.query_row("SELECT revision,head FROM native_sessions WHERE id=?1", [id.to_string()], |row| Ok((read_usize(row,0)?, row.get(1)?)))?;
            ensure!(actual == revision && current_head == head && revision > 0, "execution checkpoint changed before commit");
            let anchor: String = tx.query_row("SELECT digest FROM native_record_digests WHERE session=?1 AND sequence=?2", params![id.to_string(), i64::try_from(revision - 1)?], |row| row.get(0))?;
            let digest = subjects::record_digest(&anchor, &body);
            tx.execute("INSERT INTO native_execution_checkpoints VALUES(?1,?2,?3,?4) ON CONFLICT(session) DO UPDATE SET revision=excluded.revision,prefix_digest=excluded.prefix_digest,body=excluded.body", params![id.to_string(), i64::try_from(revision)?, digest, body])?;
            tx.commit()?;
            Ok(bytes)
        })
    }

    pub(crate) fn execution_checkpoint(
        &self,
        id: SessionId,
    ) -> Result<Option<(usize, RestoredSession)>> {
        self.with_database(|db| {
            let mut query = db.connection.prepare("SELECT c.revision,c.prefix_digest,c.body,d.digest FROM native_execution_checkpoints c LEFT JOIN native_record_digests d ON d.session=c.session AND d.sequence=c.revision-1 WHERE c.session=?1")?;
            let mut rows = query.query([id.to_string()])?;
            let Some(row) = rows.next()? else { return Ok(None); };
            let revision = read_usize(row, 0)?;
            let digest: String = row.get(1)?;
            let body = row.get_ref(2)?.as_blob()?;
            ensure!(body.len() <= hydration::MAX_EXECUTION_BYTES, "execution checkpoint exceeds its byte bound");
            let anchor: String = row.get(3)?;
            ensure!(revision > 0 && subjects::record_digest(&anchor, body) == digest, "execution checkpoint authentication differs");
            let state = hydration::decode(body)?;
            ensure!(state.session_id == id, "execution checkpoint identity differs");
            Ok(Some((revision, state)))
        })
    }
}
