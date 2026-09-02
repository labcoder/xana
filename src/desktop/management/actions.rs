//! Security-sensitive connection mutations exposed to the Desktop adapter.
//!
//! These operations keep provider probing, credential storage, managed-runtime
//! ownership, and guarded configuration mutation outside GPUI state.

use super::{
    DesktopControlPlane, DesktopError, DesktopErrorCode, DesktopSecret, bounded, control_error,
};
use crate::{
    config::{CredentialReference, ProviderKind},
    connection_management::{ConnectionManagement, ConnectionReceipt, RemovalBlocker, RemovalPlan},
    credential::{delete_secret, store_secret},
    managed::codex::{AccountStatus, CodexAppServer, LoginCancellation, LoginMode},
};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConnectionMutationReceipt {
    pub semantic_code: String,
    pub connection: String,
    pub effect: String,
    pub backup: Option<PathBuf>,
    pub retained_authority: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DesktopConnectionRemovalPlan {
    pub connection: String,
    pub blockers: Vec<String>,
    pub retains_credential: bool,
    pub retains_managed_account: bool,
    core: RemovalPlan,
}

/// One vendor-owned Codex login attempt.
///
/// The app-server process and login identifier never enter the presentation
/// protocol. Dropping an unfinished value kills its owned child process.
pub struct DesktopManagedLogin {
    connection: String,
    url: Option<String>,
    user_code: Option<String>,
    login_id: Option<String>,
    server: Option<CodexAppServer>,
}

impl DesktopManagedLogin {
    pub fn connection(&self) -> &str {
        &self.connection
    }

    pub fn authorization_url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub fn user_code(&self) -> Option<&str> {
        self.user_code.as_deref()
    }

    pub fn is_pending(&self) -> bool {
        self.login_id.is_some() && self.server.is_some()
    }

    pub async fn complete(mut self) -> Result<DesktopConnectionMutationReceipt, DesktopError> {
        let Some(mut server) = self.server.take() else {
            return Ok(simple_receipt(
                &self.connection,
                "connection.managed_login.already_complete.v1",
                "managed_login_completed",
            ));
        };
        let login_id = self.login_id.take().ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                "managed login lost its private operation identifier",
            )
        })?;
        let result = server.wait_for_login(&login_id).await;
        if let Err(error) = result {
            _ = server.shutdown().await;
            return Err(control_error(error));
        }
        let shutdown = server.shutdown().await;
        let mut receipt = simple_receipt(
            &self.connection,
            "connection.managed_login.completed.v1",
            "managed_login_completed",
        );
        if let Err(error) = shutdown {
            receipt.warnings.push(bounded(format!(
                "Login completed, but the temporary Codex app-server did not shut down cleanly: {error}"
            )));
        }
        Ok(receipt)
    }

    pub async fn cancel(mut self) -> Result<DesktopConnectionMutationReceipt, DesktopError> {
        let Some(mut server) = self.server.take() else {
            return Ok(simple_receipt(
                &self.connection,
                "connection.managed_login.cancelled.v1",
                "managed_login_cancelled",
            ));
        };
        let login_id = self.login_id.take().ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                "managed login lost its private operation identifier",
            )
        })?;
        let cancellation = server
            .cancel_login(&login_id)
            .await
            .map_err(control_error)?;
        let shutdown = server.shutdown().await;
        let mut receipt = simple_receipt(
            &self.connection,
            "connection.managed_login.cancelled.v1",
            "managed_login_cancelled",
        );
        if cancellation == LoginCancellation::NotFound {
            receipt
                .warnings
                .push("The vendor no longer reported a pending login attempt.".to_owned());
        }
        if let Err(error) = shutdown {
            receipt.warnings.push(bounded(format!(
                "The cancellation completed, but the temporary Codex app-server did not shut down cleanly: {error}"
            )));
        }
        Ok(receipt)
    }
}

impl DesktopControlPlane {
    /// Validate a replacement API key before atomically changing its OS-store value.
    pub async fn replace_credential(
        &self,
        connection: &str,
        secret: DesktopSecret,
    ) -> Result<DesktopConnectionMutationReceipt, DesktopError> {
        let manager = crate::app::model_manager(&self.paths).map_err(control_error)?;
        let configured = manager.connection(connection).map_err(control_error)?;
        let CredentialReference::Stored { id: credential_id } = configured
            .credential
            .as_ref()
            .ok_or_else(|| state_error("connection does not declare a stored credential"))?
        else {
            return Err(state_error(
                "environment credentials must be changed in their declared environment",
            ));
        };
        let credential_id = credential_id.clone();
        let models = manager
            .probe_native(connection, Some(&secret.0))
            .await
            .map_err(control_error)?;
        if models.is_empty() {
            return Err(state_error(
                "the replacement credential reached the provider but returned no models; the prior credential was preserved",
            ));
        }
        store_secret(&credential_id, &secret.0).map_err(control_error)?;
        let mut receipt = simple_receipt(
            connection,
            "connection.credential_replace.completed.v1",
            "credential_replaced",
        );
        if let Err(error) = manager.write_discovered_cache(connection, &models) {
            receipt.warnings.push(bounded(format!(
                "The credential was stored, but the catalog cache was not updated: {error}"
            )));
        }
        Ok(receipt)
    }

    pub fn delete_credential(
        &self,
        connection: &str,
    ) -> Result<DesktopConnectionMutationReceipt, DesktopError> {
        let manager = crate::app::model_manager(&self.paths).map_err(control_error)?;
        let configured = manager.connection(connection).map_err(control_error)?;
        let CredentialReference::Stored { id } = configured
            .credential
            .as_ref()
            .ok_or_else(|| state_error("connection does not declare a stored credential"))?
        else {
            return Err(state_error(
                "environment credentials must be changed in their declared environment",
            ));
        };
        let deleted = delete_secret(id).map_err(control_error)?;
        let mut receipt = simple_receipt(
            connection,
            "connection.credential_delete.completed.v1",
            "credential_deleted",
        );
        if !deleted {
            receipt
                .warnings
                .push("The credential was already absent.".to_owned());
        }
        Ok(receipt)
    }

    pub async fn begin_managed_login(
        &self,
        connection: &str,
        device_code: bool,
    ) -> Result<DesktopManagedLogin, DesktopError> {
        let manager = crate::app::model_manager(&self.paths).map_err(control_error)?;
        let configured = manager.connection(connection).map_err(control_error)?;
        if configured.kind != ProviderKind::Codex {
            return Err(state_error(
                "this connection uses an API credential rather than managed login",
            ));
        }
        let mut server = CodexAppServer::spawn(&crate::app::codex_launch(configured))
            .await
            .map_err(control_error)?;
        if !matches!(
            server.account_status().await.map_err(control_error)?,
            AccountStatus::LoggedOut
        ) {
            server.shutdown().await.map_err(control_error)?;
            return Ok(DesktopManagedLogin {
                connection: connection.to_owned(),
                url: None,
                user_code: None,
                login_id: None,
                server: None,
            });
        }
        let instructions = server
            .begin_login(if device_code {
                LoginMode::DeviceCode
            } else {
                LoginMode::Browser
            })
            .await
            .map_err(control_error)?;
        Ok(DesktopManagedLogin {
            connection: connection.to_owned(),
            url: Some(bounded(instructions.url)),
            user_code: instructions.user_code.map(bounded),
            login_id: Some(instructions.login_id),
            server: Some(server),
        })
    }

    pub async fn logout_managed(
        &self,
        connection: &str,
    ) -> Result<DesktopConnectionMutationReceipt, DesktopError> {
        let manager = crate::app::model_manager(&self.paths).map_err(control_error)?;
        let configured = manager.connection(connection).map_err(control_error)?;
        if configured.kind != ProviderKind::Codex {
            return Err(state_error(
                "this connection uses an API credential rather than managed login",
            ));
        }
        let mut server = CodexAppServer::spawn(&crate::app::codex_launch(configured))
            .await
            .map_err(control_error)?;
        if !matches!(
            server.account_status().await.map_err(control_error)?,
            AccountStatus::LoggedOut
        ) {
            server.logout().await.map_err(control_error)?;
        }
        let shutdown = server.shutdown().await;
        let mut receipt = simple_receipt(
            connection,
            "connection.managed_logout.completed.v1",
            "managed_logout_completed",
        );
        if let Err(error) = shutdown {
            receipt.warnings.push(bounded(format!(
                "Logout completed, but the temporary Codex app-server did not shut down cleanly: {error}"
            )));
        }
        Ok(receipt)
    }

    pub fn connection_removal_plan(
        &self,
        connection: &str,
        active_conversations: Vec<String>,
    ) -> Result<DesktopConnectionRemovalPlan, DesktopError> {
        let core = ConnectionManagement::open(&self.paths)
            .and_then(|management| management.removal_plan(connection, active_conversations))
            .map_err(control_error)?;
        Ok(DesktopConnectionRemovalPlan {
            connection: core.connection.clone(),
            blockers: core.blockers.iter().map(blocker_label).collect(),
            retains_credential: core.retains_credential,
            retains_managed_account: core.retains_managed_account,
            core,
        })
    }

    pub fn remove_connection(
        &self,
        plan: &DesktopConnectionRemovalPlan,
    ) -> Result<DesktopConnectionMutationReceipt, DesktopError> {
        ConnectionManagement::open(&self.paths)
            .and_then(|management| management.remove(&plan.core))
            .map(project_receipt)
            .map_err(control_error)
    }
}

fn blocker_label(blocker: &RemovalBlocker) -> String {
    match blocker {
        RemovalBlocker::SelectedForNewConversations => "Selected for new Conversations".to_owned(),
        RemovalBlocker::DefaultProfile(profile) => format!("Default Profile {profile}"),
        RemovalBlocker::Profile(profile) => format!("Profile {profile}"),
        RemovalBlocker::ActiveConversation(conversation) => {
            format!("Active Conversation {conversation}")
        }
    }
}

fn project_receipt(receipt: ConnectionReceipt) -> DesktopConnectionMutationReceipt {
    DesktopConnectionMutationReceipt {
        semantic_code: receipt.semantic_code.to_owned(),
        connection: bounded(receipt.connection),
        effect: format!("{:?}", receipt.effect).to_lowercase(),
        backup: receipt.backup,
        retained_authority: receipt
            .retained_authority
            .into_iter()
            .map(str::to_owned)
            .collect(),
        warnings: receipt.warnings.into_iter().map(bounded).collect(),
    }
}

fn simple_receipt(
    connection: &str,
    semantic_code: &str,
    effect: &str,
) -> DesktopConnectionMutationReceipt {
    DesktopConnectionMutationReceipt {
        semantic_code: semantic_code.to_owned(),
        connection: bounded(connection.to_owned()),
        effect: effect.to_owned(),
        backup: None,
        retained_authority: Vec::new(),
        warnings: Vec::new(),
    }
}

fn state_error(message: impl Into<String>) -> DesktopError {
    DesktopError::new(DesktopErrorCode::StateInvalid, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
        shell::ShellConfig,
    };
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn removal_plan_exposes_blockers_and_retained_authority_without_secrets() {
        let directory = tempdir().unwrap();
        let control =
            DesktopControlPlane::resolve(Some(directory.path().join("home").into_os_string()))
                .unwrap();
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "ollama".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        fs::create_dir_all(control.paths.config_file().parent().unwrap()).unwrap();
        fs::write(control.paths.config_file(), rendered).unwrap();

        let plan = control
            .connection_removal_plan("ollama", vec!["conversation-a".to_owned()])
            .unwrap();
        assert!(
            plan.blockers
                .contains(&"Selected for new Conversations".to_owned())
        );
        assert!(
            plan.blockers
                .contains(&"Default Profile default".to_owned())
        );
        assert!(
            plan.blockers
                .contains(&"Active Conversation conversation-a".to_owned())
        );
        assert!(!plan.retains_credential);
        assert!(!format!("{plan:?}").contains("api_key"));
    }
}
