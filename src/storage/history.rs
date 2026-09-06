//! Encrypted native journals and a small ancestry index, not another reducer.

mod adapter;
mod branch;
mod checkpoint;
mod constraints;
mod inventory;
mod orchestration;
mod path_index;
mod proof;
mod reader;
mod subjects;
mod verification;
mod vision;

#[cfg(test)]
mod execution_tests;
#[cfg(test)]
mod tests;

pub(super) use path_index::{PATH_SCHEMA, migrate_path_index};
pub(crate) use proof::{ActivePrefixProofCursor, CompactionDisclosureGuard};
pub(crate) use subjects::HistorySubject;
pub(super) use subjects::{EXECUTION_SCHEMA, migrate_execution_index};

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

#[cfg(test)]
pub(crate) enum AdmissionFault {
    WrongIndex,
    MissingIndex,
    OversizeRecord,
    ChangedCharge,
}

// Durable retention is separate from per-object reads and execution hydration.
// Legacy JSONL/full-inspection safety limits above remain unchanged.
pub(super) const MAX_PROTECTED_RECORDS: usize = 1_000_000;
const MAX_PROTECTED_BYTES: usize = 1024 * 1024 * 1024;

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
    pub(crate) fn history_exists(&self, id: SessionId) -> Result<bool> {
        self.with_database(|db| {
            Ok(db.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_sessions WHERE id=?1)",
                [id.to_string()],
                |row| row.get(0),
            )?)
        })
    }

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
        self.append_history_checked(id, revision, record, None)
    }

    /// Commit a derived record only while the exact captured privacy policy is current.
    pub(crate) fn append_history_if_privacy_generation(
        &self,
        id: SessionId,
        revision: usize,
        record: &RecordEnvelope,
        generation: u64,
    ) -> Result<()> {
        self.append_history_checked(id, revision, record, Some(generation))
    }

    fn append_history_checked(
        &self,
        id: SessionId,
        revision: usize,
        record: &RecordEnvelope,
        generation: Option<u64>,
    ) -> Result<()> {
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(generation) = generation {
                let current = tx.query_row(
                    "SELECT revision FROM privacy_generation WHERE singleton=1",
                    [],
                    |row| read_u64(row, 0),
                )?;
                let review_required = super::forgetting::memory_review_required(&tx)?;
                ensure!(
                    current == generation
                        && !review_required
                        && super::forgetting::source_allowed(&tx, id.to_string().parse()?)?,
                    "compaction source eligibility changed before commit"
                );
            }
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
        revision < MAX_PROTECTED_RECORDS,
        "native history exceeds the supported record bound"
    );
    let body = serde_json::to_vec(record)?;
    let next_bytes = bytes
        .checked_add(body.len())
        .and_then(|n| n.checked_add(1))
        .context("native history size overflow")?;
    ensure!(
        body.len() <= MAX_RECORD_BYTES && next_bytes <= MAX_PROTECTED_BYTES,
        "native history exceeds byte bounds"
    );
    constraints::validate_registration(tx, id, &record.record)?;
    tx.execute(
        "INSERT INTO native_records VALUES(?1,?2,?3,?4)",
        params![
            id.to_string(),
            i64::try_from(revision)?,
            record.record_id.to_string(),
            body
        ],
    )?;
    subjects::index_record(tx, record, revision, &body)?;
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
            let root: String = tx.query_row(
                "SELECT root_thread FROM native_sessions WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            ensure!(
                root == thread_id.to_string(),
                "Conversation head targets another thread"
            );
            path_index::move_head(tx, id, head.map(|head| head.to_string()))?;
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
