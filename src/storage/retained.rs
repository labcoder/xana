//! Per-worker compare-and-swap and immutable request receipts in protected storage.
use super::{ProtectedStore, documents};
use crate::{
    identity::AgentId,
    orchestration::retained::{RetainedWorker, WORKER_BYTES, WorkerState},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{TransactionBehavior, params};

fn name(id: AgentId) -> String {
    format!("workers/{id}")
}

fn eligible(tx: &rusqlite::Transaction<'_>, worker: &RetainedWorker) -> Result<()> {
    let generation = tx.query_row(
        "SELECT revision FROM privacy_generation WHERE singleton=1",
        [],
        |r| super::database::read_u64(r, 0),
    )?;
    ensure!(
        generation == worker.privacy_generation
            && super::forgetting::source_allowed(tx, worker.session.to_string().parse()?)?
            && !super::forgetting::memory_review_required(tx)?,
        "worker sources changed or require review"
    );
    ensure!(
        !tx.prepare("SELECT 1 FROM documents WHERE name='restore/review-required'")?
            .exists([])?,
        "restored worker authority requires fresh owner review"
    );
    ensure!(
        worker.expires_at > crate::autonomy::now()?
            && !matches!(
                worker.state,
                WorkerState::Stopped | WorkerState::Expired | WorkerState::NeedsReview
            ),
        "worker authority expired or stopped"
    );
    Ok(())
}

impl ProtectedStore {
    pub(crate) fn retained_worker(&self, id: AgentId) -> Result<RetainedWorker> {
        let bytes = self
            .document(&name(id), WORKER_BYTES)?
            .context("unknown retained worker")?;
        let worker: RetainedWorker = serde_json::from_slice(&bytes)?;
        worker.validate()?;
        ensure!(worker.id == id, "worker record identity mismatch");
        Ok(worker)
    }

    pub(crate) fn retained_create(&self, worker: RetainedWorker) -> Result<()> {
        worker.validate()?;
        ensure!(
            worker.revision == 1 && worker.state == WorkerState::Idle,
            "new retained worker must be idle"
        );
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            eligible(&tx, &worker)?;
            let count: usize = tx.query_row(
                "SELECT count(*) FROM (SELECT 1 FROM documents WHERE name >= 'workers/' AND name < 'workers0' LIMIT 1000)",
                [],
                |row| super::database::read_usize(row, 0),
            )?;
            ensure!(count < 1000, "retained worker inventory is full");
            tx.execute(
                "INSERT INTO documents(name,revision,body) VALUES(?1,1,?2)",
                params![name(worker.id), serde_json::to_vec(&worker)?],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn retained_update<T>(
        &self,
        id: AgentId,
        revision: u64,
        edit: impl FnOnce(&mut RetainedWorker) -> Result<T>,
    ) -> Result<(RetainedWorker, T)> {
        self.retained_edit(id, Some(revision), false, edit)
    }

    pub(crate) fn retained_admit<T>(
        &self,
        id: AgentId,
        revision: u64,
        edit: impl FnOnce(&mut RetainedWorker) -> Result<T>,
    ) -> Result<(RetainedWorker, T)> {
        self.retained_edit(id, Some(revision), true, edit)
    }

    /// Settle already admitted work against the latest mailbox atomically.
    /// The caller must fence its original execution/reservation identity inside
    /// `finish`; unrelated owner edits are preserved, never replayed or lost.
    pub(crate) fn retained_settle<T>(
        &self,
        id: AgentId,
        require_eligible: bool,
        finish: impl FnOnce(&mut RetainedWorker) -> Result<T>,
    ) -> Result<(RetainedWorker, T)> {
        self.retained_edit(id, None, require_eligible, finish)
    }

    fn retained_edit<T>(
        &self,
        id: AgentId,
        revision: Option<u64>,
        require_eligible: bool,
        edit: impl FnOnce(&mut RetainedWorker) -> Result<T>,
    ) -> Result<(RetainedWorker, T)> {
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let bytes = documents::read(&tx, &name(id), WORKER_BYTES)?
                .context("unknown retained worker")?;
            let mut worker: RetainedWorker = serde_json::from_slice(&bytes)?;
            worker.validate()?;
            let previous_bytes = worker.context_bytes;
            let previous_operations = worker.context_operations;
            ensure!(
                worker.id == id && revision.is_none_or(|expected| worker.revision == expected),
                "worker changed; refresh its revision before retrying"
            );
            if require_eligible {
                eligible(&tx, &worker)?;
            }
            let previous_revision = worker.revision;
            let result = edit(&mut worker)?;
            let delta_bytes = worker
                .context_bytes
                .checked_sub(previous_bytes)
                .context("context byte reservations cannot be reset")?;
            let delta_ops = worker
                .context_operations
                .checked_sub(previous_operations)
                .context("context operation reservations cannot be reset")?;
            if delta_bytes > 0 || delta_ops > 0 {
                reserve_parent_context(&tx, worker.session, delta_bytes, delta_ops)?;
            }
            worker.revision = previous_revision
                .checked_add(1)
                .context("worker revision exhausted")?;
            worker.validate()?;
            tx.execute(
                "UPDATE documents SET revision=?2,body=?3 WHERE name=?1",
                params![
                    name(id),
                    i64::try_from(worker.revision)?,
                    serde_json::to_vec(&worker)?
                ],
            )?;
            tx.commit()?;
            Ok((worker, result))
        })
    }

    pub(crate) fn retained_page(&self, after: Option<AgentId>) -> Result<Vec<RetainedWorker>> {
        let after = after.map(name).unwrap_or_else(|| "workers/".into());
        let names = self.with_database(|db| {
            let mut query = db.connection.prepare(
                "SELECT name FROM documents WHERE name>?1 AND name<'workers0' ORDER BY name LIMIT 32",
            )?;
            Ok(query.query_map([after], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        names
            .into_iter()
            .map(|name| self.retained_worker(name.trim_start_matches("workers/").parse()?))
            .collect()
    }
}

/// All workers under the original parent consume one ledger. Reservations are
/// monotone, including failed, interrupted and unknown work.
fn reserve_parent_context(
    tx: &rusqlite::Transaction<'_>,
    parent: crate::identity::SessionId,
    delta_bytes: u64,
    delta_ops: u64,
) -> Result<()> {
    let key = format!("worker-context-budget/{parent}");
    let (bytes, ops): (u64, u64) = documents::read(tx, &key, 128)?
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?
        .unwrap_or_default();
    let bytes = bytes
        .checked_add(delta_bytes)
        .filter(|total| *total <= crate::orchestration::retained::CONTEXT_TOTAL_BYTES)
        .context("parent context byte allowance exhausted")?;
    let ops = ops
        .checked_add(delta_ops)
        .filter(|total| *total <= crate::orchestration::retained::CONTEXT_TOTAL_OPS)
        .context("parent context operation allowance exhausted")?;
    tx.execute(
        "INSERT INTO documents(name,revision,body) VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body",
        params![key, serde_json::to_vec(&(bytes, ops))?],
    )?;
    Ok(())
}
