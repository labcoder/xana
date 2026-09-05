//! Owner-input controls stay outside the Agent loop, including child Agents.
use super::*;

impl Runtime {
    pub(super) async fn run_memory_control(&mut self, operation_id: OperationId, input: String) {
        let user = Message::text(Role::User, input.clone());
        if let Some(session) = &mut self.session {
            let accepted = (|| -> anyhow::Result<()> {
                let entry_id = session.append_message(user.clone())?;
                session.append_record(SessionRecord::OperationAccepted {
                    operation_id,
                    thread_id: session.thread_id(),
                    input_entry_id: entry_id,
                })?;
                Ok(())
            })();
            if let Err(error) = accepted {
                self.emit(AgentEvent::CommandRejected {
                    reason: format!("Could not persist memory control: {error:#}"),
                });
                return;
            }
        }
        self.history.push(user);
        self.emit(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        });
        let owner = self.memory.clone();
        let result=tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let owner=owner.ok_or_else(|| anyhow::anyhow!("Personal memory requires an unlocked protected home; no plaintext memory was created"))?;
            owner.respond(&input).ok_or_else(|| anyhow::anyhow!("Unknown local memory command"))?
        }).await;
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
            self.emit(AgentEvent::OperationFailed {operation_id,reason:format!("Memory control may have committed, but its conversation receipt could not be saved: {error:#}")});
            return;
        }
        self.history.push(message.clone());
        if !self.commit_operation_finished(operation_id, outcome) {
            return;
        }
        self.emit(AgentEvent::AssistantMessage {
            operation_id,
            message,
        });
        self.emit(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(outcome),
        });
    }
}
