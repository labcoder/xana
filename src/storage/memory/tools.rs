//! Atomic foreground memory effects and retry receipts inside protected storage.

mod lookup;

use super::{controls, encode, get, indexed_record};
use crate::{
    memory::{
        MemoryClaim, MemoryProvenance, MemoryRecord, MemoryState, now,
        tools::types::{CommitGuard, UpdateAction, UpdatePlan, UpdateReceipt},
    },
    storage::{ProtectedStore, candidates, database, documents, forgetting},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_RECEIPTS: usize = 16;
const RECEIPT_BYTES: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptLedger {
    version: u16,
    source_id: Uuid,
    conversation: Uuid,
    source_digest: String,
    receipts: Vec<IntentReceipt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentReceipt {
    digest: String,
    receipt: UpdateReceipt,
}

impl ProtectedStore {
    pub(crate) fn memory_tool_update(&self, plan: &UpdatePlan) -> Result<UpdateReceipt> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let guard = &plan.guard;
            let conversation = guard.context.conversation.context("Missing foreground Conversation")?;
            ensure!(guard.context.scopes().contains(&plan.intent.scope), "Memory scope is no longer current");
            let key = format!("memory/tools/{}", guard.operation_id);
            let mut ledger = read_ledger(&tx, &key, guard, conversation)?;
            let digest = blake3::hash(&serde_json::to_vec(&plan.intent)?).to_hex().to_string();
            if let Some(prior) = ledger.receipts.iter().find(|prior| prior.digest == digest) {
                // A retry discloses only its own metadata receipt. It does not
                // restore text, reapply an effect or bypass new privacy controls.
                return Ok(prior.receipt.clone());
            }
            ensure!(ledger.receipts.len() < MAX_RECEIPTS, "This owner turn reached its memory-update limit");
            check_guard(&tx, guard, false)?;
            let time = now()?;
            let origin = MemoryProvenance {
                owner_request: guard.source_id,
                conversation: Some(conversation),
                at_unix_seconds: time,
            };
            let record = match plan.intent.action {
                UpdateAction::Remember => {
                    let statement = plan.intent.statement.clone().context("Missing memory statement")?;
                    ensure!(!forgetting::statement_suppressed(&tx, &statement)?, "This fact was forgotten; use explicit owner restore controls before saving it again");
                    let record = MemoryRecord {
                        version: 1,
                        id: Uuid::new_v4(),
                        revision: 1,
                        scope: plan.intent.scope.clone(),
                        statement,
                        claim: MemoryClaim::Stated,
                        state: MemoryState::Active,
                        created: origin.clone(),
                        changed: origin,
                        valid_until_unix_seconds: None,
                    };
                    tx.execute(
                        "INSERT INTO memory_entries(id,revision,scope,body) VALUES(?1,1,?2,?3)",
                        params![record.id.to_string(), record.scope.to_string(), encode(&record)?],
                    )?;
                    record
                }
                UpdateAction::Correct | UpdateAction::Forget => revise(&tx, plan, origin)?,
            };
            if plan.intent.action != UpdateAction::Forget {
                let marker = serde_json::to_vec(&serde_json::json!({
                    "version":1,"operation_id":guard.operation_id,"source_id":guard.source_id,
                }))?;
                tx.execute(
                    "INSERT OR IGNORE INTO documents(name,revision,body) VALUES(?1,1,?2)",
                    params![format!("memory/explicit-source/{}", guard.source_id), marker],
                )?;
            }
            forgetting::advance_generation(&tx)?;
            let receipt = UpdateReceipt {
                version: 1,
                committed: true,
                action: plan.intent.action,
                id: record.id,
                revision: record.revision,
                scope: record.scope,
                state: record.state,
                source_id: guard.source_id,
            };
            ledger.receipts.push(IntentReceipt { digest, receipt: receipt.clone() });
            let bytes = serde_json::to_vec(&ledger)?;
            ensure!(bytes.len() <= RECEIPT_BYTES, "Memory receipt ledger exceeds its bound");
            tx.execute(
                "INSERT INTO documents(name,revision,body) VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=documents.revision+1,body=excluded.body",
                params![key, bytes],
            )?;
            ensure!(!guard.cancellation.is_cancelled(), "Memory request was cancelled before commit; nothing was changed");
            tx.commit()?;
            Ok(receipt)
        })
    }
}

fn read_ledger(
    tx: &Transaction<'_>,
    key: &str,
    guard: &CommitGuard,
    conversation: Uuid,
) -> Result<ReceiptLedger> {
    let Some(bytes) = documents::read(tx, key, RECEIPT_BYTES)? else {
        return Ok(ReceiptLedger {
            version: 1,
            source_id: guard.source_id,
            conversation,
            source_digest: guard.source_digest.clone(),
            receipts: Vec::new(),
        });
    };
    let ledger: ReceiptLedger = serde_json::from_slice(&bytes)?;
    ensure!(
        ledger.version == 1
            && ledger.source_id == guard.source_id
            && ledger.conversation == conversation
            && ledger.source_digest == guard.source_digest,
        "Memory operation belongs to a different owner source"
    );
    ensure!(
        ledger.receipts.len() <= MAX_RECEIPTS,
        "Memory receipt ledger exceeds its entry bound"
    );
    for prior in &ledger.receipts {
        ensure!(
            prior.digest.len() == 64 && prior.digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid memory receipt digest"
        );
        let receipt = &prior.receipt;
        ensure!(
            receipt.version == 1
                && receipt.committed
                && receipt.source_id == guard.source_id
                && !receipt.id.is_nil()
                && receipt.revision > 0
                && receipt.revision < i64::MAX as u64,
            "Invalid memory receipt metadata"
        );
        crate::memory::validate_scope(&receipt.scope)?;
    }
    Ok(ledger)
}

fn check_guard(db: &Connection, guard: &CommitGuard, reading: bool) -> Result<()> {
    ensure!(
        !guard.cancellation.is_cancelled(),
        "Memory request was cancelled; nothing was changed"
    );
    let generation = db.query_row(
        "SELECT revision FROM privacy_generation WHERE singleton=1",
        [],
        |row| database::read_u64(row, 0),
    )?;
    ensure!(
        generation == guard.generation,
        "Memory privacy changed after planning; inspect current memory and retry"
    );
    let conversation = guard
        .context
        .conversation
        .context("Missing foreground Conversation")?;
    ensure!(
        !forgetting::memory_review_required(db)?,
        "Restored memory needs explicit owner review before use or updates"
    );
    ensure!(
        forgetting::source_allowed(db, conversation)?,
        "This Conversation contains an excluded memory source; start a fresh Conversation"
    );
    for scope in guard.context.scopes() {
        let flags = controls(db, scope)?;
        ensure!(
            !flags.no_memory,
            "This is a no-memory Conversation; use explicit owner controls to change that setting"
        );
        ensure!(
            !reading || flags.use_enabled,
            "Memory use is disabled; explicit owner inspection remains separate"
        );
    }
    Ok(())
}

fn visible(db: &Connection, record: &MemoryRecord) -> Result<bool> {
    if !candidates::memory_visible(db, record)? {
        return Ok(false);
    }
    for source in [record.created.conversation, record.changed.conversation]
        .into_iter()
        .flatten()
    {
        if !forgetting::source_allowed(db, source)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn revise(
    tx: &Transaction<'_>,
    plan: &UpdatePlan,
    mut origin: MemoryProvenance,
) -> Result<MemoryRecord> {
    let intent = &plan.intent;
    let id = intent.id.context("Missing memory id")?;
    let old = get(tx, id)?;
    ensure!(
        Some(old.revision) == intent.revision,
        "Memory changed since inspection; refresh before applying this edit"
    );
    ensure!(
        old.scope == intent.scope && plan.guard.context.scopes().contains(&old.scope),
        "Memory scope changed since inspection"
    );
    ensure!(
        old.state != MemoryState::Forgotten,
        "Forgotten memory requires explicit owner restore controls"
    );
    ensure!(
        visible(tx, &old)?,
        "Memory source is excluded; this tool cannot reuse it"
    );
    let mut record = old.clone();
    record.revision = old
        .revision
        .checked_add(1)
        .context("Memory revision exhausted")?;
    origin.at_unix_seconds = origin.at_unix_seconds.max(old.changed.at_unix_seconds);
    record.changed = origin;
    match intent.action {
        UpdateAction::Correct => {
            let statement = intent
                .statement
                .clone()
                .context("Missing correction statement")?;
            ensure!(
                !forgetting::statement_suppressed(tx, &statement)?,
                "Correction would restore a forgotten fact; use explicit owner restore controls"
            );
            record.statement = statement;
            record.state = MemoryState::Active;
            record.claim = MemoryClaim::Stated;
        }
        UpdateAction::Forget => {
            forgetting::suppress(tx, &old, &record.changed)?;
            record.state = MemoryState::Forgotten;
        }
        UpdateAction::Remember => anyhow::bail!("Remember is not a revision"),
    }
    let encoded = encode(&record)?;
    let mut historical = old;
    historical.state = MemoryState::Superseded;
    tx.execute(
        "INSERT INTO memory_revisions(id,revision,body) VALUES(?1,?2,?3)",
        params![
            id.to_string(),
            i64::try_from(historical.revision)?,
            encode(&historical)?
        ],
    )?;
    ensure!(
        tx.execute(
            "UPDATE memory_entries SET revision=?2,body=?3 WHERE id=?1 AND revision=?4",
            params![
                id.to_string(),
                i64::try_from(record.revision)?,
                encoded,
                i64::try_from(historical.revision)?
            ],
        )? == 1,
        "Memory revision conflict"
    );
    Ok(record)
}
