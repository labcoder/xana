//! Encrypted native journals and a small ancestry index, not another reducer.

mod reader;

use super::ProtectedStore;
use super::database::{read_u64, read_usize};
use crate::{
    identity::SessionId,
    session::{
        MAX_RECORD_BYTES, MAX_SESSION_BYTES, MAX_SESSION_RECORDS, RecordEnvelope,
        SESSION_RECORD_VERSION, SessionRecord,
    },
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

pub(super) const SCHEMA: &str = "
CREATE TABLE native_sessions(
 id TEXT PRIMARY KEY, root_thread TEXT NOT NULL, workspace TEXT NOT NULL,
 head TEXT, revision INTEGER NOT NULL, bytes INTEGER NOT NULL, modified INTEGER NOT NULL);
CREATE TABLE native_records(
 session TEXT NOT NULL REFERENCES native_sessions(id) ON DELETE CASCADE,
 sequence INTEGER NOT NULL, id TEXT NOT NULL, body BLOB NOT NULL,
 PRIMARY KEY(session,sequence), UNIQUE(session,id));
CREATE TABLE native_entries(
 session TEXT NOT NULL, id TEXT NOT NULL, parent TEXT, sequence INTEGER NOT NULL,
 PRIMARY KEY(session,id),
 FOREIGN KEY(session,sequence) REFERENCES native_records(session,sequence) ON DELETE CASCADE);
CREATE INDEX native_workspace ON native_sessions(workspace,modified);
";

impl ProtectedStore {
    pub(crate) fn create_history(&self, records: &[RecordEnvelope]) -> Result<()> {
        let first = records
            .first()
            .context("native history requires a creation record")?;
        let SessionRecord::SessionCreated {
            thread_id,
            workspace_root,
        } = &first.record
        else {
            anyhow::bail!("native history must start with SessionCreated");
        };
        let workspace = workspace_root
            .to_str()
            .context("workspace cannot be serialized")?;
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO native_sessions VALUES(?1,?2,?3,NULL,0,0,0)",
                params![
                    first.session_id.to_string(),
                    thread_id.to_string(),
                    workspace
                ],
            )?;
            let mut bytes = 0;
            for (sequence, record) in records.iter().enumerate() {
                bytes = append(&tx, first.session_id, record, sequence, bytes)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn append_history(
        &self,
        id: SessionId,
        revision: usize,
        record: &RecordEnvelope,
    ) -> Result<()> {
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (actual, bytes): (usize, usize) = tx.query_row(
                "SELECT revision,bytes FROM native_sessions WHERE id=?1",
                [id.to_string()],
                |row| Ok((read_usize(row, 0)?, read_usize(row, 1)?)),
            )?;
            ensure!(
                actual == revision,
                "native history changed after inspection"
            );
            append(&tx, id, record, revision, bytes)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn load_history(&self, id: SessionId) -> Result<Vec<u8>> {
        self.with_database(|db| {
            let (revision, length): (usize, usize) = db
                .connection
                .query_row(
                    "SELECT revision,bytes FROM native_sessions WHERE id=?1",
                    [id.to_string()],
                    |row| Ok((read_usize(row, 0)?, read_usize(row, 1)?)),
                )
                .optional()?
                .context("native Conversation is unavailable")?;
            ensure!(
                revision <= MAX_SESSION_RECORDS && length <= MAX_SESSION_BYTES,
                "native history exceeds the supported restore bound"
            );
            let mut output = Vec::with_capacity(length);
            let mut query = db.connection.prepare(
                "SELECT sequence,body FROM native_records WHERE session=?1 ORDER BY sequence",
            )?;
            let mut rows = query.query([id.to_string()])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                ensure!(
                    read_usize(row, 0)? == count,
                    "native record sequence is discontinuous"
                );
                let body = row.get_ref(1)?.as_blob()?;
                ensure!(
                    body.len() <= MAX_RECORD_BYTES
                        && output.len().saturating_add(body.len()).saturating_add(1)
                            <= MAX_SESSION_BYTES,
                    "native record exceeds restore bounds"
                );
                output.extend_from_slice(body);
                output.push(b'\n');
                count += 1;
            }
            ensure!(
                count == revision && output.len() == length,
                "native history length or revision differs"
            );
            Ok(output)
        })
    }

    /// Caller holds the Conversation writer and has verified it never started.
    pub(crate) fn discard_history(&self, id: SessionId) -> Result<()> {
        self.with_database(|db| {
            db.connection
                .execute("DELETE FROM native_sessions WHERE id=?1", [id.to_string()])?;
            Ok(())
        })
    }
}

fn append(
    tx: &Transaction<'_>,
    id: SessionId,
    record: &RecordEnvelope,
    revision: usize,
    bytes: usize,
) -> Result<usize> {
    ensure!(
        record.session_id == id && record.version == SESSION_RECORD_VERSION,
        "native record identity or version differs"
    );
    ensure!(
        revision < MAX_SESSION_RECORDS,
        "native history exceeds the supported record bound"
    );
    let body = serde_json::to_vec(record)?;
    let next_bytes = bytes
        .checked_add(body.len())
        .and_then(|n| n.checked_add(1))
        .context("native history size overflow")?;
    ensure!(
        body.len() <= MAX_RECORD_BYTES && next_bytes <= MAX_SESSION_BYTES,
        "native history exceeds byte bounds"
    );
    tx.execute(
        "INSERT INTO native_records VALUES(?1,?2,?3,?4)",
        params![
            id.to_string(),
            i64::try_from(revision)?,
            record.record_id.to_string(),
            body
        ],
    )?;
    match &record.record {
        SessionRecord::ConversationEntryAppended { entry } => {
            tx.execute(
                "INSERT INTO native_entries VALUES(?1,?2,?3,?4)",
                params![
                    id.to_string(),
                    entry.id.to_string(),
                    entry.parent.map(|p| p.to_string()),
                    i64::try_from(revision)?
                ],
            )?;
        }
        SessionRecord::ThreadHeadMoved { thread_id, head } => {
            tx.execute(
                "UPDATE native_sessions SET head=?1 WHERE id=?2 AND root_thread=?3",
                params![
                    head.map(|h| h.to_string()),
                    id.to_string(),
                    thread_id.to_string()
                ],
            )?;
        }
        _ => {}
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let now = i64::try_from(now).context("history timestamp overflow")?;
    tx.execute(
        "UPDATE native_sessions SET revision=?1,bytes=?2,modified=?3 WHERE id=?4",
        params![
            i64::try_from(revision + 1)?,
            i64::try_from(next_bytes)?,
            now,
            id.to_string()
        ],
    )?;
    Ok(next_bytes)
}
