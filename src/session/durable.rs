use super::{
    ConversationEntry, ConversationPage, LoadedSession, RecordEnvelope, RestoredSession,
    SessionRecord, SessionStore, apply_validated, reduce, validate_envelope,
};
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
        AgentId, ContextId, ContextViewId, ConversationEntryId, OperationId, PrincipalId,
        SessionId, ThreadId,
    },
    message::Message,
    native_runtime::OperationState,
    operation::{DurableValueRef, MAX_INLINE_VALUE_BYTES},
    orchestration::ChildInspection,
    permission::PermissionAuditFact,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

const PROJECT_CONTEXT_NAME: &str = "project:AGENTS.md";
const PROJECT_INSTRUCTIONS: &str = crate::context::PROJECT_INSTRUCTIONS;
const MAX_PROJECT_SOURCE_BYTES: usize = crate::context::MAX_PROJECT_SOURCE_BYTES;
const PROJECT_VIEW_BUDGET: MaterializationBudget = MaterializationBudget {
    max_bytes: 16 * 1024,
    max_estimated_tokens: 1_024,
};

pub(crate) struct DurableSession {
    store: SessionStore,
    records: Vec<RecordEnvelope>,
    record_ids: HashSet<crate::identity::RecordId>,
    restored: RestoredSession,
    artifacts: ArtifactStore,
    agent_id: AgentId,
    owner: PrincipalId,
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
    pub(crate) children: Vec<ChildInspection>,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeConversationHandle {
    pub(crate) session_id: SessionId,
    pub(crate) modified: std::time::SystemTime,
    pub(crate) record_count: usize,
}

impl DurableSession {
    pub(crate) fn create(data_dir: &Path, workspace_root: PathBuf) -> Result<Self> {
        fs::create_dir_all(data_dir.join("artifacts"))
            .context("could not create durable artifact directory")?;
        let session_id = SessionId::new();
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

    pub(crate) fn resume(data_dir: &Path, session_id: SessionId) -> Result<(Self, SessionSummary)> {
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
        let path = SessionStore::path_for(&data_dir.join("sessions"), session_id);
        let loaded = SessionStore::inspect(&path).context("could not inspect durable session")?;
        summary_from_loaded(&path, &loaded)
    }

    pub(crate) fn inspect_restored(
        data_dir: &Path,
        session_id: SessionId,
    ) -> Result<(SessionSummary, RestoredSession)> {
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
        let path = SessionStore::path_for(&data_dir.join("sessions"), session_id);
        SessionStore::conversation_page(&path, before, limit)
            .context("could not page durable conversation")
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
        Ok(Self {
            store,
            records,
            record_ids,
            restored,
            artifacts: ArtifactStore::new(data_dir.join("artifacts")),
            agent_id,
            owner: PrincipalId::new(),
        })
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

    pub(crate) fn started_orchestration_plans(
        &self,
    ) -> Vec<crate::orchestration::OrchestrationPlanStart> {
        self.restored
            .orchestration_plans
            .values()
            .cloned()
            .collect()
    }

    pub(crate) fn path(&self) -> &Path {
        self.store.path()
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.restored.workspace_root
    }

    pub(crate) fn conversation(&self) -> Result<Vec<Message>> {
        self.restored
            .conversation_path()
            .context("could not restore conversation path")
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
            .restored
            .artifacts
            .get(&context.artifact.id)
            .context("context references an unknown artifact")?;
        let bytes = self
            .artifacts
            .read_bounded(artifact, MAX_PROJECT_SOURCE_BYTES)
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
        let envelope = RecordEnvelope::new(self.store.session_id(), record);
        validate_envelope(
            &self.restored,
            &self.record_ids,
            &envelope,
            self.records.len(),
        )
        .context("new session record failed validation")?;
        self.store
            .append(&envelope)
            .context("could not append durable session record")?;
        apply_validated(&mut self.restored, &envelope.record);
        self.record_ids.insert(envelope.record_id);
        self.records.push(envelope);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn restored(&self) -> &RestoredSession {
        &self.restored
    }
}

fn summary_from_loaded(path: &Path, loaded: &LoadedSession) -> Result<SessionSummary> {
    let restored = reduce(&loaded.records).context("could not reduce inspected session")?;
    let mut context_versions = restored.contexts.keys().copied().collect::<Vec<_>>();
    context_versions.sort();
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
        children: restored
            .children
            .values()
            .map(|child| child.inspection())
            .collect(),
    })
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
    let max_chars = budget.max_estimated_tokens.saturating_mul(3);
    let mut output = String::new();
    for character in text.chars().take(max_chars) {
        if output.len() + character.len_utf8() > budget.max_bytes {
            break;
        }
        output.push(character);
    }
    debug_assert!(output.len() <= budget.max_bytes);
    debug_assert!(estimate_tokens(&output) <= budget.max_estimated_tokens);
    output
}
