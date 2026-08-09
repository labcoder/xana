use super::{
    COLLECTION_RESULT_VERSION, ChildInspection, ChildReport, ChildReportReference,
    ChildResultSchema, ChildUsage, CollectedChildResult, CollectedValue, CollectionEntryState,
    CollectionFailurePolicy, CollectionResult, MAX_COLLECTION_PREVIEW_BYTES,
    MAX_COLLECTION_RESULT_BYTES, truncate_utf8,
};
use crate::{artifact::ArtifactStore, identity::AgentId};
use std::collections::BTreeMap;

pub(crate) enum CollectionObservation {
    Terminal(Box<ChildReport>),
    TimedOut { cancellation_requested: bool },
    SkippedAfterFailure { cancellation_requested: bool },
    Error(String),
}

pub(crate) fn build_collection_result(
    inspections: Vec<ChildInspection>,
    observations: &mut BTreeMap<AgentId, CollectionObservation>,
    failure_policy: CollectionFailurePolicy,
    artifacts: &ArtifactStore,
) -> Result<CollectionResult, String> {
    let mut entries = Vec::with_capacity(inspections.len());
    for inspection in inspections {
        let attribution = inspection.handle.admission.attribution;
        let observation = observations.remove(&attribution.agent_id).ok_or_else(|| {
            format!(
                "collection lost observation for child {}",
                attribution.agent_id
            )
        })?;
        entries.push(match observation {
            CollectionObservation::Terminal(report) => terminal_entry(
                *report,
                artifacts,
                inspection.handle.admission.limits.max_artifact_bytes,
            ),
            CollectionObservation::TimedOut {
                cancellation_requested,
            } => pending_entry(
                attribution,
                CollectionEntryState::TimedOut,
                "collection timed out before a terminal report was observed",
                cancellation_requested,
            ),
            CollectionObservation::SkippedAfterFailure {
                cancellation_requested,
            } => pending_entry(
                attribution,
                CollectionEntryState::SkippedAfterFailure,
                "fail-fast collection stopped after another child failed",
                cancellation_requested,
            ),
            CollectionObservation::Error(error) => pending_entry(
                attribution,
                CollectionEntryState::CollectionError,
                &error,
                false,
            ),
        });
    }
    let complete = entries.iter().all(|entry| {
        matches!(
            entry.state,
            CollectionEntryState::Terminal | CollectionEntryState::ArtifactUnavailable
        )
    });
    let result = CollectionResult {
        version: COLLECTION_RESULT_VERSION,
        failure_policy,
        entries,
        complete,
    };
    let encoded = serde_json::to_vec(&result)
        .map_err(|error| format!("could not encode collection result: {error}"))?;
    if encoded.len() > MAX_COLLECTION_RESULT_BYTES {
        return Err(format!(
            "collection result contains {} bytes, exceeding the {}-byte model-facing limit",
            encoded.len(),
            MAX_COLLECTION_RESULT_BYTES
        ));
    }
    Ok(result)
}

fn terminal_entry(
    report: ChildReport,
    artifacts: &ArtifactStore,
    max_artifact_bytes: usize,
) -> CollectedChildResult {
    let attribution = report.attribution.clone();
    let status = Some(report.status);
    let usage = report.usage.clone();
    let reference = Some(report.reference.clone());
    if let ChildReportReference::Artifact {
        artifact, byte_len, ..
    } = &report.reference
        && let Err(error) = artifacts.verify_reference(artifact, *byte_len, max_artifact_bytes)
    {
        return CollectedChildResult {
            attribution,
            state: CollectionEntryState::ArtifactUnavailable,
            status,
            usage,
            reference,
            value: None,
            error: Some(truncate_utf8(
                &error.to_string(),
                MAX_COLLECTION_PREVIEW_BYTES,
            )),
            cancellation_requested: false,
        };
    }

    let value = report
        .output
        .as_deref()
        .map(|output| collected_value(report.schema, output, &report.reference));
    CollectedChildResult {
        attribution,
        state: CollectionEntryState::Terminal,
        status,
        usage,
        reference,
        value,
        error: report
            .error
            .as_deref()
            .map(|error| truncate_utf8(error, MAX_COLLECTION_PREVIEW_BYTES)),
        cancellation_requested: false,
    }
}

fn collected_value(
    schema: ChildResultSchema,
    output: &str,
    reference: &ChildReportReference,
) -> CollectedValue {
    match reference {
        ChildReportReference::Artifact {
            artifact, byte_len, ..
        } => CollectedValue::ArtifactPreview {
            schema,
            preview: truncate_utf8(output, MAX_COLLECTION_PREVIEW_BYTES),
            artifact: artifact.clone(),
            byte_len: *byte_len,
        },
        ChildReportReference::Inline { .. }
            if schema == ChildResultSchema::Json
                && output.len() <= MAX_COLLECTION_PREVIEW_BYTES =>
        {
            match serde_json::from_str(output) {
                Ok(value) => CollectedValue::Json { value },
                Err(_) => CollectedValue::Preview {
                    schema,
                    text: truncate_utf8(output, MAX_COLLECTION_PREVIEW_BYTES),
                    truncated: false,
                },
            }
        }
        ChildReportReference::Inline { .. } if schema == ChildResultSchema::Summary => {
            CollectedValue::Summary {
                text: truncate_utf8(output, MAX_COLLECTION_PREVIEW_BYTES),
            }
        }
        ChildReportReference::Inline { .. } => CollectedValue::Preview {
            schema,
            text: truncate_utf8(output, MAX_COLLECTION_PREVIEW_BYTES),
            truncated: output.len() > MAX_COLLECTION_PREVIEW_BYTES,
        },
    }
}

fn pending_entry(
    attribution: super::ChildAttribution,
    state: CollectionEntryState,
    error: &str,
    cancellation_requested: bool,
) -> CollectedChildResult {
    CollectedChildResult {
        attribution,
        state,
        status: None,
        usage: ChildUsage::Unknown,
        reference: None,
        value: None,
        error: Some(truncate_utf8(error, MAX_COLLECTION_PREVIEW_BYTES)),
        cancellation_requested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{OrchestrationLimits, PermissionMode},
        identity::{AgentId, OperationId, SessionId, ThreadId},
        orchestration::{
            AgentHandleSnapshot, ChildAdmission, ChildAttribution, ChildResultSchema,
            ExecutionOwner,
        },
    };
    use tempfile::tempdir;

    fn inspection(output: String) -> (ChildInspection, ChildReport) {
        let session = SessionId::new();
        let attribution = ChildAttribution {
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
        };
        let mut handle = AgentHandleSnapshot::admitted(ChildAdmission {
            attribution: attribution.clone(),
            plan: None,
            task_preview: "task".to_owned(),
            task_hash: blake3::hash(b"task").to_hex().to_string(),
            result_schema: ChildResultSchema::Summary,
            capabilities: Vec::new(),
            permission_mode: PermissionMode::Deny,
            max_tool_rounds: 1,
            limits: OrchestrationLimits::default(),
            hard_token_limit: None,
            hard_spend_microusd: None,
        });
        let report = ChildReport::completed(attribution, output, ChildUsage::Unknown);
        handle.apply_report(&report);
        (
            ChildInspection {
                handle,
                report: Some(report.clone()),
                projected_interruption: false,
            },
            report,
        )
    }

    #[test]
    fn aggregate_serialization_stays_bounded_for_adversarial_inline_reports() {
        let directory = tempdir().expect("temporary directory");
        let artifacts = ArtifactStore::new(directory.path().to_owned());
        let mut inspections = Vec::new();
        let mut observations = BTreeMap::new();
        for _ in 0..super::super::MAX_COLLECTION_HANDLES {
            let (inspection, report) = inspection("x".repeat(32 * 1024));
            observations.insert(
                report.attribution.agent_id,
                CollectionObservation::Terminal(Box::new(report)),
            );
            inspections.push(inspection);
        }
        let result = build_collection_result(
            inspections,
            &mut observations,
            CollectionFailurePolicy::ContinueOnError,
            &artifacts,
        )
        .expect("bounded collection");
        assert!(serde_json::to_vec(&result).expect("encode").len() <= MAX_COLLECTION_RESULT_BYTES);
    }

    #[test]
    fn timeout_and_terminal_entries_preserve_requested_input_order() {
        let directory = tempdir().expect("temporary directory");
        let artifacts = ArtifactStore::new(directory.path().to_owned());
        let (first, first_report) = inspection("first".to_owned());
        let (second, _) = inspection("second".to_owned());
        let expected = [
            first.handle.admission.attribution.agent_id,
            second.handle.admission.attribution.agent_id,
        ];
        let mut observations = BTreeMap::from([
            (
                expected[1],
                CollectionObservation::TimedOut {
                    cancellation_requested: false,
                },
            ),
            (
                expected[0],
                CollectionObservation::Terminal(Box::new(first_report)),
            ),
        ]);
        let result = build_collection_result(
            vec![first, second],
            &mut observations,
            CollectionFailurePolicy::ContinueOnError,
            &artifacts,
        )
        .expect("collection");
        assert_eq!(
            result
                .entries
                .iter()
                .map(|entry| entry.attribution.agent_id)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(result.entries[1].state, CollectionEntryState::TimedOut);
    }
}
