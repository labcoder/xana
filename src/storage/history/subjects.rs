//! Exact typed historical-object lookup without hydrating an entire journal.

use super::*;
use crate::identity::{
    AgentId, ArtifactId, CompactionId, ContextId, ContextViewId, ConversationEntryId, NamedValueId,
    OperationId, OrchestrationPlanId,
};
use serde::{Deserialize, Serialize};

pub(in crate::storage) const EXECUTION_SCHEMA: &str = "
CREATE TABLE native_subjects(
 session TEXT NOT NULL, kind TEXT NOT NULL, subject TEXT NOT NULL, sequence INTEGER NOT NULL,
 PRIMARY KEY(session,kind,subject,sequence),
 FOREIGN KEY(session,sequence) REFERENCES native_records(session,sequence) ON DELETE CASCADE);
CREATE INDEX native_subjects_record ON native_subjects(session,sequence);
CREATE TABLE native_execution_checkpoints(
 session TEXT PRIMARY KEY REFERENCES native_sessions(id) ON DELETE CASCADE,
 revision INTEGER NOT NULL, prefix_digest TEXT NOT NULL, body BLOB NOT NULL);
CREATE TABLE native_record_digests(
 session TEXT NOT NULL, sequence INTEGER NOT NULL, digest TEXT NOT NULL,
 PRIMARY KEY(session,sequence),
 FOREIGN KEY(session,sequence) REFERENCES native_records(session,sequence) ON DELETE CASCADE);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HistorySubject {
    Entry(ConversationEntryId),
    Operation(OperationId),
    Artifact(ArtifactId),
    Context(ContextId),
    View(ContextViewId),
    NamedValue(NamedValueId),
    Child(AgentId),
    Compaction(CompactionId),
    Plan(OrchestrationPlanId),
    Invocation(crate::identity::ToolInvocationId),
    Result(crate::identity::ToolResultId),
    CompactionOperation(OperationId),
    ChildOperation(OperationId),
    Completion(OperationId),
    Vision(OperationId),
}

impl HistorySubject {
    pub(super) fn key(&self) -> (&'static str, String) {
        match self {
            Self::Entry(id) => ("entry", id.to_string()),
            Self::Operation(id) => ("operation", id.to_string()),
            Self::Artifact(id) => ("artifact", id.to_string()),
            Self::Context(id) => ("context", id.to_string()),
            Self::View(id) => ("view", id.to_string()),
            Self::NamedValue(id) => ("value", id.to_string()),
            Self::Child(id) => ("child", id.to_string()),
            Self::Compaction(id) => ("compaction", id.to_string()),
            Self::Plan(id) => ("plan", id.to_string()),
            Self::Invocation(id) => ("invocation", id.to_string()),
            Self::Result(id) => ("result", id.to_string()),
            Self::CompactionOperation(id) => ("compaction_operation", id.to_string()),
            Self::ChildOperation(id) => ("child_operation", id.to_string()),
            Self::Completion(id) => ("completion", id.to_string()),
            Self::Vision(id) => ("vision", id.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HistoryMetadata {
    pub(crate) revision: usize,
    pub(crate) bytes: usize,
    pub(crate) thread_id: crate::identity::ThreadId,
    pub(crate) workspace: std::path::PathBuf,
    pub(crate) head: Option<ConversationEntryId>,
    pub(crate) active_entries: usize,
}

impl ProtectedStore {
    /// Hash original active entries through the selected prefix and its first
    /// retained entry; summaries are never substituted for immutable sources.
    pub(crate) fn active_prefix_proof(
        &self,
        id: SessionId,
        count: usize,
    ) -> Result<crate::session::compaction::CompactionSourceProof> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let metadata = metadata(&tx, id)?;
            ensure!(count > 0 && count < metadata.active_entries, "compaction prefix is not an active source range");
            let mut proof = crate::session::compaction::CompactionSourceProofBuilder::new(id, count);
            let mut query = tx.prepare("SELECT p.position,p.entry,r.body FROM native_path p JOIN native_records r ON r.session=p.session AND r.sequence=p.sequence WHERE p.session=?1 AND p.position<=?2 ORDER BY p.position")?;
            let mut rows = query.query(params![id.to_string(), i64::try_from(count)?])?;
            let mut previous = None;
            let mut position = 0usize;
            while let Some(row) = rows.next()? {
                ensure!(read_usize(row, 0)? == position, "compaction source positions are discontinuous");
                let body = row.get_ref(2)?.as_blob()?;
                ensure!(body.len() <= MAX_RECORD_BYTES, "compaction source exceeds its record bound");
                let record: RecordEnvelope = serde_json::from_slice(body)?;
                ensure!(record.session_id == id && record.version == SESSION_RECORD_VERSION, "compaction source identity differs");
                let SessionRecord::ConversationEntryAppended { entry } = record.record else { anyhow::bail!("compaction source is not a Conversation entry"); };
                ensure!(entry.id.to_string() == row.get::<_, String>(1)? && entry.parent == previous, "compaction source ancestry differs");
                proof.push(entry.id, &entry.message)?;
                previous = Some(entry.id);
                position += 1;
            }
            proof.finish()
        })
    }

    pub(crate) fn history_metadata(&self, id: SessionId) -> Result<HistoryMetadata> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            metadata(&tx, id)
        })
    }

    /// A bounded suffix after a validated checkpoint; no whole-journal allocation.
    pub(crate) fn history_suffix(
        &self,
        id: SessionId,
        start: usize,
        expected_revision: usize,
    ) -> Result<Vec<RecordEnvelope>> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let current = metadata(&tx, id)?;
            ensure!(current.revision == expected_revision && start <= expected_revision, "history changed before execution hydration");
            ensure!(expected_revision - start <= MAX_SESSION_RECORDS, "execution checkpoint is too old; explicit streaming repair is required");
            let mut query = tx.prepare("SELECT sequence,body FROM native_records WHERE session=?1 AND sequence>=?2 ORDER BY sequence")?;
            let mut rows = query.query(params![id.to_string(),i64::try_from(start)?])?;
            let mut output = Vec::new();
            let mut bytes = 0usize;
            while let Some(row) = rows.next()? {
                ensure!(read_usize(row,0)? == start + output.len(), "execution suffix is discontinuous");
                let body=row.get_ref(1)?.as_blob()?;
                bytes = bytes.checked_add(body.len()).context("execution suffix size overflow")?;
                ensure!(body.len() <= MAX_RECORD_BYTES && bytes <= MAX_SESSION_BYTES,"execution suffix exceeds its byte bound");
                let record:RecordEnvelope=serde_json::from_slice(body)?;
                ensure!(record.session_id==id && record.version==SESSION_RECORD_VERSION,"execution suffix identity differs");
                output.push(record);
            }
            ensure!(output.len()+start==expected_revision,"execution suffix is incomplete");
            Ok(output)
        })
    }

    /// Exact selected-object records; exceeding a bound is an error, never a partial object.
    pub(crate) fn history_records_for(
        &self,
        id: SessionId,
        subject: HistorySubject,
    ) -> Result<Vec<RecordEnvelope>> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let (kind, key) = subject.key();
            let mut query = tx.prepare("SELECT r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind=?2 AND s.subject=?3 ORDER BY s.sequence LIMIT 4097")?;
            let mut rows = query.query(params![id.to_string(), kind, key])?;
            let mut output = Vec::new();
            let mut bytes = 0usize;
            while let Some(row) = rows.next()? {
                let body = row.get_ref(0)?.as_blob()?;
                bytes = bytes.checked_add(body.len()).context("historical object size overflow")?;
                ensure!(output.len() < 4096 && body.len() <= MAX_RECORD_BYTES && bytes <= MAX_SESSION_BYTES, "historical object exceeds its inspection bound");
                let record: RecordEnvelope = serde_json::from_slice(body)?;
                ensure!(record.session_id == id && record.version == SESSION_RECORD_VERSION && record_subjects(&record.record).contains(&subject), "historical object index differs from its record");
                output.push(record);
            }
            Ok(output)
        })
    }

    pub(crate) fn history_context_version(
        &self,
        id: SessionId,
        context: ContextId,
        version: u64,
    ) -> Result<Option<crate::context::persisted::ContextRecord>> {
        self.with_database(|db| {
            let mut query=db.connection.prepare("SELECT r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='context' AND s.subject=?2 AND json_extract(CAST(r.body AS TEXT),'$.data.context.version')=?3 LIMIT 2")?;
            let mut rows=query.query(params![id.to_string(),context.to_string(),i64::try_from(version)?])?;
            let Some(row)=rows.next()? else {return Ok(None);};
            let body=row.get_ref(0)?.as_blob()?;
            ensure!(body.len()<=MAX_RECORD_BYTES,"historical context exceeds its record bound");
            let record:RecordEnvelope=serde_json::from_slice(body)?;
            let SessionRecord::ContextRegistered {context:found}=record.record else {anyhow::bail!("context index differs");};
            ensure!(record.session_id==id && found.id==context && found.version==version && rows.next()?.is_none(),"context identity is inconsistent");
            Ok(Some(found))
        })
    }

    /// Stream one coherent revision through a trusted bounded consumer.
    #[cfg(test)]
    pub(crate) fn visit_history(
        &self,
        id: SessionId,
        mut visit: impl FnMut(usize, &RecordEnvelope) -> Result<()>,
    ) -> Result<HistoryMetadata> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let metadata = metadata(&tx, id)?;
            let mut query = tx.prepare(
                "SELECT sequence,body FROM native_records WHERE session=?1 ORDER BY sequence",
            )?;
            let mut rows = query.query([id.to_string()])?;
            let mut sequence = 0usize;
            let mut length = 0usize;
            while let Some(row) = rows.next()? {
                ensure!(
                    read_usize(row, 0)? == sequence,
                    "historical journal sequence differs"
                );
                let bytes = row.get_ref(1)?.as_blob()?;
                ensure!(
                    bytes.len() <= MAX_RECORD_BYTES,
                    "historical record exceeds its bound"
                );
                let record: RecordEnvelope = serde_json::from_slice(bytes)?;
                ensure!(
                    record.session_id == id && record.version == SESSION_RECORD_VERSION,
                    "historical record identity differs"
                );
                visit(sequence, &record)?;
                sequence += 1;
                length = length
                    .checked_add(bytes.len() + 1)
                    .context("historical journal size overflow")?;
            }
            ensure!(
                sequence == metadata.revision && length == metadata.bytes,
                "historical journal length or revision differs"
            );
            Ok(metadata)
        })
    }
}

pub(super) fn metadata(db: &rusqlite::Connection, id: SessionId) -> Result<HistoryMetadata> {
    let (revision, bytes, thread, workspace, head): (usize, usize, String, String, Option<String>) =
        db.query_row(
            "SELECT revision,bytes,root_thread,workspace,head FROM native_sessions WHERE id=?1",
            [id.to_string()],
            |row| {
                Ok((
                    read_usize(row, 0)?,
                    read_usize(row, 1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    let position: Option<usize> = db
        .query_row(
            "SELECT position FROM native_path WHERE session=?1 ORDER BY position DESC LIMIT 1",
            [id.to_string()],
            |row| read_usize(row, 0),
        )
        .optional()?;
    Ok(HistoryMetadata {
        revision,
        bytes,
        thread_id: thread.parse()?,
        workspace: workspace.into(),
        head: head.map(|head| head.parse()).transpose()?,
        active_entries: position
            .map(|position| {
                position
                    .checked_add(1)
                    .context("historical position overflow")
            })
            .transpose()?
            .unwrap_or(0),
    })
}

pub(super) fn index_record(
    tx: &Transaction<'_>,
    record: &RecordEnvelope,
    sequence: usize,
    body: &[u8],
) -> Result<()> {
    let previous = if sequence == 0 {
        String::new()
    } else {
        tx.query_row(
            "SELECT digest FROM native_record_digests WHERE session=?1 AND sequence=?2",
            params![record.session_id.to_string(), i64::try_from(sequence - 1)?],
            |row| row.get::<_, String>(0),
        )?
    };
    let digest = record_digest(&previous, body);
    tx.execute(
        "INSERT INTO native_record_digests VALUES(?1,?2,?3)",
        params![
            record.session_id.to_string(),
            i64::try_from(sequence)?,
            digest
        ],
    )?;
    for subject in record_subjects(&record.record) {
        let (kind, key) = subject.key();
        tx.execute(
            "INSERT OR IGNORE INTO native_subjects VALUES(?1,?2,?3,?4)",
            params![
                record.session_id.to_string(),
                kind,
                key,
                i64::try_from(sequence)?
            ],
        )?;
    }
    Ok(())
}

pub(super) fn record_digest(previous: &str, body: &[u8]) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"xana-native-journal-v1\0");
    hash.update(previous.as_bytes());
    hash.update(&(body.len() as u64).to_le_bytes());
    hash.update(body);
    hash.finalize().to_hex().to_string()
}

pub(in crate::storage) fn migrate_execution_index(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(EXECUTION_SCHEMA)?;
    let mut query =
        tx.prepare("SELECT session,sequence,body FROM native_records ORDER BY session,sequence")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let body = row.get_ref(2)?.as_blob()?;
        ensure!(
            body.len() <= MAX_RECORD_BYTES,
            "historical migration record exceeds its bound"
        );
        let record: RecordEnvelope = serde_json::from_slice(body)?;
        ensure!(
            record.session_id.to_string() == row.get::<_, String>(0)?
                && record.version == SESSION_RECORD_VERSION,
            "historical migration identity differs"
        );
        index_record(tx, &record, read_usize(row, 1)?, body)?;
    }
    Ok(())
}

pub(super) fn record_subjects(record: &SessionRecord) -> Vec<HistorySubject> {
    use HistorySubject as S;
    match record {
        SessionRecord::VisionReceiptRecorded { receipt } => {
            receipt.operation().map(S::Vision).into_iter().collect()
        }
        SessionRecord::ConversationEntryAppended { entry } => vec![S::Entry(entry.id)],
        SessionRecord::OperationStateChanged { operation_id, .. }
        | SessionRecord::OperationAccepted { operation_id, .. }
        | SessionRecord::FiniteOperationAccepted { operation_id, .. }
        | SessionRecord::AdapterOperationAccepted { operation_id, .. }
        | SessionRecord::StepStarted { operation_id, .. }
        | SessionRecord::OperationSuspended { operation_id, .. }
        | SessionRecord::OperationFinished { operation_id, .. }
        | SessionRecord::AdapterOperationFinished { operation_id, .. }
        | SessionRecord::RecoveryDecisionAppended { operation_id, .. } => {
            vec![S::Operation(*operation_id)]
        }
        SessionRecord::InvocationIntentAppended { intent } => {
            vec![
                S::Operation(intent.operation_id),
                S::Invocation(intent.invocation_id),
                S::Result(intent.result_id),
            ]
        }
        SessionRecord::CompletionEvidenceRecorded { evidence } => vec![
            S::Operation(evidence.generation),
            S::Completion(evidence.generation),
        ],
        SessionRecord::InvocationResultAppended { result } => {
            vec![S::Operation(result.operation_id)]
        }
        SessionRecord::RoundBudgetDecisionAppended { decision } => {
            vec![S::Operation(decision.operation_id)]
        }
        SessionRecord::NamedValueSet { value } => {
            vec![S::Operation(value.operation_id), S::NamedValue(value.id)]
        }
        SessionRecord::ArtifactRegistered { artifact } => vec![S::Artifact(artifact.reference.id)],
        SessionRecord::ContextRegistered { context } => vec![S::Context(context.id)],
        SessionRecord::ContextViewRegistered { view } => vec![S::View(view.id)],
        SessionRecord::OrchestrationPlanStarted { start } => {
            vec![S::Operation(start.operation_id), S::Plan(start.plan_id)]
        }
        SessionRecord::ChildAdmitted { handle } => {
            vec![
                S::Child(handle.admission.attribution.agent_id),
                S::ChildOperation(handle.admission.attribution.operation_id),
            ]
        }
        SessionRecord::ChildrenBatchAdmitted { handles } => handles
            .iter()
            .flat_map(|handle| {
                [
                    S::Child(handle.admission.attribution.agent_id),
                    S::ChildOperation(handle.admission.attribution.operation_id),
                ]
            })
            .collect(),
        SessionRecord::ChildLifecycleChanged { agent_id, .. } => vec![S::Child(*agent_id)],
        SessionRecord::ChildReportCommitted { report } => {
            vec![S::Child(report.attribution.agent_id)]
        }
        SessionRecord::ConversationCompacted { checkpoint } => vec![
            S::Compaction(checkpoint.id),
            S::CompactionOperation(checkpoint.operation_id),
        ],
        SessionRecord::SessionCreated { .. }
        | SessionRecord::ConversationBranched { .. }
        | SessionRecord::ThreadHeadMoved { .. }
        | SessionRecord::PermissionAudited { .. }
        | SessionRecord::NamedContextSet { .. } => Vec::new(),
    }
}
