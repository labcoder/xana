//! Bounded protected execution checkpoints; immutable history stays queryable.

use super::RestoredSession;
use crate::native_runtime::OperationState;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub(crate) const MAX_EXECUTION_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_EXECUTION_ENTRIES: usize = 2048;
const MAX_EXECUTION_OBJECTS: usize = 4096;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionCheckpoint {
    pub(crate) version: u32,
    pub(crate) state: RestoredSession,
}

/// Eviction changes only the disposable execution projection, never journal rows.
/// Exact historical inspection crosses the protected owner's query interface.
pub(crate) fn trim_completed(state: &mut RestoredSession) {
    let completed: HashSet<_> = state
        .operation_details
        .iter()
        .filter(|(id, operation)| {
            operation.finished.is_some()
                && matches!(state.operations.get(id), Some(OperationState::Finished(_)))
        })
        .map(|(id, _)| *id)
        .collect();
    state
        .operation_details
        .retain(|id, _| !completed.contains(id));
    state.operations.retain(|id, _| !completed.contains(id));
    state
        .orchestration_plans
        .retain(|_, plan| !completed.contains(&plan.operation_id));
    state
        .named_values
        .retain(|_, value| !completed.contains(&value.operation_id));
    state.children.retain(|_, child| {
        !(child.handle.lifecycle.is_terminal()
            && completed.contains(&child.handle.admission.attribution.parent_operation_id))
    });
    // Audit history is durable evidence, not an input to future transition validation.
    state.audits.clear();
}

/// Called only at a completed compaction/clear boundary, not between a resource
/// registration and the next record that links it into an operation/context.
pub(crate) fn trim_inactive_objects(state: &mut RestoredSession) {
    use crate::operation::{DurableValueRef, InvocationOutcome};
    let mut contexts: HashSet<_> = state.named_context.values().copied().collect();
    let mut artifacts = HashSet::new();
    let mut retain_value = |value: &DurableValueRef| match value {
        DurableValueRef::Artifact(reference) => {
            artifacts.insert(reference.id);
        }
        DurableValueRef::Context { id, version } => {
            contexts.insert((*id, *version));
        }
        DurableValueRef::InlineJson(_) => {}
    };
    for value in state.named_values.values() {
        retain_value(&value.value);
    }
    for operation in state.operation_details.values() {
        for result in operation.results.values() {
            if let InvocationOutcome::Completed { output } = &result.outcome {
                retain_value(output);
            }
        }
    }
    for entry in state.entries.values() {
        artifacts.extend(entry.message.artifacts().map(|item| item.reference.id));
    }
    state.contexts.retain(|key, _| contexts.contains(key));
    artifacts.extend(state.contexts.values().map(|context| context.artifact.id));
    state.artifacts.retain(|id, _| artifacts.contains(id));
    state.views.clear();
}

/// Replace only a verified active prefix with its continuation checkpoint.
/// Unfinished operations keep their exact entry/ancestor evidence even when it
/// is no longer on the visible path; bounds may refuse, never silently discard it.
pub(crate) fn archive_compacted_prefix(state: &mut RestoredSession) -> Result<()> {
    let checkpoint = state.active_compaction()?.cloned();
    if checkpoint.is_none() && state.head.is_some() {
        return Ok(());
    }
    let mut keep = HashSet::new();
    if let Some(checkpoint) = &checkpoint {
        let retained = checkpoint
            .source_entry_count
            .checked_sub(state.retained_offset())
            .ok_or_else(|| anyhow::anyhow!("active checkpoint precedes the archived prefix"))?;
        keep.extend(
            state
                .conversation_entry_path()?
                .iter()
                .skip(retained)
                .map(|entry| entry.id),
        );
    }
    for operation in state
        .operation_details
        .values()
        .filter(|operation| operation.finished.is_none())
    {
        for origin in
            std::iter::once(operation.input_entry_id).chain(operation.steps.values().copied())
        {
            let mut cursor = Some(origin);
            let mut visited = HashSet::new();
            while let Some(id) = cursor {
                if state
                    .archived_prefix
                    .as_ref()
                    .is_some_and(|prefix| prefix.source_end == id)
                {
                    break;
                }
                ensure!(
                    visited.insert(id),
                    "unfinished operation ancestry contains a cycle"
                );
                let entry = state.entries.get(&id).ok_or_else(|| {
                    anyhow::anyhow!("unfinished operation entry evidence is unavailable")
                })?;
                keep.insert(id);
                cursor = entry.parent;
            }
        }
    }
    state.entries.retain(|id, _| keep.contains(id));
    state.compactions.clear();
    if let Some(checkpoint) = &checkpoint {
        state.compactions.push(checkpoint.clone());
    }
    state.archived_prefix = checkpoint;
    Ok(())
}

pub(crate) fn validate_bounds(state: &RestoredSession) -> Result<()> {
    ensure!(
        state.entries.len() <= MAX_EXECUTION_ENTRIES,
        "retained execution history needs compaction before another turn"
    );
    ensure!(
        [
            state.operations.len(),
            state.operation_details.len(),
            state.artifacts.len(),
            state.contexts.len(),
            state.views.len(),
            state.named_context.len(),
            state.named_values.len(),
            state.orchestration_plans.len(),
            state.children.len(),
            state.compactions.len(),
            state.audits.len(),
        ]
        .into_iter()
        .all(|count| count <= MAX_EXECUTION_OBJECTS),
        "active execution state exceeds its object bound; unresolved state was not discarded"
    );
    Ok(())
}

/// Refuse the record that would overflow a retained map, before journal commit.
/// Count checks do not clone or serialize the cache on the normal append path.
pub(crate) fn validate_append_bounds(
    state: &RestoredSession,
    record: &super::SessionRecord,
) -> Result<()> {
    use super::SessionRecord as R;
    let (count, additional) = match record {
        R::OperationAccepted { .. } => {
            (state.operations.len().max(state.operation_details.len()), 1)
        }
        R::OperationStateChanged { operation_id, .. } => (
            state.operations.len(),
            usize::from(!state.operations.contains_key(operation_id)),
        ),
        R::ArtifactRegistered { .. } => (state.artifacts.len(), 1),
        R::ContextRegistered { .. } => (state.contexts.len(), 1),
        R::ContextViewRegistered { .. } => (state.views.len(), 1),
        R::NamedContextSet { name, .. } => (
            state.named_context.len(),
            usize::from(!state.named_context.contains_key(name)),
        ),
        R::NamedValueSet { .. } => (state.named_values.len(), 1),
        R::OrchestrationPlanStarted { .. } => (state.orchestration_plans.len(), 1),
        R::ChildAdmitted { .. } => (state.children.len(), 1),
        R::ChildrenBatchAdmitted { handles } => (state.children.len(), handles.len()),
        R::PermissionAudited { .. } => (state.audits.len(), 1),
        R::ConversationCompacted { .. } => (state.compactions.len(), 1),
        _ => (0, 0),
    };
    ensure!(
        count.saturating_add(additional) <= MAX_EXECUTION_OBJECTS,
        "next record would exceed the retained object bound; compact before continuing"
    );
    Ok(())
}

pub(crate) fn encode(state: &RestoredSession) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    struct BorrowedCheckpoint<'a> {
        version: u32,
        state: &'a RestoredSession,
    }
    validate_bounds(state)?;
    let body = serde_json::to_vec(&BorrowedCheckpoint { version: 1, state })?;
    ensure!(
        body.len() <= MAX_EXECUTION_BYTES,
        "active execution checkpoint exceeds its byte bound"
    );
    Ok(body)
}

pub(crate) fn decode(body: &[u8]) -> Result<RestoredSession> {
    ensure!(
        body.len() <= MAX_EXECUTION_BYTES,
        "execution checkpoint exceeds its byte bound"
    );
    let checkpoint: ExecutionCheckpoint = serde_json::from_slice(body)?;
    ensure!(
        checkpoint.version == 1,
        "unsupported execution checkpoint version"
    );
    validate_bounds(&checkpoint.state)?;
    Ok(checkpoint.state)
}

pub(super) mod context_map {
    use crate::{context::persisted::ContextRecord, identity::ContextId};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub(crate) fn serialize<S: Serializer>(
        value: &BTreeMap<(ContextId, u64), ContextRecord>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<(ContextId, u64), ContextRecord>, D::Error> {
        let entries = Vec::<((ContextId, u64), ContextRecord)>::deserialize(deserializer)?;
        let count = entries.len();
        let map: BTreeMap<_, _> = entries.into_iter().collect();
        if map.len() != count {
            return Err(serde::de::Error::custom(
                "duplicate checkpoint context identity",
            ));
        }
        Ok(map)
    }
}
