//! Transactional active-path positions: bounded page reads without a full DAG cache.

use super::*;

pub(in crate::storage) const PATH_SCHEMA: &str = "
CREATE TABLE native_path(
 session TEXT NOT NULL REFERENCES native_sessions(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position >= 0), entry TEXT NOT NULL,
 sequence INTEGER NOT NULL,
 PRIMARY KEY(session,position), UNIQUE(session,entry),
 FOREIGN KEY(session,entry) REFERENCES native_entries(session,id) ON DELETE CASCADE,
 FOREIGN KEY(session,sequence) REFERENCES native_records(session,sequence) ON DELETE CASCADE);
";

/// Schema upgrades run exclusively; the index is never built by a read-only page.
pub(in crate::storage) fn migrate_path_index(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(PATH_SCHEMA)?;
    let mut sessions = tx.prepare("SELECT id,head FROM native_sessions ORDER BY id")?;
    let mut rows = sessions.query([])?;
    while let Some(row) = rows.next()? {
        let id: SessionId = row.get::<_, String>(0)?.parse()?;
        rebuild(tx, id, row.get(1)?)?;
    }
    Ok(())
}

pub(super) fn move_head(tx: &Transaction<'_>, id: SessionId, head: Option<String>) -> Result<()> {
    let session = id.to_string();
    let Some(head) = head else {
        tx.execute("DELETE FROM native_path WHERE session=?1", [session])?;
        return Ok(());
    };
    let existing: Option<i64> = tx
        .query_row(
            "SELECT position FROM native_path WHERE session=?1 AND entry=?2",
            params![session, head],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(position) = existing {
        tx.execute(
            "DELETE FROM native_path WHERE session=?1 AND position>?2",
            params![session, position],
        )?;
        return Ok(());
    }
    let (parent, sequence) = indexed_entry(tx, &session, &head)?;
    let tail: Option<(String, i64, usize)> = tx.query_row(
        "SELECT entry,position,sequence FROM native_path WHERE session=?1 ORDER BY position DESC LIMIT 1",
        [&session], |row| Ok((row.get(0)?, row.get(1)?, read_usize(row, 2)?)),
    ).optional()?;
    if parent.as_ref() == tail.as_ref().map(|tail| &tail.0) {
        ensure!(
            tail.as_ref().is_none_or(|tail| tail.2 < sequence),
            "Conversation ancestry sequence is invalid"
        );
        tx.execute(
            "INSERT INTO native_path VALUES(?1,?2,?3,?4)",
            params![
                session,
                tail.map_or(0, |tail| tail.1 + 1),
                head,
                i64::try_from(sequence)?
            ],
        )?;
        return Ok(());
    }
    rebuild(tx, id, Some(head))
}

fn indexed_entry(tx: &Transaction<'_>, session: &str, id: &str) -> Result<(Option<String>, usize)> {
    tx.query_row(
        "SELECT parent,sequence FROM native_entries WHERE session=?1 AND id=?2",
        params![session, id],
        |row| Ok((row.get(0)?, read_usize(row, 1)?)),
    )
    .optional()?
    .context("Conversation ancestry references an unavailable entry")
}

fn rebuild(tx: &Transaction<'_>, id: SessionId, head: Option<String>) -> Result<()> {
    let session = id.to_string();
    // Parent entries must already exist when an entry is appended, so strictly
    // decreasing journal sequence proves acyclicity without retaining every ID.
    let mut cursor = head.clone();
    let mut previous = usize::MAX;
    let mut count = 0usize;
    while let Some(entry) = cursor {
        let (parent, sequence) = indexed_entry(tx, &session, &entry)?;
        ensure!(
            sequence < previous,
            "Conversation ancestry contains a cycle or forward reference"
        );
        ensure!(
            count < MAX_PROTECTED_RECORDS,
            "Conversation ancestry exceeds the supported restore bound"
        );
        previous = sequence;
        count += 1;
        cursor = parent;
    }
    tx.execute("DELETE FROM native_path WHERE session=?1", [&session])?;
    let mut cursor = head;
    let mut insert = tx.prepare("INSERT INTO native_path VALUES(?1,?2,?3,?4)")?;
    while let Some(entry) = cursor {
        let (parent, sequence) = indexed_entry(tx, &session, &entry)?;
        count -= 1;
        insert.execute(params![
            session,
            i64::try_from(count)?,
            entry,
            i64::try_from(sequence)?
        ])?;
        cursor = parent;
    }
    Ok(())
}
