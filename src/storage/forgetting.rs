//! Durable suppression and generation fences; no semantic matching or plaintext mirrors.
//!
//! Forgetting conservatively quarantines its originating Conversation from
//! automatic extraction, recall and compaction. Explicit history inspection
//! stays separate; shared artifacts are never garbage-collected by deletion.
use super::ProtectedStore;
use crate::memory::{MemoryProvenance, MemoryRecord, SourceDeletionPreview, SourceDeletionReceipt};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

mod lineage;

pub(super) const SCHEMA: &str = "
CREATE TABLE privacy_generation(singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL);
INSERT INTO privacy_generation VALUES(1,0);
CREATE TABLE forgotten_facts(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, scope TEXT NOT NULL, fingerprint TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX forgotten_fingerprint ON forgotten_facts(fingerprint);
CREATE TABLE excluded_sources(conversation TEXT PRIMARY KEY, reason TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE deletion_receipts(id TEXT PRIMARY KEY, conversation TEXT NOT NULL, body BLOB NOT NULL);
";

pub(super) fn advance_generation(tx: &Transaction<'_>) -> Result<()> {
    ensure!(tx.execute("UPDATE privacy_generation SET revision=revision+1 WHERE singleton=1 AND revision<9223372036854775807", [])? == 1, "privacy generation exhausted");
    Ok(())
}

fn generation(db: &Connection) -> Result<u64> {
    Ok(db.query_row(
        "SELECT revision FROM privacy_generation WHERE singleton=1",
        [],
        |r| super::database::read_u64(r, 0),
    )?)
}

fn fingerprint(statement: &str) -> String {
    blake3::hash(
        statement
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            .as_bytes(),
    )
    .to_hex()
    .to_string()
}

pub(super) fn statement_suppressed(db: &Connection, statement: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM forgotten_facts WHERE fingerprint=?1 LIMIT 1",
            [fingerprint(statement)],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub(super) fn suppress(
    tx: &Transaction<'_>,
    old: &MemoryRecord,
    origin: &MemoryProvenance,
) -> Result<()> {
    tx.execute("INSERT INTO forgotten_facts VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,scope=excluded.scope,fingerprint=excluded.fingerprint,at=excluded.at", params![old.id.to_string(), i64::try_from(old.revision + 1)?,old.scope.to_string(),fingerprint(&old.statement),i64::try_from(origin.at_unix_seconds)?])?;
    for conversation in [old.created.conversation, old.changed.conversation]
        .into_iter()
        .flatten()
    {
        exclude_source(tx, conversation, "forgotten source", origin.at_unix_seconds)?;
    }
    Ok(())
}

pub(super) fn restore_fact(tx: &Transaction<'_>, id: Uuid) -> Result<()> {
    tx.execute("DELETE FROM forgotten_facts WHERE id=?1", [id.to_string()])?;
    Ok(())
}

fn exclude_source(tx: &Transaction<'_>, id: Uuid, reason: &str, at: u64) -> Result<()> {
    tx.execute("INSERT INTO excluded_sources VALUES(?1,?2,?3) ON CONFLICT(conversation) DO UPDATE SET reason=CASE WHEN excluded_sources.reason='owner deleted source history' THEN excluded_sources.reason ELSE excluded.reason END,at=MAX(excluded.at,excluded_sources.at)", params![id.to_string(),reason,i64::try_from(at)?])?;
    Ok(())
}

pub(super) fn source_allowed(db: &Connection, id: Uuid) -> Result<bool> {
    lineage::source_allowed(db, id)
}

pub(super) fn memory_review_required(db: &Connection) -> Result<bool> {
    let marker = super::documents::read(db, "restore/review-required", 4096)?;
    let Some(marker) = marker else {
        return Ok(false);
    };
    ensure!(marker.len() <= 4096, "restore review marker exceeds bound");
    let reviewed = super::documents::read(db, "restore/memory-reviewed", 32)?;
    Ok(reviewed.as_deref() != Some(blake3::hash(&marker).as_bytes().as_slice()))
}

impl ProtectedStore {
    pub(crate) fn memory_requires_review(&self) -> Result<bool> {
        self.with_database(|db| memory_review_required(&db.connection))
    }
    pub(crate) fn review_restored_memory(&self, review: Option<&str>) -> Result<serde_json::Value> {
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let marker=super::documents::read(&tx,"restore/review-required",4096)?;
            let Some(marker)=marker else {ensure!(review.is_none(),"this store has no pending restore review");return Ok(serde_json::json!({"review_required":false,"applied":false}));};
            ensure!(marker.len()<=4096,"restore review marker exceeds bound");
            let (records,max_revision)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(revision),0) FROM memory_entries",[],|r|Ok((super::database::read_u64(r,0)?,super::database::read_u64(r,1)?)))?;
            let exclusions=tx.query_row("SELECT COUNT(*) FROM excluded_sources",[],|r|super::database::read_u64(r,0))?;
            let stamp=blake3::hash(format!("{}:{}:{records}:{max_revision}:{exclusions}:{}",self.id(),blake3::hash(&marker).to_hex(),generation(&tx)?).as_bytes()).to_hex().to_string();
            if let Some(review)=review {
                ensure!(review==stamp,"restored memory changed after review; preview again");
                tx.execute("INSERT INTO documents(name,revision,body) VALUES('restore/memory-reviewed',1,?1) ON CONFLICT(name) DO UPDATE SET revision=documents.revision+1,body=excluded.body",[blake3::hash(&marker).as_bytes().as_slice()])?;
                advance_generation(&tx)?;
            }
            let review_required=memory_review_required(&tx)?;
            tx.commit()?;
            Ok(serde_json::json!({"review_required":review_required,"review":stamp,"records":records,"excluded_sources":exclusions,"applied":review.is_some(),"notice":"Inspect memory list/show before confirming. Known prior exclusions remain enforced. A backup restored on another home may lack later forget decisions; forget those records before enabling. This review enables eligible memory/context only, never restored automation or provider-held history."}))
        })
    }
    /// Restore gating controls learned context, but exclusion forbids even old
    /// task summaries from automatically repopulating a prompt.
    pub(crate) fn source_reuse_allowed(&self, conversation: Uuid) -> Result<bool> {
        self.with_database(|db| source_allowed(&db.connection, conversation))
    }
    /// Capture at work admission, then recheck in the write transaction.
    pub(crate) fn privacy_generation(&self) -> Result<u64> {
        self.with_database(|db| generation(&db.connection))
    }

    pub(crate) fn source_eligible(&self, conversation: Uuid) -> Result<bool> {
        self.with_database(|db| {
            let gated = memory_review_required(&db.connection)?;
            Ok(!gated && source_allowed(&db.connection, conversation)?)
        })
    }

    pub(crate) fn source_deletion_preview(
        &self,
        conversation: Uuid,
    ) -> Result<SourceDeletionPreview> {
        ensure!(
            !conversation.is_nil(),
            "Conversation identity must not be nil"
        );
        self.with_database(|db| deletion_preview(&db.connection, conversation, self.id()))
    }

    pub(crate) fn delete_source_history(
        &self,
        conversation: Uuid,
        review: &str,
        now: u64,
    ) -> Result<SourceDeletionReceipt> {
        let _writer = self.session_writer(conversation.to_string().parse()?)?;
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let preview = deletion_preview(&tx, conversation, self.id())?;
            ensure!(
                preview.review == review,
                "source changed after preview; review deletion again"
            );
            exclude_source(&tx, conversation, "owner deleted source history", now)?;
            tx.execute(
                "DELETE FROM native_sessions WHERE id=?1",
                [conversation.to_string()],
            )?;
            let receipt = SourceDeletionReceipt {
                conversation,
                receipt: Uuid::new_v4(),
                deleted_records: preview.records,
                at_unix_seconds: now,
                artifacts_retained: true,
            };
            tx.execute(
                "INSERT INTO deletion_receipts VALUES(?1,?2,?3)",
                params![
                    receipt.receipt.to_string(),
                    conversation.to_string(),
                    serde_json::to_vec(&receipt)?
                ],
            )?;
            advance_generation(&tx)?;
            tx.commit()?;
            Ok(receipt)
        })
    }

    /// Copy exclusions from a locked prior generation into a restored store.
    /// Pages are bounded and the restore marker stays set until owner review.
    pub(crate) fn reconcile_exclusions_from(&self, prior: &ProtectedStore) -> Result<()> {
        prior.with_database(|source| self.with_database(|destination| {
            let version:u32=source.connection.query_row("SELECT version FROM store_identity",[],|r|r.get(0))?;
            if version < 4 { return Ok(()); }
            let tx=destination.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (table, columns, key) in [("forgotten_facts","id,revision,scope,fingerprint,at","id"),("excluded_sources","conversation,reason,at","conversation"),("deletion_receipts","id,conversation,body","id")] {
                let mut last=String::new();
                loop {
                    let sql=format!("SELECT {columns} FROM {table} WHERE {key}>?1 ORDER BY {key} LIMIT 64");
                    let mut q=source.connection.prepare(&sql)?;
                    let count=q.column_count();
                    let mut cursor=q.query([&last])?;
                    let mut seen=0;
                    while let Some(source_row)=cursor.next()? {
                        // Validate borrowed SQLite values before allocating an owned row.
                        let row=bounded_exclusion_row(table,source_row)?;
                        let rusqlite::types::Value::Text(id)=&row[0] else { anyhow::bail!("invalid exclusion identity"); };
                        last=id.clone();
                        let placeholders=(1..=count).map(|n|format!("?{n}")).collect::<Vec<_>>().join(",");
                        tx.execute(&format!("INSERT OR REPLACE INTO {table}({columns}) VALUES({placeholders})"),rusqlite::params_from_iter(row))?;
                        seen+=1;
                    }
                    if seen<64 { break; }
                }
            }
            // A snapshot's formerly-active copy may predate the later forget.
            let mut q=tx.prepare("SELECT e.body,f.revision,f.at FROM memory_entries e JOIN forgotten_facts f ON e.id=f.id")?;
            let mut rows=q.query([])?;
            while let Some(row)=rows.next()? {
                let bytes=row.get_ref(0)?.as_blob()?;
                ensure!(bytes.len()<=crate::memory::RECORD_BYTES,"memory restore record exceeds bound");
                let mut record:MemoryRecord=serde_json::from_slice(bytes)?;
                record.validate()?;
                record.revision = record.revision.max(super::database::read_u64(row, 1)?).checked_add(1).context("restored memory revision exhausted")?;
                record.changed = MemoryProvenance {
                    owner_request: Uuid::new_v4(),
                    conversation: None,
                    at_unix_seconds: record.changed.at_unix_seconds.max(super::database::read_u64(row, 2)?),
                };
                record.state=crate::memory::MemoryState::Forgotten;
                record.validate()?;
                tx.execute("UPDATE memory_entries SET body=?2,revision=?3 WHERE id=?1",params![record.id.to_string(),serde_json::to_vec(&record)?,i64::try_from(record.revision)?])?;
            }
            drop(rows);drop(q);
            tx.execute("DELETE FROM native_sessions WHERE id IN (SELECT conversation FROM excluded_sources WHERE reason='owner deleted source history')", [])?;
            advance_generation(&tx)?;
            tx.commit()?;
            Ok(())
        }))
    }
}

/// Each copied row is at most a small metadata record, never an unrestricted
/// SQLite Value batch; corrupt private records fail before a Rust body allocation.
fn bounded_exclusion_row(
    table: &str,
    row: &rusqlite::Row<'_>,
) -> Result<Vec<rusqlite::types::Value>> {
    use rusqlite::types::Value;
    fn text<'a>(row: &'a rusqlite::Row<'_>, index: usize, limit: usize) -> Result<&'a str> {
        let value = row.get_ref(index)?.as_str()?;
        ensure!(
            value.len() <= limit,
            "restore exclusion text exceeds its bound"
        );
        ensure!(
            !value.chars().any(char::is_control),
            "invalid restore exclusion text"
        );
        Ok(value)
    }
    fn id(row: &rusqlite::Row<'_>, index: usize) -> Result<String> {
        let value = text(row, index, 36)?;
        let id: Uuid = value.parse()?;
        ensure!(
            !id.is_nil() && id.to_string() == value,
            "invalid restore exclusion identity"
        );
        Ok(value.into())
    }
    fn integer(row: &rusqlite::Row<'_>, index: usize) -> Result<Value> {
        Ok(Value::Integer(i64::try_from(super::database::read_u64(
            row, index,
        )?)?))
    }
    Ok(match table {
        "forgotten_facts" => {
            let scope = text(row, 2, 64)?;
            scope.parse::<crate::memory::MemoryScope>()?;
            let fingerprint = text(row, 3, 64)?;
            ensure!(
                fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid restore exclusion fingerprint"
            );
            vec![
                Value::Text(id(row, 0)?),
                integer(row, 1)?,
                Value::Text(scope.into()),
                Value::Text(fingerprint.into()),
                integer(row, 4)?,
            ]
        }
        "excluded_sources" => vec![
            Value::Text(id(row, 0)?),
            Value::Text(text(row, 1, 128)?.into()),
            integer(row, 2)?,
        ],
        "deletion_receipts" => {
            let id = id(row, 0)?;
            let conversation = text(row, 1, 36)?;
            let bytes = row.get_ref(2)?.as_blob()?;
            ensure!(
                bytes.len() <= 1024,
                "restore deletion receipt exceeds its bound"
            );
            let receipt: SourceDeletionReceipt = serde_json::from_slice(bytes)?;
            ensure!(
                receipt.receipt.to_string() == id
                    && receipt.conversation.to_string() == conversation
                    && !receipt.conversation.is_nil(),
                "restore deletion receipt identity mismatch"
            );
            vec![
                Value::Text(id),
                Value::Text(conversation.into()),
                Value::Blob(bytes.into()),
            ]
        }
        _ => anyhow::bail!("unsupported restore exclusion table"),
    })
}

fn deletion_preview(
    db: &Connection,
    conversation: Uuid,
    store: Uuid,
) -> Result<SourceDeletionPreview> {
    let (records, bytes): (u64, u64) = db
        .query_row(
            "SELECT revision,bytes FROM native_sessions WHERE id=?1",
            [conversation.to_string()],
            |r| {
                Ok((
                    super::database::read_u64(r, 0)?,
                    super::database::read_u64(r, 1)?,
                ))
            },
        )
        .optional()?
        .context(
            "native source Conversation not found; vendor-owned histories cannot be deleted here",
        )?;
    let review = blake3::hash(
        format!(
            "{store}:{conversation}:{records}:{bytes}:{}",
            generation(db)?
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    Ok(SourceDeletionPreview{conversation,records,bytes,review,notice:"Deletes this inactive native Conversation's stored history only. Personal facts remain unless forgotten separately; shared artifacts, external backups and provider copies are retained. This is not secure erasure.".into()})
}

#[cfg(test)]
mod tests;
