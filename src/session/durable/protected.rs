//! Protected execution hydration, separate from complete historical inspection.

use super::*;
use crate::{session::hydration, storage::ProtectedStore};

pub(crate) fn restore_execution(
    home: &ProtectedStore,
    id: SessionId,
) -> Result<(RestoredSession, usize)> {
    let metadata = home.history_metadata(id)?;
    let checkpoint = home.execution_checkpoint(id)?;
    let (mut state, start) = if let Some((revision, state)) = checkpoint {
        anyhow::ensure!(
            revision <= metadata.revision,
            "execution checkpoint is ahead of its journal"
        );
        (state, revision)
    } else {
        // Existing small protected journals receive a checkpoint on their next
        // owner resume. Legacy recovery snapshots retain their original bounds.
        let loaded = SessionStore::inspect_protected(home, id)?;
        (reduce(&loaded.records)?, loaded.records.len())
    };
    let suffix = home.history_suffix(id, start, metadata.revision)?;
    let mut seen = HashSet::new();
    for (offset, envelope) in suffix.iter().enumerate() {
        super::inspection::dependencies(home, &mut state, &envelope.record)?;
        hydration::validate_bounds(&state)?;
        let proof = match &envelope.record {
            SessionRecord::ConversationCompacted { checkpoint } if state.retained_offset() > 0 => {
                Some(home.active_prefix_proof(id, checkpoint.source_entry_count)?)
            }
            _ => None,
        };
        validate_envelope_with_compaction_proof(
            &state,
            &seen,
            envelope,
            start + offset,
            proof.as_ref(),
        )?;
        apply_validated(&mut state, &envelope.record);
        seen.insert(envelope.record_id);
    }
    anyhow::ensure!(
        state.session_id == id
            && state.thread_id == metadata.thread_id
            && state.workspace_root == metadata.workspace
            && state.head == metadata.head,
        "execution checkpoint metadata differs from its journal"
    );
    // The archived checkpoint must be the exact immutable committed checkpoint,
    // not an independently editable replacement summary.
    if let Some(prefix) = &state.archived_prefix {
        let records =
            home.history_records_for(id, crate::storage::HistorySubject::Compaction(prefix.id))?;
        anyhow::ensure!(records.iter().any(|record| matches!(&record.record, SessionRecord::ConversationCompacted { checkpoint } if checkpoint == prefix)), "archived checkpoint differs from its source record");
    }
    hydration::trim_completed(&mut state);
    hydration::archive_compacted_prefix(&mut state)?;
    hydration::validate_bounds(&state)?;
    let retained = state.conversation_entry_path()?.len();
    anyhow::ensure!(
        retained.checked_add(state.retained_offset()) == Some(metadata.active_entries),
        "execution history count differs from its active path"
    );
    anyhow::ensure!(
        home.history_metadata(id)?.revision == metadata.revision,
        "history changed during execution inspection"
    );
    Ok((state, metadata.revision))
}

pub(super) fn execution_summary(
    home: &ProtectedStore,
    state: &RestoredSession,
    revision: usize,
) -> Result<SessionSummary> {
    let metadata = home.history_metadata(state.session_id)?;
    anyhow::ensure!(
        metadata.revision == revision,
        "history changed before execution summary"
    );
    let path = state.conversation_entry_path()?;
    let inventory = home.history_inventory(state.session_id)?;
    Ok(SessionSummary {
        session_id: state.session_id,
        path: home.database_path(),
        record_count: revision,
        repair_truncate_to: None,
        unfinished: state.unfinished_operations(),
        artifact_count: inventory.artifacts,
        artifact_bytes: inventory.artifact_bytes,
        context_versions: state.contexts.keys().copied().collect(),
        context_version_count: inventory.context_versions,
        child_count: inventory.children,
        bounded_details: true,
        children: state
            .children
            .values()
            .map(|child| child.inspection())
            .collect(),
        compaction_count: inventory.compactions,
        compactions: inventory.recent_compactions,
        branch: state.branch.clone(),
        active_entry_count: metadata.active_entries,
        recent_active_entry_ids: path
            .iter()
            .rev()
            .take(MAX_INSPECTED_ENTRY_IDS)
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
    })
}

impl DurableSession {
    pub(super) fn branch_protected(
        home: ProtectedStore,
        source: SessionId,
        point: ConversationEntryId,
    ) -> Result<(Self, NativeBranchReceipt)> {
        let target = SessionId::new();
        let (store, restored) =
            SessionStore::branch_protected(home.clone(), source, point, target)?;
        let count = restored
            .branch
            .as_ref()
            .context("branch lineage is absent")?
            .shared_entry_count;
        let revision = store
            .protected_revision()
            .context("protected branch revision is absent")?;
        let execution_bytes = hydration::encode(&restored)?.len();
        let session = Self {
            store,
            records: Vec::new(),
            record_ids: HashSet::new(),
            restored,
            artifacts: ArtifactStore::protected(home),
            agent_id: AgentId::for_session(target),
            owner: PrincipalId::new(),
            execution_bytes,
            checkpoint_revision: revision,
            staged_branch_revision: Some(revision),
            compaction_prefix: None,
        };
        Ok((
            session,
            NativeBranchReceipt {
                source_session_id: source,
                source_entry_id: point,
                target_session_id: target,
                shared_entry_count: count,
            },
        ))
    }
    pub(crate) fn inspect_execution_protected(
        home: &ProtectedStore,
        id: SessionId,
    ) -> Result<RestoredSession> {
        restore_execution(home, id).map(|(state, _)| state)
    }

    pub(super) fn checkpoint_execution(&mut self) -> Result<()> {
        let Some(home) = self.store.protected_home() else {
            return Ok(());
        };
        hydration::trim_completed(&mut self.restored);
        // A periodic checkpoint may fall between EntryAppended and HeadMoved.
        // Only explicit compaction/clear boundaries may discard inactive entries;
        // the next head record must still be able to reference a just-written one.
        let revision = self
            .store
            .protected_revision()
            .context("protected writer has no revision")?;
        self.execution_bytes =
            home.save_execution_checkpoint(self.session_id(), revision, &self.restored)?;
        self.checkpoint_revision = revision;
        Ok(())
    }

    pub(super) fn preflight_execution(&mut self, envelope: &RecordEnvelope) -> Result<()> {
        let Some(home) = self.store.protected_home() else {
            return Ok(());
        };
        let before = (
            self.restored.entries.len(),
            self.restored.artifacts.len(),
            self.restored.contexts.len(),
        );
        super::inspection::dependencies(home, &mut self.restored, &envelope.record)?;
        hydration::validate_bounds(&self.restored)?;
        hydration::validate_append_bounds(&self.restored, &envelope.record)?;
        let after = (
            self.restored.entries.len(),
            self.restored.artifacts.len(),
            self.restored.contexts.len(),
        );
        if before != after {
            // Historical dependencies are rare on the hot append path. Account
            // their exact size when loaded, rather than serializing the entire
            // execution cache on every ordinary message or tool event.
            self.execution_bytes = self
                .execution_bytes
                .max(hydration::encode(&self.restored)?.len());
        }
        if matches!(
            envelope.record,
            SessionRecord::ConversationEntryAppended { .. }
        ) {
            anyhow::ensure!(
                self.restored.entries.len() < hydration::MAX_EXECUTION_ENTRIES,
                "retained execution history needs compaction before appending another message"
            );
        }
        let shrinking = matches!(
            envelope.record,
            SessionRecord::ConversationCompacted { .. }
                | SessionRecord::ThreadHeadMoved { head: None, .. }
        );
        let reserve = serde_json::to_vec(envelope)?.len().saturating_mul(4);
        anyhow::ensure!(
            shrinking
                || self.execution_bytes.saturating_add(reserve) <= hydration::MAX_EXECUTION_BYTES,
            "active execution state exceeds its byte bound; compact before continuing, unresolved state was retained"
        );
        Ok(())
    }
}
