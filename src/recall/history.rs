use super::*;
use crate::message::{ContentBlock, Message};
use tokio_util::sync::CancellationToken;

pub(super) fn source_text(message: &Message) -> Result<String> {
    let mut text = format!("{:?}:\n", message.role);
    for block in &message.content {
        match block {
            ContentBlock::Text(value) => {
                text.push_str(value);
                text.push('\n');
            }
            ContentBlock::ToolResult(result) => {
                text.push_str("Untrusted tool result:\n");
                text.push_str(&result.output);
                text.push('\n');
            }
            ContentBlock::ToolCall(call) => {
                text.push_str("Tool called: ");
                text.push_str(&call.name);
                text.push('\n');
            }
            ContentBlock::Image(_) => {
                text.push_str("[image attachment; use original artifact for vision]\n")
            }
        }
        ensure!(
            text.len() <= MAX_SOURCE_BYTES,
            "history source exceeds recall text bound"
        );
    }
    Ok(text)
}

impl RecallOwner {
    pub(crate) fn rebuild(&self) -> Result<()> {
        self.store
            .recall_reset_scope(&self.scope(self.conversation)?)
    }
    /// One bounded batch per eligible Conversation, with a durable cursor.
    /// Repeating refresh advances existing history without re-reading old bodies.
    pub(crate) fn refresh_history(&self, cancel: &CancellationToken) -> Result<Progress> {
        let generation = self.store.privacy_generation()?;
        let scope = self.scope(self.conversation)?;
        let project = crate::project::ProjectStore::open(&self.paths)?
            .membership(&self.conversation.to_string())?;
        let conversations = if let Some(project) = project {
            let project = crate::project::ProjectStore::open(&self.paths)?.get(project)?;
            self.store
                .list_histories(&project.canonical_workspace)?
                .into_iter()
                .map(|handle| handle.session_id)
                .collect::<Vec<_>>()
        } else {
            vec![self.conversation]
        };
        ensure!(
            conversations.len() <= 256,
            "Project recall exceeds256 Conversation refresh scope; use exact Conversation queries"
        );
        let mut progress = Progress::default();
        for conversation in conversations {
            ensure!(
                !cancel.is_cancelled(),
                "recall indexing cancelled; committed cursor remains resumable"
            );
            if !self.eligible(conversation)? {
                continue;
            }
            let source_scope = self.scope(conversation)?;
            let (next, records, pending) = self
                .store
                .recall_history_batch(conversation, &source_scope)?;
            for record in records {
                ensure!(
                    record.session_id == conversation
                        && record.version == crate::session::SESSION_RECORD_VERSION,
                    "recall source identity differs"
                );
                progress.inspected += 1;
                let (key, source, text) = match record.record {
                    crate::session::SessionRecord::ConversationEntryAppended { entry } => (
                        format!("conversation:{conversation}/entry:{}", entry.id),
                        Source::Conversation {
                            conversation,
                            entry: entry.id,
                        },
                        source_text(&entry.message),
                    ),
                    crate::session::SessionRecord::ArtifactRegistered { artifact } => (
                        format!(
                            "conversation:{conversation}/artifact:{}",
                            artifact.reference.id
                        ),
                        Source::Artifact {
                            conversation,
                            artifact: artifact.reference.id,
                        },
                        artifact_text(&self.store, &artifact),
                    ),
                    _ => continue,
                };
                let Ok(text) = text else {
                    progress.skipped += 1;
                    continue;
                };
                let indexed = IndexedSource {
                    key,
                    scope: source_scope.clone(),
                    source,
                    hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
                };
                self.store.recall_index(&indexed, &text, generation)?;
                progress.indexed += 1;
            }
            ensure!(
                self.scope(conversation)? == source_scope
                    && self.scope(self.conversation)? == scope,
                "Project or Profile changed during recall indexing"
            );
            self.store
                .recall_advance(conversation, &source_scope, next)?;
            progress.pending |= pending;
        }
        Ok(progress)
    }
}

pub(super) fn artifact_text(
    store: &ProtectedStore,
    artifact: &crate::artifact::ArtifactRecord,
) -> Result<String> {
    let media = artifact
        .media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    ensure!(
        matches!(media, "text/plain" | "text/markdown" | "application/json"),
        "artifact is not a supported text recall source"
    );
    let bytes = crate::artifact::ArtifactStore::protected(store.clone())
        .read_bounded(artifact, MAX_SOURCE_BYTES)?;
    String::from_utf8(bytes).context("recall artifact must be UTF-8 text")
}
