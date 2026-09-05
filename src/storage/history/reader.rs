use super::*;
use crate::session::{ConversationPage, NativeConversationHandle};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

struct Entry {
    parent: Option<String>,
    sequence: usize,
    bytes: usize,
}

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
        const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
        self.with_database(|db| {
            let mut head: Option<String> = db.connection.query_row("SELECT head FROM native_sessions WHERE id=?1", [id.to_string()], |row| row.get(0))?;
            // Index metadata only; payloads are fetched only for the selected page.
            let mut query = db.connection.prepare("SELECT e.id,e.parent,e.sequence,length(r.body) FROM native_entries e JOIN native_records r ON r.session=e.session AND r.sequence=e.sequence WHERE e.session=?1 LIMIT ?2")?;
            let mut rows = query.query(params![id.to_string(),i64::try_from(MAX_SESSION_RECORDS+1)?])?;
            let mut entries = HashMap::new();
            while let Some(row) = rows.next()? {
                ensure!(entries.len() < MAX_SESSION_RECORDS, "Conversation index exceeds restore bounds");
                let bytes = read_usize(row,3)?;
                ensure!(bytes <= MAX_RECORD_BYTES, "Conversation entry exceeds record bounds");
                entries.insert(row.get::<_,String>(0)?, Entry { parent: row.get(1)?, sequence: read_usize(row,2)?, bytes });
            }
            let mut seen = HashSet::new();
            let mut chain = Vec::new();
            while let Some(entry_id) = head {
                ensure!(seen.insert(entry_id.clone()), "Conversation ancestry contains a cycle");
                let entry = entries.get(&entry_id).context("Conversation ancestry references an unavailable entry")?;
                chain.push((entry_id,entry));
                head = entry.parent.clone();
            }
            chain.reverse();
            let total = chain.len();
            let limit = limit.clamp(1,128);
            let mut bytes = 0usize;
            let (start,end) = if let Some(start) = from {
                let start = start.min(total);
                let mut end = start;
                while end < start.saturating_add(limit).min(total) {
                    let next = chain[end].1.bytes;
                    if bytes.saturating_add(next) > MAX_PAGE_BYTES { break; }
                    bytes += next;
                    end += 1;
                }
                (start,end)
            } else {
                let end = before.unwrap_or(total).min(total);
                let mut start = end;
                while start > end.saturating_sub(limit) {
                    let next = chain[start-1].1.bytes;
                    if bytes.saturating_add(next) > MAX_PAGE_BYTES { break; }
                    bytes += next;
                    start -= 1;
                }
                (start,end)
            };
            let mut messages = Vec::with_capacity(end-start);
            let mut read = db.connection.prepare("SELECT body FROM native_records WHERE session=?1 AND sequence=?2")?;
            for (entry_id,entry) in &chain[start..end] {
                let body: Vec<u8> = read.query_row(params![id.to_string(),i64::try_from(entry.sequence)?], |row| row.get(0))?;
                ensure!(body.len() == entry.bytes, "Conversation entry changed while paging");
                let record: RecordEnvelope = serde_json::from_slice(&body)?;
                ensure!(record.session_id == id && record.version == SESSION_RECORD_VERSION, "Conversation record identity differs");
                let SessionRecord::ConversationEntryAppended { entry } = record.record else { anyhow::bail!("Conversation index does not reference an entry"); };
                ensure!(entry.id.to_string() == *entry_id, "Conversation entry identity differs");
                messages.push(entry.message);
            }
            Ok(ConversationPage { messages, start, total, has_older: start > 0 })
        })
    }
}
