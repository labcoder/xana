//! Owner-input controls stay outside the Agent loop, including child Agents.
use super::*;

impl Runtime {
    pub(super) async fn run_memory_control(
        &mut self,
        operation_id: OperationId,
        input: String,
        completion_contract: Option<(
            crate::completion_evidence::WorkKind,
            crate::completion_evidence::CompletionContract,
        )>,
        adapter: Option<crate::operation::adapter::DesktopCommandKey>,
    ) {
        let user = Message::text(Role::User, input.clone());
        let entry = if let Some(session) = &mut self.session {
            match session.append_message(user.clone()) {
                Ok(entry) => Some(entry),
                Err(error) => {
                    self.storage_diagnostic(operation_id);
                    self.emit(AgentEvent::CommandRejected {
                        reason: format!("Could not persist memory control: {error:#}"),
                    });
                    return;
                }
            }
        } else {
            None
        };
        self.history.push(user.clone());
        self.emit(AgentEvent::UserMessageCommitted {
            operation_id,
            message: user,
        });
        if let (Some(session), Some(entry)) = (&mut self.session, entry) {
            let accepted = if let Some(binding) = adapter {
                SessionRecord::AdapterOperationAccepted {
                    operation_id,
                    thread_id: session.thread_id(),
                    input_entry_id: entry,
                    binding,
                }
            } else if let Some((kind, contract)) = completion_contract {
                let mut completion = crate::completion_evidence::CompletionEvidence::new(
                    operation_id,
                    kind,
                    crate::completion_evidence::EvidenceOwner::Native,
                    crate::completion_evidence::CompletionClaim::Interrupted,
                    contract,
                )
                .expect("validated finite contract");
                completion.evaluate();
                SessionRecord::FiniteOperationAccepted {
                    operation_id,
                    thread_id: session.thread_id(),
                    input_entry_id: entry,
                    completion,
                }
            } else {
                SessionRecord::OperationAccepted {
                    operation_id,
                    thread_id: session.thread_id(),
                    input_entry_id: entry,
                }
            };
            let commit = session.append_record(accepted).and_then(|()| {
                if let Some(configuration) = &self.execution_configuration {
                    session.append_record(SessionRecord::OperationConfigurationBound {
                        operation_id,
                        configuration_digest: configuration.digest(),
                    })?;
                }
                Ok(())
            });
            if let Err(error) = commit {
                self.storage_diagnostic(operation_id);
                self.emit(AgentEvent::CommandRejected {
                    reason: format!("Could not persist memory control: {error:#}"),
                });
                return;
            }
        }
        self.emit(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        });
        let owner = self.memory.clone();
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let owner = owner.ok_or_else(|| anyhow::anyhow!(crate::memory::UNAVAILABLE_NOTICE))?;
            owner
                .respond(&input)
                .ok_or_else(|| anyhow::anyhow!("Unknown local memory command"))?
        })
        .await;
        let (reply, outcome) = match result {
            Ok(Ok(reply)) => (reply, OperationOutcome::Completed),
            Ok(Err(error)) => (
                format!("Xana memory control failed: {error:#}"),
                OperationOutcome::Failed,
            ),
            Err(_) => (
                "Xana memory control stopped unexpectedly; inspect the record before retrying."
                    .into(),
                OperationOutcome::Failed,
            ),
        };
        let message = Message::text(Role::Assistant, reply);
        if let Some(session) = &mut self.session
            && let Err(error) = session.append_message(message.clone())
        {
            self.storage_diagnostic(operation_id);
            self.emit(AgentEvent::OperationFailed {operation_id,reason:format!("Memory control may have committed, but its conversation receipt could not be saved: {error:#}")});
            return;
        }
        self.history.push(message.clone());
        let delivered = crate::completion_evidence::message_text(&message);
        self.emit(AgentEvent::AssistantMessage {
            operation_id,
            message,
        });
        let claim = if outcome == OperationOutcome::Completed {
            crate::completion_evidence::CompletionClaim::Completed
        } else {
            crate::completion_evidence::CompletionClaim::Failed
        };
        let completion_supported = self
            .record_completion_evidence(operation_id, claim, &delivered)
            .await;
        let outcome = if outcome == OperationOutcome::Completed && !completion_supported {
            OperationOutcome::Failed
        } else {
            outcome
        };
        if !self.commit_operation_finished(operation_id, outcome) {
            return;
        }
        self.emit(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(outcome),
        });
    }
}
