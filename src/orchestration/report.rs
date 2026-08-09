use super::{
    ChildAttribution, ChildReport, ChildReportReference, ChildResultSchema, ChildUsage,
    truncate_utf8,
};
use crate::{
    artifact::{ArtifactRecord, ArtifactStore, MAX_ARTIFACT_BYTES},
    config::OrchestrationLimits,
    identity::PrincipalId,
};

const REPORT_MEDIA_TYPE: &str = "text/plain; charset=utf-8";
const JSON_MEDIA_TYPE: &str = "application/json";
const MAX_ARTIFACT_PREVIEW_BYTES: usize = 4 * 1024;

pub(crate) struct MaterializedChildReport {
    pub(crate) report: ChildReport,
    pub(crate) artifact: Option<ArtifactRecord>,
}

pub(crate) async fn materialize_completed_report(
    attribution: ChildAttribution,
    schema: ChildResultSchema,
    output: String,
    usage: ChildUsage,
    limits: OrchestrationLimits,
    artifacts: ArtifactStore,
    owner: PrincipalId,
) -> MaterializedChildReport {
    let (output, media_type) = match normalize_output(schema, output) {
        Ok(normalized) => normalized,
        Err(reason) => {
            return MaterializedChildReport {
                report: ChildReport::failed_with_schema(
                    attribution,
                    schema,
                    reason,
                    limits.max_report_bytes,
                ),
                artifact: None,
            };
        }
    };
    if output.len() <= limits.max_report_bytes {
        let byte_len = output.len();
        return MaterializedChildReport {
            report: ChildReport::completed_with_reference(
                attribution,
                schema,
                output,
                usage,
                ChildReportReference::Inline { byte_len },
            ),
            artifact: None,
        };
    }
    let artifact_limit = limits.max_artifact_bytes.min(MAX_ARTIFACT_BYTES);
    if output.len() > artifact_limit {
        return MaterializedChildReport {
            report: ChildReport::failed_with_schema(
                attribution,
                schema,
                format!(
                    "child output contains {} bytes, exceeding the {}-byte artifact limit",
                    output.len(),
                    artifact_limit
                ),
                limits.max_report_bytes,
            ),
            artifact: None,
        };
    }

    let preview_limit = limits.max_report_bytes.min(MAX_ARTIFACT_PREVIEW_BYTES);
    let preview = truncate_utf8(&output, preview_limit);
    let bytes = output.into_bytes();
    let byte_len = bytes.len() as u64;
    let stored =
        tokio::task::spawn_blocking(move || artifacts.put(&bytes, media_type, owner)).await;
    let artifact = match stored {
        Ok(Ok((artifact, _))) => artifact,
        Ok(Err(error)) => {
            return MaterializedChildReport {
                report: ChildReport::failed_with_schema(
                    attribution,
                    schema,
                    format!("could not persist child report artifact: {error}"),
                    limits.max_report_bytes,
                ),
                artifact: None,
            };
        }
        Err(error) => {
            return MaterializedChildReport {
                report: ChildReport::failed_with_schema(
                    attribution,
                    schema,
                    format!("child report artifact task failed: {error}"),
                    limits.max_report_bytes,
                ),
                artifact: None,
            };
        }
    };
    let reference = ChildReportReference::Artifact {
        artifact: artifact.reference.clone(),
        byte_len,
        preview_byte_len: preview.len(),
    };
    MaterializedChildReport {
        report: ChildReport::completed_with_reference(
            attribution,
            schema,
            preview,
            usage,
            reference,
        ),
        artifact: Some(artifact),
    }
}

fn normalize_output(
    schema: ChildResultSchema,
    output: String,
) -> Result<(String, &'static str), String> {
    match schema {
        ChildResultSchema::Summary => Ok((output, REPORT_MEDIA_TYPE)),
        ChildResultSchema::Json => {
            let value = serde_json::from_str::<serde_json::Value>(&output)
                .map_err(|error| format!("child result is not valid JSON: {error}"))?;
            let canonical = serde_json::to_string(&value)
                .map_err(|error| format!("child JSON result cannot be encoded: {error}"))?;
            Ok((canonical, JSON_MEDIA_TYPE))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::OrchestrationLimits,
        identity::{AgentId, OperationId, SessionId, ThreadId},
        orchestration::{ChildAttribution, ExecutionOwner},
    };
    use tempfile::tempdir;

    fn attribution() -> ChildAttribution {
        let session = SessionId::new();
        ChildAttribution {
            agent_id: AgentId::new(),
            parent_agent_id: AgentId::for_session(session),
            operation_id: OperationId::new(),
            parent_operation_id: OperationId::new(),
            thread_id: ThreadId::new(),
            route: "worker".to_owned(),
            profile: "worker".to_owned(),
            owner: ExecutionOwner::Native,
            connection: "local".to_owned(),
            model: "small".to_owned(),
        }
    }

    #[tokio::test]
    async fn exact_inline_boundary_stays_inline_and_one_over_uses_an_artifact() {
        let directory = tempdir().expect("temporary directory");
        let artifacts = ArtifactStore::new(directory.path().to_owned());
        let limits = OrchestrationLimits {
            max_report_bytes: 4,
            max_artifact_bytes: 16,
            ..Default::default()
        };
        let inline = materialize_completed_report(
            attribution(),
            ChildResultSchema::Summary,
            "1234".to_owned(),
            ChildUsage::Unknown,
            limits.clone(),
            artifacts.clone(),
            PrincipalId::new(),
        )
        .await;
        assert!(matches!(
            inline.report.reference,
            ChildReportReference::Inline { byte_len: 4 }
        ));
        assert!(inline.artifact.is_none());

        let overflow = materialize_completed_report(
            attribution(),
            ChildResultSchema::Summary,
            "12345".to_owned(),
            ChildUsage::Unknown,
            limits,
            artifacts,
            PrincipalId::new(),
        )
        .await;
        assert!(matches!(
            overflow.report.reference,
            ChildReportReference::Artifact { byte_len: 5, .. }
        ));
        assert_eq!(overflow.artifact.expect("artifact").byte_len, 5);
    }

    #[tokio::test]
    async fn json_schema_canonicalizes_or_returns_an_attributed_failure() {
        let directory = tempdir().expect("temporary directory");
        let artifacts = ArtifactStore::new(directory.path().to_owned());
        let valid = materialize_completed_report(
            attribution(),
            ChildResultSchema::Json,
            "{ \"answer\": 42 }".to_owned(),
            ChildUsage::Unknown,
            OrchestrationLimits::default(),
            artifacts.clone(),
            PrincipalId::new(),
        )
        .await;
        assert_eq!(valid.report.output.as_deref(), Some("{\"answer\":42}"));
        let invalid = materialize_completed_report(
            attribution(),
            ChildResultSchema::Json,
            "not json".to_owned(),
            ChildUsage::Unknown,
            OrchestrationLimits::default(),
            artifacts,
            PrincipalId::new(),
        )
        .await;
        assert_eq!(
            invalid.report.status,
            crate::orchestration::ChildTerminalStatus::Failed
        );
        assert!(
            invalid
                .report
                .error
                .as_deref()
                .is_some_and(|error| { error.contains("child result is not valid JSON") })
        );
    }

    #[tokio::test]
    async fn artifact_persistence_failure_becomes_a_bounded_attributed_report() {
        let directory = tempdir().expect("temporary directory");
        let invalid_root = directory.path().join("not-a-directory");
        std::fs::write(&invalid_root, b"file").expect("blocking path fixture");
        let limits = OrchestrationLimits {
            max_report_bytes: 64,
            max_artifact_bytes: 128,
            ..Default::default()
        };
        let result = materialize_completed_report(
            attribution(),
            ChildResultSchema::Summary,
            "x".repeat(65),
            ChildUsage::Unknown,
            limits,
            ArtifactStore::new(invalid_root),
            PrincipalId::new(),
        )
        .await;
        assert!(result.artifact.is_none());
        assert_eq!(
            result.report.status,
            crate::orchestration::ChildTerminalStatus::Failed
        );
        assert!(
            result
                .report
                .error
                .as_deref()
                .is_some_and(|error| error.contains("could not persist child report artifact"))
        );
    }
}
