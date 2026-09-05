//! Bounded authorization catalog for artifacts already visible to host clients.

use super::protocol::ArtifactResult;
use crate::{
    artifact::{ArtifactRecord, ArtifactStore},
    frontend::{
        ClientEvent, ClientSnapshot,
        semantic::{ContentPartV1, DecodedSemanticEventV1, SemanticEventV1, SemanticSnapshotV1},
    },
    identity::ArtifactId,
    message::Message,
    native_runtime::AgentEvent,
    resource::MAX_RESOURCE_SOURCE_BYTES,
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
            access.observe_semantic_snapshot(&snapshot.semantic);
        }
        access
    }

    pub(crate) fn observe(&self, event: &ClientEvent) {
        match event {
            ClientEvent::Runtime(event) => match event.as_ref() {
                AgentEvent::AssistantMessage { message, .. }
                | AgentEvent::ToolFinished {
                    result: message, ..
                } => self.observe_messages(std::slice::from_ref(message)),
                _ => {}
            },
            ClientEvent::Semantic(envelope) => {
                if let Ok(DecodedSemanticEventV1::Known(event)) = envelope.decode() {
                    match *event {
                        SemanticEventV1::ContentAppended { ref parts, .. }
                        | SemanticEventV1::FinalContent { ref parts, .. } => {
                            self.observe_content(parts);
                        }
                        SemanticEventV1::AttachmentUpserted { ref attachment } => {
                            self.observe_records(std::iter::once(&attachment.resource.artifact));
                        }
                        _ => {}
                    }
                }
            }
            ClientEvent::Managed(_) | ClientEvent::PayloadOmitted { .. } => {}
        }
    }

    pub(crate) fn fetch(
        &self,
        request_id: super::protocol::ArtifactRequestId,
        artifact_id: ArtifactId,
        offset: u64,
        requested_bytes: usize,
    ) -> ArtifactResult {
        let preview_limit = requested_bytes.min(MAX_ARTIFACT_PREVIEW_BYTES);
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
        match self.store.read_verified_range(
            &record,
            offset,
            preview_limit,
            MAX_RESOURCE_SOURCE_BYTES,
        ) {
            Ok(range) => ArtifactResult::accepted(
                request_id,
                record,
                range.offset,
                range.bytes,
                range.truncated_after,
            ),
            Err(error) => ArtifactResult::rejected(
                request_id,
                format!("artifact could not be verified: {error}"),
            ),
        }
    }

    fn observe_messages(&self, messages: &[Message]) {
        self.observe_records(messages.iter().flat_map(Message::artifacts));
    }

    fn observe_semantic_snapshot(&self, snapshot: &SemanticSnapshotV1) {
        self.observe_content(&snapshot.content);
        for parts in snapshot.authoritative_finals.values() {
            self.observe_content(parts);
        }
        self.observe_records(
            snapshot
                .attachments
                .iter()
                .map(|attachment| &attachment.resource.artifact),
        );
    }

    fn observe_content(&self, parts: &[ContentPartV1]) {
        self.observe_records(parts.iter().filter_map(|part| match part {
            ContentPartV1::Resource(resource) => Some(&resource.artifact),
            _ => None,
        }));
    }

    fn observe_records<'a>(&self, values: impl IntoIterator<Item = &'a ArtifactRecord>) {
        let Ok(mut records) = self.records.lock() else {
            return;
        };
        for record in values {
            if records.len() >= MAX_AUTHORIZED_ARTIFACTS
                && !records.contains_key(&record.reference.id)
            {
                break;
            }
            records.insert(record.reference.id, record.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_tool_result_authorizes_only_its_registered_reference() {
        let data = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(data.path().join("artifacts"));
        let (artifact, _) = store
            .put(
                &vec![b'x'; 80 * 1024],
                "application/json",
                crate::identity::PrincipalId::new(),
            )
            .unwrap();
        let access = ArtifactAccess::new(store, None);
        let fetch = |id| {
            access.fetch(
                super::super::protocol::ArtifactRequestId::new(),
                id,
                0,
                4096,
            )
        };
        assert!(!fetch(artifact.reference.id).accepted);
        let mut result = crate::message::ToolResult::success("call", "bounded preview");
        result.artifact = Some(Box::new(artifact.clone()));
        access.observe(&ClientEvent::bounded(AgentEvent::ToolFinished {
            operation_id: crate::identity::OperationId::new(),
            invocation_id: crate::identity::ToolInvocationId::new(),
            result: Message::tool_result(result),
        }));
        let fetched = fetch(artifact.reference.id);
        assert!(fetched.accepted);
        assert_eq!(fetched.preview.len(), 4096);
        assert!(fetched.preview_truncated);
        assert!(!fetch(ArtifactId::new()).accepted);
    }
    use crate::{
        frontend::ClientEvent,
        identity::{OperationId, PrincipalId},
        message::{ContentBlock, Message, Role},
        native_runtime::AgentEvent,
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
            0,
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
            0,
            usize::MAX,
        );
        assert!(result.accepted);
        assert_eq!(result.preview.len(), MAX_ARTIFACT_PREVIEW_BYTES);
        assert!(result.preview_truncated);
        assert_eq!(result.record, Some(record));
    }
}
