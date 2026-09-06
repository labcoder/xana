//! Current privacy and target proof checks shared by inspection and publication.
use super::*;

pub(super) fn content_allowed(db: &Connection, stored: &StoredCandidate) -> Result<bool> {
    let r = &stored.record;
    for source in &r.sources {
        if !super::super::forgetting::source_allowed(db, source.conversation)? {
            return Ok(false);
        }
    }
    if let CandidateOrigin::OwnerDraft {
        conversation: Some(id),
        ..
    } = r.origin
        && !super::super::forgetting::source_allowed(db, id)?
    {
        return Ok(false);
    }
    if let Some(memory) = &stored.before {
        for id in [memory.created.conversation, memory.changed.conversation]
            .into_iter()
            .flatten()
        {
            if !super::super::forgetting::source_allowed(db, id)? {
                return Ok(false);
            }
        }
        if super::super::forgetting::statement_suppressed(db, &memory.statement)? {
            return Ok(false);
        }
    }
    if let CandidatePayload::Memory {
        statement,
        memory_id,
        ..
    } = &r.payload
    {
        if let Some(text) = statement
            && super::super::forgetting::statement_suppressed(db, text)?
        {
            return Ok(false);
        }
        if let Some(id) = memory_id {
            let current = super::super::memory::get(db, *id)?;
            if current.state == MemoryState::Forgotten {
                return Ok(false);
            }
            for id in [current.created.conversation, current.changed.conversation]
                .into_iter()
                .flatten()
            {
                if !super::super::forgetting::source_allowed(db, id)? {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

pub(super) fn stale_reason(db: &Connection, stored: &StoredCandidate) -> Result<Option<String>> {
    let r = &stored.record;
    let reason = if !content_allowed(db, stored)? {
        Some(
            "The candidate source was forgotten or excluded; its content and rollback are unavailable.",
        )
    } else if super::super::forgetting::memory_review_required(db)? {
        Some("The restored store requires memory review; old candidate approval is invalid.")
    } else if generation(db)? != r.privacy_generation {
        Some("Privacy, source or memory generation changed; no automatic rebase is permitted.")
    } else if matches!(r.origin, CandidateOrigin::LegacyImport) {
        Some(
            "This legacy candidate lacks immutable extraction evidence; use an explicit owner remember/correction instead.",
        )
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(Some(reason.into()));
    }
    for original in &stored.controls {
        if super::super::memory::controls(db, original.scope.clone())? != *original {
            return Ok(Some(
                "Scope or consent controls changed since staging.".into(),
            ));
        }
    }
    for source in &r.sources {
        let current: Option<(String, String, String)> = db
            .query_row(
                "SELECT conversation,hash,state FROM learning_sources WHERE id=?1",
                [source.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if !current.is_some_and(|(conversation, hash, state)| {
            conversation == source.conversation.to_string()
                && hash == source.hash
                && state == "processed"
        }) {
            return Ok(Some(
                "The immutable owner-source reference changed or is unavailable.".into(),
            ));
        }
    }
    if let CandidatePayload::Memory {
        memory_id: Some(id),
        statement,
        ..
    } = &r.payload
    {
        let current = super::super::memory::get(db, *id)?;
        if Some(current.revision) != stored.target_revision
            || current.scope != r.scope
            || statement.as_ref() != Some(&current.statement)
        {
            return Ok(Some("The target memory base, scope or content changed; refresh and review the owner's current record.".into()));
        }
    }
    Ok(None)
}

pub(super) fn inspect(db: &Connection, mut stored: StoredCandidate) -> Result<CandidateInspection> {
    let allowed = content_allowed(db, &stored)?;
    let stale_reason = stale_reason(db, &stored)?;
    let retained = match &stored.record.payload {
        CandidatePayload::Memory { statement, .. } => statement.is_some(),
        CandidatePayload::Skill { markdown, .. } => markdown.is_some(),
    };
    let can_approve =
        stale_reason.is_none() && retained && stored.record.state == CandidateState::Staged;
    if !allowed {
        stored.record.hide_content();
        stored.before = None;
    }
    if stale_reason.is_some() && stored.record.state == CandidateState::Staged {
        stored.record.state = CandidateState::Stale;
    }
    let diff = if !allowed {
        "Content unavailable: source privacy/forgetting rules also apply to candidate inspection and rollback.".into()
    } else {
        match &stored.record.payload {
            CandidatePayload::Memory { statement:Some(text), .. } => format!("- {:?}\n+ Active scoped memory (only after review)\n  {text}",stored.before.as_ref().map(|m|m.state).unwrap_or(MemoryState::Candidate)),
            CandidatePayload::Memory { statement:None, .. } => "Sensitive helper text was not copied. Approval cannot recover it; use a fresh explicit owner 'remember' request with the fact and desired scope.".into(),
            CandidatePayload::Skill { name, markdown } => format!("- No installed Skill change\n+ Inert draft: {name}\n{}\nReview only marks this draft reviewed; it does not install, discover, load or execute it.",markdown.as_deref().unwrap_or("[content unavailable]")),
        }
    };
    Ok(CandidateInspection {
        record: stored.record,
        before: stored.before,
        diff,
        can_approve,
        stale_reason,
    })
}
