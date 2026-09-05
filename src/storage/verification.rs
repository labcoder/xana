//! Full offline verification supplements normal bounded per-object reads.

use super::{ProtectedStore, database::read_u64};
use crate::{artifact::ContentHash, identity::SessionId, session::SessionStore};
use anyhow::{Result, ensure};

impl ProtectedStore {
    pub(crate) fn verify_content(&self) -> Result<()> {
        #[cfg(test)]
        let mut timing = VerifyTiming::new(std::env::var_os("XANA_M6_VERIFY_TIMING").is_some());
        #[cfg(test)]
        let started = timing.start();
        self.verify()?;
        #[cfg(test)]
        timing.record("database_integrity", started);
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
                #[cfg(test)]
                let started = timing.start();
                self.verify_immutable_history(
                    id,
                    &lengths,
                    #[cfg(test)]
                    timing.enabled,
                )?;
                #[cfg(test)]
                timing.record("immutable_history", started);
                #[cfg(test)]
                let started = timing.start();
                self.verify_historical_transitions(id)?;
                #[cfg(test)]
                timing.record("operation_transitions", started);
                #[cfg(test)]
                let started = timing.start();
                crate::session::DurableSession::inspect_execution_protected(self, id)?;
                #[cfg(test)]
                timing.record("execution_restore", started);
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
        #[cfg(test)]
        let started = timing.start();
        for (hash, _, length) in objects {
            let hash = ContentHash::parse(hash).map_err(anyhow::Error::msg)?;
            self.export_artifact(&hash, length, &mut std::io::sink())?;
        }
        #[cfg(test)]
        {
            timing.record("artifact_decrypt", started);
            timing.report("full_verify");
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

/// Opt-in test instrumentation reports only phase names, counts and elapsed time.
/// It neither changes verification nor appears in production builds.
#[cfg(test)]
pub(in crate::storage) struct VerifyTiming {
    pub(in crate::storage) enabled: bool,
    phases: std::collections::BTreeMap<&'static str, (u64, std::time::Duration)>,
}

#[cfg(test)]
impl VerifyTiming {
    pub(in crate::storage) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            phases: Default::default(),
        }
    }

    pub(in crate::storage) fn start(&self) -> Option<std::time::Instant> {
        self.enabled.then(std::time::Instant::now)
    }

    pub(in crate::storage) fn record(
        &mut self,
        phase: &'static str,
        started: Option<std::time::Instant>,
    ) {
        if let Some(started) = started {
            let value = self.phases.entry(phase).or_default();
            value.0 += 1;
            value.1 += started.elapsed();
        }
    }

    pub(in crate::storage) fn report(&self, group: &'static str) {
        for (phase, (count, duration)) in &self.phases {
            eprintln!(
                "verify_timing group={group} phase={phase} calls={count} elapsed_us={}",
                duration.as_micros()
            );
        }
    }
}
