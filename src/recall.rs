//! Source-cited local task/knowledge retrieval, never a personal-memory writer.

mod history;
mod knowledge;
#[cfg(test)]
mod tests;
mod tool;

use crate::{
    identity::{ConversationEntryId, SessionId},
    paths::XanaPaths,
    storage::ProtectedStore,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[cfg(test)]
use knowledge::KnowledgeRoot;
pub(crate) use tool::RecallTool;
pub(crate) const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const CHUNK_BYTES: usize = 4096;
pub(crate) const MAX_HITS: usize = 8;
pub(crate) const MAX_RESULT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Source {
    Conversation {
        conversation: SessionId,
        entry: ConversationEntryId,
    },
    Artifact {
        conversation: SessionId,
        artifact: crate::identity::ArtifactId,
    },
    File {
        root: Uuid,
        relative_path: PathBuf,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Citation {
    pub(crate) source: Source,
    pub(crate) source_hash: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Hit {
    pub(crate) citation: Citation,
    pub(crate) text: String,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Progress {
    pub(crate) inspected: usize,
    pub(crate) indexed: usize,
    pub(crate) skipped: usize,
    pub(crate) pending: bool,
}
#[derive(Clone)]
pub(crate) struct RecallOwner {
    pub(crate) store: ProtectedStore,
    pub(crate) paths: XanaPaths,
    pub(crate) conversation: SessionId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IndexedSource {
    pub(crate) key: String,
    pub(crate) scope: String,
    pub(crate) source: Source,
    pub(crate) hash: String,
}

impl RecallOwner {
    fn inclusion_name(&self) -> String {
        format!("recall/inclusions/{}", self.conversation)
    }
    pub(crate) fn inclusions(&self) -> Result<Vec<SessionId>> {
        self.store
            .document(&self.inclusion_name(), 4096)?
            .map_or(Ok(Vec::new()), |bytes| Ok(serde_json::from_slice(&bytes)?))
    }
    pub(crate) fn include(&self, source: SessionId, enabled: bool) -> Result<()> {
        let projects = crate::project::ProjectStore::open(&self.paths)?;
        let project = projects
            .membership(&self.conversation.to_string())?
            .context("ungrouped Conversations cannot broaden recall")?;
        ensure!(
            projects.membership(&source.to_string())? == Some(project),
            "explicit recall inclusion must remain in the same Project"
        );
        self.scope(source)?;
        let mut included = self.inclusions()?;
        included.retain(|id| *id != source);
        if enabled {
            ensure!(
                included.len() < 32,
                "explicit recall inclusion limit reached"
            );
            included.push(source);
        }
        self.store.recall_policy(
            &self.inclusion_name(),
            Some(&serde_json::to_vec(&included)?),
        )
    }
    fn eligible(&self, source: SessionId) -> Result<bool> {
        if !self.store.source_eligible(source.to_string().parse()?)? {
            return Ok(false);
        }
        let current = self.scope(self.conversation)?;
        let Ok(actual) = self.scope(source) else {
            return Ok(false);
        };
        if actual == current {
            return Ok(true);
        }
        if !self.inclusions()?.contains(&source) {
            return Ok(false);
        }
        let projects = crate::project::ProjectStore::open(&self.paths)?;
        let project = projects.membership(&self.conversation.to_string())?;
        Ok(project.is_some() && project == projects.membership(&source.to_string())?)
    }
    pub(crate) fn open(paths: &XanaPaths, conversation: SessionId) -> Result<Self> {
        let store = ProtectedStore::configured(paths.data_dir())?
            .context("recall requires unlocked protected storage")?;
        let owner = Self {
            store,
            paths: paths.clone(),
            conversation,
        };
        owner.scope(conversation)?;
        Ok(owner)
    }
    pub(crate) fn scope(&self, conversation: SessionId) -> Result<String> {
        ensure!(
            self.store.history_exists(conversation)?,
            "recall source Conversation does not exist"
        );
        let registry = crate::project::ProjectStore::open(&self.paths)?;
        let project = registry.membership(&conversation.to_string())?;
        let profile = crate::profile::ProfileStore::open(&self.paths)
            .snapshot(&conversation.to_string())?
            .context("recall requires an immutable Conversation Profile")?;
        Ok(match project {
            Some(project) => format!("project:{project}/profile:{}", profile.profile_id),
            None => format!("conversation:{conversation}"),
        })
    }
    pub(crate) fn search(&self, query: &str, route: Option<&str>) -> Result<Vec<Hit>> {
        ensure!(
            query.len() <= 512 && !query.trim().is_empty(),
            "recall query must contain1..512 bytes"
        );
        let generation = self.store.privacy_generation()?;
        ensure!(
            self.store
                .source_eligible(self.conversation.to_string().parse()?)?,
            "recall is suspended by this Conversation's privacy barrier"
        );
        let scope = self.scope(self.conversation)?;
        let mut results = Vec::new();
        let mut total = 0usize;
        for candidate in self
            .store
            .recall_candidates(query, &scope, &self.inclusions()?)?
        {
            let Some(text) = self.materialize(&candidate, route)? else {
                continue;
            };
            let Some(selected) = text.get(candidate.start..candidate.end) else {
                continue;
            };
            if selected.len() > CHUNK_BYTES || total + selected.len() > MAX_RESULT_BYTES {
                continue;
            }
            total += selected.len();
            results.push(Hit {
                citation: candidate,
                text: selected.to_owned(),
            });
            if results.len() == MAX_HITS {
                break;
            }
        }
        ensure!(
            self.scope(self.conversation)? == scope
                && self.store.privacy_generation()? == generation,
            "recall authorization changed while materializing evidence"
        );
        Ok(results)
    }
    fn materialize(&self, citation: &Citation, route: Option<&str>) -> Result<Option<String>> {
        let text = match &citation.source {
            Source::Conversation {
                conversation,
                entry,
            } => {
                if !self.eligible(*conversation)? {
                    return Ok(None);
                }
                let records = self.store.history_records_for(
                    *conversation,
                    crate::storage::HistorySubject::Entry(*entry),
                )?;
                let Some(record) = records.first() else {
                    return Ok(None);
                };
                let crate::session::SessionRecord::ConversationEntryAppended { entry: record } =
                    &record.record
                else {
                    return Ok(None);
                };
                let text = history::source_text(&record.message)?;
                if !self.eligible(*conversation)? {
                    return Ok(None);
                }
                text
            }
            Source::Artifact {
                conversation,
                artifact,
            } => {
                if !self.eligible(*conversation)? {
                    return Ok(None);
                }
                let records = self.store.history_records_for(
                    *conversation,
                    crate::storage::HistorySubject::Artifact(*artifact),
                )?;
                let Some(crate::session::RecordEnvelope {
                    record: crate::session::SessionRecord::ArtifactRegistered { artifact },
                    ..
                }) = records.first()
                else {
                    return Ok(None);
                };
                let Ok(text) = history::artifact_text(&self.store, artifact) else {
                    return Ok(None);
                };
                if !self.eligible(*conversation)? {
                    return Ok(None);
                }
                text
            }
            Source::File {
                root,
                relative_path,
            } => {
                let Some(root) = self.knowledge_root(*root)? else {
                    return Ok(None);
                };
                if root.scope != self.scope(self.conversation)?
                    || route.is_some_and(|route| {
                        !root
                            .disclosure_routes
                            .iter()
                            .any(|allowed| allowed == route)
                    })
                {
                    return Ok(None);
                }
                match knowledge::read_source(&root, relative_path) {
                    Ok(text) => text,
                    Err(_) => return Ok(None),
                }
            }
        };
        Ok(
            (blake3::hash(text.as_bytes()).to_hex().as_str() == citation.source_hash)
                .then_some(text),
        )
    }
}
