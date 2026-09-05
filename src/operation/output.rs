//! Preserve complete durable evidence while bounding its model-facing preview.

use super::DurableValueRef;
use crate::message::ToolResult;

/// A narrow evidence sink for nondurable child loops. The host owns storage
/// and the registration acknowledgement; the agent does not own a session.
pub(crate) trait ToolOutputRecorder: Send + Sync {
    fn record(
        &self,
        result: ToolResult,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<ToolResult>>;
}

const MAX_PREVIEW_BYTES: usize = 4 * 1024;

pub(crate) fn for_model(output: String, stored: &DurableValueRef) -> String {
    let DurableValueRef::Artifact(reference) = stored else {
        return output;
    };
    let end = output.floor_char_boundary(output.len().min(MAX_PREVIEW_BYTES));
    serde_json::json!({
        "preview": &output[..end],
        "preview_truncated": end < output.len(),
        "complete_output_bytes": output.len(),
        "artifact": reference,
        "encoding": "JSON string",
        "notice": "The complete output is retained in the immutable artifact, not in this preview. TUI /artifact ID and Desktop artifact actions inspect this reference. For more detail request a narrower read; do not repeat an action with side effects just to obtain its output.",
    }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        artifact::ArtifactStore,
        session::{DurableSession, SessionRecord, SessionStore},
    };

    #[test]
    fn oversized_tool_evidence_is_retrievable_and_not_duplicated_into_the_prompt() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut session =
            DurableSession::create(data.path(), workspace.path().canonicalize().unwrap()).unwrap();
        let output = format!("{}END_OF_EVIDENCE", "🦀你好".repeat(8_000));
        let stored = session
            .store_json_value(serde_json::Value::String(output.clone()))
            .unwrap();
        let preview: serde_json::Value =
            serde_json::from_str(&for_model(output.clone(), &stored)).unwrap();
        assert_eq!(preview["preview_truncated"], true);
        assert!(preview["preview"].as_str().unwrap().len() <= MAX_PREVIEW_BYTES);
        assert!(
            !preview["preview"]
                .as_str()
                .unwrap()
                .contains("END_OF_EVIDENCE")
        );
        let DurableValueRef::Artifact(reference) = stored else {
            panic!("must be an artifact")
        };
        assert_eq!(
            preview["artifact"],
            serde_json::to_value(&reference).unwrap()
        );
        let store = ArtifactStore::new(data.path().join("artifacts"));
        let loaded = SessionStore::inspect(session.path()).unwrap();
        let artifact = loaded
            .records
            .iter()
            .find_map(|record| match &record.record {
                SessionRecord::ArtifactRegistered { artifact }
                    if artifact.reference == reference =>
                {
                    Some(artifact)
                }
                _ => None,
            })
            .expect("durable registration precedes the model preview");
        let bytes = store.read_bounded(artifact, 256 * 1024).unwrap();
        assert_eq!(serde_json::from_slice::<String>(&bytes).unwrap(), output);
    }

    #[test]
    fn small_inline_results_remain_exact() {
        let output = "small complete result".to_owned();
        assert_eq!(
            for_model(
                output.clone(),
                &DurableValueRef::InlineJson(output.clone().into())
            ),
            output
        );
    }
}
