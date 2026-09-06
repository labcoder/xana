//! Extract facts only from execution-owned messages and durable invocation records.

use super::*;
use crate::{
    message::{ContentBlock, Message, Role, ToolResultStatus},
    operation::{DurableValueRef, InvocationOutcome, InvocationTarget},
    session::RestoredOperation,
};
use std::collections::BTreeMap;

pub(crate) fn from_operation(
    evidence: &mut CompletionEvidence,
    operation: &RestoredOperation,
    artifact: impl Fn(&ArtifactRef) -> Option<ArtifactRecord>,
) {
    if operation.operation_id != evidence.generation {
        evidence.omitted_observations = true;
        return;
    }
    for id in operation.invocation_order.iter().take(MAX_EVIDENCE_ITEMS) {
        let Some(intent) = operation.intents.get(id) else {
            evidence.omitted_observations = true;
            continue;
        };
        let InvocationTarget::Tool { name, .. } = &intent.target else {
            continue;
        };
        let result = operation.results.get(id);
        let safe = intent.saved_replay_safety == crate::tool::ReplaySafety::Safe;
        let status = result.and_then(|result| result.command_status);
        let outcome = match result.map(|result| &result.outcome) {
            Some(InvocationOutcome::Completed { .. }) => EffectOutcome::Acknowledged,
            Some(InvocationOutcome::Failed { .. }) if safe || status.is_some() => {
                EffectOutcome::Failed
            }
            Some(InvocationOutcome::Failed { .. }) => EffectOutcome::Unknown,
            Some(InvocationOutcome::Declined { .. }) => EffectOutcome::Declined,
            Some(InvocationOutcome::Interrupted { .. }) | None => EffectOutcome::Unknown,
        };
        observe(
            evidence,
            *id,
            name,
            &intent.final_arguments,
            outcome,
            safe,
            status,
        );
        if let Some(InvocationOutcome::Completed {
            output: DurableValueRef::Artifact(reference),
        }) = result.map(|result| &result.outcome)
        {
            match artifact(reference) {
                Some(record) => add_artifact(evidence, record),
                None => evidence.omitted_observations = true,
            }
        }
    }
    evidence.omitted_observations |= operation.invocation_order.len() > MAX_EVIDENCE_ITEMS;
}

/// Native children keep a bounded per-execution message history. These are the
/// adapter's ToolResult values, not text that a model says is a tool receipt.
pub(crate) fn from_messages(evidence: &mut CompletionEvidence, messages: &[Message]) {
    let mut calls = BTreeMap::new();
    let mut index = 0_u64;
    for message in messages {
        for block in &message.content {
            match block {
                ContentBlock::ToolCall(call) if message.role == Role::Assistant => {
                    if calls.len() >= MAX_EVIDENCE_ITEMS {
                        evidence.omitted_observations = true;
                        continue;
                    }
                    if calls.insert(call.id.as_str(), call).is_some() {
                        evidence.omitted_observations = true;
                    }
                }
                ContentBlock::ToolResult(result) if message.role == Role::Tool => {
                    let Some(call) = calls.remove(result.call_id.as_str()) else {
                        evidence.omitted_observations = true;
                        continue;
                    };
                    index = index.saturating_add(1);
                    let id: ToolInvocationId = uuid::Uuid::new_v5(
                        &uuid::Uuid::NAMESPACE_OID,
                        format!("completion/{}/{index}/{}", evidence.generation, call.id)
                            .as_bytes(),
                    )
                    .to_string()
                    .parse()
                    .expect("UUID");
                    // Unknown extension effects are conservatively treated as possibly mutating.
                    let safe = matches!(
                        call.name.as_str(),
                        "read_file"
                            | "list_files"
                            | "find_files"
                            | "grep_files"
                            | "read_document"
                            | "xana_docs"
                            | "web_fetch"
                    );
                    observe(
                        evidence,
                        id,
                        &call.name,
                        &call.arguments,
                        if result.status == ToolResultStatus::Success {
                            EffectOutcome::Acknowledged
                        } else if safe || result.command_status.is_some() {
                            EffectOutcome::Failed
                        } else {
                            EffectOutcome::Unknown
                        },
                        safe,
                        result.command_status,
                    );
                    if let Some(artifact) = &result.artifact {
                        add_artifact(evidence, artifact.as_ref().clone());
                    }
                }
                _ => {}
            }
        }
    }
    if !calls.is_empty() {
        evidence.omitted_observations = true;
    }
}

fn observe(
    evidence: &mut CompletionEvidence,
    id: ToolInvocationId,
    name: &str,
    arguments: &serde_json::Value,
    outcome: EffectOutcome,
    safe: bool,
    status: Option<CommandStatus>,
) {
    if evidence.effects.len() >= MAX_EVIDENCE_ITEMS {
        evidence.omitted_observations = true;
        return;
    }
    if evidence.effects.iter().any(|fact| fact.invocation == id) {
        evidence.omitted_observations = true;
        return;
    }
    if !safe {
        evidence.work_revision = evidence.work_revision.saturating_add(1);
    }
    evidence.effects.push(EffectEvidence {
        invocation: id,
        generation: evidence.generation,
        outcome,
        replay_safe: safe,
    });
    if name == "run_command" {
        let Some(command) = arguments.get("command").and_then(serde_json::Value::as_str) else {
            evidence.omitted_observations = true;
            return;
        };
        let cwd = arguments
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(".");
        evidence.checks.push(CheckEvidence {
            invocation: id,
            generation: evidence.generation,
            command_digest: command_digest(command, cwd),
            work_revision: evidence.work_revision,
            outcome: match status {
                Some(status) if status.success && status.exit_code == Some(0) => {
                    CheckOutcome::Passed
                }
                Some(_) => CheckOutcome::Failed,
                None => CheckOutcome::Unavailable,
            },
            exit_code: status.and_then(|status| status.exit_code),
        });
    }
}

pub(crate) fn add_artifact(evidence: &mut CompletionEvidence, artifact: ArtifactRecord) {
    if evidence
        .artifacts
        .iter()
        .any(|fact| fact.artifact.reference == artifact.reference)
    {
        return;
    }
    if evidence.artifacts.len() >= MAX_EVIDENCE_ITEMS {
        evidence.omitted_observations = true;
        return;
    }
    evidence.artifacts.push(ArtifactEvidence {
        generation: evidence.generation,
        artifact,
        verified: false,
    });
}
