//! Closed deterministic operations over a retained worker's immutable evidence.
//! Full verification work, not only returned slices, consumes the shared byte
//! allowance. No code evaluation, implicit model call, or ambient file discovery.
#[cfg(test)]
mod tests;
mod tool;
pub(crate) use tool::WorkerContextTool;

use super::retained::{CONTEXT_TOTAL_BYTES, CONTEXT_TOTAL_OPS, EVIDENCE_LIMIT, execution};
use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ArtifactStore},
    identity::{AgentId, PrincipalId},
    paths::XanaPaths,
    storage::ProtectedStore,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_INPUTS: usize = 16;
const MAX_SELECTED_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 256 * 1024;
const MAX_VERIFIED_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceRange {
    pub(crate) artifact: ArtifactRef,
    pub(crate) offset: u64,
    pub(crate) length: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Transform {
    Trim,
    Lowercase,
    Uppercase,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reducer {
    CountBytes,
    CountLines,
    Concat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ContextOperation {
    Search {
        inputs: Vec<EvidenceRange>,
        query: String,
    },
    Slice {
        input: EvidenceRange,
    },
    Filter {
        inputs: Vec<EvidenceRange>,
        contains: String,
    },
    Map {
        inputs: Vec<EvidenceRange>,
        transform: Transform,
    },
    Reduce {
        inputs: Vec<EvidenceRange>,
        reducer: Reducer,
    },
    Derive {
        inputs: Vec<EvidenceRange>,
        label: String,
    },
    Cite {
        inputs: Vec<EvidenceRange>,
    },
}

impl ContextOperation {
    fn inputs(&self) -> &[EvidenceRange] {
        match self {
            Self::Slice { input } => std::slice::from_ref(input),
            Self::Search { inputs, .. }
            | Self::Filter { inputs, .. }
            | Self::Map { inputs, .. }
            | Self::Reduce { inputs, .. }
            | Self::Derive { inputs, .. }
            | Self::Cite { inputs } => inputs,
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.inputs().is_empty() && self.inputs().len() <= MAX_INPUTS,
            "context operation requires 1–16 inputs"
        );
        let mut total = 0usize;
        for input in self.inputs() {
            ensure!(
                input.length > 0 && input.length <= 64 * 1024,
                "context range must be 1–65536 bytes"
            );
            total = total
                .checked_add(input.length)
                .context("context range overflow")?;
        }
        ensure!(
            total <= MAX_SELECTED_BYTES,
            "selected context exceeds 256 KiB"
        );
        match self {
            Self::Search { query, .. }
            | Self::Filter {
                contains: query, ..
            }
            | Self::Derive { label: query, .. } => ensure!(
                !query.trim().is_empty() && query.len() <= 256 && !query.contains('\0'),
                "context query/label must be 1–256 bytes"
            ),
            _ => (),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextWorkReceipt {
    pub(crate) id: Uuid,
    pub(crate) state: ContextWorkState,
    pub(crate) verified_bytes: u64,
    pub(crate) selected_bytes: usize,
    pub(crate) input_count: usize,
    pub(crate) model_calls: u64,
    pub(crate) result: Option<ArtifactRecord>,
    pub(crate) preview: String,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextWorkState {
    Reserved,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}

#[derive(Debug, Serialize)]
struct Fragment {
    text: String,
    citations: Vec<EvidenceRange>,
}
#[derive(Debug, Serialize)]
struct Derived {
    version: u32,
    operation: ContextOperation,
    fragments: Vec<Fragment>,
}

pub(crate) fn execute(
    paths: &XanaPaths,
    store: &ProtectedStore,
    id: AgentId,
    revision: u64,
    operation: ContextOperation,
    cancellation: &CancellationToken,
) -> Result<ContextWorkReceipt> {
    let worker = store.retained_worker(id)?;
    execution::check_scope(paths, store, &worker)?;
    ensure!(
        !cancellation.is_cancelled(),
        "context operation cancelled before admission"
    );
    operation.validate()?;
    let inputs = operation.inputs();
    let artifacts = inputs
        .iter()
        .map(|range| {
            worker
                .evidence
                .iter()
                .find(|a| a.reference == range.artifact)
                .cloned()
                .context("context source is not selected immutable worker evidence")
        })
        .collect::<Result<Vec<_>>>()?;
    let verified_bytes = artifacts.iter().try_fold(0u64, |total, artifact| {
        total
            .checked_add(artifact.byte_len)
            .context("verification work overflow")
    })?;
    ensure!(
        verified_bytes <= MAX_VERIFIED_BYTES,
        "context verification work exceeds 16 MiB; split the request"
    );
    for (range, artifact) in inputs.iter().zip(&artifacts) {
        ensure!(
            range
                .offset
                .checked_add(range.length as u64)
                .is_some_and(|end| end <= artifact.byte_len),
            "context range is outside immutable source"
        );
    }
    let mut receipt = ContextWorkReceipt {
        id: Uuid::new_v4(),
        state: ContextWorkState::Reserved,
        verified_bytes,
        selected_bytes: inputs.iter().map(|i| i.length).sum(),
        input_count: inputs.len(),
        model_calls: 0,
        result: None,
        preview: String::new(),
        error: None,
    };
    let (reserved, ()) = store.retained_admit(id, revision, |worker| {
        ensure!(
            !worker
                .context_receipt
                .as_ref()
                .is_some_and(|r| r.state == ContextWorkState::Reserved),
            "context operation is active or unknown; inspect or recover it before retrying"
        );
        ensure!(
            worker.evidence.len() < EVIDENCE_LIMIT,
            "worker evidence inventory is full"
        );
        worker.context_bytes = worker
            .context_bytes
            .checked_add(verified_bytes)
            .filter(|n| *n <= CONTEXT_TOTAL_BYTES)
            .context("cumulative context byte allowance exhausted")?;
        worker.context_operations = worker
            .context_operations
            .checked_add(1)
            .filter(|n| *n <= CONTEXT_TOTAL_OPS)
            .context("cumulative context operation allowance exhausted")?;
        worker.context_receipt = Some(receipt.clone());
        Ok(())
    })?;
    let result = materialize(
        &ArtifactStore::protected(store.clone()),
        &operation,
        &artifacts,
        cancellation,
    );
    // Scope/forget/stop races prevent a result becoming eligible. The reservation
    // remains charged on errors and process death; unknown work is never zero.
    let result = result.and_then(|bytes| {
        ensure!(!cancellation.is_cancelled(), "context operation cancelled");
        execution::check_scope(paths, store, &reserved)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let text = value["fragments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        receipt.preview = crate::orchestration::truncate_utf8(&text, 2048);
        ArtifactStore::protected(store.clone())
            .put(&bytes, "application/json", PrincipalId::new())
            .map(|(record, _)| record)
            .map_err(Into::into)
    });
    receipt.state = if result.is_ok() {
        ContextWorkState::Completed
    } else if cancellation.is_cancelled() {
        ContextWorkState::Cancelled
    } else {
        ContextWorkState::Failed
    };
    receipt.result = result.as_ref().ok().cloned();
    receipt.error = result
        .as_ref()
        .err()
        .map(|error| crate::orchestration::truncate_utf8(&error.to_string(), 2048));
    let finish = |worker: &mut super::retained::RetainedWorker| {
        ensure!(
            worker
                .context_receipt
                .as_ref()
                .is_some_and(|r| r.id == receipt.id && r.state == ContextWorkState::Reserved)
                && worker.cancellation == reserved.cancellation,
            "another context operation replaced this reservation"
        );
        if let Some(record) = &receipt.result {
            worker.evidence.push(record.clone());
        }
        worker.context_receipt = Some(receipt.clone());
        Ok(())
    };
    store.retained_settle(id, result.is_ok(), finish)?;
    result?;
    Ok(receipt)
}

fn materialize(
    store: &ArtifactStore,
    operation: &ContextOperation,
    artifacts: &[ArtifactRecord],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>> {
    materialize_observed(store, operation, artifacts, cancellation, |_| {})
}

fn materialize_observed(
    store: &ArtifactStore,
    operation: &ContextOperation,
    artifacts: &[ArtifactRecord],
    cancellation: &CancellationToken,
    mut visited: impl FnMut(usize),
) -> Result<Vec<u8>> {
    let mut fragments = Vec::new();
    for (index, (range, artifact)) in operation.inputs().iter().zip(artifacts).enumerate() {
        visited(index);
        ensure!(!cancellation.is_cancelled(), "context operation cancelled");
        let bytes = store
            .read_verified_range(
                artifact,
                range.offset,
                range.length,
                crate::artifact::MAX_ARTIFACT_BYTES,
            )?
            .bytes;
        let text = String::from_utf8(bytes)
            .context("context requires complete UTF-8 range boundaries and text sources")?;
        match operation {
            ContextOperation::Search { query, .. }
            | ContextOperation::Filter {
                contains: query, ..
            } => {
                let mut offset = range.offset;
                for line in text.split_inclusive('\n') {
                    ensure!(!cancellation.is_cancelled(), "context operation cancelled");
                    if line.contains(query) {
                        ensure!(
                            fragments.len() < 128,
                            "more than 128 matches; narrow the query/ranges"
                        );
                        fragments.push(Fragment {
                            text: line.into(),
                            citations: vec![EvidenceRange {
                                artifact: range.artifact.clone(),
                                offset,
                                length: line.len(),
                            }],
                        });
                    }
                    offset += line.len() as u64;
                }
            }
            ContextOperation::Cite { .. } => fragments.push(Fragment {
                text: String::new(),
                citations: vec![range.clone()],
            }),
            ContextOperation::Map { transform, .. } => fragments.push(Fragment {
                text: match transform {
                    Transform::Trim => text.trim().into(),
                    Transform::Lowercase => text.to_lowercase(),
                    Transform::Uppercase => text.to_uppercase(),
                },
                citations: vec![range.clone()],
            }),
            _ => fragments.push(Fragment {
                text,
                citations: vec![range.clone()],
            }),
        }
    }
    if let ContextOperation::Reduce { reducer, .. } = operation {
        let text = match reducer {
            Reducer::CountBytes => fragments
                .iter()
                .map(|f| f.text.len())
                .sum::<usize>()
                .to_string(),
            Reducer::CountLines => fragments
                .iter()
                .map(|f| f.text.lines().count())
                .sum::<usize>()
                .to_string(),
            Reducer::Concat => fragments
                .iter()
                .map(|f| f.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        fragments = vec![Fragment {
            text,
            citations: operation.inputs().to_vec(),
        }];
    }
    let bytes = serde_json::to_vec(&Derived {
        version: 1,
        operation: operation.clone(),
        fragments,
    })?;
    ensure!(
        bytes.len() <= MAX_RESULT_BYTES,
        "derived result exceeds 256 KiB; use smaller ranges"
    );
    Ok(bytes)
}
