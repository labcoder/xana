//! Durable bounded incremental queue and atomic learning admission.
use super::ProtectedStore;
use crate::memory::{learning::*, *};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

pub(super) const SCHEMA:&str="
CREATE TABLE learning_queue(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,conversation TEXT NOT NULL,generation INTEGER NOT NULL,body BLOB NOT NULL);
CREATE TABLE learning_sources(id TEXT PRIMARY KEY,conversation TEXT NOT NULL,hash TEXT NOT NULL,state TEXT NOT NULL);
CREATE INDEX learning_sources_state ON learning_sources(state);
CREATE INDEX memory_candidate_count ON memory_entries(CASE WHEN json_valid(CAST(body AS TEXT)) THEN json_extract(CAST(body AS TEXT),'$.state') ELSE 'invalid' END);
";
impl ProtectedStore {
    pub(crate) fn retire_stale_learning(&self) -> Result<()> {
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut q =
                tx.prepare("SELECT id,body FROM learning_queue ORDER BY sequence LIMIT 64")?;
            let values = q
                .query_map([], |r| {
                    let body = r.get_ref(1)?.as_blob()?;
                    if body.len() > 16 * 1024 {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    Ok((r.get::<_, String>(0)?, body.to_vec()))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(q);
            let mut retired = 0;
            for (id, body) in values {
                ensure!(body.len() <= 16 * 1024, "learning source exceeds bound");
                let source: LearningSource = serde_json::from_slice(&body)?;
                if !source_current(&tx, &source)? {
                    tx.execute("DELETE FROM learning_queue WHERE id=?1", [&id])?;
                    ensure!(tx.execute(
                        "UPDATE learning_sources SET state='excluded_after_change' WHERE id=?1",
                        [&id],
                    )? == 1, "learning source identity disappeared during retirement");
                    retired += 1;
                }
            }
            if retired > 0 {
                let receipt = LearningRetirement {
                    at_unix_seconds: crate::memory::now()?,
                    sources: retired,
                    reason: "Source, scope, consent or restore eligibility changed. These queued sources were excluded without automatic rebasing; original Conversation history is unchanged.".into(),
                };
                let bytes = serde_json::to_vec(&receipt)?;
                ensure!(bytes.len() <= 4096, "learning retirement receipt exceeds bound");
                tx.execute("INSERT INTO documents(name,revision,body) VALUES('memory/learning-retirement',1,?1) ON CONFLICT(name) DO UPDATE SET revision=documents.revision+1,body=excluded.body", [bytes])?;
            }
            tx.commit()?;
            Ok(())
        })
    }
    pub(crate) fn enqueue_learning(&self, source: &LearningSource) -> Result<bool> {
        let bytes = serde_json::to_vec(source)?;
        ensure!(
            bytes.len() <= 16 * 1024 && source.text.len() <= SOURCE_BYTES,
            "learning source exceeds bound"
        );
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if tx.query_row("SELECT 1 FROM learning_sources WHERE id=?1",[source.id.to_string()],|_|Ok(())).optional()?.is_some() {return Ok(false);}
            if !source_current(&tx,source)? {return Ok(false);}
            let count=tx.query_row("SELECT COUNT(*) FROM learning_queue",[],|r|super::database::read_u64(r,0))?;
            ensure!(count<QUEUE_LIMIT as u64,"Learning queue is full (1,000 sources); processing is paused visibly, and the original Conversation is retained");
            tx.execute("INSERT INTO learning_sources VALUES(?1,?2,?3,'pending')",params![source.id.to_string(),source.context.conversation.unwrap().to_string(),source.hash])?;
            tx.execute("INSERT INTO learning_queue(id,conversation,generation,body) VALUES(?1,?2,?3,?4)",params![source.id.to_string(),source.context.conversation.unwrap().to_string(),i64::try_from(source.generation)?,bytes])?;
            tx.commit()?;Ok(true)
        })
    }
    pub(crate) fn learning_batch(&self) -> Result<Vec<LearningSource>> {
        self.with_database(|db| {
            // The bounded retirement pass may leave more stale rows behind.
            // Read sources and current authority from one snapshot, and never
            // return excluded data merely because it fell beyond that page.
            let tx = db.connection.transaction()?;
            let mut q = tx.prepare("SELECT body FROM learning_queue ORDER BY sequence LIMIT ?1")?;
            let mut rows = q.query([BATCH_LIMIT as i64])?;
            let mut sources = Vec::new();
            let mut bytes = 0;
            while let Some(row) = rows.next()? {
                let body = row.get_ref(0)?.as_blob()?;
                ensure!(
                    body.len() <= 16 * 1024,
                    "learning source record exceeds bound"
                );
                let source: LearningSource = serde_json::from_slice(body)?;
                ensure!(
                    source.text.len() <= SOURCE_BYTES
                        && source.hash == blake3::hash(source.text.as_bytes()).to_hex().to_string(),
                    "learning source digest mismatch"
                );
                if !source_current(&tx, &source)? {
                    continue;
                }
                if bytes + source.text.len() > 12 * 1024 {
                    break;
                }
                bytes += source.text.len();
                sources.push(source);
            }
            Ok(sources)
        })
    }
    pub(crate) fn commit_learning(
        &self,
        sources: &[LearningSource],
        suggestions: &[Suggestion],
        route: &LearningRoute,
    ) -> Result<usize> {
        ensure!(
            sources.len() <= BATCH_LIMIT && suggestions.len() <= 16,
            "learning batch exceeds bound"
        );
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let actual:Option<Vec<u8>>=tx.query_row("SELECT CASE WHEN length(body)<=4096 THEN body END FROM documents WHERE name='memory/learning-route'",[],|r|r.get(0)).optional()?.flatten();
            ensure!(actual.as_deref().map(serde_json::from_slice::<LearningRoute>).transpose()?.as_ref()==Some(route),"learning route changed before commit");
            for source in sources {
                ensure!(source_current(&tx,source)?,"learning source or controls changed; stale helper output was not committed");
                let stored:String=tx.query_row("SELECT hash FROM learning_sources WHERE id=?1 AND state='pending'",[source.id.to_string()],|r|r.get(0))?;
                ensure!(stored==source.hash,"learning evidence changed");
            }
            let mut count=0;
            let mut seen=std::collections::HashSet::new();
            for suggestion in suggestions {
                let source=sources.iter().find(|s|s.id==suggestion.source).ok_or_else(||anyhow::anyhow!("helper invented a source identity"))?;
                // Do not copy sensitive information into a new personal-memory
                // record without fresh owner consent; original history is separate.
                if !seen.insert((source.id,suggestion.quote.as_str())) {continue;}
                ensure!(!suggestion.quote.trim().is_empty() && suggestion.quote.len()<=512 && source.text.contains(&suggestion.quote),"learning suggestion has no bounded exact owner quote");
                // Conflicting duplicate classifications cannot gain authority
                // by appearing first. The bounded batch uses the stricter claim.
                let mut combined = suggestion.clone();
                for other in suggestions.iter().filter(|other| other.source==source.id && other.quote==suggestion.quote) {
                    combined.sensitive |= other.sensitive;
                    if other.claim == crate::memory::MemoryClaim::Inferred {
                        combined.claim = crate::memory::MemoryClaim::Inferred;
                    }
                    if other.preference != suggestion.preference {
                        combined.preference = None;
                    }
                }
                let suggestion = &combined;
                if suggestion.sensitive {
                    super::candidates::learned(&tx,source,suggestion,route,None)?;
                    continue;
                }
                let record=record_for(source,suggestion)?;
                if super::forgetting::statement_suppressed(&tx,&record.statement)? || super::forgetting::statement_suppressed(&tx,&suggestion.quote)? {continue;}
                let body=serde_json::to_vec(&record)?;ensure!(body.len()<=RECORD_BYTES,"learned record exceeds bound");
                tx.execute("INSERT INTO memory_entries(id,revision,scope,body) VALUES(?1,1,?2,?3)",params![record.id.to_string(),record.scope.to_string(),body])?;
                super::candidates::learned(&tx,source,suggestion,route,Some(&record))?;
                count+=1;
            }
            for source in sources {
                tx.execute("DELETE FROM learning_queue WHERE id=?1",[source.id.to_string()])?;
                tx.execute("UPDATE learning_sources SET state='processed' WHERE id=?1",[source.id.to_string()])?;
            }
            tx.commit()?;Ok(count)
        })
    }
    pub(crate) fn learning_status(&self) -> Result<LearningStatus> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let pending = tx.query_row("SELECT COUNT(*) FROM learning_queue", [], |r| {
                super::database::read_u64(r, 0)
            })?;
            let candidates = tx.query_row(
                "SELECT COUNT(*) FROM learning_candidates WHERE state IN ('staged','stale')",
                [],
                |r| super::database::read_u64(r, 0),
            )?;
            let excluded_after_change = tx.query_row(
                "SELECT COUNT(*) FROM learning_sources WHERE state='excluded_after_change'",
                [],
                |r| super::database::read_u64(r, 0),
            )?;
            Ok(LearningStatus {
                pending,
                candidates,
                excluded_after_change,
                last_retirement: super::documents::read(&tx, "memory/learning-retirement", 4096)?
                    .map(|b| serde_json::from_slice(&b))
                    .transpose()?,
                route: super::documents::read(&tx, "memory/learning-route", 4096)?
                    .map(|b| serde_json::from_slice(&b))
                    .transpose()?,
                last_receipt: super::documents::read(&tx, "memory/learning-receipt", 4096)?
                    .map(|b| serde_json::from_slice(&b))
                    .transpose()?,
                disclosure: DISCLOSURE,
            })
        })
    }
}
fn source_current(db: &rusqlite::Connection, source: &LearningSource) -> Result<bool> {
    // Explicit foreground handling wins over queued background interpretation.
    // Rechecked in the committing transaction, so an in-flight helper cannot
    // publish duplicate or differently scoped facts from the same owner input.
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM documents WHERE name=?1)",
        [format!("memory/explicit-source/{}", source.id)],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(false);
    }
    let generation = db.query_row(
        "SELECT revision FROM privacy_generation WHERE singleton=1",
        [],
        |r| super::database::read_u64(r, 0),
    )?;
    let Some(conversation) = source.context.conversation else {
        return Ok(false);
    };
    if generation != source.generation || !super::forgetting::source_allowed(db, conversation)? {
        return Ok(false);
    }
    if super::forgetting::memory_review_required(db)? {
        return Ok(false);
    }
    for scope in source.context.scopes() {
        let flags = super::memory::controls(db, scope)?;
        if flags.no_memory || !flags.learning_enabled {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
