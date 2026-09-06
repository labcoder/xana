//! Immutable identity checks survive eviction from the execution projection.

use super::*;

pub(super) fn validate_registration(
    tx: &Transaction<'_>,
    id: SessionId,
    record: &SessionRecord,
) -> Result<()> {
    validate_registration_before(tx, id, record, None)
}

/// Reuse immutable registration invariants when replaying a historical prefix.
pub(super) fn validate_registration_before(
    tx: &Transaction<'_>,
    id: SessionId,
    record: &SessionRecord,
    before: Option<usize>,
) -> Result<()> {
    let before = before.map(i64::try_from).transpose()?;
    if let SessionRecord::VisionReceiptRecorded { receipt } = record {
        let mut query = tx.prepare("SELECT r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='vision' AND s.subject=?2 AND (?3 IS NULL OR s.sequence<?3) ORDER BY s.sequence LIMIT 3")?;
        let mut rows = query.query(params![id.to_string(), receipt.operation_id, before])?;
        let mut previous = None;
        while let Some(row) = rows.next()? {
            let body = row.get_ref(0)?.as_blob()?;
            ensure!(
                body.len() <= MAX_RECORD_BYTES,
                "vision receipt exceeds its bound"
            );
            let envelope: RecordEnvelope = serde_json::from_slice(body)?;
            let SessionRecord::VisionReceiptRecorded { receipt: prior } = envelope.record else {
                anyhow::bail!("vision receipt index differs");
            };
            ensure!(
                crate::vision::receipt::valid_transition(previous.as_ref(), &prior, id),
                "vision receipt history differs"
            );
            previous = Some(prior);
        }
        ensure!(
            crate::vision::receipt::valid_transition(previous.as_ref(), receipt, id),
            "vision receipt transition differs from immutable history"
        );
        for source in receipt.sources.iter().chain(receipt.derivative.iter()) {
            let mut query = tx.prepare("SELECT r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='artifact' AND s.subject=?2 AND (?3 IS NULL OR s.sequence<?3) ORDER BY s.sequence LIMIT 2")?;
            let mut rows = query.query(params![id.to_string(), source.artifact_id, before])?;
            let row = rows
                .next()?
                .context("vision source is not registered in this Conversation")?;
            let body = row.get_ref(0)?.as_blob()?;
            ensure!(
                body.len() <= MAX_RECORD_BYTES,
                "vision source record exceeds its bound"
            );
            let envelope: RecordEnvelope = serde_json::from_slice(body)?;
            let SessionRecord::ArtifactRegistered { artifact } = envelope.record else {
                anyhow::bail!("vision source index differs");
            };
            ensure!(
                artifact.reference.id.to_string() == source.artifact_id
                    && artifact.reference.content_hash.as_str() == source.digest
                    && artifact.media_type == source.media_type
                    && artifact.byte_len == source.byte_len
                    && rows.next()?.is_none(),
                "vision source identity differs from its registered artifact"
            );
        }
        if receipt.status == crate::vision::receipt::VisionStatus::NativeSubmitted {
            let admitted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='operation' AND s.subject=?2 AND (?3 IS NULL OR s.sequence<?3) AND json_extract(CAST(r.body AS TEXT),'$.kind')='operation_accepted')", params![id.to_string(), receipt.operation_id, before], |row| row.get(0))?;
            ensure!(
                admitted,
                "native vision submission lacks durable runtime admission"
            );
        }
    }
    if let SessionRecord::OperationStateChanged { operation_id, .. } = record {
        let finished:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='operation' AND s.subject=?2 AND (?3 IS NULL OR s.sequence<?3) AND json_extract(CAST(r.body AS TEXT),'$.kind') IN ('operation_finished','adapter_operation_finished'))",params![id.to_string(),operation_id.to_string(),before],|row|row.get(0))?;
        ensure!(
            !finished,
            "finished historical operation cannot be restarted after execution eviction"
        );
    }
    let unique = match record {
        SessionRecord::OperationAccepted { operation_id, .. }
        | SessionRecord::FiniteOperationAccepted { operation_id, .. }
        | SessionRecord::AdapterOperationAccepted { operation_id, .. } => {
            vec![
                HistorySubject::Operation(*operation_id),
                HistorySubject::ChildOperation(*operation_id),
            ]
        }
        SessionRecord::ArtifactRegistered { artifact } => {
            vec![HistorySubject::Artifact(artifact.reference.id)]
        }
        SessionRecord::ContextViewRegistered { view } => vec![HistorySubject::View(view.id)],
        SessionRecord::NamedValueSet { value } => vec![HistorySubject::NamedValue(value.id)],
        SessionRecord::OrchestrationPlanStarted { start } => {
            vec![HistorySubject::Plan(start.plan_id)]
        }
        SessionRecord::ChildAdmitted { handle } => {
            vec![
                HistorySubject::Child(handle.admission.attribution.agent_id),
                HistorySubject::ChildOperation(handle.admission.attribution.operation_id),
                HistorySubject::Operation(handle.admission.attribution.operation_id),
            ]
        }
        SessionRecord::ChildrenBatchAdmitted { handles } => handles
            .iter()
            .flat_map(|handle| {
                [
                    HistorySubject::Child(handle.admission.attribution.agent_id),
                    HistorySubject::ChildOperation(handle.admission.attribution.operation_id),
                    HistorySubject::Operation(handle.admission.attribution.operation_id),
                ]
            })
            .collect(),
        SessionRecord::ConversationCompacted { checkpoint } => vec![
            HistorySubject::Compaction(checkpoint.id),
            HistorySubject::CompactionOperation(checkpoint.operation_id),
        ],
        SessionRecord::InvocationIntentAppended { intent } => vec![
            HistorySubject::Invocation(intent.invocation_id),
            HistorySubject::Result(intent.result_id),
        ],
        _ => Vec::new(),
    };
    for subject in unique {
        let (kind, key) = subject.key();
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_subjects WHERE session=?1 AND kind=?2 AND subject=?3 AND (?4 IS NULL OR sequence<?4))",params![id.to_string(),kind,key,before],|row|row.get(0))?;
        ensure!(
            !exists,
            "immutable historical identity already exists: {kind}"
        );
    }
    if let SessionRecord::ContextRegistered { context } = record {
        let mut query=tx.prepare("SELECT r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='context' AND s.subject=?2 AND (?3 IS NULL OR s.sequence<?3) ORDER BY s.sequence DESC LIMIT 1")?;
        let mut rows = query.query(params![id.to_string(), context.id.to_string(), before])?;
        let version = if let Some(row) = rows.next()? {
            let body = row.get_ref(0)?.as_blob()?;
            ensure!(
                body.len() <= MAX_RECORD_BYTES,
                "historical context exceeds its record bound"
            );
            let envelope: RecordEnvelope = serde_json::from_slice(body)?;
            let SessionRecord::ContextRegistered { context: previous } = envelope.record else {
                anyhow::bail!("historical context index differs");
            };
            previous
                .version
                .checked_add(1)
                .context("context version overflow")?
        } else {
            1
        };
        ensure!(
            context.version == version,
            "context version differs from immutable history"
        );
    }
    Ok(())
}
