//! Position-indexed bounded history pages under one coherent database snapshot.

use super::*;
use crate::session::{ConversationPage, NativeConversationHandle};
use std::{
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PAGE_MESSAGES: usize = 128;

impl ProtectedStore {
    pub(crate) fn list_histories(&self, workspace: &Path) -> Result<Vec<NativeConversationHandle>> {
        self.with_database(|db| {
            let mut statement = db.connection.prepare("SELECT id,revision,modified FROM native_sessions WHERE workspace=?1 ORDER BY modified DESC,id DESC LIMIT 10001")?;
            let mut rows = statement.query([workspace.to_str().context("workspace cannot be serialized")?])?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                ensure!(result.len() < 10_000, "Conversation listing exceeds its bound");
                let session_id = row.get::<_,String>(0)?.parse()?;
                let record_count = read_usize(row,1)?;
                let modified = UNIX_EPOCH.checked_add(Duration::from_millis(read_u64(row,2)?)).context("Conversation time is invalid")?;
                result.push(NativeConversationHandle { session_id, record_count, modified });
            }
            Ok(result)
        })
    }

    pub(crate) fn history_page(
        &self,
        id: SessionId,
        before: Option<usize>,
        from: Option<usize>,
        limit: usize,
    ) -> Result<ConversationPage> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let page = page(&tx, id, before, from, limit, None)?;
            tx.commit()?;
            Ok(page)
        })
    }

    /// An append-stable saved-page capability: the anchored prefix must still
    /// be on this Conversation's active path. No whole-history body is loaded.
    pub(crate) fn history_page_anchored(
        &self,
        id: SessionId,
        workspace: &Path,
        anchor: Option<(usize, Option<String>)>,
        before: Option<usize>,
        from: Option<usize>,
    ) -> Result<(ConversationPage, Option<String>)> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let recorded_workspace: String = tx.query_row(
                "SELECT workspace FROM native_sessions WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            ensure!(
                Path::new(&recorded_workspace) == workspace,
                "Conversation belongs to another workspace"
            );
            if let Some((total, tail)) = &anchor {
                let current_tail: Option<String> = if *total == 0 {
                    None
                } else {
                    tx.query_row(
                        "SELECT entry FROM native_path WHERE session=?1 AND position=?2",
                        params![id.to_string(), i64::try_from(total - 1)?],
                        |row| row.get(0),
                    )
                    .optional()?
                };
                ensure!(
                    &current_tail == tail && (*total == 0 || tail.is_some()),
                    "saved history changed; return to Live and reopen history"
                );
            }
            let page = page(
                &tx,
                id,
                before,
                from,
                MAX_PAGE_MESSAGES,
                anchor.map(|anchor| anchor.0),
            )?;
            let tail = if page.total == 0 {
                None
            } else {
                Some(tx.query_row(
                    "SELECT entry FROM native_path WHERE session=?1 AND position=?2",
                    params![id.to_string(), i64::try_from(page.total - 1)?],
                    |row| row.get(0),
                )?)
            };
            tx.commit()?;
            Ok((page, tail))
        })
    }
}

fn page(
    tx: &Transaction<'_>,
    id: SessionId,
    before: Option<usize>,
    from: Option<usize>,
    limit: usize,
    ceiling: Option<usize>,
) -> Result<ConversationPage> {
    let session = id.to_string();
    let head: Option<String> = tx.query_row(
        "SELECT head FROM native_sessions WHERE id=?1",
        [&session],
        |row| row.get(0),
    )?;
    let tail: Option<(usize, String)> = tx.query_row(
        "SELECT position,entry FROM native_path WHERE session=?1 ORDER BY position DESC LIMIT 1",
        [&session], |row| Ok((read_usize(row, 0)?, row.get(1)?)),
    ).optional()?;
    ensure!(
        head.as_ref() == tail.as_ref().map(|tail| &tail.1),
        "Conversation page index head differs"
    );
    let total = match tail {
        Some((position, _)) => position
            .checked_add(1)
            .context("Conversation position overflow")?,
        None => 0,
    };
    ensure!(
        total <= MAX_PROTECTED_RECORDS,
        "Conversation path exceeds the supported restore bound"
    );
    ensure!(
        ceiling.is_none_or(|ceiling| ceiling <= total),
        "saved history is no longer active"
    );
    let total = ceiling.unwrap_or(total);
    let limit = limit.clamp(1, MAX_PAGE_MESSAGES);
    let forward = from.is_some();
    let (low, high) = if let Some(start) = from {
        let start = start.min(total);
        (start, start.saturating_add(limit).min(total))
    } else {
        let end = before.unwrap_or(total).min(total);
        (end.saturating_sub(limit), end)
    };
    let sql = if forward {
        "SELECT p.position,p.entry,r.body,e.parent,prior.entry FROM native_path p JOIN native_records r ON r.session=p.session AND r.sequence=p.sequence JOIN native_entries e ON e.session=p.session AND e.id=p.entry LEFT JOIN native_path prior ON prior.session=p.session AND prior.position=p.position-1 WHERE p.session=?1 AND p.position>=?2 AND p.position<?3 ORDER BY p.position ASC LIMIT 128"
    } else {
        "SELECT p.position,p.entry,r.body,e.parent,prior.entry FROM native_path p JOIN native_records r ON r.session=p.session AND r.sequence=p.sequence JOIN native_entries e ON e.session=p.session AND e.id=p.entry LEFT JOIN native_path prior ON prior.session=p.session AND prior.position=p.position-1 WHERE p.session=?1 AND p.position>=?2 AND p.position<?3 ORDER BY p.position DESC LIMIT 128"
    };
    let mut query = tx.prepare(sql)?;
    let mut rows = query.query(params![session, i64::try_from(low)?, i64::try_from(high)?])?;
    let mut messages = Vec::with_capacity(high - low);
    let mut bytes = 0usize;
    let mut bounded = false;
    while let Some(row) = rows.next()? {
        let position = read_usize(row, 0)?;
        let expected = if forward {
            low + messages.len()
        } else {
            high - messages.len() - 1
        };
        ensure!(
            position == expected,
            "Conversation page index is discontinuous"
        );
        let body = row.get_ref(2)?.as_blob()?;
        ensure!(
            body.len() <= MAX_RECORD_BYTES,
            "Conversation entry exceeds record bounds"
        );
        if bytes.saturating_add(body.len()) > MAX_PAGE_BYTES {
            bounded = true;
            break;
        }
        let record: RecordEnvelope = serde_json::from_slice(body)?;
        ensure!(
            record.session_id == id && record.version == SESSION_RECORD_VERSION,
            "Conversation record identity differs"
        );
        let SessionRecord::ConversationEntryAppended { entry } = record.record else {
            anyhow::bail!("Conversation index does not reference an entry");
        };
        let parent: Option<String> = row.get(3)?;
        let previous: Option<String> = row.get(4)?;
        ensure!(
            parent == previous && (position == 0 || previous.is_some()),
            "Conversation page index ancestry differs"
        );
        ensure!(
            entry.id.to_string() == row.get::<_, String>(1)?
                && entry.parent.map(|parent| parent.to_string()) == parent,
            "Conversation entry identity or ancestry differs"
        );
        bytes += body.len();
        messages.push(entry.message);
    }
    ensure!(
        bounded || messages.len() == high - low,
        "Conversation page index is incomplete"
    );
    let start = if forward { low } else { high - messages.len() };
    if !forward {
        messages.reverse();
    }
    Ok(ConversationPage {
        messages,
        start,
        total,
        has_older: start > 0,
    })
}
