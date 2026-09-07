//! Indexed encrypted learning envelopes; publication never leaves this owner.
//! Payload reads and rollback recheck source exclusions, including after restore.

mod evidence;
mod review;
#[cfg(test)]
mod tests;

use super::ProtectedStore;
use crate::memory::{candidates::*, *};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE learning_candidates(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,revision INTEGER NOT NULL CHECK(revision>0),scope TEXT NOT NULL,state TEXT NOT NULL,body BLOB NOT NULL);
CREATE INDEX candidate_scope_page ON learning_candidates(scope,sequence);
CREATE INDEX candidate_state ON learning_candidates(state);
CREATE INDEX candidate_memory_target ON learning_candidates(json_extract(CAST(body AS TEXT),'$.record.payload.memory_id'));
";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCandidate {
    record: CandidateRecord,
    controls: Vec<MemoryControls>,
    before: Option<MemoryRecord>,
    target_revision: Option<u64>,
}

fn encode(stored: &StoredCandidate) -> Result<Vec<u8>> {
    let r = &stored.record;
    ensure!(
        r.version == 1 && !r.id.is_nil() && (1..=64).contains(&r.revision),
        "invalid candidate identity or revision"
    );
    crate::memory::validate_scope(&r.scope)?;
    ensure!(
        r.sources.len() <= 8 && stored.controls.len() <= 4 && r.events.len() <= 64,
        "candidate metadata exceeds bounds"
    );
    ensure!(
        r.content_hash == payload_hash(&r.payload)?,
        "candidate payload digest mismatch"
    );
    ensure!(
        r.changed_at_unix_seconds >= r.created_at_unix_seconds
            && r.changed_at_unix_seconds < i64::MAX as u64,
        "invalid candidate timestamp"
    );
    match &r.payload {
        CandidatePayload::Memory {
            statement,
            memory_id,
            base_revision,
            ..
        } => {
            if let Some(text) = statement {
                crate::memory::validate_statement(text)?;
            }
            ensure!(
                memory_id.is_some() == base_revision.is_some()
                    && memory_id.is_some() == stored.target_revision.is_some(),
                "candidate target/base metadata mismatch"
            );
            if let Some(id) = memory_id {
                ensure!(
                    !id.is_nil()
                        && statement.is_some()
                        && base_revision.is_some_and(|v| v > 0 && v < i64::MAX as u64)
                        && stored.target_revision >= *base_revision,
                    "invalid candidate memory base"
                );
            } else {
                ensure!(
                    statement.is_none()
                        && stored.before.is_none()
                        && r.risk == CandidateRisk::Sensitive
                        && r.validation == CandidateValidation::SensitiveContentNotRetained,
                    "a payload-free candidate must remain sensitive metadata"
                );
            }
        }
        CandidatePayload::Skill { name, markdown } => {
            ensure!(
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "Skill draft name must contain 1–64 letters, digits, hyphens or underscores"
            );
            if let Some(text) = markdown {
                ensure!(
                    !text.trim().is_empty() && text.len() <= SKILL_BYTES && clean_text(text),
                    "Skill draft must contain 1–32 KiB of inert Markdown without terminal controls"
                );
            }
            ensure!(
                matches!(r.origin, CandidateOrigin::OwnerDraft { .. })
                    && r.risk == CandidateRisk::Procedure
                    && r.validation == CandidateValidation::OwnerDraftInertOnly
                    && stored.target_revision.is_none()
                    && stored.before.is_none()
                    && r.rollback_revision.is_none(),
                "Skill draft cannot claim memory publication authority"
            );
        }
    }
    for source in &r.sources {
        ensure!(
            !source.id.is_nil()
                && !source.conversation.is_nil()
                && source.revision == 1
                && source.hash.len() == 64
                && source.hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid immutable candidate source"
        );
    }
    match &r.origin {
        CandidateOrigin::OwnerDraft {
            request,
            conversation,
        } => ensure!(
            !request.is_nil() && conversation.is_none_or(|id| !id.is_nil()) && r.sources.is_empty(),
            "invalid owner draft provenance"
        ),
        CandidateOrigin::Extractor {
            connection,
            model,
            route_digest,
        } => {
            ensure!(
                r.sources.len() == 1
                    && [connection, model, route_digest]
                        .into_iter()
                        .all(|v| !v.is_empty()
                            && v.len() <= 256
                            && !v.chars().any(char::is_control)),
                "invalid extractor provenance"
            );
            ensure!(
                r.scope == MemoryScope::Conversation(r.sources[0].conversation),
                "extraction cannot widen owner-source scope"
            );
        }
        CandidateOrigin::LegacyImport => ensure!(
            r.validation == CandidateValidation::LegacyEvidenceUnavailable
                && r.risk == CandidateRisk::LegacyUnverified,
            "legacy candidate cannot invent qualification"
        ),
    }
    if !matches!(r.origin, CandidateOrigin::LegacyImport) {
        ensure!(
            stored.controls.iter().any(|c| c.scope == MemoryScope::User)
                && stored.controls.iter().any(|c| c.scope == r.scope),
            "candidate consent scope is incomplete"
        );
    }
    if let Some(before) = &stored.before {
        before.validate()?;
        ensure!(
            matches!(
                &r.payload,
                CandidatePayload::Memory {
                    memory_id: Some(id),
                    base_revision: Some(revision),
                    statement: Some(text),
                    ..
                } if *id == before.id && *revision == before.revision && text == &before.statement
            ) && before.scope == r.scope
                && r.rollback_revision == Some(before.revision),
            "candidate rollback pre-image differs from base"
        );
    } else {
        ensure!(
            r.rollback_revision.is_none(),
            "candidate has no rollback pre-image"
        );
    }
    if let Some(reason) = &r.rejection_reason {
        ensure!(
            !reason.trim().is_empty() && reason.len() <= 1024 && clean_text(reason),
            "invalid rejection reason"
        );
    }
    if let Some(last) = r.events.last() {
        ensure!(
            last.revision == r.revision
                && last.state == r.state
                && last.at_unix_seconds == r.changed_at_unix_seconds,
            "candidate event head mismatch"
        );
        ensure!(
            r.events
                .windows(2)
                .all(|pair| pair[1].revision == pair[0].revision + 1
                    && pair[1].at_unix_seconds >= pair[0].at_unix_seconds),
            "candidate review history is not contiguous"
        );
        for event in &r.events {
            ensure!(
                event.at_unix_seconds >= r.created_at_unix_seconds
                    && (matches!(event.actor, CandidateActor::Owner)
                        || (event.revision == 1
                            && event.state == CandidateState::AutoApplied
                            && matches!(
                                r.validation,
                                CandidateValidation::OrdinaryStatedAllowlistV1
                                    | CandidateValidation::OrdinaryPreferenceV2
                            ))),
                "invalid candidate promotion actor"
            );
        }
    } else {
        ensure!(
            r.revision == 1 && matches!(r.state, CandidateState::Staged | CandidateState::Stale),
            "candidate lifecycle lacks review evidence"
        );
    }
    let bytes = serde_json::to_vec(stored)?;
    ensure!(
        bytes.len() <= CANDIDATE_BYTES,
        "candidate record exceeds 64 KiB"
    );
    Ok(bytes)
}

fn payload_hash(payload: &CandidatePayload) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(payload)?)
        .to_hex()
        .to_string())
}
fn clean_text(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
}
fn state_key(state: CandidateState) -> Result<String> {
    Ok(serde_json::to_value(state)?
        .as_str()
        .context("invalid candidate state")?
        .into())
}
fn generation(db: &Connection) -> Result<u64> {
    Ok(db.query_row(
        "SELECT revision FROM privacy_generation WHERE singleton=1",
        [],
        |r| super::database::read_u64(r, 0),
    )?)
}

fn decode(row: &rusqlite::Row<'_>, col: usize) -> Result<StoredCandidate> {
    let body = row.get_ref(col)?.as_blob()?;
    ensure!(
        body.len() <= CANDIDATE_BYTES,
        "candidate exceeds bounded read"
    );
    let stored: StoredCandidate = serde_json::from_slice(body)?;
    encode(&stored)?;
    ensure!(
        row.get::<_, String>(col + 1)? == stored.record.id.to_string()
            && super::database::read_u64(row, col + 2)? == stored.record.revision
            && row.get::<_, String>(col + 3)? == stored.record.scope.to_string()
            && row.get::<_, String>(col + 4)? == state_key(stored.record.state)?,
        "candidate routing metadata mismatch"
    );
    Ok(stored)
}

fn get(db: &Connection, id: Uuid) -> Result<StoredCandidate> {
    let mut q =
        db.prepare("SELECT body,id,revision,scope,state FROM learning_candidates WHERE id=?1")?;
    let mut rows = q.query([id.to_string()])?;
    decode(rows.next()?.context("learning candidate not found")?, 0)
}

fn insert(db: &Connection, stored: &StoredCandidate) -> Result<()> {
    db.execute(
        "INSERT INTO learning_candidates(id,revision,scope,state,body) VALUES(?1,?2,?3,?4,?5)",
        params![
            stored.record.id.to_string(),
            i64::try_from(stored.record.revision)?,
            stored.record.scope.to_string(),
            state_key(stored.record.state)?,
            encode(stored)?
        ],
    )?;
    Ok(())
}

/// The old memory inspection/export surface must not bypass an excluded
/// candidate's privacy fence. A fresh owner correction with different text is
/// independent; explicit owner-created records retain their existing contract.
pub(super) fn memory_visibility_required(db: &Connection) -> Result<bool> {
    let version: u32 = db.query_row("SELECT version FROM store_identity", [], |r| r.get(0))?;
    if version < 10 {
        return Ok(false);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM learning_candidates LIMIT 1)",
        [],
        |r| r.get(0),
    )?)
}

pub(super) fn memory_visible(db: &Connection, memory: &MemoryRecord) -> Result<bool> {
    let version: u32 = db.query_row("SELECT version FROM store_identity", [], |r| r.get(0))?;
    if version < 10 {
        return Ok(true);
    }
    let mut q = db.prepare("SELECT body,id,revision,scope,state FROM learning_candidates WHERE json_extract(CAST(body AS TEXT),'$.record.payload.memory_id')=?1 LIMIT 2")?;
    let mut rows = q.query([memory.id.to_string()])?;
    let Some(row) = rows.next()? else {
        return Ok(true);
    };
    let stored = decode(row, 0)?;
    ensure!(
        rows.next()?.is_none(),
        "multiple candidate owners for one memory target"
    );
    // Only owner memory controls can advance an existing target beyond the
    // candidate's publication revision. In particular, confirmed Restore must
    // keep the current explicit fact inspectable, while old candidate evidence
    // remains excluded. Learning never revises an existing target in place.
    if memory.state == MemoryState::Active
        && stored
            .target_revision
            .is_some_and(|revision| memory.revision > revision)
        && memory.changed.owner_request != memory.created.owner_request
    {
        return Ok(true);
    }
    if matches!(
        &stored.record.payload,
        CandidatePayload::Memory { statement: Some(text), .. } if text != &memory.statement
    ) {
        return Ok(true);
    }
    evidence::content_allowed(db, &stored)
}

impl ProtectedStore {
    pub(crate) fn candidate_page(
        &self,
        scope: Option<&MemoryScope>,
        after: Option<u64>,
    ) -> Result<CandidatePage> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let sql = if scope.is_some() {
                "SELECT sequence,body,id,revision,scope,state FROM learning_candidates WHERE sequence>?1 AND scope=?2 ORDER BY sequence LIMIT ?3"
            } else {
                "SELECT sequence,body,id,revision,scope,state FROM learning_candidates WHERE sequence>?1 ORDER BY sequence LIMIT ?2"
            };
            let mut q = tx.prepare(sql)?;
            let after = i64::try_from(after.unwrap_or(0))?;
            let mut rows = if let Some(scope) = scope {
                q.query(params![after, scope.to_string(), (CANDIDATE_PAGE + 1) as i64])?
            } else {
                q.query(params![after, (CANDIDATE_PAGE + 1) as i64])?
            };
            let mut records = Vec::new();
            let mut cursor = None;
            while let Some(row) = rows.next()? {
                if records.len() == CANDIDATE_PAGE {
                    return Ok(CandidatePage {
                        records,
                        next_after: cursor,
                    });
                }
                let stored = decode(row, 1)?;
                let stale = evidence::stale_reason(&tx, &stored)?.is_some();
                let r = &stored.record;
                records.push(CandidateSummary {
                    id: r.id,
                    revision: r.revision,
                    scope: r.scope.clone(),
                    target_kind: r.target_kind(),
                    state: if stale && r.state == CandidateState::Staged {
                        CandidateState::Stale
                    } else {
                        r.state
                    },
                    risk: r.risk,
                    changed_at_unix_seconds: r.changed_at_unix_seconds,
                });
                cursor = Some(super::database::read_u64(row, 0)?);
            }
            Ok(CandidatePage {
                records,
                next_after: None,
            })
        })
    }

    pub(crate) fn candidate_inspect(&self, id: Uuid) -> Result<CandidateInspection> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            evidence::inspect(&tx, get(&tx, id)?)
        })
    }

    pub(crate) fn candidate_stage_skill(
        &self,
        scope: MemoryScope,
        name: String,
        markdown: String,
        origin: MemoryProvenance,
    ) -> Result<CandidateRecord> {
        crate::memory::validate_scope(&scope)?;
        let payload = CandidatePayload::Skill {
            name,
            markdown: Some(markdown),
        };
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                !super::forgetting::memory_review_required(&tx)?,
                "review restored memory state before staging a draft"
            );
            if let Some(conversation) = origin.conversation {
                ensure!(
                    super::forgetting::source_allowed(&tx, conversation)?,
                    "draft source is excluded; start a new Conversation"
                );
            }
            let mut scopes = vec![MemoryScope::User, scope.clone()];
            if let Some(id) = origin.conversation {
                scopes.push(MemoryScope::Conversation(id));
            }
            scopes.dedup();
            let controls = scopes
                .into_iter()
                .map(|s| super::memory::controls(&tx, s))
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                controls.iter().all(|f| !f.no_memory),
                "no-memory mode prevents retained draft capture"
            );
            let record = CandidateRecord {
                version: 1,
                id: Uuid::new_v4(),
                revision: 1,
                scope,
                content_hash: payload_hash(&payload)?,
                payload,
                origin: CandidateOrigin::OwnerDraft {
                    request: origin.owner_request,
                    conversation: origin.conversation,
                },
                sources: vec![],
                privacy_generation: generation(&tx)?,
                risk: CandidateRisk::Procedure,
                validation: CandidateValidation::OwnerDraftInertOnly,
                state: CandidateState::Staged,
                created_at_unix_seconds: origin.at_unix_seconds,
                changed_at_unix_seconds: origin.at_unix_seconds,
                events: vec![],
                rejection_reason: None,
                rollback_revision: None,
            };
            insert(
                &tx,
                &StoredCandidate {
                    record: record.clone(),
                    controls,
                    before: None,
                    target_revision: None,
                },
            )?;
            tx.commit()?;
            Ok(record)
        })
    }
}

/// Imports old inactive records without inventing extractor evidence or review.
pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(SCHEMA)?;
    let mut after = 0;
    loop {
        let ids: Vec<(u64, Uuid)> = {
            let mut q = db.prepare("SELECT sequence,id FROM memory_entries WHERE sequence>?1 AND (CASE WHEN json_valid(CAST(body AS TEXT)) THEN json_extract(CAST(body AS TEXT),'$.state') ELSE 'invalid' END)='candidate' ORDER BY sequence LIMIT 32")?;
            q.query_map([i64::try_from(after)?], |r| {
                Ok((super::database::read_u64(r, 0)?, r.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (seq, id) = row?;
                Ok((seq, id.parse()?))
            })
            .collect::<Result<_>>()?
        };
        if ids.is_empty() {
            break;
        }
        for (seq, id) in ids {
            let memory = super::memory::get(db, id)?;
            let payload = CandidatePayload::Memory {
                memory_id: Some(id),
                base_revision: Some(memory.revision),
                statement: Some(memory.statement.clone()),
                claim: memory.claim,
            };
            let record = CandidateRecord {
                version: 1,
                id: Uuid::new_v4(),
                revision: 1,
                scope: memory.scope.clone(),
                content_hash: payload_hash(&payload)?,
                payload,
                origin: CandidateOrigin::LegacyImport,
                sources: vec![],
                privacy_generation: generation(db)?,
                risk: CandidateRisk::LegacyUnverified,
                validation: CandidateValidation::LegacyEvidenceUnavailable,
                state: CandidateState::Stale,
                created_at_unix_seconds: memory.created.at_unix_seconds,
                changed_at_unix_seconds: memory.changed.at_unix_seconds,
                events: vec![],
                rejection_reason: None,
                rollback_revision: Some(memory.revision),
            };
            insert(
                db,
                &StoredCandidate {
                    record,
                    controls: vec![],
                    target_revision: Some(memory.revision),
                    before: Some(memory),
                },
            )?;
            after = seq;
        }
    }
    Ok(())
}

/// Called inside the same learning transaction as the target insert.
pub(super) fn learned(
    db: &Connection,
    source: &learning::LearningSource,
    suggestion: &learning::Suggestion,
    route: &learning::LearningRoute,
    memory: Option<&MemoryRecord>,
) -> Result<()> {
    let auto = memory.is_some_and(|m| m.state == MemoryState::Active);
    let payload = CandidatePayload::Memory {
        memory_id: memory.map(|m| m.id),
        base_revision: memory.map(|m| m.revision),
        statement: memory.map(|m| m.statement.clone()),
        claim: suggestion.claim,
    };
    let at = crate::memory::now()?;
    let record = CandidateRecord {
        version: 1,
        id: Uuid::new_v4(),
        revision: 1,
        scope: MemoryScope::Conversation(
            source
                .context
                .conversation
                .context("learning source has no Conversation")?,
        ),
        content_hash: payload_hash(&payload)?,
        payload,
        origin: CandidateOrigin::Extractor {
            connection: route.connection.clone(),
            model: route.model.clone(),
            route_digest: route.digest.clone(),
        },
        sources: vec![CandidateSource {
            id: source.id,
            revision: 1,
            conversation: source.context.conversation.unwrap(),
            hash: source.hash.clone(),
        }],
        privacy_generation: source.generation,
        risk: if suggestion.sensitive {
            CandidateRisk::Sensitive
        } else if auto {
            CandidateRisk::Ordinary
        } else if suggestion.claim == MemoryClaim::Inferred {
            CandidateRisk::Inferred
        } else {
            CandidateRisk::Ambiguous
        },
        validation: if suggestion.sensitive {
            CandidateValidation::SensitiveContentNotRetained
        } else if auto {
            CandidateValidation::OrdinaryPreferenceV2
        } else {
            CandidateValidation::ExactOwnerQuote
        },
        state: if auto {
            CandidateState::AutoApplied
        } else {
            CandidateState::Staged
        },
        created_at_unix_seconds: at,
        changed_at_unix_seconds: at,
        events: if auto {
            vec![CandidateEvent {
                revision: 1,
                state: CandidateState::AutoApplied,
                actor: CandidateActor::DeterministicPolicy,
                at_unix_seconds: at,
            }]
        } else {
            vec![]
        },
        rejection_reason: None,
        rollback_revision: None,
    };
    let controls = source
        .context
        .scopes()
        .into_iter()
        .map(|s| super::memory::controls(db, s))
        .collect::<Result<Vec<_>>>()?;
    insert(
        db,
        &StoredCandidate {
            record,
            controls,
            before: None,
            target_revision: memory.map(|m| m.revision),
        },
    )
}
