use super::record::{ConversationEntry, RecordEnvelope, SESSION_RECORD_VERSION, SessionRecord};
use crate::{
    artifact::ArtifactRecord,
    context::persisted::{ContextRecord, ContextViewRecord},
    identity::*,
    message::Message,
    permission::PermissionAuditFact,
    runtime::OperationState,
};
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt,
    path::PathBuf,
};

#[derive(Debug)]
pub(crate) struct RestoredSession {
    pub(crate) session_id: SessionId,
    pub(crate) thread_id: ThreadId,
    pub(crate) workspace_root: PathBuf,
    pub(crate) head: Option<ConversationEntryId>,
    pub(crate) entries: BTreeMap<ConversationEntryId, ConversationEntry>,
    pub(crate) operations: BTreeMap<OperationId, OperationState>,
    pub(crate) audits: Vec<PermissionAuditFact>,
    pub(crate) artifacts: BTreeMap<ArtifactId, ArtifactRecord>,
    pub(crate) contexts: BTreeMap<(ContextId, u64), ContextRecord>,
    pub(crate) views: BTreeMap<ContextViewId, ContextViewRecord>,
    pub(crate) named_context: BTreeMap<String, (ContextId, u64)>,
}

pub(crate) fn reduce(records: &[RecordEnvelope]) -> Result<RestoredSession, ReductionError> {
    let first = records.first().ok_or(ReductionError::MissingCreation)?;
    let SessionRecord::SessionCreated {
        thread_id,
        workspace_root,
    } = &first.record
    else {
        return Err(ReductionError::MissingCreation);
    };
    if first.version != SESSION_RECORD_VERSION {
        return Err(ReductionError::UnsupportedVersion(first.version));
    }
    let mut state = RestoredSession {
        session_id: first.session_id,
        thread_id: *thread_id,
        workspace_root: workspace_root.clone(),
        head: None,
        entries: BTreeMap::new(),
        operations: BTreeMap::new(),
        audits: Vec::new(),
        artifacts: BTreeMap::new(),
        contexts: BTreeMap::new(),
        views: BTreeMap::new(),
        named_context: BTreeMap::new(),
    };
    let mut record_ids = HashSet::new();

    for (index, envelope) in records.iter().enumerate() {
        if envelope.version != SESSION_RECORD_VERSION {
            return Err(ReductionError::UnsupportedVersion(envelope.version));
        }
        if envelope.session_id != state.session_id {
            return Err(ReductionError::WrongSession { index });
        }
        if !record_ids.insert(envelope.record_id) {
            return Err(ReductionError::DuplicateRecord { index });
        }
        if index == 0 {
            continue;
        }

        match &envelope.record {
            SessionRecord::SessionCreated { .. } => {
                return Err(ReductionError::SecondCreation { index });
            }
            SessionRecord::ConversationEntryAppended { entry } => {
                if entry
                    .parent
                    .is_some_and(|parent| !state.entries.contains_key(&parent))
                {
                    return Err(ReductionError::UnknownParent { entry: entry.id });
                }
                if state.entries.insert(entry.id, entry.clone()).is_some() {
                    return Err(ReductionError::DuplicateEntry { entry: entry.id });
                }
            }
            SessionRecord::ThreadHeadMoved { thread_id, head } => {
                if *thread_id != state.thread_id {
                    return Err(ReductionError::UnknownThread { thread: *thread_id });
                }
                if head.is_some_and(|entry| !state.entries.contains_key(&entry)) {
                    return Err(ReductionError::UnknownHead { head: *head });
                }
                state.head = *head;
            }
            SessionRecord::OperationStateChanged {
                operation_id,
                state: next,
            } => {
                let previous = state.operations.get(operation_id).copied();
                if !valid_operation_transition(previous, *next) {
                    return Err(ReductionError::InvalidOperationTransition {
                        operation: *operation_id,
                        previous,
                        next: *next,
                    });
                }
                state.operations.insert(*operation_id, *next);
            }
            SessionRecord::PermissionAudited { fact } => state.audits.push(fact.clone()),
            SessionRecord::ArtifactRegistered { artifact } => {
                if state
                    .artifacts
                    .insert(artifact.reference.id, artifact.clone())
                    .is_some()
                {
                    return Err(ReductionError::DuplicateArtifact {
                        artifact: artifact.reference.id,
                    });
                }
            }
            SessionRecord::ContextRegistered { context } => {
                let Some(artifact) = state.artifacts.get(&context.artifact.id) else {
                    return Err(ReductionError::UnknownArtifact {
                        artifact: context.artifact.id,
                    });
                };
                if artifact.reference.content_hash != context.content_hash
                    || context.artifact.content_hash != context.content_hash
                    || artifact.byte_len != context.logical_size
                {
                    return Err(ReductionError::ContextArtifactMismatch {
                        context: context.id,
                    });
                }
                let expected = state
                    .contexts
                    .keys()
                    .filter_map(|(id, version)| (*id == context.id).then_some(*version))
                    .max()
                    .map_or(1, |version| version + 1);
                if context.version != expected {
                    return Err(ReductionError::NonMonotonicContext {
                        context: context.id,
                        expected,
                        actual: context.version,
                    });
                }
                if state
                    .contexts
                    .insert((context.id, context.version), context.clone())
                    .is_some()
                {
                    return Err(ReductionError::DuplicateContext {
                        context: context.id,
                        version: context.version,
                    });
                }
            }
            SessionRecord::ContextViewRegistered { view } => {
                if !state
                    .contexts
                    .contains_key(&(view.source, view.source_version))
                {
                    return Err(ReductionError::UnknownContextVersion {
                        context: view.source,
                        version: view.source_version,
                    });
                }
                if state.views.insert(view.id, view.clone()).is_some() {
                    return Err(ReductionError::DuplicateView { view: view.id });
                }
            }
            SessionRecord::NamedContextSet {
                name,
                context_id,
                version,
            } => {
                if name.trim().is_empty() {
                    return Err(ReductionError::InvalidContextName);
                }
                if !state.contexts.contains_key(&(*context_id, *version)) {
                    return Err(ReductionError::UnknownContextVersion {
                        context: *context_id,
                        version: *version,
                    });
                }
                state
                    .named_context
                    .insert(name.clone(), (*context_id, *version));
            }
        }
    }

    state.validate_conversation_path()?;
    Ok(state)
}

impl RestoredSession {
    pub(crate) fn conversation_path(&self) -> Result<Vec<Message>, ReductionError> {
        let mut path = Vec::new();
        let mut cursor = self.head;
        let mut seen = HashSet::new();
        while let Some(id) = cursor {
            if !seen.insert(id) {
                return Err(ReductionError::CyclicConversation { entry: id });
            }
            let entry = self
                .entries
                .get(&id)
                .ok_or(ReductionError::UnknownHead { head: Some(id) })?;
            path.push(entry.message.clone());
            cursor = entry.parent;
        }
        path.reverse();
        Ok(path)
    }

    fn validate_conversation_path(&self) -> Result<(), ReductionError> {
        self.conversation_path().map(|_| ())
    }

    pub(crate) fn unfinished_operations(&self) -> Vec<(OperationId, OperationState)> {
        self.operations
            .iter()
            .filter_map(|(id, state)| {
                (!matches!(state, OperationState::Finished(_))).then_some((*id, *state))
            })
            .collect()
    }
}

fn valid_operation_transition(previous: Option<OperationState>, next: OperationState) -> bool {
    matches!(
        (previous, next),
        (None, OperationState::Running)
            | (Some(OperationState::Running), OperationState::Suspended)
            | (Some(OperationState::Running), OperationState::Finished(_))
            | (Some(OperationState::Suspended), OperationState::Running)
            | (Some(OperationState::Suspended), OperationState::Finished(_))
    )
}

#[derive(Debug, PartialEq)]
pub(crate) enum ReductionError {
    MissingCreation,
    UnsupportedVersion(u32),
    WrongSession {
        index: usize,
    },
    DuplicateRecord {
        index: usize,
    },
    SecondCreation {
        index: usize,
    },
    DuplicateEntry {
        entry: ConversationEntryId,
    },
    UnknownParent {
        entry: ConversationEntryId,
    },
    UnknownThread {
        thread: ThreadId,
    },
    UnknownHead {
        head: Option<ConversationEntryId>,
    },
    CyclicConversation {
        entry: ConversationEntryId,
    },
    InvalidOperationTransition {
        operation: OperationId,
        previous: Option<OperationState>,
        next: OperationState,
    },
    DuplicateArtifact {
        artifact: ArtifactId,
    },
    UnknownArtifact {
        artifact: ArtifactId,
    },
    ContextArtifactMismatch {
        context: ContextId,
    },
    DuplicateContext {
        context: ContextId,
        version: u64,
    },
    NonMonotonicContext {
        context: ContextId,
        expected: u64,
        actual: u64,
    },
    UnknownContextVersion {
        context: ContextId,
        version: u64,
    },
    DuplicateView {
        view: ContextViewId,
    },
    InvalidContextName,
}

impl fmt::Display for ReductionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid durable session record sequence: {self:?}"
        )
    }
}

impl Error for ReductionError {}
