use super::{
    ConversationEntry, LoadedSession, RecordEnvelope, RestoredSession, SessionRecord, SessionStore,
    reduce,
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
    },
    identity::{
        AgentId, ContextId, ContextViewId, ConversationEntryId, OperationId, PrincipalId,
        SessionId, ThreadId,
    },
    message::Message,
    operation::{DurableValueRef, MAX_INLINE_VALUE_BYTES},
    permission::PermissionAuditFact,
    runtime::OperationState,
};
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

const PROJECT_CONTEXT_NAME: &str = "project:AGENTS.md";
const PROJECT_INSTRUCTIONS: &str = "AGENTS.md";
const MAX_PROJECT_SOURCE_BYTES: usize = 64 * 1024;
const PROJECT_VIEW_BUDGET: MaterializationBudget = MaterializationBudget {
    max_bytes: 16 * 1024,
    max_estimated_tokens: 1_024,
};

pub(crate) struct DurableSession {
    store: SessionStore,
    records: Vec<RecordEnvelope>,
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

    fn from_open_store(
        data_dir: &Path,
        store: SessionStore,
        records: Vec<RecordEnvelope>,
    ) -> Result<Self> {
        let restored = reduce(&records).context("could not reduce durable session")?;
        Ok(Self {
            store,
            records,
            restored,
            artifacts: ArtifactStore::new(data_dir.join("artifacts")),
            agent_id: AgentId::new(),
            owner: PrincipalId::new(),
        })
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.store.session_id()
    }

    pub(crate) fn thread_id(&self) -> ThreadId {
        self.restored.thread_id
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
        self.store
            .append(&envelope)
            .context("could not append durable session record")?;
        self.records.push(envelope);
        self.restored = reduce(&self.records).context("new session record failed reduction")?;
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
    })
}

fn read_project_instructions(workspace_root: &Path) -> Result<Option<Vec<u8>>> {
    let path = workspace_root.join(PROJECT_INSTRUCTIONS);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(source).with_context(|| format!("could not inspect {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "project context source {} must be a regular file and not a symlink",
            path.display()
        );
    }
    let mut bytes = Vec::with_capacity(MAX_PROJECT_SOURCE_BYTES.min(metadata.len() as usize));
    File::open(&path)
        .with_context(|| format!("could not open {}", path.display()))?
        .take((MAX_PROJECT_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() > MAX_PROJECT_SOURCE_BYTES {
        bail!(
            "project context source {} exceeds the {}-byte limit",
            path.display(),
            MAX_PROJECT_SOURCE_BYTES
        );
    }
    Ok(Some(bytes))
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
