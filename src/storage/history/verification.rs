//! Offline immutable-history/index verification, separate from execution restore.

use super::*;
use crate::{
    artifact::{ArtifactRecord, ContentHash},
    identity::{ArtifactId, ConversationEntryId},
    session::{
        CompactionCheckpoint,
        compaction::{CompactionSourceProof, CompactionSourceProofBuilder},
    },
};
use std::collections::{BTreeSet, HashMap};

const PROOF_PAGE: usize = 128;
// The planner can prefer the covering primary key, which scans the whole
// Conversation because sequence follows kind/subject there. This schema-v7+
// record index bounds each lookup to one record without weakening set equality.
const SUBJECT_LOOKUP: &str = "SELECT kind,subject FROM native_subjects INDEXED BY native_subjects_record WHERE session=?1 AND sequence=?2 LIMIT 129";
#[cfg(test)]
mod tests;

impl ProtectedStore {
    pub(crate) fn history_record_sequence(
        &self,
        id: SessionId,
        record: crate::identity::RecordId,
    ) -> Result<usize> {
        self.with_database(|db| {
            Ok(db.connection.query_row(
                "SELECT sequence FROM native_records WHERE session=?1 AND id=?2",
                params![id.to_string(), record.to_string()],
                |r| read_usize(r, 0),
            )?)
        })
    }

    pub(crate) fn verify_historical_transitions(&self, id: SessionId) -> Result<()> {
        let revision = self.history_metadata(id)?.revision;
        for kind in ["operation", "child"] {
            let mut after = String::new();
            loop {
                let subjects=self.with_database(|db| {
                    let mut query=db.connection.prepare("SELECT DISTINCT subject FROM native_subjects WHERE session=?1 AND kind=?2 AND subject>?3 ORDER BY subject LIMIT 128")?;
                    let values=query.query_map(params![id.to_string(),kind,after],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(values)
                })?;
                if subjects.is_empty() {
                    break;
                }
                for subject in subjects {
                    if kind == "operation" {
                        crate::session::DurableSession::inspect_operation_protected(
                            self,
                            id,
                            subject.parse()?,
                        )?;
                    } else {
                        crate::session::DurableSession::verify_child_protected(
                            self,
                            id,
                            subject.parse()?,
                        )?;
                    }
                    after = subject;
                }
            }
        }
        ensure!(
            self.history_metadata(id)?.revision == revision,
            "history changed during transition verification"
        );
        Ok(())
    }
    /// Walk original records once without retaining the journal. SQLCipher and
    /// the digest-bound execution snapshot remain the writer's trust boundary;
    /// this verifies all derived indexes and source references independently.
    pub(crate) fn verify_immutable_history(
        &self,
        id: SessionId,
        objects: &HashMap<&str, u64>,
        #[cfg(test)] timed: bool,
    ) -> Result<()> {
        self.with_database(|db| {
            #[cfg(test)]
            let mut timing = crate::storage::verification::VerifyTiming::new(timed);
            let tx = db.connection.transaction()?;
            let expected: (usize, usize, String, String, Option<String>) = tx.query_row(
                "SELECT revision,bytes,root_thread,workspace,head FROM native_sessions WHERE id=?1", [id.to_string()],
                |r| Ok((read_usize(r,0)?, read_usize(r,1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
            let snapshot: Option<(usize, String)> = tx.query_row(
                "SELECT revision,prefix_digest FROM native_execution_checkpoints WHERE session=?1", [id.to_string()],
                |r| Ok((read_usize(r,0)?, r.get(1)?))).optional()?;
            ensure!(snapshot.as_ref().is_none_or(|(revision,_)| *revision > 0 && *revision <= expected.0), "execution snapshot revision differs");
            let mut statement = tx.prepare("SELECT sequence,id,body FROM native_records WHERE session=?1 ORDER BY sequence")?;
            let mut rows = statement.query([id.to_string()])?;
            let mut sequence = 0usize;
            let mut length = 0usize;
            let mut digest = String::new();
            let mut head = None;
            let mut entries = 0usize;
            while let Some(row) = rows.next()? {
                ensure!(read_usize(row,0)? == sequence, "immutable journal sequence is discontinuous");
                let body = row.get_ref(2)?.as_blob()?;
                ensure!(body.len() <= MAX_RECORD_BYTES, "immutable record exceeds its byte bound");
                let record: RecordEnvelope = serde_json::from_slice(body)?;
                ensure!(record.session_id == id && record.version == SESSION_RECORD_VERSION && record.record_id.to_string() == row.get::<_,String>(1)?, "immutable record identity differs");
                digest = subjects::record_digest(&digest, body);
                let indexed: String = tx.query_row("SELECT digest FROM native_record_digests WHERE session=?1 AND sequence=?2", params![id.to_string(), i64::try_from(sequence)?], |r| r.get(0))?;
                ensure!(digest == indexed, "immutable journal digest differs");
                if snapshot.as_ref().is_some_and(|(revision,_)| *revision == sequence + 1) {
                    let mut query = tx.prepare("SELECT body FROM native_execution_checkpoints WHERE session=?1")?;
                    let mut snapshot_rows = query.query([id.to_string()])?;
                    let snapshot_row = snapshot_rows.next()?.context("execution snapshot disappeared")?;
                    let snapshot_body = snapshot_row.get_ref(0)?.as_blob()?;
                    ensure!(snapshot_body.len() <= crate::session::hydration::MAX_EXECUTION_BYTES && subjects::record_digest(&digest, snapshot_body) == snapshot.as_ref().expect("matched snapshot").1, "execution snapshot differs from original journal prefix");
                    let state = crate::session::hydration::decode(snapshot_body)?;
                    ensure!(state.session_id == id, "execution snapshot belongs to another Conversation");
                }
                #[cfg(test)]
                let started = timing.start();
                verify_subjects(&tx, &record, sequence)?;
                #[cfg(test)]
                timing.record("subjects", started);
                #[cfg(test)]
                let started = timing.start();
                super::constraints::validate_registration_before(&tx, id, &record.record, Some(sequence))?;
                #[cfg(test)]
                timing.record("registration_constraints", started);
                match &record.record {
                    SessionRecord::SessionCreated {thread_id, workspace_root} => ensure!(sequence == 0 && thread_id.to_string() == expected.2 && workspace_root.to_str() == Some(expected.3.as_str()), "Conversation creation metadata differs"),
                    _ if sequence == 0 => anyhow::bail!("immutable Conversation lacks its creation record"),
                    SessionRecord::ConversationBranched { lineage } => {
                        ensure!(sequence == 1 && entries == 0 && lineage.source_session_id != id && lineage.shared_entry_count > 0 && lineage.shared_entry_count <= MAX_PROTECTED_RECORDS, "historical branch lineage differs");
                        let mut cursor = Some(lineage.source_entry_id);
                        let mut previous = usize::MAX;
                        for _ in 0..lineage.shared_entry_count {
                            let entry = cursor.context("branch source ancestry ended early")?;
                            let (parent, position) = entry_metadata(&tx, id, entry)?;
                            ensure!(position < previous, "branch source ancestry cycles");
                            previous = position;
                            cursor = parent;
                        }
                        ensure!(cursor.is_none(), "branch source count differs from copied ancestry");
                    }
                    SessionRecord::ConversationEntryAppended {entry} => {
                        let edge = entry_metadata(&tx, id, entry.id)?;
                        ensure!(edge.0 == entry.parent && edge.1 == sequence, "Conversation entry index differs from immutable body");
                        if let Some(parent) = entry.parent { ensure!(entry_metadata(&tx,id,parent)?.1 < sequence, "Conversation entry has forward or cyclic ancestry"); }
                        for block in &entry.message.content {
                            let reference = match block {
                                crate::message::ContentBlock::Image(image) => Some(&image.artifact),
                                crate::message::ContentBlock::ToolResult(result) => result.artifact.as_deref(),
                                _ => None,
                            };
                            if let Some(reference) = reference {
                                ensure!(registered_artifact(&tx, id, reference.reference.id, sequence)? == *reference, "Conversation attachment differs from its registered artifact");
                            }
                        }
                        entries += 1;
                    }
                    SessionRecord::ThreadHeadMoved { thread_id, head: next } => {
                        ensure!(thread_id.to_string() == expected.2, "Conversation head belongs to another thread");
                        if let Some(next) = next { ensure!(entry_metadata(&tx,id,*next)?.1 < sequence, "Conversation head references a future entry"); }
                        head = next.map(|id| id.to_string());
                    }
                    SessionRecord::ArtifactRegistered {artifact} => {
                        ContentHash::parse(artifact.reference.content_hash.as_str().to_owned()).map_err(anyhow::Error::msg)?;
                        ensure!(objects.get(artifact.reference.content_hash.as_str()) == Some(&artifact.byte_len), "Conversation references a missing or mismatched artifact");
                    }
                    SessionRecord::ContextRegistered {context} => {
                        let artifact = registered_artifact(&tx,id,context.artifact.id,sequence)?;
                        ensure!(artifact.reference == context.artifact && artifact.reference.content_hash == context.content_hash && artifact.byte_len == context.logical_size, "context source hash or size differs from its artifact");
                        let previous: usize = tx.query_row("SELECT COUNT(*) FROM native_subjects WHERE session=?1 AND kind='context' AND subject=?2 AND sequence<?3", params![id.to_string(), context.id.to_string(),i64::try_from(sequence)?], |r| read_usize(r,0))?;
                        ensure!(context.version == u64::try_from(previous)?.checked_add(1).context("context version overflow")?, "context versions are not monotonic");
                    }
                    SessionRecord::ContextViewRegistered {view} => verify_context(&tx,id,view.source,view.source_version,sequence)?,
                    SessionRecord::OperationAccepted {input_entry_id,..} => ensure!(entry_metadata(&tx,id,*input_entry_id)?.1 < sequence,"operation input references a future entry"),
                    SessionRecord::StepStarted {assistant_entry_id,..} => ensure!(entry_metadata(&tx,id,*assistant_entry_id)?.1 < sequence,"operation step references a future entry"),
                    SessionRecord::InvocationResultAppended {result} => {
                        if let crate::operation::InvocationOutcome::Completed{output}=&result.outcome { verify_value(&tx,id,output,sequence)?; }
                    }
                    SessionRecord::NamedValueSet {value} => verify_value(&tx,id,&value.value,sequence)?,
                    SessionRecord::ChildReportCommitted {report} => {
                        if let crate::orchestration::ChildReportReference::Artifact{artifact,..}=&report.reference {
                            ensure!(registered_artifact(&tx,id,artifact.id,sequence)?.reference == *artifact,"child report artifact source differs");
                        }
                    }
                    SessionRecord::NamedContextSet {name,context_id,version} => {
                        ensure!(!name.trim().is_empty(), "named context has an empty name");
                        verify_context(&tx,id,*context_id,*version,sequence)?;
                    }
                    SessionRecord::ConversationCompacted {checkpoint} => {
                        ensure!(checkpoint.version == crate::session::COMPACTION_CHECKPOINT_VERSION && checkpoint.budget.is_valid_checkpoint_plan(), "historical compaction version or budget differs");
                        #[cfg(test)]
                        let started = timing.start();
                        verify_compaction_position(&tx,id,checkpoint,head.as_deref().map(str::parse).transpose()?,sequence)?;
                        #[cfg(test)]
                        timing.record("compaction_position", started);
                        #[cfg(test)]
                        let started = timing.start();
                        let proof = historical_proof(&tx,id,checkpoint,sequence)?;
                        #[cfg(test)]
                        timing.record("compaction_source_proof", started);
                        ensure!(proof.matches(id,checkpoint) && crate::session::compaction::validate_summary(&checkpoint.summary,checkpoint.budget.summary_max_bytes), "historical compaction source or summary differs");
                        ensure!(checkpoint.semantic.as_ref().is_none_or(|provenance| provenance.valid_for(&checkpoint.summary)), "semantic compaction provenance differs");
                    }
                    _ => {}
                }
                length = length.checked_add(body.len()+1).context("immutable journal size overflow")?;
                sequence += 1;
            }
            ensure!(sequence == expected.0 && length == expected.1 && head == expected.4, "Conversation journal metadata differs");
            for (table, count) in [("native_records",sequence), ("native_record_digests",sequence), ("native_entries",entries)] {
                let actual = tx.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE session=?1"), [id.to_string()], |r| read_usize(r,0))?;
                ensure!(actual == count, "Conversation index inventory differs");
            }
            #[cfg(test)]
            let started = timing.start();
            verify_active_path(&tx,id,head.as_deref())?;
            #[cfg(test)]
            {
                timing.record("active_path", started);
                timing.report("immutable_history");
            }
            Ok(())
        })
    }
}

fn verify_subjects(tx: &Transaction<'_>, record: &RecordEnvelope, sequence: usize) -> Result<()> {
    let expected: BTreeSet<_> = subjects::record_subjects(&record.record)
        .into_iter()
        .map(|subject| {
            let (kind, key) = subject.key();
            (kind.to_owned(), key)
        })
        .collect();
    let mut statement = tx.prepare(SUBJECT_LOOKUP)?;
    let actual: BTreeSet<(String, String)> = statement
        .query_map(
            params![record.session_id.to_string(), i64::try_from(sequence)?],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(
        actual == expected,
        "historical subject index differs from original record"
    );
    Ok(())
}

fn entry_metadata(
    tx: &Transaction<'_>,
    session: SessionId,
    entry: ConversationEntryId,
) -> Result<(Option<ConversationEntryId>, usize)> {
    let (parent, sequence): (Option<String>, usize) = tx.query_row(
        "SELECT parent,sequence FROM native_entries WHERE session=?1 AND id=?2",
        params![session.to_string(), entry.to_string()],
        |r| Ok((r.get(0)?, read_usize(r, 1)?)),
    )?;
    Ok((parent.map(|parent| parent.parse()).transpose()?, sequence))
}

fn historical_proof(
    tx: &Transaction<'_>,
    session: SessionId,
    checkpoint: &CompactionCheckpoint,
    before: usize,
) -> Result<CompactionSourceProof> {
    let total = checkpoint
        .source_entry_count
        .checked_add(1)
        .context("compaction count overflow")?;
    ensure!(
        total > 1 && total <= MAX_PROTECTED_RECORDS,
        "compaction source exceeds supported history range"
    );
    // Retain only one metadata anchor per page, never historical message bodies.
    let mut anchors = Vec::with_capacity(total.div_ceil(PROOF_PAGE));
    let mut cursor = Some(checkpoint.retained_tail_start);
    let mut previous = before;
    for index in 0..total {
        let id = cursor.context("historical compaction ancestry ended early")?;
        if index % PROOF_PAGE == 0 {
            anchors.push((id, (total - index).min(PROOF_PAGE)));
        }
        let (parent, sequence) = entry_metadata(tx, session, id)?;
        ensure!(
            sequence < previous,
            "historical compaction ancestry cycles or references future state"
        );
        previous = sequence;
        cursor = parent;
    }
    ensure!(
        cursor.is_none(),
        "historical compaction count does not cover its original prefix"
    );
    let mut proof = CompactionSourceProofBuilder::new(session, checkpoint.source_entry_count);
    for (tail, count) in anchors.into_iter().rev() {
        let mut ids = Vec::with_capacity(count);
        let mut cursor = Some(tail);
        for _ in 0..count {
            let id = cursor.context("historical proof page ended early")?;
            let (parent, sequence) = entry_metadata(tx, session, id)?;
            ids.push((id, parent, sequence));
            cursor = parent;
        }
        for (id, parent, sequence) in ids.into_iter().rev() {
            let record = original_record(tx, session, sequence)?;
            ensure!(
                record.session_id == session && record.version == SESSION_RECORD_VERSION,
                "historical proof identity differs"
            );
            let SessionRecord::ConversationEntryAppended { entry } = record.record else {
                anyhow::bail!("historical proof is not an immutable entry");
            };
            ensure!(
                entry.id == id && entry.parent == parent,
                "historical proof ancestry differs from original bytes"
            );
            proof.push(id, &entry.message)?;
        }
    }
    proof.finish()
}

/// The source digest proves immutable bytes, not that those bytes were active
/// when compacted. Reconstruct the former reducer's positional/predecessor
/// checks through bounded metadata cursors, including paths later cleared.
fn verify_compaction_position(
    tx: &Transaction<'_>,
    session: SessionId,
    checkpoint: &CompactionCheckpoint,
    head: Option<ConversationEntryId>,
    before: usize,
) -> Result<()> {
    ensure!(
        ancestor_on_path(tx, session, checkpoint.retained_tail_start, head, before)?,
        "historical compaction source is not on its current Conversation path"
    );
    let mut query = tx.prepare(
        "SELECT sequence FROM native_subjects WHERE session=?1 AND kind='compaction' AND sequence<?2 ORDER BY sequence DESC",
    )?;
    let mut rows = query.query(params![session.to_string(), i64::try_from(before)?])?;
    while let Some(row) = rows.next()? {
        let record = original_record(tx, session, read_usize(row, 0)?)?;
        let SessionRecord::ConversationCompacted {
            checkpoint: previous,
        } = record.record
        else {
            anyhow::bail!("historical compaction index references another record kind");
        };
        // Earlier streamed records have already passed their own original-byte
        // proof. An immutable retained-tail ancestor identifies that same prefix.
        if ancestor_on_path(tx, session, previous.retained_tail_start, head, before)? {
            ensure!(
                checkpoint.previous_checkpoint == Some(previous.id)
                    && checkpoint.source_entry_count > previous.source_entry_count,
                "historical compaction predecessor or source advancement differs"
            );
            return Ok(());
        }
    }
    ensure!(
        checkpoint.previous_checkpoint.is_none(),
        "historical compaction references an inactive predecessor"
    );
    Ok(())
}

fn ancestor_on_path(
    tx: &Transaction<'_>,
    session: SessionId,
    ancestor: ConversationEntryId,
    mut cursor: Option<ConversationEntryId>,
    mut before: usize,
) -> Result<bool> {
    for _ in 0..MAX_PROTECTED_RECORDS {
        let Some(entry) = cursor else {
            return Ok(false);
        };
        let (parent, sequence) = entry_metadata(tx, session, entry)?;
        ensure!(
            sequence < before,
            "historical active path cycles or references future state"
        );
        if entry == ancestor {
            return Ok(true);
        }
        cursor = parent;
        before = sequence;
    }
    anyhow::bail!("historical active path exceeds its bound")
}

fn registered_artifact(
    tx: &Transaction<'_>,
    session: SessionId,
    artifact: ArtifactId,
    before: usize,
) -> Result<ArtifactRecord> {
    let sequence = tx.query_row("SELECT sequence FROM native_subjects WHERE session=?1 AND kind='artifact' AND subject=?2 AND sequence<?3 ORDER BY sequence DESC LIMIT 1", params![session.to_string(),artifact.to_string(),i64::try_from(before)?],|r|read_usize(r,0))?;
    let record = original_record(tx, session, sequence)?;
    let SessionRecord::ArtifactRegistered { artifact: record } = record.record else {
        anyhow::bail!("artifact source is not registered");
    };
    ensure!(
        record.reference.id == artifact,
        "artifact source identity differs"
    );
    Ok(record)
}

fn original_record(
    tx: &Transaction<'_>,
    session: SessionId,
    sequence: usize,
) -> Result<RecordEnvelope> {
    let mut query =
        tx.prepare("SELECT body FROM native_records WHERE session=?1 AND sequence=?2")?;
    let mut rows = query.query(params![session.to_string(), i64::try_from(sequence)?])?;
    let row = rows
        .next()?
        .context("immutable source record is unavailable")?;
    let body = row.get_ref(0)?.as_blob()?;
    ensure!(
        body.len() <= MAX_RECORD_BYTES,
        "immutable source record exceeds its byte bound"
    );
    let record: RecordEnvelope = serde_json::from_slice(body)?;
    ensure!(
        record.session_id == session && record.version == SESSION_RECORD_VERSION,
        "immutable source record identity differs"
    );
    Ok(record)
}

fn verify_context(
    tx: &Transaction<'_>,
    session: SessionId,
    context: crate::identity::ContextId,
    version: u64,
    before: usize,
) -> Result<()> {
    let exists:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='context' AND s.subject=?2 AND s.sequence<?3 AND json_extract(CAST(r.body AS TEXT),'$.data.context.version')=?4)",params![session.to_string(),context.to_string(),i64::try_from(before)?,i64::try_from(version)?],|r|r.get(0))?;
    ensure!(
        exists,
        "context view references an unavailable immutable source version"
    );
    Ok(())
}

fn verify_value(
    tx: &Transaction<'_>,
    session: SessionId,
    value: &crate::operation::DurableValueRef,
    before: usize,
) -> Result<()> {
    match value {
        crate::operation::DurableValueRef::InlineJson(_) => {}
        crate::operation::DurableValueRef::Artifact(reference) => ensure!(
            registered_artifact(tx, session, reference.id, before)?.reference == *reference,
            "operation artifact reference differs"
        ),
        crate::operation::DurableValueRef::Context { id, version } => {
            verify_context(tx, session, *id, *version, before)?
        }
    }
    Ok(())
}

fn verify_active_path(tx: &Transaction<'_>, session: SessionId, head: Option<&str>) -> Result<()> {
    let mut statement = tx.prepare(
        "SELECT position,entry,sequence FROM native_path WHERE session=?1 ORDER BY position",
    )?;
    let mut rows = statement.query([session.to_string()])?;
    let mut previous = None;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let id: ConversationEntryId = row.get::<_, String>(1)?.parse()?;
        let (parent, sequence) = entry_metadata(tx, session, id)?;
        ensure!(
            read_usize(row, 0)? == count && parent == previous && read_usize(row, 2)? == sequence,
            "active path index differs from original ancestry"
        );
        previous = Some(id);
        count += 1;
    }
    ensure!(
        previous.map(|id| id.to_string()).as_deref() == head,
        "active path tail differs from the journal head"
    );
    Ok(())
}
