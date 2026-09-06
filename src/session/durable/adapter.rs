//! Adapter records are committed by the sole session writer.
use super::*;
use crate::operation::adapter::{DesktopCommandKey, DesktopCommandResultRef, message_digest};

impl DurableSession {
    pub(crate) fn validate_adapter_scope(&self, binding: &DesktopCommandKey) -> Result<()> {
        binding.validate()?;
        let home = self.store.protected_home();
        let data = if let Some(home) = home {
            home.data_dir()
        } else {
            self.store
                .path()
                .parent()
                .and_then(Path::parent)
                .context("session data owner unavailable")?
        };
        let profile = Self::adapter_profile_digest(data, home, self.session_id())?;
        anyhow::ensure!(
            binding.matches_owner(self.session_id(), home.map(|home| home.id()), &profile),
            "adapter owner scope differs"
        );
        Ok(())
    }

    pub(crate) fn adapter_profile_digest(
        data: &Path,
        home: Option<&crate::storage::ProtectedStore>,
        session: SessionId,
    ) -> Result<String> {
        let document: crate::private_state::ProjectRegistryDocument = if let Some(home) = home {
            let bytes = home
                .document("interoperable/projects.json", 4 * 1024 * 1024)?
                .context("Profile registry is unavailable")?;
            let document: crate::private_state::ProjectRegistryDocument =
                serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                document.version
                    == crate::private_state::ProjectRegistryDocument::default().version,
                "Profile registry version differs"
            );
            document
        } else {
            anyhow::ensure!(
                matches!(
                    crate::storage::ProtectedStore::status(data)?,
                    crate::storage::StorageStatus::Legacy
                ),
                "protected custody is unavailable"
            );
            // Do not let a concurrent migration turn this legacy read into an
            // implicit credential acquisition through the generic facade.
            let bytes = crate::bounded_file::read(
                &data.join("interoperable/projects.json"),
                4 * 1024 * 1024,
            )?;
            let document: crate::private_state::ProjectRegistryDocument =
                serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                document.version
                    == crate::private_state::ProjectRegistryDocument::default().version,
                "Profile registry version differs"
            );
            document
        };
        document
            .conversation_profiles
            .get(&session.to_string())
            .map(|profile| profile.digest.clone())
            .context("Conversation has no immutable Profile snapshot")
    }

    pub(crate) fn finish_record(
        &self,
        operation_id: OperationId,
        outcome: crate::native_runtime::OperationOutcome,
    ) -> Result<SessionRecord> {
        let Some(operation) = self.restored.operation_details.get(&operation_id) else {
            bail!("operation is unavailable");
        };
        if operation.adapter.is_none() {
            return Ok(SessionRecord::OperationFinished {
                operation_id,
                outcome,
            });
        }
        // A known committed answer remains evidence when a later gate fails;
        // keeping its reference must never upgrade the terminal outcome.
        let result_entry = self
            .restored
            .head
            .and_then(|id| self.restored.entries.get(&id))
            .filter(|entry| {
                entry.message.role == crate::message::Role::Assistant
                    && crate::session::reduce::adapter_output_descends_from_input(
                        &self.restored,
                        entry.id,
                        operation.input_entry_id,
                    )
            })
            .map(|entry| -> Result<_> {
                Ok(DesktopCommandResultRef {
                    entry_id: entry.id.to_string().parse()?,
                    message_digest: message_digest(&entry.message)?,
                })
            })
            .transpose()?;
        Ok(SessionRecord::AdapterOperationFinished {
            operation_id,
            outcome,
            result_entry,
        })
    }
}
