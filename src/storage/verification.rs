//! Full offline verification supplements normal bounded per-object reads.

use super::{ProtectedStore, database::read_u64};
use crate::{artifact::ContentHash, identity::SessionId, session::SessionStore};
use anyhow::{Result, ensure};

impl ProtectedStore {
    pub(crate) fn verify_content(&self) -> Result<()> {
        self.verify()?;
        let objects = self.object_inventory()?;
        let lengths: std::collections::HashMap<_, _> = objects
            .iter()
            .map(|(hash, _, length)| (hash.as_str(), *length))
            .collect();
        let sessions = self.with_database(|db| {
            let mut query = db
                .connection
                .prepare("SELECT id FROM native_sessions ORDER BY id LIMIT 100001")?;
            let ids = query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                ids.len() <= 100_000,
                "verification exceeds Conversation inventory bound"
            );
            Ok(ids)
        })?;
        for id in sessions {
            let id: SessionId = id.parse()?;
            let version: u32 = self.with_database(|db| {
                Ok(db
                    .connection
                    .query_row("SELECT version FROM store_identity", [], |r| r.get(0))?)
            })?;
            if version >= 7 {
                self.verify_immutable_history(id, &lengths)?;
                self.verify_historical_transitions(id)?;
                crate::session::DurableSession::inspect_execution_protected(self, id)?;
                continue;
            }
            // Recovery snapshots keep their historical schema read-only; old
            // journals retain their original bounded full-reduction contract.
            let loaded = SessionStore::inspect_protected(self, id)?;
            let restored = crate::session::reduce(&loaded.records)?;
            for artifact in restored.artifacts.values() {
                ensure!(
                    lengths.get(artifact.reference.content_hash.as_str())
                        == Some(&artifact.byte_len),
                    "Conversation references a missing or mismatched artifact"
                );
            }
        }
        for (hash, _, length) in objects {
            let hash = ContentHash::parse(hash).map_err(anyhow::Error::msg)?;
            self.export_artifact(&hash, length, &mut std::io::sink())?;
        }
        Ok(())
    }

    pub(super) fn object_inventory(&self) -> Result<Vec<(String, String, u64)>> {
        self.with_database(|db| {
            let mut query = db.connection.prepare(
                "SELECT hash,file_id,length FROM encrypted_artifacts ORDER BY hash LIMIT 100001",
            )?;
            let objects = query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, read_u64(row, 2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                objects.len() <= 100_000,
                "verification exceeds artifact inventory bound"
            );
            Ok(objects)
        })
    }
}
