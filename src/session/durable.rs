use super::{
    COMPACTION_CHECKPOINT_VERSION, CompactionCheckpoint, CompactionError, CompactionReason,
    CompactionSummary, ConversationEntry, ConversationPage, LoadedSession, NativeBranchLineage,
    PromptContinuation, RecordEnvelope, RestoredSession, SessionRecord, SessionStore,
    apply_validated, reduce, validate_envelope_with_compaction_proof,
};
mod adapter;
mod inspection;
mod preparation;
mod protected;
use crate::{
    artifact::{ArtifactStore, ContentHash},
    context::{
        ContextSource, SourceOrigin, SourceProvenance, TransientSourceId,
        TrustClass as PromptTrustClass, canonical_text, estimate_tokens,
        persisted::{
            ContextKind, ContextRecord, ContextViewRecord, MaterializationBudget, Provenance,
            TrustClass, ViewSelector,
        },
        read_project_instructions,
    },
    identity::{
        AgentId, CompactionId, ContextId, ContextViewId, ConversationEntryId, OperationId,
        PrincipalId, SessionId, ThreadId,
    },
    message::Message,
    native_runtime::OperationState,
    operation::{DurableValueRef, MAX_INLINE_VALUE_BYTES},
    orchestration::ChildInspection,
    permission::PermissionAuditFact,
    prompt::PromptBudgetPlan,
};
use anyhow::{Context, Result, bail};
pub(crate) use preparation::CompactionPreparation;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

const PROJECT_CONTEXT_NAME: &str = "project:AGENTS.md";
const MAX_INSPECTED_COMPACTIONS: usize = 64;
const MAX_INSPECTED_ENTRY_IDS: usize = 128;
const PROJECT_INSTRUCTIONS: &str = crate::context::PROJECT_INSTRUCTIONS;
const MAX_PROJECT_SOURCE_BYTES: usize = crate::context::MAX_PROJECT_SOURCE_BYTES;
const PROJECT_VIEW_BUDGET: MaterializationBudget = MaterializationBudget {
    // Authored instructions cannot be clipped before the compiler sees them.
    // The model-aware request budget decides whether this complete source fits.
    max_bytes: MAX_PROJECT_SOURCE_BYTES,
    max_estimated_tokens: MAX_PROJECT_SOURCE_BYTES,
};

pub(crate) struct DurableSession {
    store: SessionStore,
    records: Vec<RecordEnvelope>,
    record_ids: HashSet<crate::identity::RecordId>,
    restored: RestoredSession,
    artifacts: ArtifactStore,
    agent_id: AgentId,
    owner: PrincipalId,
    // Protected journals retain an execution projection, not an ever-growing
    // copy of every committed record; legacy files keep their existing bound.
    execution_bytes: usize,
    checkpoint_revision: usize,
    staged_branch_revision: Option<usize>,
    compaction_prefix: Option<super::compaction::CompactionPrefixAccumulator>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionSummary {
    pub(crate) session_id: SessionId,
    pub(crate) path: PathBuf,
    pub(crate) record_count: usize,
    pub(crate) repair_truncate_to: Option<u64>,
    pub(crate) unfinished: Vec<(OperationId, OperationState)>,
    pub(crate) artifact_count: usize,
    pub(crate) artifact_bytes: u64,
    pub(crate) context_versions: Vec<(ContextId, u64)>,
    pub(crate) context_version_count: usize,
    pub(crate) children: Vec<ChildInspection>,
    pub(crate) child_count: usize,
    pub(crate) bounded_details: bool,
    pub(crate) compaction_count: usize,
    pub(crate) compactions: Vec<CompactionCheckpoint>,
    pub(crate) branch: Option<NativeBranchLineage>,
    pub(crate) active_entry_count: usize,
    pub(crate) recent_active_entry_ids: Vec<ConversationEntryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeBranchReceipt {
    pub(crate) source_session_id: SessionId,
    pub(crate) source_entry_id: ConversationEntryId,
    pub(crate) target_session_id: SessionId,
    pub(crate) shared_entry_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeConversationHandle {
    pub(crate) session_id: SessionId,
    pub(crate) modified: std::time::SystemTime,
    pub(crate) record_count: usize,
}

impl DurableSession {
    /// Workspace-only routing checks must not hydrate a retained transcript.
    pub(crate) fn inspect_workspace_root(
        data_dir: &Path,
        id: SessionId,
    ) -> anyhow::Result<PathBuf> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return Ok(home.history_metadata(id)?.workspace);
        }
        Ok(Self::inspect_restored(data_dir, id)?.1.workspace_root)
    }
    pub(crate) fn create_protected(
        home: crate::storage::ProtectedStore,
        workspace_root: PathBuf,
        session_id: SessionId,
    ) -> Result<Self> {
        let created = RecordEnvelope::new(
            session_id,
            SessionRecord::SessionCreated {
                thread_id: ThreadId::new(),
                workspace_root,
            },
        );
        let data_dir = home.data_dir().to_owned();
        let store = SessionStore::create_protected(home, std::slice::from_ref(&created))?;
        Self::from_open_store(&data_dir, store, vec![created])
    }

    pub(crate) fn resume_protected(
        home: crate::storage::ProtectedStore,
        session_id: SessionId,
    ) -> Result<(Self, SessionSummary)> {
        let store = SessionStore::open_protected(home.clone(), session_id)?;
        let (restored, revision) = protected::restore_execution(&home, session_id)?;
        let summary = protected::execution_summary(&home, &restored, revision)?;
        let mut session = Self {
            store,
            records: Vec::new(),
            record_ids: HashSet::new(),
            restored,
            artifacts: ArtifactStore::protected(home),
            agent_id: AgentId::for_session(session_id),
            owner: PrincipalId::new(),
            execution_bytes: 0,
            checkpoint_revision: revision,
            staged_branch_revision: None,
            compaction_prefix: None,
        };
        session.checkpoint_execution()?;
        Ok((session, summary))
    }

    pub(crate) fn inspect_protected(
        home: &crate::storage::ProtectedStore,
        session_id: SessionId,
    ) -> Result<(SessionSummary, RestoredSession)> {
        let loaded = SessionStore::inspect_protected(home, session_id)?;
        Ok((
            summary_from_loaded(&home.database_path(), &loaded)?,
            reduce(&loaded.records)?,
        ))
    }

    pub(crate) fn create(data_dir: &Path, workspace_root: PathBuf) -> Result<Self> {
        Self::create_with_id(data_dir, workspace_root, SessionId::new())
    }

    pub(crate) fn create_with_id(
        data_dir: &Path,
        workspace_root: PathBuf,
        session_id: SessionId,
    ) -> Result<Self> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return Self::create_protected(home, workspace_root, session_id);
        }
        fs::create_dir_all(data_dir.join("artifacts"))
            .context("could not create durable artifact directory")?;
        let thread_id = ThreadId::new();
        let created = RecordEnvelope::new(
            session_id,
            SessionRecord::SessionCreated {
                thread_id,
                workspace_root,
            },
        );
        let store = SessionStore::create(&data_dir.join("sessions"), created.clone())
            .context("could not create durable session")?;
        Self::from_open_store(data_dir, store, vec![created])
    }

    pub(crate) fn branch_at(
        data_dir: &Path,
        source_session_id: SessionId,
        source_entry_id: ConversationEntryId,
    ) -> Result<(Self, NativeBranchReceipt)> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return Self::branch_protected(home, source_session_id, source_entry_id);
        }
        let (_, source) = Self::inspect_restored(data_dir, source_session_id)
            .context("could not inspect branch source")?;
        let source_path = source
            .conversation_entry_path()
            .context("could not restore branch source history")?;
        let source_index = source_path
            .iter()
            .position(|entry| entry.id == source_entry_id)
            .with_context(|| {
                format!(
                    "entry {source_entry_id} is not on the active history path of Conversation {source_session_id}"
                )
            })?;
        let shared = source_path[..=source_index]
            .iter()
            .map(|entry| (*entry).clone())
            .collect::<Vec<_>>();
        let target_session_id = SessionId::new();
        let lineage = NativeBranchLineage {
            source_session_id,
            source_entry_id,
            shared_entry_count: shared.len(),
        };
        let thread_id = ThreadId::new();
        let mut records = Vec::with_capacity(shared.len() + 3);
        records.push(RecordEnvelope::new(
            target_session_id,
            SessionRecord::SessionCreated {
                thread_id,
                workspace_root: source.workspace_root.clone(),
            },
        ));
        records.push(RecordEnvelope::new(
            target_session_id,
            SessionRecord::ConversationBranched {
                lineage: lineage.clone(),
            },
        ));
        let mut copied_artifacts = HashSet::new();
        for artifact in shared.iter().flat_map(|entry| entry.message.artifacts()) {
            if copied_artifacts.insert(artifact.reference.id) {
                records.push(RecordEnvelope::new(
                    target_session_id,
                    SessionRecord::ArtifactRegistered {
                        artifact: artifact.clone(),
                    },
                ));
            }
        }
        records.extend(shared.into_iter().map(|entry| {
            RecordEnvelope::new(
                target_session_id,
                SessionRecord::ConversationEntryAppended { entry },
            )
        }));
        records.push(RecordEnvelope::new(
            target_session_id,
            SessionRecord::ThreadHeadMoved {
                thread_id,
                head: Some(source_entry_id),
            },
        ));
        reduce(&records).context("could not validate native branch target")?;
        let store = match crate::storage::ProtectedStore::configured(data_dir)? {
            Some(home) => SessionStore::create_protected(home, &records)?,
            None => {
                fs::create_dir_all(data_dir.join("artifacts"))
                    .context("could not create durable artifact directory")?;
                SessionStore::create_batch(&data_dir.join("sessions"), &records)
                    .context("could not commit native branch target")?
            }
        };
        let target = Self::from_open_store(data_dir, store, records)?;
        let receipt = NativeBranchReceipt {
            source_session_id,
            source_entry_id,
            target_session_id,
            shared_entry_count: lineage.shared_entry_count,
        };
        Ok((target, receipt))
    }

    /// Removes a session that this process has just created but has not started.
    ///
    /// The consuming receiver retains the exclusive writer lock until after the
    /// in-memory record set is checked, so another process cannot append between
    /// validation and removal. This is intentionally narrower than a general
    /// session-deletion operation.
    pub(crate) fn discard_unstarted(self) -> Result<()> {
        if self.records.len() != 1
            || !matches!(self.records[0].record, SessionRecord::SessionCreated { .. })
        {
            bail!("refusing to discard a session after execution has started");
        }
        remove_staged_session(self, "empty session")
    }

    pub(crate) fn discard_staged_branch(self) -> Result<()> {
        if self.staged_branch_revision.is_some()
            && self.staged_branch_revision == self.store.protected_revision()
        {
            return remove_staged_session(self, "branch target");
        }
        let valid_creation = matches!(
            self.records.first().map(|record| &record.record),
            Some(SessionRecord::SessionCreated { .. })
        );
        let valid_lineage = self.records.len() == 1
            || matches!(
                self.records.get(1).map(|record| &record.record),
                Some(SessionRecord::ConversationBranched { .. })
            );
        let mut saw_head = false;
        let valid_tail = self
            .records
            .iter()
            .skip(2)
            .enumerate()
            .all(|(index, envelope)| match &envelope.record {
                SessionRecord::ConversationEntryAppended { .. }
                | SessionRecord::ArtifactRegistered { .. }
                    if !saw_head =>
                {
                    true
                }
                SessionRecord::ThreadHeadMoved { .. }
                    if !saw_head && index + 3 == self.records.len() =>
                {
                    saw_head = true;
                    true
                }
                _ => false,
            });
        let valid = valid_creation && valid_lineage && valid_tail;
        if !valid {
            bail!("refusing to discard a branch target after execution has started");
        }
        remove_staged_session(self, "branch target")
    }

    pub(crate) fn resume(data_dir: &Path, session_id: SessionId) -> Result<(Self, SessionSummary)> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return Self::resume_protected(home, session_id);
        }
        let path = SessionStore::path_for(&data_dir.join("sessions"), session_id);
        let loaded = SessionStore::inspect(&path).context("could not inspect durable session")?;
        let summary = summary_from_loaded(&path, &loaded)?;
        let records = loaded.records.clone();
        let store = SessionStore::open_for_resume(&path, loaded)
            .context("could not open inspected session for resume")?;
        let session = Self::from_open_store(data_dir, store, records)?;
        Ok((session, summary))
    }

    pub(crate) fn inspect(data_dir: &Path, session_id: SessionId) -> Result<SessionSummary> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            let (state, revision) = protected::restore_execution(&home, session_id)?;
            return protected::execution_summary(&home, &state, revision);
        }
        let path = SessionStore::path_for(&data_dir.join("sessions"), session_id);
        let loaded = SessionStore::inspect(&path).context("could not inspect durable session")?;
        summary_from_loaded(&path, &loaded)
    }

    pub(crate) fn inspect_restored(
        data_dir: &Path,
        session_id: SessionId,
    ) -> Result<(SessionSummary, RestoredSession)> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return Self::inspect_protected(&home, session_id);
        }
        let path = SessionStore::path_for(&data_dir.join("sessions"), session_id);
        let loaded = SessionStore::inspect(&path).context("could not inspect durable session")?;
        let summary = summary_from_loaded(&path, &loaded)?;
        let restored = reduce(&loaded.records).context("could not reduce inspected session")?;
        Ok((summary, restored))
    }

    pub(crate) fn conversation_page(
        data_dir: &Path,
        session_id: SessionId,
        before: Option<usize>,
        limit: usize,
    ) -> Result<ConversationPage> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return home.history_page(session_id, before, None, limit);
        }
        let path = SessionStore::path_for(&data_dir.join("sessions"), session_id);
        SessionStore::conversation_page(&path, before, limit)
            .context("could not page durable conversation")
    }

    pub(crate) fn conversation_page_from(
        data_dir: &Path,
        session_id: SessionId,
        start: usize,
        limit: usize,
    ) -> Result<ConversationPage> {
        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return home.history_page(session_id, None, Some(start), limit);
        }
        let path = SessionStore::path_for(&data_dir.join("sessions"), session_id);
        SessionStore::conversation_page_from(&path, start, limit)
            .context("could not page newer durable conversation")
    }

    pub(crate) fn latest_for_workspace(
        data_dir: &Path,
        workspace_root: &Path,
    ) -> Result<Option<SessionId>> {
        Ok(Self::list_for_workspace(data_dir, workspace_root)?
            .into_iter()
            .next()
            .map(|entry| entry.session_id))
    }

    pub(crate) fn list_for_workspace(
        data_dir: &Path,
        workspace_root: &Path,
    ) -> Result<Vec<NativeConversationHandle>> {
        const MAX_SESSION_FILES: usize = 10_000;

        if let Some(home) = crate::storage::ProtectedStore::configured(data_dir)? {
            return home.list_histories(workspace_root);
        }

        let sessions_dir = data_dir.join("sessions");
        let entries = match fs::read_dir(&sessions_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("could not list durable sessions"),
        };
        let mut conversations = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= MAX_SESSION_FILES {
                bail!("session selection exceeds the {MAX_SESSION_FILES}-file limit");
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(session_id) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .and_then(|value| value.parse::<SessionId>().ok())
            else {
                continue;
            };
            let Ok(loaded) = SessionStore::inspect(&path) else {
                continue;
            };
            let Ok(restored) = reduce(&loaded.records) else {
                continue;
            };
            if restored.workspace_root != workspace_root {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            conversations.push(NativeConversationHandle {
                session_id,
                modified,
                record_count: loaded.records.len(),
            });
        }
        conversations.sort_by(|left, right| {
            right.modified.cmp(&left.modified).then_with(|| {
                right
                    .session_id
                    .to_string()
                    .cmp(&left.session_id.to_string())
            })
        });
        Ok(conversations)
    }

    fn from_open_store(
        data_dir: &Path,
        store: SessionStore,
        records: Vec<RecordEnvelope>,
    ) -> Result<Self> {
        let restored = reduce(&records).context("could not reduce durable session")?;
        let agent_id = AgentId::for_session(restored.session_id);
        let record_ids = records.iter().map(|record| record.record_id).collect();
        let artifacts = match store.protected_home() {
            Some(home) => ArtifactStore::protected(home.clone()),
            None => ArtifactStore::new(data_dir.join("artifacts")),
        };
        let mut session = Self {
            store,
            records,
            record_ids,
            restored,
            artifacts,
            agent_id,
            owner: PrincipalId::new(),
            execution_bytes: 0,
            checkpoint_revision: 0,
            staged_branch_revision: None,
            compaction_prefix: None,
        };
        if session.store.protected_home().is_some() {
            session.checkpoint_execution()?;
        }
        Ok(session)
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.store.session_id()
    }

    pub(crate) fn thread_id(&self) -> ThreadId {
        self.restored.thread_id
    }

    pub(crate) fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    pub(crate) fn artifact_owner(&self) -> PrincipalId {
        self.owner
    }

    pub(crate) fn completion_evidence(&self) -> &[crate::completion_evidence::CompletionEvidence] {
        &self.restored.completion_evidence
    }

    pub(crate) fn completion_artifact_store(&self) -> ArtifactStore {
        self.artifacts.clone()
    }

    /// A planned-call rejection has a committed assistant request but no
    /// invocation intent (and no effect). It must not disappear from completion
    /// merely because collection primarily follows dispatched invocations.
    pub(crate) fn completion_calls_prepared(
        &self,
        operation: &crate::session::RestoredOperation,
    ) -> bool {
        let limit = crate::completion_evidence::MAX_EVIDENCE_ITEMS;
        if operation.steps.len() > limit || operation.intents.len() > limit {
            return false;
        }
        let mut requested = std::collections::BTreeSet::new();
        for (step, entry) in &operation.steps {
            let Some(entry) = self.restored.entries.get(entry) else {
                return false;
            };
            if entry.message.role != crate::message::Role::Assistant {
                return false;
            }
            for content in &entry.message.content {
                if let crate::message::ContentBlock::ToolCall(call) = content
                    && (requested.len() >= limit || !requested.insert((*step, call.id.as_str())))
                {
                    return false;
                }
            }
        }
        let prepared = operation
            .intents
            .values()
            .map(|intent| (intent.step_id, intent.model_call_id.as_str()))
            .collect::<std::collections::BTreeSet<_>>();
        prepared.len() == operation.intents.len() && requested == prepared
    }

    pub(crate) fn completion_children(
        &self,
        parent: OperationId,
    ) -> Vec<super::reduce::RestoredChild> {
        self.restored
            .children
            .values()
            .filter(|child| child.handle.admission.attribution.parent_operation_id == parent)
            .take(65)
            .cloned()
            .collect()
    }

    pub(crate) fn started_orchestration_plans(
        &self,
    ) -> Vec<crate::orchestration::OrchestrationPlanStart> {
        self.restored
            .orchestration_plans
            .values()
            .cloned()
            .collect()
    }

    /// Completed children may leave the execution projection, but their original
    /// admission budgets remain charged across every owner resume.
    pub(crate) fn orchestration_reservations(
        &self,
    ) -> Result<Vec<crate::orchestration::ReservationRequest>> {
        if let Some(home) = self.store.protected_home() {
            return home.history_orchestration_reservations(self.session_id());
        }
        Ok(self
            .restored
            .children
            .values()
            .map(|child| crate::orchestration::ReservationRequest::from(&child.handle.admission))
            .collect())
    }

    pub(crate) fn path(&self) -> &Path {
        self.store.path()
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.restored.workspace_root
    }

    pub(crate) fn conversation(&self) -> Result<Vec<Message>> {
        anyhow::ensure!(
            self.restored.archived_prefix.is_none(),
            "full Conversation history requires the paged history interface"
        );
        self.restored
            .conversation_path()
            .context("could not restore conversation path")
    }

    pub(crate) fn initial_conversation_page(&self) -> Result<ConversationPage> {
        if let Some(home) = self.store.protected_home() {
            return home.history_page(self.session_id(), None, None, 128);
        }
        let messages = self.conversation()?;
        Ok(ConversationPage {
            total: messages.len(),
            messages,
            start: 0,
            has_older: false,
        })
    }

    pub(crate) fn retained_pressure(&self) -> bool {
        self.store.protected_home().is_some()
            && (self.restored.entries.len() >= 1024 || self.execution_bytes >= 8 * 1024 * 1024)
    }

    pub(crate) fn prompt_continuation(&self) -> Result<PromptContinuation> {
        let path = self
            .restored
            .conversation_entry_path()
            .context("could not restore prompt continuation path")?;
        let checkpoint = self
            .restored
            .active_compaction()
            .context("could not resolve active compaction checkpoint")?
            .cloned();
        let start = checkpoint.as_ref().map_or(0, |checkpoint| {
            checkpoint.source_entry_count - self.restored.retained_offset()
        });
        Ok(PromptContinuation {
            history: path[start..]
                .iter()
                .map(|entry| entry.message.clone())
                .collect(),
            checkpoint,
        })
    }

    #[cfg(test)]
    pub(crate) fn compact_conversation(
        &mut self,
        operation_id: OperationId,
        reason: CompactionReason,
        budget: &PromptBudgetPlan,
    ) -> Result<CompactionCheckpoint> {
        let candidate = self.prepare_compaction(operation_id, reason, budget)?;
        self.commit_compaction(candidate)
    }

    pub(crate) fn begin_compaction(
        &self,
        operation_id: OperationId,
        reason: CompactionReason,
        budget: &PromptBudgetPlan,
    ) -> Result<CompactionPreparation> {
        CompactionPreparation::begin(self, operation_id, reason, budget)
    }

    #[cfg(test)]
    pub(crate) fn prepare_compaction(
        &self,
        operation_id: OperationId,
        reason: CompactionReason,
        budget: &PromptBudgetPlan,
    ) -> Result<super::CompactionCandidate> {
        let mut preparation = self.begin_compaction(operation_id, reason, budget)?;
        while !preparation.is_ready() {
            preparation = preparation.advance()?;
        }
        preparation.finish()
    }

    pub(crate) fn commit_compaction(
        &mut self,
        candidate: super::CompactionCandidate,
    ) -> Result<CompactionCheckpoint> {
        if let Some(guard) = &candidate.source_guard {
            guard.recheck()?;
        }
        if let Some(home) = self.store.protected_home() {
            anyhow::ensure!(
                home.source_eligible(self.session_id().to_string().parse()?)?
                    && candidate.privacy_generation == Some(home.privacy_generation()?),
                "compaction source eligibility changed; retry after reviewing current privacy controls"
            );
        }
        let checkpoint = candidate.checkpoint;
        // append validates the current immutable source range/digest and prior
        // checkpoint before writing; a stale asynchronous result cannot win.
        self.append_guarded(
            SessionRecord::ConversationCompacted {
                checkpoint: checkpoint.clone(),
            },
            candidate.privacy_generation,
            candidate.source_proof.as_ref(),
        )?;
        self.compaction_prefix = candidate
            .source_proof
            .as_ref()
            .map(|proof| proof.accumulator());
        Ok(checkpoint)
    }

    #[cfg(test)]
    pub(crate) fn append_operation_state(
        &mut self,
        operation_id: OperationId,
        state: OperationState,
    ) -> Result<()> {
        self.append(SessionRecord::OperationStateChanged {
            operation_id,
            state,
        })
    }

    pub(crate) fn append_audit(&mut self, fact: PermissionAuditFact) -> Result<()> {
        self.append(SessionRecord::PermissionAudited { fact })
    }

    pub(crate) fn append_record(&mut self, record: SessionRecord) -> Result<()> {
        self.append(record)
    }

    pub(crate) fn stored_artifact(
        &self,
        value: &DurableValueRef,
    ) -> Option<crate::artifact::ArtifactRecord> {
        match value {
            DurableValueRef::Artifact(reference) => {
                self.restored.artifacts.get(&reference.id).cloned()
            }
            DurableValueRef::InlineJson(_) | DurableValueRef::Context { .. } => None,
        }
    }

    pub(crate) fn inspect_stored_artifact(
        &self,
        value: &DurableValueRef,
    ) -> Result<Option<crate::artifact::ArtifactRecord>> {
        if let Some(artifact) = self.stored_artifact(value) {
            return Ok(Some(artifact));
        }
        let (Some(home), DurableValueRef::Artifact(reference)) =
            (self.store.protected_home(), value)
        else {
            return Ok(None);
        };
        let records = home.history_records_for(
            self.session_id(),
            crate::storage::HistorySubject::Artifact(reference.id),
        )?;
        anyhow::ensure!(
            records.len() <= 1,
            "artifact identity is duplicated in history"
        );
        Ok(records.into_iter().find_map(|record| match record.record {
            SessionRecord::ArtifactRegistered { artifact } if artifact.reference == *reference => {
                Some(artifact)
            }
            _ => None,
        }))
    }

    pub(crate) fn store_tool_output(
        &mut self,
        value: serde_json::Value,
    ) -> Result<(DurableValueRef, Option<crate::artifact::ArtifactRecord>)> {
        let stored = self.store_json_value(value)?;
        let artifact = self.inspect_stored_artifact(&stored)?;
        Ok((stored, artifact))
    }

    pub(crate) fn store_json_value(&mut self, value: serde_json::Value) -> Result<DurableValueRef> {
        let bytes = serde_json::to_vec(&value).context("could not encode durable JSON value")?;
        if bytes.len() <= MAX_INLINE_VALUE_BYTES {
            return Ok(DurableValueRef::InlineJson(value));
        }
        let (artifact, _) = self
            .artifacts
            .put(&bytes, "application/json", self.owner)
            .context("could not store durable JSON artifact")?;
        self.append(SessionRecord::ArtifactRegistered {
            artifact: artifact.clone(),
        })?;
        Ok(DurableValueRef::Artifact(artifact.reference))
    }

    pub(crate) fn operation_has_pending(&self, operation_id: OperationId) -> bool {
        self.restored
            .operation_details
            .get(&operation_id)
            .is_some_and(|operation| {
                operation
                    .invocation_order
                    .iter()
                    .any(|id| !operation.results.contains_key(id))
            })
    }

    pub(crate) fn restored_operation(
        &self,
        operation_id: OperationId,
    ) -> Option<crate::session::RestoredOperation> {
        self.restored.operation_details.get(&operation_id).cloned()
    }

    /// Restore denial authority for unfinished work without loading historical transcripts.
    /// Intents also cover older execution checkpoints that discarded audit projections.
    pub(crate) fn unfinished_permission_evidence(
        &self,
    ) -> impl Iterator<Item = &PermissionAuditFact> {
        self.restored
            .audits
            .iter()
            .filter(|fact| {
                self.restored
                    .operation_details
                    .get(&fact.request.operation_id)
                    .is_some_and(|operation| operation.finished.is_none())
            })
            .chain(
                self.restored
                    .operation_details
                    .values()
                    .filter(|operation| operation.finished.is_none())
                    .flat_map(|operation| {
                        operation.intents.values().map(|intent| &intent.permission)
                    }),
            )
    }

    /// Count requested calls, including rejected preparation, without inventing effects.
    pub(crate) fn repeated_tool_patterns(&self, operation_id: OperationId) -> u32 {
        let Some(operation) = self.restored.operation_details.get(&operation_id) else {
            return 0;
        };
        let mut patterns = std::collections::BTreeMap::<[u8; 32], u32>::new();
        let mut repeats = 0_u32;
        for entry in operation
            .steps
            .values()
            .filter_map(|id| self.restored.entries.get(id))
        {
            for block in &entry.message.content {
                if let crate::message::ContentBlock::ToolCall(call) = block {
                    let count = patterns
                        .entry(*call.pattern_fingerprint().as_bytes())
                        .or_default();
                    if *count > 0 {
                        repeats = repeats.saturating_add(1);
                    }
                    *count = count.saturating_add(1);
                }
            }
        }
        repeats
    }

    pub(crate) fn round_budget_suspension(
        &self,
    ) -> Option<crate::native_runtime::RoundBudgetSuspension> {
        self.restored
            .operations
            .iter()
            .rev()
            .filter(|(_, state)| **state == OperationState::Suspended)
            .find_map(|(operation_id, _)| {
                let operation = self.restored.operation_details.get(operation_id)?;
                let suspension = operation.suspensions.iter().rev().find_map(|reason| {
                    let crate::operation::SuspensionReason::RoundBudgetReached(suspension) = reason
                    else {
                        return None;
                    };
                    Some(suspension)
                })?;
                (!operation
                    .round_budget_decisions
                    .iter()
                    .any(|decision| decision.suspension_id == suspension.id))
                .then(|| (**suspension).clone())
            })
    }

    pub(crate) fn append_message(&mut self, message: Message) -> Result<ConversationEntryId> {
        let entry_id = ConversationEntryId::new();
        self.append(SessionRecord::ConversationEntryAppended {
            entry: ConversationEntry {
                id: entry_id,
                parent: self.restored.head,
                agent_id: self.agent_id,
                message,
            },
        })?;
        self.append(SessionRecord::ThreadHeadMoved {
            thread_id: self.restored.thread_id,
            head: Some(entry_id),
        })?;
        Ok(entry_id)
    }

    pub(crate) fn clear_conversation(&mut self) -> Result<()> {
        self.compaction_prefix = None;
        self.append(SessionRecord::ThreadHeadMoved {
            thread_id: self.restored.thread_id,
            head: None,
        })
    }

    pub(crate) fn refresh_project_context(&mut self) -> Result<Vec<ContextSource>> {
        let Some(bytes) = read_project_instructions(&self.restored.workspace_root)? else {
            return Ok(Vec::new());
        };
        let canonical = canonical_text(
            std::str::from_utf8(&bytes).context("root AGENTS.md is not valid UTF-8")?,
        );
        let canonical_bytes = canonical.as_bytes();
        let source_hash = ContentHash::for_bytes(canonical_bytes);

        let context = match self
            .restored
            .named_context
            .get(PROJECT_CONTEXT_NAME)
            .and_then(|key| self.restored.contexts.get(key))
            .cloned()
        {
            Some(existing) if existing.content_hash == source_hash => existing,
            previous => {
                let (artifact, _) = self
                    .artifacts
                    .put(canonical_bytes, "text/markdown; charset=utf-8", self.owner)
                    .context("could not store project context artifact")?;
                self.append(SessionRecord::ArtifactRegistered {
                    artifact: artifact.clone(),
                })?;
                let context = ContextRecord {
                    id: previous
                        .as_ref()
                        .map_or_else(ContextId::new, |record| record.id),
                    version: previous.as_ref().map_or(1, |record| record.version + 1),
                    artifact: artifact.reference.clone(),
                    kind: ContextKind::ProjectInstructions,
                    content_hash: artifact.reference.content_hash.clone(),
                    logical_size: artifact.byte_len,
                    provenance: Provenance::ProjectFile {
                        relative_path: PathBuf::from(PROJECT_INSTRUCTIONS),
                    },
                    trust: TrustClass::Project,
                    owner: self.owner,
                };
                self.append(SessionRecord::ContextRegistered {
                    context: context.clone(),
                })?;
                self.append(SessionRecord::NamedContextSet {
                    name: PROJECT_CONTEXT_NAME.to_owned(),
                    context_id: context.id,
                    version: context.version,
                })?;
                context
            }
        };

        let (text, selected_hash) =
            self.materialize(&context, &ViewSelector::Full, PROJECT_VIEW_BUDGET)?;
        let view = ContextViewRecord {
            id: ContextViewId::new(),
            source: context.id,
            source_version: context.version,
            selector: ViewSelector::Full,
            content_hash: selected_hash,
            budget: PROJECT_VIEW_BUDGET,
        };
        self.append(SessionRecord::ContextViewRegistered { view })?;

        Ok(vec![ContextSource {
            id: TransientSourceId::new(PROJECT_CONTEXT_NAME),
            provenance: SourceProvenance {
                display_name: "persisted root AGENTS.md".to_owned(),
                path: Some(PathBuf::from(PROJECT_INSTRUCTIONS)),
                origin: SourceOrigin::ProjectFile,
            },
            trust: PromptTrustClass::Project,
            content: text,
            max_tokens: PROJECT_VIEW_BUDGET.max_estimated_tokens,
        }])
    }

    pub(crate) fn materialize(
        &self,
        context: &ContextRecord,
        selector: &ViewSelector,
        budget: MaterializationBudget,
    ) -> Result<(String, ContentHash)> {
        if budget.max_bytes == 0 || budget.max_estimated_tokens == 0 {
            bail!("context materialization budgets must be nonzero");
        }
        let artifact = self
            .inspect_stored_artifact(&DurableValueRef::Artifact(context.artifact.clone()))?
            .context("context references an unknown artifact")?;
        let bytes = self
            .artifacts
            .read_bounded(&artifact, MAX_PROJECT_SOURCE_BYTES)
            .context("could not read context artifact")?;
        let source = canonical_text(
            std::str::from_utf8(&bytes).context("context artifact is not valid UTF-8")?,
        );
        let selected = select_text(&source, selector)?;
        let bounded = bound_text(&selected, budget);
        let hash = ContentHash::for_bytes(bounded.as_bytes());
        Ok((bounded, hash))
    }

    fn append(&mut self, record: SessionRecord) -> Result<()> {
        self.append_guarded(record, None, None)
    }

    fn append_guarded(
        &mut self,
        record: SessionRecord,
        privacy_generation: Option<u64>,
        source_proof: Option<&super::compaction::CompactionSourceProof>,
    ) -> Result<()> {
        let envelope = RecordEnvelope::new(self.store.session_id(), record);
        self.preflight_execution(&envelope)?;
        validate_envelope_with_compaction_proof(
            &self.restored,
            &self.record_ids,
            &envelope,
            self.store
                .protected_revision()
                .unwrap_or(self.records.len()),
            source_proof,
        )
        .context("new session record failed validation")?;
        if let Some(home) = self.store.protected_home() {
            let intent = match &envelope.record {
                SessionRecord::InvocationIntentAppended { intent } => Some((intent, false)),
                SessionRecord::InvocationResultAppended { result }
                    if matches!(
                        result.outcome,
                        crate::operation::InvocationOutcome::Completed { .. }
                    ) =>
                {
                    self.restored
                        .operation_details
                        .get(&result.operation_id)
                        .and_then(|operation| operation.intents.get(&result.invocation_id))
                        .map(|intent| (intent, true))
                }
                _ => None,
            };
            if let Some((intent, completed)) = intent {
                // Attribution precedes the append acknowledgement. If it fails,
                // the already executed effect remains uncertain, never replay-safe.
                crate::autonomy::triggers::files::record_invocation(home, intent, completed)?;
            }
        }
        self.store
            .append_guarded(&envelope, privacy_generation)
            .context("could not append durable session record")?;
        apply_validated(&mut self.restored, &envelope.record);
        if self.store.protected_home().is_some() {
            self.execution_bytes = self
                .execution_bytes
                .saturating_add(serde_json::to_vec(&envelope)?.len().saturating_mul(4));
            // Creation/branch rollback is only available before the first append.
            self.records.clear();
            self.record_ids.clear();
            self.staged_branch_revision = None;
            let immediate = matches!(
                envelope.record,
                SessionRecord::ConversationCompacted { .. }
                    | SessionRecord::ThreadHeadMoved { head: None, .. }
            );
            if immediate {
                super::hydration::archive_compacted_prefix(&mut self.restored)?;
                super::hydration::trim_inactive_objects(&mut self.restored);
            }
            if immediate
                || self
                    .store
                    .protected_revision()
                    .unwrap_or(0)
                    .saturating_sub(self.checkpoint_revision)
                    >= 64
            {
                self.checkpoint_execution()?;
            }
        } else {
            self.record_ids.insert(envelope.record_id);
            self.records.push(envelope);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn restored(&self) -> &RestoredSession {
        &self.restored
    }
}

fn summary_from_loaded(path: &Path, loaded: &LoadedSession) -> Result<SessionSummary> {
    let restored = reduce(&loaded.records).context("could not reduce inspected session")?;
    let active_entries = restored
        .conversation_entry_path()
        .context("could not restore inspected Conversation history")?;
    let active_entry_count = active_entries.len();
    let recent_active_entry_ids = active_entries
        .iter()
        .rev()
        .take(MAX_INSPECTED_ENTRY_IDS)
        .map(|entry| entry.id)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut context_versions = restored.contexts.keys().copied().collect::<Vec<_>>();
    context_versions.sort();
    let compaction_count = restored.compactions.len();
    let compactions = restored
        .compactions
        .iter()
        .rev()
        .take(MAX_INSPECTED_COMPACTIONS)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(SessionSummary {
        session_id: restored.session_id,
        path: path.to_owned(),
        record_count: loaded.records.len(),
        repair_truncate_to: loaded.repair.as_ref().map(|repair| repair.truncate_to),
        unfinished: restored.unfinished_operations(),
        artifact_count: restored.artifacts.len(),
        artifact_bytes: restored
            .artifacts
            .values()
            .map(|artifact| artifact.byte_len)
            .sum(),
        context_versions,
        context_version_count: restored.contexts.len(),
        child_count: restored.children.len(),
        bounded_details: false,
        children: restored
            .children
            .values()
            .map(|child| child.inspection())
            .collect(),
        compaction_count,
        compactions,
        branch: restored.branch,
        active_entry_count,
        recent_active_entry_ids,
    })
}

fn remove_staged_session(session: DurableSession, kind: &str) -> Result<()> {
    if let Some(home) = session.store.protected_home() {
        home.discard_history(session.session_id())?;
        return Ok(());
    }
    let path = session.store.path().to_owned();
    let lock_path = path.with_extension("jsonl.lock");
    drop(session);
    fs::remove_file(&path)
        .with_context(|| format!("could not roll back {kind} {}", path.display()))?;
    match fs::remove_file(&lock_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        // A stale, empty lock file carries no session state and can be reused
        // safely by the next writer, so cleanup is best-effort.
        Err(_) => {}
    }
    Ok(())
}

fn select_text(source: &str, selector: &ViewSelector) -> Result<String> {
    match selector {
        ViewSelector::Full => Ok(source.to_owned()),
        ViewSelector::Lines { start, end } => {
            if *start == 0 || end < start {
                bail!("invalid inclusive context line range {start}..={end}");
            }
            Ok(source
                .lines()
                .enumerate()
                .filter(|(index, _)| (*start..=*end).contains(&(index + 1)))
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n"))
        }
        ViewSelector::LiteralSearch { query, max_matches } => {
            if query.is_empty() || *max_matches == 0 {
                bail!("literal context search requires a query and nonzero match limit");
            }
            Ok(source
                .lines()
                .filter(|line| line.contains(query))
                .take(*max_matches)
                .collect::<Vec<_>>()
                .join("\n"))
        }
    }
}

fn bound_text(text: &str, budget: MaterializationBudget) -> String {
    let output = crate::context::bounded_text(text, budget.max_bytes, budget.max_estimated_tokens)
        .to_owned();
    debug_assert!(output.len() <= budget.max_bytes);
    debug_assert!(estimate_tokens(&output) <= budget.max_estimated_tokens);
    output
}
