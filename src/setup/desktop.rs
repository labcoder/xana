//! Presentation-neutral setup adapter used by graphical clients.
//!
//! The terminal wizard and Desktop both end at the same `SetupDraft`, live
//! discovery, and atomic installation transaction. This module deliberately
//! contains no GPUI concepts and never returns a secret.

use super::{SetupCredential, SetupDraft};
use crate::{
    config::{PermissionMode, ProviderKind, XanaConfig},
    credential::{OsSecretStore, SecretString},
    model_catalog::{ModelDescriptor, ModelManager},
    paths::XanaPaths,
};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopSetupCredential {
    None,
    Environment(String),
    Stored { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSetupDraft {
    pub(crate) kind: ProviderKind,
    pub(crate) connection: String,
    pub(crate) base_url: Option<String>,
    pub(crate) codex_program: Option<String>,
    pub(crate) codex_home: Option<PathBuf>,
    pub(crate) credential: DesktopSetupCredential,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) permission_mode: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSetupCommit {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) discovered_model_count: usize,
    pub(crate) replaced_existing_configuration: bool,
    pub(crate) backup_created: bool,
}

pub(crate) async fn discover_for_desktop(
    draft: &DesktopSetupDraft,
    paths: &XanaPaths,
    secret: Option<&SecretString>,
) -> Result<Vec<ModelDescriptor>> {
    let provisional = as_setup_draft(draft, secret, "setup-probe")?;
    super::establish(&provisional, paths).await
}

pub(crate) async fn commit_for_desktop(
    draft: &DesktopSetupDraft,
    paths: &XanaPaths,
    secret: Option<&SecretString>,
) -> Result<DesktopSetupCommit> {
    let selected = draft
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("setup requires a selected model")?;
    let concrete = as_setup_draft(draft, secret, selected)?;
    let models = super::establish(&concrete, paths)
        .await
        .context("could not revalidate the connection before setup commit")?;
    if !models.iter().any(|model| model.id == selected) {
        bail!(
            "the established connection no longer advertises model {selected:?}; refresh and choose an available model"
        );
    }

    let replaced_existing_configuration = paths.config_file().exists();
    let rendered = XanaConfig::render_initial(concrete.initial_config())
        .context("could not render the validated setup configuration")?;
    let rendered = super::custom::merge_existing_connection_if_valid(paths, rendered)
        .context("could not merge the setup connection into existing configuration")?;
    super::storage::FreshPlan::automatic(paths)?.apply(paths, &crate::storage::OsCustody)?;
    let selection_path = paths.data_dir().join("selection.toml");
    let stored = match (&concrete.credential, concrete.staged_secret.as_ref()) {
        (SetupCredential::Stored { id }, Some(secret)) => Some((id.as_str(), secret)),
        _ => None,
    };
    super::install_with_preferences(
        paths.config_file(),
        &rendered,
        stored,
        &OsSecretStore,
        None,
        Some(&selection_path),
    )?;
    let registry = XanaConfig::parse_registry(&rendered)
        .context("configuration committed but its model catalog could not be reopened")?;
    ModelManager::new(registry, paths.cache_dir().to_owned(), selection_path)
        .write_discovered_cache(&concrete.connection, &models)
        .context("configuration committed but its model catalog could not be cached")?;
    crate::private_state::ensure_interoperable_records(paths).context(
        "configuration committed but private state requires `xana config migrate --apply`",
    )?;
    super::state::clear_blank(paths)?;

    Ok(DesktopSetupCommit {
        connection: concrete.connection,
        model: selected.to_owned(),
        discovered_model_count: models.len(),
        replaced_existing_configuration,
        backup_created: replaced_existing_configuration,
    })
}

pub(crate) fn commit_blank_for_desktop(paths: &XanaPaths) -> Result<()> {
    anyhow::ensure!(
        crate::config::ConfigReadiness::inspect(paths.config_file())
            == crate::config::ConfigReadiness::Missing,
        "blank setup requires an unconfigured home"
    );
    super::storage::FreshPlan::automatic(paths)?.apply(paths, &crate::storage::OsCustody)?;
    super::state::commit_blank(paths)
}

fn as_setup_draft(
    draft: &DesktopSetupDraft,
    secret: Option<&SecretString>,
    model: &str,
) -> Result<SetupDraft> {
    let connection = draft.connection.trim();
    if connection.is_empty() {
        bail!("connection name must not be blank");
    }
    let credential = match &draft.credential {
        DesktopSetupCredential::None => SetupCredential::None,
        DesktopSetupCredential::Environment(variable) => {
            let variable = variable.trim();
            if variable.is_empty() {
                bail!("credential environment variable must not be blank");
            }
            SetupCredential::Environment(variable.to_owned())
        }
        DesktopSetupCredential::Stored { id } => {
            if secret.is_none() {
                bail!("a stored credential requires a new secret value");
            }
            let id = id.trim();
            if id.is_empty() {
                bail!("stored credential identifier must not be blank");
            }
            SetupCredential::Stored { id: id.to_owned() }
        }
    };
    Ok(SetupDraft {
        kind: draft.kind,
        connection: connection.to_owned(),
        base_url: draft
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        codex_program: draft
            .codex_program
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        codex_home: draft.codex_home.clone(),
        credential,
        staged_secret: secret
            .map(|secret| SecretString::new(secret.expose().to_owned()))
            .transpose()?,
        model: model.to_owned(),
        reasoning_effort: draft.reasoning_effort.clone(),
        permission_mode: draft.permission_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(credential: DesktopSetupCredential) -> DesktopSetupDraft {
        DesktopSetupDraft {
            kind: ProviderKind::OpenAi,
            connection: "openai".to_owned(),
            base_url: None,
            codex_program: None,
            codex_home: None,
            credential,
            model: Some("gpt-test".to_owned()),
            reasoning_effort: None,
            permission_mode: PermissionMode::Ask,
        }
    }

    #[test]
    fn stored_credential_requires_write_only_secret() {
        let draft = draft(DesktopSetupCredential::Stored {
            id: "openai".to_owned(),
        });
        assert!(as_setup_draft(&draft, None, "probe").is_err());
        let secret = SecretString::new("secret".to_owned()).unwrap();
        let concrete = as_setup_draft(&draft, Some(&secret), "probe").unwrap();
        assert!(matches!(
            concrete.credential,
            SetupCredential::Stored { .. }
        ));
    }

    #[test]
    fn environment_credential_rejects_blank_variable() {
        let draft = draft(DesktopSetupCredential::Environment("  ".to_owned()));
        assert!(as_setup_draft(&draft, None, "probe").is_err());
    }
}
