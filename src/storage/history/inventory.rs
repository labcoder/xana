//! Scalar historical counts and bounded detail previews, not full-state restore.

use super::*;

pub(crate) struct HistoryInventory {
    pub(crate) artifacts: usize,
    pub(crate) artifact_bytes: u64,
    pub(crate) context_versions: usize,
    pub(crate) children: usize,
    pub(crate) compactions: usize,
    pub(crate) recent_compactions: Vec<crate::session::CompactionCheckpoint>,
}

impl ProtectedStore {
    pub(crate) fn history_inventory(&self, id: SessionId) -> Result<HistoryInventory> {
        self.with_database(|db| {
            let tx=db.connection.transaction()?;
            let count=|kind:&str|->Result<usize>{Ok(tx.query_row("SELECT COUNT(DISTINCT subject) FROM native_subjects WHERE session=?1 AND kind=?2",params![id.to_string(),kind],|row|read_usize(row,0))?)};
            let artifacts=count("artifact")?;
            let children=count("child")?;
            let compactions=count("compaction")?;
            let context_versions=tx.query_row("SELECT COUNT(*) FROM native_subjects WHERE session=?1 AND kind='context'",[id.to_string()],|row|read_usize(row,0))?;
            let artifact_bytes=tx.query_row("SELECT COALESCE(SUM(json_extract(CAST(r.body AS TEXT),'$.data.artifact.byte_len')),0) FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='artifact'",[id.to_string()],|row|read_u64(row,0))?;
            let mut query=tx.prepare("SELECT r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='compaction' ORDER BY s.sequence DESC LIMIT 64")?;
            let mut rows=query.query([id.to_string()])?;
            let mut recent_compactions=Vec::new();
            while let Some(row)=rows.next()? {
                let body=row.get_ref(0)?.as_blob()?;
                ensure!(body.len()<=MAX_RECORD_BYTES,"compaction detail exceeds its record bound");
                let record:RecordEnvelope=serde_json::from_slice(body)?;
                let SessionRecord::ConversationCompacted {checkpoint}=record.record else {anyhow::bail!("compaction index differs");};
                ensure!(record.session_id==id,"compaction detail belongs to another Conversation");
                recent_compactions.push(checkpoint);
            }
            recent_compactions.reverse();
            Ok(HistoryInventory {artifacts,artifact_bytes,context_versions,children,compactions,recent_compactions})
        })
    }
}
