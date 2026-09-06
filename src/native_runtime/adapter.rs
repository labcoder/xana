//! Durable duplicate admission checks occur before user entries or effects.
use super::*;

impl Runtime {
    pub(super) async fn start_correlated(
        &mut self,
        binding: crate::operation::adapter::DesktopCommandKey,
        input: String,
        images: Vec<crate::vision::ImageRef>,
    ) {
        let operation_id = binding.operation();
        let result = (|| -> anyhow::Result<bool> {
            let session = self.session.as_ref().ok_or_else(|| {
                anyhow::anyhow!("correlated commands require durable native history")
            })?;
            session.validate_adapter_scope(&binding)?;
            let mut message = Message::text(Role::User, input.clone());
            message.content.extend(
                images
                    .iter()
                    .cloned()
                    .map(crate::message::ContentBlock::Image),
            );
            anyhow::ensure!(
                binding.matches_message(&message)?,
                "adapter command payload collision"
            );
            if let Some(existing) = session.inspect_stored_operation(operation_id)? {
                anyhow::ensure!(
                    existing.adapter.as_ref() == Some(&binding),
                    "adapter command identity collision"
                );
                return Ok(false);
            }
            Ok(true)
        })();
        match result {
            Ok(true) => {
                self.start_turn(operation_id, input, images, None, Some(binding), None)
                    .await
            }
            Ok(false) => self.emit(AgentEvent::CommandRejected {
                reason:
                    "command was already admitted; inspect its durable outcome without replaying it"
                        .into(),
            }),
            Err(error) => self.emit(AgentEvent::CommandRejected {
                reason: format!("correlated command was not admitted: {error:#}"),
            }),
        }
    }
}
