//! Child output capture uses the existing artifact store and parent writer.

use super::ChildCommitSender;
use crate::{
    artifact::ArtifactStore,
    identity::PrincipalId,
    message::ToolResult,
    operation::{
        DurableValueRef, MAX_INLINE_VALUE_BYTES,
        output::{ToolOutputRecorder, for_model},
    },
    session::SessionRecord,
};
use futures::future::BoxFuture;

pub(super) struct ChildEvidence {
    pub(super) store: ArtifactStore,
    pub(super) owner: PrincipalId,
    pub(super) commits: ChildCommitSender,
}

impl ToolOutputRecorder for ChildEvidence {
    fn record(&self, mut result: ToolResult) -> BoxFuture<'_, anyhow::Result<ToolResult>> {
        Box::pin(async move {
            let bytes = serde_json::to_vec(&result.output)?;
            if bytes.len() <= MAX_INLINE_VALUE_BYTES {
                return Ok(result);
            }
            let store = self.store.clone();
            let owner = self.owner;
            let (artifact, _) =
                tokio::task::spawn_blocking(move || store.put(&bytes, "application/json", owner))
                    .await??;
            // No reference leaves this sink before the parent's durable ack.
            self.commits
                .append(SessionRecord::ArtifactRegistered {
                    artifact: artifact.clone(),
                })
                .await?;
            result.output = for_model(
                result.output,
                &DurableValueRef::Artifact(artifact.reference.clone()),
            );
            result.artifact = Some(Box::new(artifact));
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn child_evidence_requires_the_parent_acknowledgement() {
        let data = tempfile::tempdir().unwrap();
        let (commits, mut receiver) = ChildCommitSender::channel();
        let recorder = ChildEvidence {
            store: ArtifactStore::new(data.path().join("artifacts")),
            owner: PrincipalId::new(),
            commits,
        };
        let pending = recorder.record(ToolResult::success("large", "x".repeat(40 * 1024)));
        tokio::pin!(pending);
        let command = tokio::select! { command = receiver.recv() => command.unwrap(), result = &mut pending => panic!("must wait for registration: {result:?}") };
        assert!(matches!(
            command.record,
            SessionRecord::ArtifactRegistered { .. }
        ));
        command
            .acknowledged
            .send(Err("writer failed".into()))
            .unwrap();
        assert!(pending.await.is_err());

        let pending = recorder.record(ToolResult::success("large", "y".repeat(40 * 1024)));
        tokio::pin!(pending);
        let command = tokio::select! { command = receiver.recv() => command.unwrap(), result = &mut pending => panic!("must wait: {result:?}") };
        command.acknowledged.send(Ok(())).unwrap();
        let result = pending.await.unwrap();
        assert!(result.artifact.is_some());
        assert!(result.output.len() < 8 * 1024);
    }
}
