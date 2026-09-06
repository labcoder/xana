//! Bounded output lineage for an exact adapter finish, including head rewinds.

use super::*;
use crate::identity::{ConversationEntryId, RecordId};

impl ProtectedStore {
    pub(crate) fn history_adapter_output(
        &self,
        session: SessionId,
        input: ConversationEntryId,
        finish: RecordId,
    ) -> Result<Vec<RecordEnvelope>> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let start: usize = tx.query_row("SELECT sequence FROM native_entries WHERE session=?1 AND id=?2", params![session.to_string(),input.to_string()], |row| read_usize(row,0))?;
            let end: usize = tx.query_row("SELECT sequence FROM native_records WHERE session=?1 AND id=?2", params![session.to_string(),finish.to_string()], |row| read_usize(row,0))?;
            ensure!(end > start && end - start <= 4096, "adapter output ancestry exceeds its inspection bound");
            let mut query = tx.prepare("SELECT sequence,body FROM native_records WHERE session=?1 AND sequence>=?2 AND sequence<?3 ORDER BY sequence LIMIT 4097")?;
            let mut rows = query.query(params![session.to_string(),i64::try_from(start)?,i64::try_from(end)?])?;
            let mut output = Vec::new();
            let mut bytes = 0usize;
            while let Some(row) = rows.next()? {
                ensure!(read_usize(row,0)? == start + output.len(), "adapter output journal is discontinuous");
                let body = row.get_ref(1)?.as_blob()?;
                bytes = bytes.checked_add(body.len()).context("adapter output size overflow")?;
                ensure!(body.len() <= MAX_RECORD_BYTES && bytes <= MAX_SESSION_BYTES, "adapter output exceeds its byte bound");
                let envelope: RecordEnvelope = serde_json::from_slice(body)?;
                ensure!(envelope.session_id == session && envelope.version == SESSION_RECORD_VERSION, "adapter output journal identity differs");
                output.push(envelope);
            }
            ensure!(output.len() == end - start, "adapter output journal is incomplete");
            Ok(output)
        })
    }
}
