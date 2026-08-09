//! Bounded authorization catalog for artifacts already visible to host clients.

use super::protocol::ArtifactResult;
use crate::{
    artifact::{ArtifactRecord, ArtifactStore},
    frontend::{ClientEvent, ClientSnapshot},
    identity::ArtifactId,
    message::{ContentBlock, Message},
    runtime::AgentEvent,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub(crate) const MAX_AUTHORIZED_ARTIFACTS: usize = 512;
pub(crate) const MAX_ARTIFACT_PREVIEW_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct ArtifactAccess {
    store: ArtifactStore,
    records: Arc<Mutex<HashMap<ArtifactId, ArtifactRecord>>>,
}

impl ArtifactAccess {
    pub(crate) fn new(store: ArtifactStore, snapshot: Option<&ClientSnapshot>) -> Self {
        let access = Self {
            store,
            records: Arc::new(Mutex::new(HashMap::new())),
        };
        if let Some(snapshot) = snapshot {
            access.observe_messages(&snapshot.conversation);
        }
        access
    }

    pub(crate) fn observe(&self, event: &ClientEvent) {
        if let ClientEvent::Runtime(event) = event
            && let AgentEvent::AssistantMessage { message, .. } = event.as_ref()
        {
            self.observe_messages(std::slice::from_ref(message));
        }
    }

    pub(crate) fn fetch(
        &self,
        request_id: super::protocol::ArtifactRequestId,
        artifact_id: ArtifactId,
        requested_preview_bytes: usize,
    ) -> ArtifactResult {
        let preview_limit = requested_preview_bytes.min(MAX_ARTIFACT_PREVIEW_BYTES);
        let record = self
            .records
            .lock()
            .ok()
            .and_then(|records| records.get(&artifact_id).cloned());
        let Some(record) = record else {
            return ArtifactResult::rejected(
                request_id,
                "artifact is missing, expired, or not authorized for this host",
            );
        };
        match self.store.read_verified_preview(&record, preview_limit) {
            Ok((preview, truncated)) => {
                ArtifactResult::accepted(request_id, record, preview, truncated)
            }
            Err(error) => ArtifactResult::rejected(
                request_id,
                format!("artifact could not be verified: {error}"),
            ),
        }
    }

    fn observe_messages(&self, messages: &[Message]) {
        let Ok(mut records) = self.records.lock() else {
            return;
        };
        for image in messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                ContentBlock::Image(image) => Some(image),
                _ => None,
            })
        {
            if records.len() >= MAX_AUTHORIZED_ARTIFACTS
                && !records.contains_key(&image.artifact.reference.id)
            {
                break;
            }
            records.insert(image.artifact.reference.id, image.artifact.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frontend::ClientEvent,
        identity::{OperationId, PrincipalId},
        message::{ContentBlock, Message, Role},
        runtime::AgentEvent,
        vision::ImageRef,
    };
    use tempfile::tempdir;

    #[test]
    fn only_visible_immutable_references_return_verified_bounded_previews() {
        let directory = tempdir().unwrap();
        let store = ArtifactStore::new(directory.path().to_owned());
        let bytes = vec![b'x'; MAX_ARTIFACT_PREVIEW_BYTES + 9];
        let (record, _) = store
            .put(&bytes, "application/octet-stream", PrincipalId::new())
            .unwrap();
        let access = ArtifactAccess::new(store, None);
        let missing = access.fetch(
            super::super::protocol::ArtifactRequestId::new(),
            record.reference.id,
            MAX_ARTIFACT_PREVIEW_BYTES,
        );
        assert!(!missing.accepted);

        access.observe(&ClientEvent::bounded(AgentEvent::AssistantMessage {
            operation_id: OperationId::new(),
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Image(ImageRef {
                    artifact: record.clone(),
                    media_type: record.media_type.clone(),
                    byte_len: record.byte_len,
                    width: None,
                    height: None,
                })],
            },
        }));
        let result = access.fetch(
            super::super::protocol::ArtifactRequestId::new(),
            record.reference.id,
            usize::MAX,
        );
        assert!(result.accepted);
        assert_eq!(result.preview.len(), MAX_ARTIFACT_PREVIEW_BYTES);
        assert!(result.preview_truncated);
        assert_eq!(result.record, Some(record));
    }
}
