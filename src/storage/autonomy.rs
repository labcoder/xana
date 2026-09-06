//! Transactional scheduled intent and terminal receipts in SQLCipher. SQL indexes
//! bound candidate selection; BLOB lengths are checked before Rust allocation.
use super::ProtectedStore;
use super::database::{read_u64, read_usize};
use crate::autonomy::{
    HostPolicy, JOB_BYTES, Job, JobEdit, JobState, PAGE_SIZE, QUEUE_LIMIT, RunOutcome, RunReceipt,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE autonomy_jobs(sequence INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
 revision INTEGER NOT NULL CHECK(revision>0), state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 7),
 due INTEGER NOT NULL, body BLOB NOT NULL);
CREATE INDEX autonomy_due ON autonomy_jobs(state,due,sequence);
CREATE TABLE autonomy_receipts(sequence INTEGER PRIMARY KEY, job TEXT NOT NULL,
 occurrence TEXT NOT NULL UNIQUE, body BLOB NOT NULL);
CREATE INDEX autonomy_receipt_job ON autonomy_receipts(job,sequence);";
const POLICY: &str = "autonomy/host-policy";
const STOP_IMPACT: &str = "autonomy/stop-impact";

impl ProtectedStore {
    pub(crate) fn autonomy_attention_baseline(&self) -> Result<(u64, u64)> {
        self.with_database(|db| Ok(db.connection.query_row("SELECT (SELECT COALESCE(MAX(sequence),0) FROM autonomy_jobs),(SELECT COALESCE(MAX(sequence),0) FROM autonomy_receipts)",[],|row|Ok((read_u64(row,0)?,read_u64(row,1)?)))?))
    }

    pub(crate) fn autonomy_attention_jobs(&self, after: u64) -> Result<Vec<(u64, Job)>> {
        self.with_database(|db| {
            let mut statement=db.connection.prepare("SELECT sequence,id,revision,state,due,body FROM autonomy_jobs WHERE state IN (0,1,2,3,4) AND sequence>?1 ORDER BY sequence LIMIT 16")?;
            Ok(statement.query_map([i64::try_from(after)?],|row|Ok((read_u64(row,0)?,stored_job(row,1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub(crate) fn autonomy_attention_receipts(
        &self,
        after: u64,
    ) -> Result<Vec<(u64, Job, RunReceipt)>> {
        self.with_database(|db| {
            let mut statement=db.connection.prepare("SELECT r.sequence,j.id,j.revision,j.state,j.due,j.body,r.body,r.occurrence FROM autonomy_receipts r JOIN autonomy_jobs j ON j.id=r.job WHERE r.sequence>?1 ORDER BY r.sequence LIMIT 16")?;
            let rows=statement.query_map([i64::try_from(after)?],|row|Ok((read_u64(row,0)?,stored_job(row,1)?,blob(row,6,20*1024)?,row.get::<_,String>(7)?)))?;
            rows.map(|row| {
                let (sequence,job,bytes,occurrence)=row?;
                let receipt:RunReceipt=serde_json::from_slice(&bytes)?;
                receipt.validate()?;
                ensure!(receipt.occurrence.to_string()==occurrence,"attention receipt identity differs from its index");
                Ok((sequence,job,receipt))
            }).collect()
        })
    }

    pub(crate) fn autonomy_due(&self, now: i64) -> Result<Vec<Job>> {
        self.with_database(|db| {
            let mut statement = db.connection.prepare("SELECT id,revision,state,due,body FROM autonomy_jobs WHERE state=0 AND due<=?1 ORDER BY due,sequence LIMIT 8")?;
            Ok(statement.query_map([now], |r| stored_job(r,0))?.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub(crate) fn autonomy_observed(&self, mut observed: Job) -> Result<()> {
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = get(&tx, observed.id)?;
            // A late source reply cannot undo pause/cancel or a newer observation.
            if current.revision != observed.revision || current.state != JobState::Ready {
                return Ok(());
            }
            ensure!(
                current.action == observed.action
                    && current.scope == observed.scope
                    && current.authorized == observed.authorized
                    && current.conversation == observed.conversation
                    && current.name == observed.name
                    && current.budget == observed.budget
                    && current.expires_at == observed.expires_at
                    && current.schedule == observed.schedule
                    && current.occurrence == observed.occurrence
                    && current.pause_after_run == observed.pause_after_run
                    && current.last_receipt == observed.last_receipt
                    && matches!(observed.state, JobState::Ready | JobState::NeedsYou)
                    && current
                        .trigger
                        .as_ref()
                        .zip(observed.trigger.as_ref())
                        .is_some_and(|(left, right)| left.same_source(right)),
                "observation cannot change task/source authority"
            );
            if observed.state == JobState::NeedsYou {
                let checked = observed
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.observation().last_checked)
                    .context("source failure requires an observation instant")?;
                let receipt = RunReceipt {
                    completion: None,
                    occurrence: Uuid::new_v4(),
                    scheduled_at: current.next.at,
                    finished_at: checked,
                    outcome: RunOutcome::NeedsYou,
                    detail: "Source observation requires owner review; no task execution started"
                        .into(),
                    coalesced: checked > current.next.at,
                    dst_adjusted: false,
                };
                insert_receipt(&tx, observed.id, &receipt)?;
                observed.last_receipt = Some(receipt);
            }
            save(&tx, &mut observed)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn autonomy_record_outputs(
        &self,
        name: &str,
        bound: usize,
        update: impl FnOnce(Option<&[u8]>) -> Result<Vec<u8>>,
    ) -> Result<()> {
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // No source watchers means no additional own-output tracking cost.
            if !tx.prepare("SELECT 1 FROM autonomy_jobs WHERE state IN (0,1,3,4) LIMIT 1")?.exists([])? { return Ok(()); }
            let previous=super::documents::read(&tx,name,bound)?;
            let body=update(previous.as_deref())?;
            ensure!(body.len()<=bound,"own-output attribution exceeds bound");
            tx.execute("INSERT INTO documents(name,revision,body) VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body",params![name,body])?;
            tx.commit()?;
            Ok(())
        })
    }
    pub(crate) fn autonomy_stop_impact(&self) -> Result<Option<crate::autonomy::host::StopImpact>> {
        self.document(STOP_IMPACT, 8192)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn autonomy_record_stop_clients(
        &self,
        revision: u64,
        clients: crate::autonomy::host::StopClients,
    ) -> Result<()> {
        ensure!(
            clients.count == clients.identities.len() && clients.count <= 32,
            "stop client snapshot exceeds bound"
        );
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let bytes = super::documents::read(&tx, STOP_IMPACT, 8192)?;
            if let Some(bytes) = bytes {
                let mut impact: crate::autonomy::host::StopImpact = serde_json::from_slice(&bytes)?;
                // A late host acknowledgement cannot overwrite a newer stop.
                if impact.policy_revision == revision && impact.attached_clients.is_none() {
                    impact.attached_clients = Some(clients);
                    let bytes = serde_json::to_vec(&impact)?;
                    ensure!(bytes.len() <= 8192, "stop impact exceeds bound");
                    tx.execute(
                        "UPDATE documents SET body=?2,revision=revision+1 WHERE name=?1",
                        params![STOP_IMPACT, bytes],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn autonomy_policy(&self) -> Result<HostPolicy> {
        Ok(self
            .document(POLICY, 4096)?
            .map(|b| serde_json::from_slice(&b))
            .transpose()?
            .unwrap_or_default())
    }

    pub(crate) fn autonomy_edit_policy(
        &self,
        revision: u64,
        edit: impl FnOnce(&mut HostPolicy),
    ) -> Result<HostPolicy> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let bytes = tx.query_row("SELECT body FROM documents WHERE name=?1", [POLICY], |r| blob(r,0,4096)).optional()?;
            let mut policy: HostPolicy = bytes.map(|b| serde_json::from_slice(&b)).transpose()?.unwrap_or_default();
            ensure!(policy.revision == revision, "host policy changed; refresh before editing");
            edit(&mut policy);
            ensure!(!policy.startup_enabled || policy.detached_enabled, "OS startup requires explicit detached enablement");
            policy.revision = revision.checked_add(1).context("host policy revision exhausted")?;
            if policy.stop_requested {
                let mut query = tx.prepare("SELECT id,revision,state,due,body FROM autonomy_jobs WHERE state IN (1,2) LIMIT 2")?;
                let jobs = query.query_map([], |row| stored_job(row, 0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                ensure!(jobs.len() <= 1, "running schedule count exceeds the single-job bound");
                let impact = crate::autonomy::host::StopImpact {
                    policy_revision: policy.revision,
                    requested_at: crate::autonomy::now()?,
                    active_job: jobs.into_iter().next().map(|job| crate::autonomy::host::StopJob { id: job.id, conversation: job.conversation, name: job.name }),
                    attached_clients: None,
                };
                let bytes = serde_json::to_vec(&impact)?;
                ensure!(bytes.len() <= 8192, "stop impact exceeds bound");
                tx.execute("INSERT INTO documents(name,revision,body) VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body", params![STOP_IMPACT,bytes])?;
            }
            tx.execute("INSERT INTO documents(name,revision,body) VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body", params![POLICY,serde_json::to_vec(&policy)?])?;
            tx.commit()?;
            Ok(policy)
        })
    }

    pub(crate) fn autonomy_create(&self, job: Job) -> Result<Job> {
        job.validate()?;
        ensure!(
            job.state == JobState::Ready && job.revision == 1 && job.authorized,
            "new schedule requires explicit owner authorization"
        );
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                !tx.prepare("SELECT 1 FROM documents WHERE name='restore/review-required'")?
                    .exists([])?,
                "restored unattended authority requires owner review"
            );
            let count: usize = tx.query_row(
                "SELECT count(*) FROM (SELECT 1 FROM autonomy_jobs WHERE state<5 LIMIT 1000)",
                [],
                |r| read_usize(r, 0),
            )?;
            ensure!(count < QUEUE_LIMIT, "pending schedule queue is full");
            tx.execute(
                "INSERT INTO autonomy_jobs(id,revision,state,due,body) VALUES(?1,1,0,?2,?3)",
                params![
                    job.id.to_string(),
                    job.not_before,
                    serde_json::to_vec(&job)?
                ],
            )?;
            tx.commit()?;
            Ok(job)
        })
    }

    pub(crate) fn autonomy_job(&self, id: Uuid) -> Result<Job> {
        self.with_database(|db| {
            Ok(db.connection.query_row(
                "SELECT id,revision,state,due,body FROM autonomy_jobs WHERE id=?1",
                [id.to_string()],
                |r| stored_job(r, 0),
            )?)
        })
    }

    pub(crate) fn autonomy_page(&self, after: u64) -> Result<Vec<(u64, Job)>> {
        self.with_database(|db| {
            let mut statement = db.connection.prepare("SELECT sequence,id,revision,state,due,body FROM autonomy_jobs WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
            let rows = statement.query_map(params![i64::try_from(after)?,PAGE_SIZE as i64],|r|Ok((read_u64(r,0)?,stored_job(r,1)?)))?;
            rows.map(|row|Ok(row?)).collect()
        })
    }

    pub(crate) fn autonomy_receipts(&self, id: Uuid, after: u64) -> Result<Vec<(u64, RunReceipt)>> {
        self.with_database(|db| {
            let mut statement = db.connection.prepare("SELECT sequence,body FROM autonomy_receipts WHERE job=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            let rows = statement.query_map(params![id.to_string(),i64::try_from(after)?,PAGE_SIZE as i64],|r|Ok((read_u64(r,0)?,blob(r,1,20*1024)?)))?;
            rows.map(|row| { let (sequence,bytes)=row?; let receipt: RunReceipt=serde_json::from_slice(&bytes)?; receipt.validate()?; Ok((sequence,receipt)) }).collect()
        })
    }

    pub(crate) fn autonomy_edit(
        &self,
        id: Uuid,
        revision: u64,
        edit: JobEdit,
        now: i64,
    ) -> Result<Job> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut job = get(&tx,id)?;
            ensure!(job.revision == revision, "schedule changed; refresh its revision before editing");
            match edit {
                JobEdit::Pause => match job.state {
                    JobState::Running => job.pause_after_run=true,
                    JobState::Ready | JobState::Paused => job.state=JobState::Paused,
                    _ => anyhow::bail!("only ready, paused or running work can be paused"),
                },
                JobEdit::Cancel => match job.state {
                    JobState::Running | JobState::CancelRequested => {job.state=JobState::CancelRequested;job.authorized=false;},
                    _ if job.state.active() => { job.state=JobState::Cancelled; job.authorized=false; },
                    _ => anyhow::bail!("schedule is already terminal"),
                },
                JobEdit::Resume { review_unknown } => {
                    ensure!(matches!(job.state,JobState::Paused|JobState::NeedsYou), "only paused or needs-you work can resume");
                    ensure!(job.expires_at > now && job.authorized, "schedule authority expired or revoked; create a newly reviewed task");
                    ensure!(job.last_receipt.as_ref().is_none_or(|r| r.outcome != RunOutcome::Unknown) || review_unknown, "unknown effect requires explicit --review-unknown; inspect its receipt first");
                    if job.state == JobState::NeedsYou && job.trigger.is_some() { ensure!(review_unknown, "trigger scope/effects review requires explicit --review-unknown"); }
                    if review_unknown && let Some(crate::autonomy::triggers::Trigger::Files(watch)) = job.trigger.as_mut() {
                        watch.unknown_generation = crate::autonomy::triggers::files::review_generation(&tx)?;
                        watch.candidate = None;
                    }
                    // Explicit review creates a new occurrence; the old receipt and
                    // usage charges remain immutable, including uncertain work.
                    job.state=JobState::Ready;
                    job.pause_after_run=false;
                    job.not_before=now.max(job.next.at);
                }
            }
            save(&tx,&mut job)?;
            tx.commit()?;
            Ok(job)
        })
    }

    /// Only the descriptor-lease owner calls this. One immediate transaction
    /// additionally prevents competing claims and records the attempt pre-effect.
    pub(crate) fn autonomy_claim(&self, now: i64) -> Result<Option<Job>> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(!tx.prepare("SELECT 1 FROM documents WHERE name='restore/review-required'")?.exists([])?, "restored unattended authority requires owner review");
            if tx.prepare("SELECT 1 FROM autonomy_jobs WHERE state IN (1,2) LIMIT 1")?.exists([])? { return Ok(None); }
            let job = tx.query_row("SELECT id,revision,state,due,body FROM autonomy_jobs WHERE state=0 AND due<=?1 ORDER BY due,sequence LIMIT 1",[now],|r|stored_job(r,0)).optional()?;
            let Some(mut job)=job else { return Ok(None) };
            ensure!(job.not_before<=now && job.state==JobState::Ready, "schedule index does not match its payload");
            if job.expires_at>now && job.authorized && job.trigger.as_ref().is_some_and(|t|!t.observation().pending) { return Ok(None); }
            if job.expires_at<=now || !job.authorized {
                job.state=JobState::Expired;
                let receipt=RunReceipt { completion: None, occurrence:Uuid::new_v4(),scheduled_at:job.next.at,finished_at:now,outcome:RunOutcome::Expired,detail:"Authority expired before dispatch; no work started".into(),coalesced:now>job.next.at,dst_adjusted:job.next.dst_adjusted };
                insert_receipt(&tx,job.id,&receipt)?;
                job.last_receipt=Some(receipt);
                save(&tx,&mut job)?;
                tx.commit()?;
                return Ok(None);
            }
            job.state=JobState::Running;
            job.occurrence=Some(Uuid::new_v4());
            save(&tx,&mut job)?;
            tx.commit()?;
            Ok(Some(job))
        })
    }

    pub(crate) fn autonomy_defer(&self, id: Uuid, occurrence: Uuid, until: i64) -> Result<()> {
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut job = get(&tx, id)?;
            ensure!(job.occurrence == Some(occurrence), "stale occurrence");
            job.state = if job.state == JobState::CancelRequested {
                JobState::Cancelled
            } else if job.pause_after_run {
                JobState::Paused
            } else {
                JobState::Ready
            };
            job.occurrence = None;
            job.not_before = until;
            save(&tx, &mut job)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn autonomy_finish(&self, id: Uuid, receipt: RunReceipt) -> Result<Job> {
        receipt.validate()?;
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut job = get(&tx, id)?;
            ensure!(
                job.occurrence == Some(receipt.occurrence),
                "stale occurrence completion"
            );
            ensure!(
                job.next.at == receipt.scheduled_at,
                "receipt does not identify the scheduled occurrence"
            );
            insert_receipt(&tx, id, &receipt)?;
            job.occurrence = None;
            let trigger_completed = job
                .trigger
                .as_ref()
                .is_some_and(|trigger| trigger.completed());
            if matches!(
                receipt.outcome,
                RunOutcome::Completed | RunOutcome::Cancelled | RunOutcome::Expired
            ) && let Some(trigger) = job.trigger.as_mut()
            {
                trigger.observation_mut().pending = false;
            }
            job.state = match receipt.outcome {
                RunOutcome::Unknown | RunOutcome::NeedsYou => JobState::NeedsYou,
                RunOutcome::Cancelled => JobState::Cancelled,
                RunOutcome::Expired => JobState::Expired,
                RunOutcome::Completed => {
                    if job.state == JobState::CancelRequested {
                        JobState::Cancelled
                    } else if trigger_completed {
                        JobState::Completed
                    } else if let Some(next) =
                        job.schedule.after(receipt.finished_at.max(job.next.at))?
                    {
                        job.not_before = next.at;
                        job.next = next;
                        if job.pause_after_run {
                            JobState::Paused
                        } else {
                            JobState::Ready
                        }
                    } else {
                        JobState::Completed
                    }
                }
            };
            job.last_receipt = Some(receipt);
            save(&tx, &mut job)?;
            tx.commit()?;
            Ok(job)
        })
    }

    /// Restart never executes an interrupted occurrence. This is called only
    /// after acquiring the same-home OS host lease, not by observers or startup.
    pub(crate) fn autonomy_recover(&self, now: i64) -> Result<()> {
        let jobs = self.with_database(|db| {
            let mut statement = db.connection.prepare(
                "SELECT id,revision,state,due,body FROM autonomy_jobs WHERE state IN (1,2) LIMIT 2",
            )?;
            let rows = statement.query_map([], |r| stored_job(r, 0))?;
            rows.map(|row| Ok(row?)).collect::<Result<Vec<_>>>()
        })?;
        ensure!(
            jobs.len() <= 1,
            "running schedule count exceeds the single-job bound"
        );
        for job in jobs {
            self.autonomy_finish(job.id,RunReceipt { completion: None,
                occurrence:job.occurrence.context("missing running occurrence")?,
                scheduled_at:job.next.at,finished_at:now,outcome:RunOutcome::Unknown,
                detail:"Previous host exited without a terminal receipt. Work may have occurred; review before a new attempt".into(),
                coalesced:now>job.next.at,dst_adjusted:job.next.dst_adjusted,
            })?;
        }
        Ok(())
    }
}

fn get(tx: &Transaction<'_>, id: Uuid) -> Result<Job> {
    Ok(tx.query_row(
        "SELECT id,revision,state,due,body FROM autonomy_jobs WHERE id=?1",
        [id.to_string()],
        |r| stored_job(r, 0),
    )?)
}
fn save(tx: &Transaction<'_>, job: &mut Job) -> Result<()> {
    let previous = job.revision;
    job.revision = previous
        .checked_add(1)
        .context("schedule revision exhausted")?;
    job.validate()?;
    ensure!(tx.execute("UPDATE autonomy_jobs SET revision=?1,state=?2,due=?3,body=?4 WHERE id=?5 AND revision=?6",params![i64::try_from(job.revision)?,job.state.code(),job.not_before,serde_json::to_vec(job)?,job.id.to_string(),i64::try_from(previous)?])?==1,"schedule changed concurrently");
    Ok(())
}
fn insert_receipt(tx: &Transaction<'_>, id: Uuid, receipt: &RunReceipt) -> Result<()> {
    receipt.validate()?;
    tx.execute(
        "INSERT INTO autonomy_receipts(job,occurrence,body) VALUES(?1,?2,?3)",
        params![
            id.to_string(),
            receipt.occurrence.to_string(),
            serde_json::to_vec(receipt)?
        ],
    )?;
    Ok(())
}
fn decode(bytes: &[u8]) -> Result<Job> {
    let job: Job = serde_json::from_slice(bytes)?;
    job.validate()?;
    Ok(job)
}
fn stored_job(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Job> {
    let job =
        decode(&blob(row, offset + 4, JOB_BYTES)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let identity = match row.get_ref(offset)? {
        rusqlite::types::ValueRef::Text(bytes) => bytes,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    if identity != job.id.to_string().as_bytes()
        || read_u64(row, offset + 1)? != job.revision
        || row.get::<_, i64>(offset + 2)? != job.state.code()
        || row.get::<_, i64>(offset + 3)? != job.not_before
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(job)
}
fn blob(row: &rusqlite::Row<'_>, index: usize, maximum: usize) -> rusqlite::Result<Vec<u8>> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Blob(bytes) if bytes.len() <= maximum => Ok(bytes.to_vec()),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_job_payloads_fail_at_the_sqlite_borrow_boundary() {
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &crate::storage::RecoveryIdentity::generate(),
            &crate::storage::TestCustody::default(),
        )
        .unwrap();
        let id = Uuid::new_v4();
        store.with_database(|db| {db.connection.execute("INSERT INTO autonomy_jobs(id,revision,state,due,body) VALUES(?1,1,0,1,zeroblob(?2))",params![id.to_string(),JOB_BYTES as i64+1])?;Ok(())}).unwrap();
        assert!(store.autonomy_job(id).is_err());
        assert!(store.autonomy_page(0).is_err());
        assert!(store.autonomy_claim(2).is_err());
    }
}
