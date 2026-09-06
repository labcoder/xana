//! Atomic shared admission and immutable settlement inside the protected store.

use super::{ProtectedStore, database::read_u64};
use crate::usage_budget::{Admission, BudgetPolicy, Receipt, UsageRecord, WorkClass};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

pub(super) const SCHEMA: &str = "
CREATE TABLE usage_requests(sequence INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
 operation TEXT NOT NULL, root TEXT NOT NULL, job TEXT NOT NULL, day INTEGER NOT NULL,
 background INTEGER NOT NULL, charge INTEGER NOT NULL, admission BLOB NOT NULL, receipt BLOB);
CREATE INDEX usage_day ON usage_requests(day,background);
CREATE INDEX usage_root ON usage_requests(root);
CREATE INDEX usage_job ON usage_requests(job);
CREATE INDEX usage_operation ON usage_requests(operation);
CREATE TABLE usage_counters(scope TEXT NOT NULL, key TEXT NOT NULL,
 requests INTEGER NOT NULL CHECK(requests>=0), tokens INTEGER NOT NULL CHECK(tokens>=0),
 PRIMARY KEY(scope,key));";
const POLICY: &str = "usage/policy";
const RECORD_BYTES: usize = 16 * 1024;

impl ProtectedStore {
    /// Read the same indexed counters and policy as admission in one snapshot;
    /// never hydrate request history merely to describe the remaining budget.
    pub(crate) fn usage_remaining(
        &self,
        root: &str,
        job: &str,
        class: WorkClass,
        now_day: u64,
    ) -> Result<crate::usage_budget::RemainingAllowance> {
        ensure!(
            !root.is_empty() && root.len() <= 128 && !job.is_empty() && job.len() <= 128,
            "invalid usage identity"
        );
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let bytes = tx
                .query_row("SELECT body FROM documents WHERE name=?1", [POLICY], |r| {
                    bounded_blob(r, 0, 4096)?.ok_or(rusqlite::Error::InvalidQuery)
                })
                .optional()?;
            let policy: BudgetPolicy = bytes
                .map(|b| serde_json::from_slice(&b))
                .transpose()?
                .unwrap_or_default();
            policy.validate()?;
            let last_day =
                tx.query_row("SELECT coalesce(max(day),0) FROM usage_requests", [], |r| {
                    read_u64(r, 0)
                })?;
            let day = now_day.max(last_day).to_string();
            let (day_requests, day_tokens) = counter(&tx, "day", &day)?;
            let (root_requests, root_tokens) = counter(&tx, "root", root)?;
            let request_cap = if class == WorkClass::Background {
                policy
                    .daily_requests
                    .saturating_sub(policy.foreground_request_reserve)
            } else {
                policy.daily_requests
            };
            let requests = request_cap
                .saturating_sub(day_requests)
                .min(policy.root_requests.saturating_sub(root_requests));
            let mut tokens = [
                policy
                    .daily_tokens
                    .map(|cap| cap.saturating_sub(day_tokens)),
                policy
                    .root_tokens
                    .map(|cap| cap.saturating_sub(root_tokens)),
            ]
            .into_iter()
            .flatten()
            .min();
            let mut exceeded = day_requests > request_cap
                || root_requests > policy.root_requests
                || policy.daily_tokens.is_some_and(|cap| day_tokens > cap)
                || policy.root_tokens.is_some_and(|cap| root_tokens > cap);
            if class == WorkClass::Background {
                let (_, background_tokens) = counter(&tx, "background_day", &day)?;
                let (_, job_tokens) = counter(&tx, "job", job)?;
                exceeded |= background_tokens > policy.background_daily_tokens.min(32_768)
                    || job_tokens
                        > policy
                            .background_job_tokens
                            .min(crate::autonomy::JOB_TOKENS);
                tokens = [
                    tokens,
                    Some(
                        policy
                            .background_daily_tokens
                            .min(32_768)
                            .saturating_sub(background_tokens),
                    ),
                    Some(
                        policy
                            .background_job_tokens
                            .min(crate::autonomy::JOB_TOKENS)
                            .saturating_sub(job_tokens),
                    ),
                ]
                .into_iter()
                .flatten()
                .min();
            }
            Ok(crate::usage_budget::RemainingAllowance {
                requests,
                tokens,
                exceeded,
            })
        })
    }

    pub(crate) fn usage_attribution(&self, operation: &str) -> Result<Option<Admission>> {
        self.with_database(|db| {
            let bytes = db.connection.query_row("SELECT admission FROM usage_requests WHERE operation=?1 ORDER BY sequence LIMIT 1", [operation], |r| required_blob(r, 0)).optional()?;
            bytes.map(|b| serde_json::from_slice(&b).map_err(Into::into)).transpose()
        })
    }
    pub(crate) fn usage_policy(&self) -> Result<BudgetPolicy> {
        let policy = self
            .document(POLICY, 4096)?
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()?
            .unwrap_or_default();
        BudgetPolicy::validate(&policy)?;
        Ok(policy)
    }

    #[cfg(test)]
    pub(crate) fn set_usage_policy(&self, policy: &BudgetPolicy) -> Result<()> {
        policy.validate()?;
        self.set_document(POLICY, &serde_json::to_vec(policy)?, 4096)
    }

    /// Apply one owner's field edits against the latest policy under the same
    /// writer transaction used by admission. Unrelated concurrent edits survive.
    pub(crate) fn update_usage_policy(
        &self,
        edit: impl FnOnce(&mut BudgetPolicy),
    ) -> Result<BudgetPolicy> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let bytes = tx.query_row("SELECT body FROM documents WHERE name=?1", [POLICY], |r| bounded_blob(r,0,4096)?.ok_or(rusqlite::Error::InvalidQuery)).optional()?;
            let mut policy: BudgetPolicy = bytes.map(|b| serde_json::from_slice(&b)).transpose()?.unwrap_or_default();
            policy.validate()?;
            let previous = policy.clone();
            edit(&mut policy);
            policy.validate()?;
            if policy != previous {
                tx.execute("INSERT INTO documents(name,revision,body) VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body", params![POLICY,serde_json::to_vec(&policy)?])?;
            }
            tx.commit()?;
            Ok(policy)
        })
    }

    pub(crate) fn reserve_usage(&self, admission: &Admission, now_day: u64) -> Result<()> {
        admission.validate()?;
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(!tx.prepare("SELECT 1 FROM documents WHERE name='usage/restore-review-required'")?.exists([])?, "restored usage may omit later charges; inspect xana usage ledger, then explicitly review xana budget --accept-restored-usage before dispatch");
            // Read policy in the admission transaction, not before waiting for its lock.
            let bytes = tx.query_row("SELECT body FROM documents WHERE name=?1", [POLICY], |r| bounded_blob(r, 0, 4096)?.ok_or(rusqlite::Error::InvalidQuery)).optional()?;
            let policy: BudgetPolicy = bytes.map(|b| serde_json::from_slice(&b)).transpose()?.unwrap_or_default();
            policy.validate()?;
            let last_day = tx.query_row("SELECT coalesce(max(day),0) FROM usage_requests", [], |r| read_u64(r,0))?;
            let day = now_day.max(last_day);
            let (day_calls, day_tokens) = counter(&tx, "day", &day.to_string())?;
            let (_, background_tokens) = counter(&tx, "background_day", &day.to_string())?;
            let (root_calls, root_tokens) = counter(&tx, "root", &admission.root)?;
            ensure!(day_calls < policy.daily_requests && root_calls < policy.root_requests, "usage request allowance exhausted; no request dispatched");
            let fits = |used: u64, cap: Option<u64>| cap.is_none_or(|cap| used.checked_add(admission.reserved_tokens).is_some_and(|next| next <= cap));
            ensure!(fits(day_tokens, policy.daily_tokens) && fits(root_tokens, policy.root_tokens), "usage token allowance exhausted; no request dispatched");
            if admission.class == WorkClass::Background {
                ensure!(day_calls < policy.daily_requests - policy.foreground_request_reserve, "foreground request headroom is reserved");
                let (_, job_tokens) = counter(&tx, "job", &admission.job)?;
                ensure!(fits(background_tokens, Some(policy.background_daily_tokens.min(32_768))) && fits(job_tokens, Some(policy.background_job_tokens.min(crate::autonomy::JOB_TOKENS))), "background usage allowance exhausted; work remains pending");
                ensure!(!tx.prepare("SELECT 1 FROM documents WHERE name='restore/review-required'")?.exists([])?, "restored background authority requires review");
            }
            tx.execute("INSERT INTO usage_requests(id,operation,root,job,day,background,charge,admission) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![admission.id,admission.operation,admission.root,admission.job,i64::try_from(day)?,admission.class == WorkClass::Background,i64::try_from(admission.reserved_tokens)?,serde_json::to_vec(admission)?])?;
            update_counters(&tx, admission, day, 1, i64::try_from(admission.reserved_tokens)?)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn settle_usage(&self, id: &str, receipt: &Receipt) -> Result<()> {
        if let Some(observation) = &receipt.cumulative {
            ensure!(
                observation.counter.len() <= 4096
                    && !observation.counter.chars().any(char::is_control)
                    && observation.total_tokens <= 1_000_000_000_000,
                "invalid cumulative usage observation"
            );
            ensure!(
                receipt.total_tokens.is_none(),
                "cumulative and per-request usage cannot be conflated"
            );
        }
        for value in [receipt.total_tokens, receipt.reported_cost_microunits]
            .into_iter()
            .flatten()
        {
            ensure!(
                value <= 1_000_000_000_000,
                "reported usage exceeds accounting bound"
            );
        }
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (charge, existing): (u64, Option<Vec<u8>>) = tx.query_row(
                "SELECT charge,receipt FROM usage_requests WHERE id=?1",
                [id],
                |r| Ok((read_u64(r, 0)?, bounded_blob(r, 1, RECORD_BYTES)?)),
            )?;
            if let Some(existing) = existing {
                ensure!(
                    serde_json::from_slice::<Receipt>(&existing)? == *receipt,
                    "usage receipt already settled differently"
                );
            } else {
                let (admission, day): (Vec<u8>, u64) = tx.query_row(
                    "SELECT admission,day FROM usage_requests WHERE id=?1",
                    [id],
                    |r| Ok((required_blob(r, 0)?, read_u64(r, 1)?)),
                )?;
                let admission: Admission = serde_json::from_slice(&admission)?;
                let delta =
                    i64::try_from(receipt.total_tokens.unwrap_or(charge))? - i64::try_from(charge)?;
                update_counters(&tx, &admission, day, 0, delta)?;
                tx.execute(
                    "UPDATE usage_requests SET charge=?2,receipt=?3 WHERE id=?1",
                    params![
                        id,
                        i64::try_from(receipt.total_tokens.unwrap_or(charge))?,
                        serde_json::to_vec(receipt)?
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn usage_page(
        &self,
        root: Option<&str>,
        job: Option<&str>,
        after: Option<u64>,
    ) -> Result<Vec<UsageRecord>> {
        self.with_database(|db| {
            let mut statement = db.connection.prepare("SELECT sequence,day,admission,charge,receipt FROM usage_requests WHERE sequence>?1 AND (?2 IS NULL OR root=?2) AND (?3 IS NULL OR job=?3) ORDER BY sequence LIMIT 128")?;
            let rows = statement.query_map(params![i64::try_from(after.unwrap_or(0))?,root,job], |r| Ok((read_u64(r,0)?,read_u64(r,1)?,required_blob(r,2)?,read_u64(r,3)?,bounded_blob(r,4,RECORD_BYTES)?)))?;
            rows.map(|row| { let (sequence,day,admission,charged_tokens,receipt) = row?;
                Ok(UsageRecord { sequence,day,admission: serde_json::from_slice(&admission)?,charged_tokens,receipt: receipt.map(|b| serde_json::from_slice(&b)).transpose()? }) }).collect()
        })
    }
}

// Inspect SQLite's borrowed value before allocating a Rust-owned copy. A corrupt
// oversized receipt must fail closed, never turn into an "unsettled" null.
fn bounded_blob(
    row: &rusqlite::Row<'_>,
    column: usize,
    limit: usize,
) -> rusqlite::Result<Option<Vec<u8>>> {
    use rusqlite::types::ValueRef;
    match row.get_ref(column)? {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(bytes) if bytes.len() <= limit => Ok(Some(bytes.to_vec())),
        value => Err(rusqlite::Error::FromSqlConversionFailure(
            column,
            value.data_type(),
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "usage record exceeds its read bound or has an invalid type",
            )
            .into(),
        )),
    }
}

fn required_blob(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Vec<u8>> {
    bounded_blob(row, column, RECORD_BYTES)?.ok_or(rusqlite::Error::InvalidQuery)
}

fn counter(tx: &rusqlite::Transaction<'_>, scope: &str, key: &str) -> Result<(u64, u64)> {
    Ok(tx
        .query_row(
            "SELECT requests,tokens FROM usage_counters WHERE scope=?1 AND key=?2",
            params![scope, key],
            |r| Ok((read_u64(r, 0)?, read_u64(r, 1)?)),
        )
        .optional()?
        .unwrap_or_default())
}

fn update_counters(
    tx: &rusqlite::Transaction<'_>,
    admission: &Admission,
    day: u64,
    requests: i64,
    tokens: i64,
) -> Result<()> {
    let day = day.to_string();
    for (scope, key) in [
        ("day", day.as_str()),
        ("root", &admission.root),
        ("job", &admission.job),
        ("background_day", day.as_str()),
    ] {
        if scope == "background_day" && admission.class != WorkClass::Background {
            continue;
        }
        // Separate INSERT/UPDATE allows negative settlement deltas without an
        // invalid intermediate row, and both remain in the receipt transaction.
        tx.execute("INSERT INTO usage_counters(scope,key,requests,tokens) VALUES(?1,?2,0,0) ON CONFLICT DO NOTHING", params![scope,key])?;
        tx.execute("UPDATE usage_counters SET requests=requests+?3,tokens=tokens+?4 WHERE scope=?1 AND key=?2", params![scope,key,requests,tokens])?;
    }
    Ok(())
}
