//! Exact selected-operation inspection, with only its referenced immutable data.

use super::*;
use crate::{
    operation::InvocationOutcome,
    storage::{HistorySubject, ProtectedStore},
};

impl DurableSession {
    pub(crate) fn inspect_operation_protected(
        home: &ProtectedStore,
        id: SessionId,
        operation: OperationId,
    ) -> Result<Option<crate::session::RestoredOperation>> {
        protected_operation(home, id, operation)
    }

    pub(crate) fn verify_child_protected(
        home: &ProtectedStore,
        id: SessionId,
        child: AgentId,
    ) -> Result<()> {
        let child_records = home.history_records_for(id, HistorySubject::Child(child))?;
        let first = child_records
            .first()
            .context("child evidence is unavailable")?;
        let parents: HashSet<_> = match &first.record {
            SessionRecord::ChildAdmitted { handle } => {
                [handle.admission.attribution.parent_operation_id]
                    .into_iter()
                    .collect()
            }
            SessionRecord::ChildrenBatchAdmitted { handles } => handles
                .iter()
                .map(|handle| handle.admission.attribution.parent_operation_id)
                .collect(),
            _ => bail!("child history lacks an admission"),
        };
        let first_sequence = home.history_record_sequence(id, first.record_id)?;
        let mut records = Vec::new();
        let mut bytes = 0usize;
        for parent in parents {
            for record in home.history_records_for(id, HistorySubject::Operation(parent))? {
                let sequence = home.history_record_sequence(id, record.record_id)?;
                if sequence < first_sequence {
                    bytes = bytes
                        .checked_add(serde_json::to_vec(&record)?.len())
                        .context("child inspection size overflow")?;
                    anyhow::ensure!(
                        records.len() < 4096
                            && bytes <= super::super::hydration::MAX_EXECUTION_BYTES,
                        "child prerequisites exceed bounded inspection"
                    );
                    records.push((sequence, record));
                }
            }
        }
        for record in child_records {
            bytes = bytes
                .checked_add(serde_json::to_vec(&record)?.len())
                .context("child inspection size overflow")?;
            anyhow::ensure!(
                records.len() < 4096 && bytes <= super::super::hydration::MAX_EXECUTION_BYTES,
                "child evidence exceeds bounded inspection"
            );
            records.push((home.history_record_sequence(id, record.record_id)?, record));
        }
        records.sort_by_key(|(sequence, _)| *sequence);
        let metadata = home.history_metadata(id)?;
        let mut state = reduce(&[RecordEnvelope::new(
            id,
            SessionRecord::SessionCreated {
                thread_id: metadata.thread_id,
                workspace_root: metadata.workspace,
            },
        )])?;
        let mut seen = HashSet::new();
        for (sequence, record) in records {
            if seen.contains(&record.record_id) {
                continue;
            }
            dependencies(home, &mut state, &record.record)?;
            super::super::validate_envelope(&state, &seen, &record, sequence)?;
            apply_validated(&mut state, &record.record);
            seen.insert(record.record_id);
        }
        anyhow::ensure!(
            state.children.contains_key(&child),
            "child identity differs from its inspection"
        );
        Ok(())
    }
    pub(crate) fn inspect_operation(
        data_dir: &Path,
        session_id: SessionId,
        operation_id: OperationId,
    ) -> Result<crate::session::RestoredOperation> {
        if let Some(home) = ProtectedStore::configured(data_dir)? {
            return protected_operation(&home, session_id, operation_id)?
                .context("operation does not exist in this Conversation");
        }
        Self::inspect_restored(data_dir, session_id)?
            .1
            .operation_details
            .remove(&operation_id)
            .context("operation does not exist in this Conversation")
    }

    pub(crate) fn inspect_stored_operation(
        &self,
        operation_id: OperationId,
    ) -> Result<Option<crate::session::RestoredOperation>> {
        if let Some(operation) = self.restored.operation_details.get(&operation_id) {
            return Ok(Some(operation.clone()));
        }
        match self.store.protected_home() {
            Some(home) => protected_operation(home, self.session_id(), operation_id),
            None => Ok(None),
        }
    }
}

fn protected_operation(
    home: &ProtectedStore,
    id: SessionId,
    operation: OperationId,
) -> Result<Option<crate::session::RestoredOperation>> {
    let records = home.history_records_for(id, HistorySubject::Operation(operation))?;
    if records.is_empty() {
        return Ok(None);
    }
    let meta = home.history_metadata(id)?;
    let created = RecordEnvelope::new(
        id,
        SessionRecord::SessionCreated {
            thread_id: meta.thread_id,
            workspace_root: meta.workspace,
        },
    );
    let mut state = reduce(&[created])?;
    let mut seen = HashSet::new();
    for (index, record) in records.iter().enumerate() {
        dependencies(home, &mut state, &record.record)?;
        super::super::validate_envelope(&state, &seen, record, index + 1)?;
        apply_validated(&mut state, &record.record);
        seen.insert(record.record_id);
    }
    Ok(state.operation_details.remove(&operation))
}

pub(super) fn dependencies(
    home: &ProtectedStore,
    state: &mut RestoredSession,
    record: &SessionRecord,
) -> Result<()> {
    match record {
        SessionRecord::OperationAccepted { input_entry_id, .. }
        | SessionRecord::FiniteOperationAccepted { input_entry_id, .. } => {
            entry(home, state, *input_entry_id)?
        }
        SessionRecord::StepStarted {
            assistant_entry_id, ..
        } => entry(home, state, *assistant_entry_id)?,
        SessionRecord::InvocationResultAppended { result } => {
            if let InvocationOutcome::Completed { output } = &result.outcome {
                value(home, state, output)?;
            }
        }
        SessionRecord::NamedValueSet { value: record } => value(home, state, &record.value)?,
        SessionRecord::ChildReportCommitted { report } => {
            if let crate::orchestration::ChildReportReference::Artifact {
                artifact: reference,
                ..
            } = &report.reference
            {
                artifact(home, state, reference.id)?;
            }
        }
        SessionRecord::ConversationEntryAppended { entry } => {
            for item in entry.message.artifacts() {
                artifact(home, state, item.reference.id)?;
            }
        }
        SessionRecord::ContextRegistered { context } => {
            artifact(home, state, context.artifact.id)?;
            if context.version > 1 {
                value(
                    home,
                    state,
                    &DurableValueRef::Context {
                        id: context.id,
                        version: context.version - 1,
                    },
                )?;
            }
        }
        SessionRecord::ContextViewRegistered { view } => value(
            home,
            state,
            &DurableValueRef::Context {
                id: view.source,
                version: view.source_version,
            },
        )?,
        SessionRecord::NamedContextSet {
            context_id,
            version,
            ..
        } => value(
            home,
            state,
            &DurableValueRef::Context {
                id: *context_id,
                version: *version,
            },
        )?,
        _ => {}
    }
    Ok(())
}

fn entry(
    home: &ProtectedStore,
    state: &mut RestoredSession,
    id: ConversationEntryId,
) -> Result<()> {
    if state.entries.contains_key(&id) {
        return Ok(());
    }
    let mut records = home.history_records_for(state.session_id, HistorySubject::Entry(id))?;
    anyhow::ensure!(
        records.len() == 1,
        "operation entry evidence is missing or duplicated"
    );
    let SessionRecord::ConversationEntryAppended { entry } = records.remove(0).record else {
        bail!("entry index differs");
    };
    state.entries.insert(id, entry);
    Ok(())
}

fn value(
    home: &ProtectedStore,
    state: &mut RestoredSession,
    value: &DurableValueRef,
) -> Result<()> {
    match value {
        DurableValueRef::InlineJson(_) => {}
        DurableValueRef::Artifact(reference) => artifact(home, state, reference.id)?,
        DurableValueRef::Context { id, version } => {
            if !state.contexts.contains_key(&(*id, *version)) {
                let context = home
                    .history_context_version(state.session_id, *id, *version)?
                    .context("operation context evidence is missing")?;
                artifact(home, state, context.artifact.id)?;
                state.contexts.insert((*id, *version), context);
            }
        }
    }
    Ok(())
}

fn artifact(
    home: &ProtectedStore,
    state: &mut RestoredSession,
    id: crate::identity::ArtifactId,
) -> Result<()> {
    if state.artifacts.contains_key(&id) {
        return Ok(());
    }
    let mut records = home.history_records_for(state.session_id, HistorySubject::Artifact(id))?;
    anyhow::ensure!(
        records.len() == 1,
        "operation artifact evidence is missing or duplicated"
    );
    let SessionRecord::ArtifactRegistered { artifact } = records.remove(0).record else {
        bail!("artifact index differs");
    };
    state.artifacts.insert(id, artifact);
    Ok(())
}
