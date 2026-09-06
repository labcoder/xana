//! Finite-work declarations and one-shot verification belong to the owner.

use super::*;
use crate::completion_evidence::{CompletionClaim, CompletionEvidence};

impl Runtime {
    /// No declaration means an ordinary conversational turn, not an implicit
    /// acceptance score. Reserved verification after restart is never retried.
    pub(super) async fn record_completion_evidence(
        &mut self,
        operation_id: OperationId,
        claim: CompletionClaim,
        final_text: &str,
    ) -> bool {
        let Some(session) = &self.session else {
            return true;
        };
        let Some(mut evidence) = session
            .completion_evidence()
            .iter()
            .find(|item| item.generation == operation_id)
            .cloned()
        else {
            return true;
        };
        if evidence.revision != 1 {
            return self.publish_completion_evidence(operation_id, claim, evidence);
        }
        if let Some(budget) = self.agent.completion_usage_budget() {
            match tokio::task::spawn_blocking(move || budget.remaining(operation_id)).await {
                Ok(Ok(remaining)) => {
                    evidence.budget.remaining_requests = Some(remaining.requests);
                    evidence.budget.remaining_tokens = remaining.tokens;
                    evidence.budget.exhausted =
                        remaining.requests == 0 || remaining.tokens == Some(0);
                    evidence.budget.exceeded = remaining.exceeded;
                }
                _ => {
                    // A broken ledger is not an unlimited allowance.
                    evidence.omitted_observations = true;
                    self.storage_diagnostic(operation_id);
                }
            }
        }
        let session = self
            .session
            .as_mut()
            .expect("finite owner session retained");
        evidence.revision = 2;
        evidence.claim = claim;
        if !final_text.is_empty() {
            evidence.delivered(final_text.as_bytes());
        }
        if let Some(operation) = session.restored_operation(operation_id) {
            evidence.omitted_observations |= !session.completion_calls_prepared(&operation);
            crate::completion_evidence::from_operation(&mut evidence, &operation, |reference| {
                session.stored_artifact(&crate::operation::DurableValueRef::Artifact(
                    reference.clone(),
                ))
            });
        } else {
            evidence.omitted_observations = true;
        }
        let children = session.completion_children(operation_id);
        evidence.omitted_observations |=
            children.len() > crate::completion_evidence::MAX_EVIDENCE_ITEMS;
        for child in children
            .into_iter()
            .take(crate::completion_evidence::MAX_EVIDENCE_ITEMS)
        {
            let report = child.report.as_ref();
            evidence
                .child_reports
                .push(crate::completion_evidence::ChildEvidenceRef {
                    generation: child.handle.admission.attribution.operation_id,
                    receipt_digest: report
                        .and_then(|report| serde_json::to_vec(report).ok())
                        .map(|bytes| crate::artifact::ContentHash::for_bytes(&bytes)),
                    outcome: report.and_then(|report| report.evidence.as_ref()).map_or(
                        crate::completion_evidence::EvidenceOutcome::NeedsAttention,
                        |evidence| evidence.outcome,
                    ),
                });
        }
        // Only the owner's already registered immutable artifacts are eligible;
        // missing declared references stay unsatisfied, never trigger a read.
        let result = (|| -> anyhow::Result<bool> {
            let reserved = evidence.reserve_verifier(
                claim == CompletionClaim::Completed,
                claim == CompletionClaim::Cancelled,
            )?;
            session.append_record(SessionRecord::CompletionEvidenceRecorded {
                evidence: evidence.clone(),
            })?;
            Ok(reserved)
        })();
        let reserved = match result {
            Ok(value) => value,
            Err(error) => {
                self.storage_diagnostic(operation_id);
                self.emit(AgentEvent::OperationFailed {
                    operation_id,
                    reason: format!("could not commit completion evidence: {error:#}"),
                });
                return false;
            }
        };
        if reserved {
            let store = session.completion_artifact_store();
            let reserved_evidence = evidence.clone();
            // A small byte-ceiling pass runs off the event-loop thread. No model,
            // process or effect replay is available to this verifier.
            let cancelled = tokio_util::sync::CancellationToken::new();
            let worker_cancelled = cancelled.clone();
            let worker = tokio::task::spawn_blocking(move || {
                crate::completion_evidence::verify_artifacts(
                    reserved_evidence,
                    &store,
                    &worker_cancelled,
                )
            });
            let events = self.events.clone();
            let (result, shutdown) = await_verifier(&mut self.commands, operation_id, &cancelled, worker, || {
                let _ = events.send(AgentEvent::CommandRejected { reason: "completion verification is active; wait or cancel before changing this Conversation".to_owned() });
            }).await;
            self.stop_after_verification |= shutdown;
            if shutdown {
                self.terminal_diagnostic(
                    Some(operation_id),
                    crate::failure::TerminalOutcome::HostShutdown,
                    crate::failure::FailureDetails::new(
                        crate::failure::FailureCategory::HostShutdown,
                        crate::failure::FailureStage::Shutdown,
                    ),
                );
            } else if cancelled.is_cancelled() {
                self.terminal_diagnostic(
                    Some(operation_id),
                    crate::failure::TerminalOutcome::Cancelled,
                    crate::failure::FailureDetails::new(
                        crate::failure::FailureCategory::Cancelled,
                        crate::failure::FailureStage::Execution,
                    ),
                );
            }
            if let Ok(Ok(mut verified)) = result {
                if cancelled.is_cancelled() {
                    verified.verification =
                        crate::completion_evidence::VerificationState::Cancelled;
                    verified.evaluate();
                }
                if let Err(error) = self
                    .session
                    .as_mut()
                    .expect("owner session retained")
                    .append_record(SessionRecord::CompletionEvidenceRecorded {
                        evidence: verified.clone(),
                    })
                {
                    self.storage_diagnostic(operation_id);
                    self.emit(AgentEvent::OperationFailed {
                        operation_id,
                        reason: format!("could not commit verification result: {error:#}"),
                    });
                    return false;
                }
                evidence = verified;
            }
            // A failed worker leaves a durable Reserved receipt (needs attention).
        }
        self.publish_completion_evidence(operation_id, claim, evidence)
    }

    fn publish_completion_evidence(
        &mut self,
        operation_id: OperationId,
        claim: CompletionClaim,
        evidence: CompletionEvidence,
    ) -> bool {
        let supported = evidence.supported();
        let summary = evidence.summary();
        self.emit(AgentEvent::CompletionEvidenceRecorded {
            operation_id,
            evidence,
        });
        if claim == CompletionClaim::Completed && !supported {
            self.emit(AgentEvent::OperationFailed {
                operation_id,
                reason: summary,
            });
            return false;
        }
        true
    }

    pub(super) fn emit_restored_completion_evidence(&self) {
        if let Some(session) = &self.session {
            for evidence in session.completion_evidence() {
                self.emit(AgentEvent::CompletionEvidenceRecorded {
                    operation_id: evidence.generation,
                    evidence: evidence.clone(),
                });
            }
        }
    }
}

/// Keep the blocking verifier owned until it joins, while accepting cancellation
/// for this generation. Fair selection prevents a command flood starving a
/// completed verifier; unrelated commands cannot mutate its frozen input.
async fn await_verifier(
    commands: &mut mpsc::Receiver<RuntimeCommand>,
    operation: OperationId,
    cancelled: &tokio_util::sync::CancellationToken,
    mut worker: JoinHandle<anyhow::Result<CompletionEvidence>>,
    mut rejected: impl FnMut(),
) -> (
    Result<anyhow::Result<CompletionEvidence>, tokio::task::JoinError>,
    bool,
) {
    let mut shutdown = false;
    loop {
        tokio::select! {
            command = commands.recv(), if !shutdown => match command {
                Some(RuntimeCommand::InterruptOperation { operation_id }) if operation_id == operation => cancelled.cancel(),
                Some(RuntimeCommand::Shutdown) | None => { cancelled.cancel(); shutdown = true; }
                Some(_) => rejected(),
            },
            result = &mut worker => return (result, shutdown),
        }
    }
}

#[cfg(test)]
mod tests;
