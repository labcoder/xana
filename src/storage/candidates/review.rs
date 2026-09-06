//! Owner-only exact candidate/target CAS; no general harness promotion seam.
use super::*;

impl ProtectedStore {
    pub(crate) fn candidate_review(
        &self,
        id: Uuid,
        revision: u64,
        edit: CandidateEdit,
        mut origin: MemoryProvenance,
    ) -> Result<CandidateRecord> {
        self.with_database(|db| {
            let tx = db
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut stored = get(&tx, id)?;
            ensure!(
                stored.record.revision == revision,
                "candidate changed since inspection; refresh before review"
            );
            ensure!(
                revision < 64,
                "candidate reached its bounded review history; create a new explicit draft"
            );
            origin.at_unix_seconds = origin
                .at_unix_seconds
                .max(stored.record.changed_at_unix_seconds);
            // Rejection and archive may retire stale metadata; neither can publish
            // a target. All activation/rollback paths require current proof.
            if matches!(edit, CandidateEdit::Approve { .. } | CandidateEdit::Undo)
                && let Some(reason) = evidence::stale_reason(&tx, &stored)?
            {
                anyhow::bail!("candidate is stale: {reason}");
            }
            match edit {
                CandidateEdit::Approve { confirm_sensitive } => {
                    ensure!(
                        stored.record.state == CandidateState::Staged,
                        "only a staged candidate may be reviewed"
                    );
                    if stored.record.risk == CandidateRisk::Sensitive {
                        ensure!(
                            confirm_sensitive,
                            "sensitive candidate requires explicit consent"
                        );
                    }
                    match &stored.record.payload {
                        CandidatePayload::Memory {
                            memory_id,
                            statement,
                            ..
                        } => {
                            ensure!(
                                statement.is_some(),
                                "sensitive helper content was not retained; use a fresh explicit owner remember request instead"
                            );
                            let old = super::super::memory::get(
                                &tx,
                                memory_id.context("candidate has no memory target")?,
                            )?;
                            ensure!(
                                old.state == MemoryState::Candidate,
                                "candidate target is no longer staged"
                            );
                            let mut current = old.clone();
                            current.state = MemoryState::Active;
                            // Explicit review approves the represented claim; it
                            // does not turn an inference into an owner-stated fact.
                            current.changed = origin.clone();
                            current.revision = old
                                .revision
                                .checked_add(1)
                                .context("memory revision exhausted")?;
                            replace_memory(&tx, &old, &current)?;
                            stored.record.rollback_revision = Some(old.revision);
                            stored.before = Some(old);
                            stored.target_revision = Some(current.revision);
                            stored.record.state = CandidateState::Approved;
                        }
                        CandidatePayload::Skill { markdown, .. } => {
                            ensure!(markdown.is_some(), "Skill draft content is unavailable");
                            stored.record.state = CandidateState::ReviewedOnly;
                        }
                    }
                }
                CandidateEdit::Undo => {
                    ensure!(
                        matches!(
                            stored.record.state,
                            CandidateState::AutoApplied
                                | CandidateState::Approved
                                | CandidateState::ReviewedOnly
                        ),
                        "this candidate has no active publication to undo"
                    );
                    if let CandidatePayload::Memory {
                        memory_id: Some(id),
                        ..
                    } = stored.record.payload
                    {
                        let old = super::super::memory::get(&tx, id)?;
                        ensure!(
                            old.state == MemoryState::Active,
                            "only the exact active memory publication may be undone"
                        );
                        let mut current = old.clone();
                        // Undo revokes this publication. It never revives an old
                        // value over a concurrent owner correction or forgetting.
                        current.state = MemoryState::Stale;
                        current.changed = origin.clone();
                        current.revision = old
                            .revision
                            .checked_add(1)
                            .context("memory revision exhausted")?;
                        replace_memory(&tx, &old, &current)?;
                        stored.target_revision = Some(current.revision);
                    }
                    stored.record.state = CandidateState::Undone;
                }
                CandidateEdit::Reject { reason } => {
                    ensure!(
                        matches!(
                            stored.record.state,
                            CandidateState::Staged | CandidateState::Stale
                        ),
                        "only pending candidates may be rejected; use Undo for a publication"
                    );
                    ensure!(
                        !reason.trim().is_empty() && reason.len() <= 1024 && clean_text(&reason),
                        "rejection reason must contain 1–1024 bytes without terminal controls"
                    );
                    stored.record.rejection_reason = Some(reason);
                    stored.record.state = CandidateState::Rejected;
                }
                CandidateEdit::Archive => {
                    if matches!(
                        stored.record.state,
                        CandidateState::AutoApplied | CandidateState::Approved
                    ) && let CandidatePayload::Memory {
                        memory_id: Some(id),
                        ..
                    } = stored.record.payload
                    {
                        let current = super::super::memory::get(&tx, id)?;
                        ensure!(
                            current.state != MemoryState::Active
                                || Some(current.revision) != stored.target_revision,
                            "undo or disable the active memory publication before archiving its candidate"
                        );
                    }
                    stored.record.state = CandidateState::Archived;
                }
            }
            stored.record.revision += 1;
            stored.record.changed_at_unix_seconds = origin.at_unix_seconds;
            // Only a successful publication/undo advances the privacy generation.
            // The publishing candidate tracks that resulting revision for undo.
            if stored.record.target_kind() == CandidateTargetKind::Memory
                && matches!(
                    stored.record.state,
                    CandidateState::Approved | CandidateState::Undone
                )
            {
                super::super::forgetting::advance_generation(&tx)?;
                stored.record.privacy_generation = generation(&tx)?;
            }
            stored.record.events.push(CandidateEvent {
                revision: stored.record.revision,
                state: stored.record.state,
                actor: CandidateActor::Owner,
                at_unix_seconds: origin.at_unix_seconds,
            });
            ensure!(
                tx.execute(
                    "UPDATE learning_candidates SET revision=?2,state=?3,body=?4 WHERE id=?1 AND revision=?5",
                    params![
                        id.to_string(),
                        i64::try_from(stored.record.revision)?,
                        state_key(stored.record.state)?,
                        encode(&stored)?,
                        i64::try_from(revision)?
                    ]
                )? == 1,
                "candidate review conflict"
            );
            let result = evidence::inspect(&tx, stored)?.record;
            tx.commit()?;
            Ok(result)
        })
    }
}

fn replace_memory(db: &Connection, old: &MemoryRecord, current: &MemoryRecord) -> Result<()> {
    let mut historical = old.clone();
    historical.state = MemoryState::Superseded;
    db.execute(
        "INSERT INTO memory_revisions(id,revision,body) VALUES(?1,?2,?3)",
        params![
            old.id.to_string(),
            i64::try_from(old.revision)?,
            super::super::memory::encode(&historical)?
        ],
    )?;
    ensure!(
        db.execute(
            "UPDATE memory_entries SET revision=?2,scope=?3,body=?4 WHERE id=?1 AND revision=?5",
            params![
                old.id.to_string(),
                i64::try_from(current.revision)?,
                current.scope.to_string(),
                super::super::memory::encode(current)?,
                i64::try_from(old.revision)?
            ]
        )? == 1,
        "memory publication conflict"
    );
    Ok(())
}
