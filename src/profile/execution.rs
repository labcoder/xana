//! Immutable per-operation configuration provenance, separate from Conversation identity.

use super::ResolvedProfile;
use super::{ProfileScope, ProfileStore};
use crate::{config::XanaConfig, paths::XanaPaths};
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub(crate) fn inputs_digest(paths: &XanaPaths) -> Result<String> {
    let mut digest = blake3::Hasher::new();
    digest.update(env!("CARGO_PKG_VERSION").as_bytes());
    for (path, limit, optional) in [
        (paths.config_file().to_owned(), 1024 * 1024, false),
        (paths.data_dir().join("selection.toml"), 64 * 1024, true),
    ] {
        match crate::bounded_file::read(&path, limit) {
            Ok(bytes) => {
                digest.update(&(bytes.len() as u64).to_le_bytes());
                digest.update(&bytes);
            }
            Err(crate::bounded_file::BoundedReadError::Io { source, .. })
                if optional && source.kind() == std::io::ErrorKind::NotFound =>
            {
                digest.update(&0_u64.to_le_bytes());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(digest.finalize().to_hex().to_string())
}

pub(crate) fn resolve_current(
    paths: &XanaPaths,
    prior: Option<&ResolvedProfile>,
) -> Result<ExecutionConfiguration> {
    let before = inputs_digest(paths)?;
    let registry = XanaConfig::load_registry_from(paths.config_file())?;
    let manager = crate::model_catalog::ModelManager::new(
        registry.clone(),
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    );
    let selected = manager.selected()?;
    let store = ProfileStore::open(paths);
    let profile = match prior {
        Some(prior) if matches!(prior.scope, ProfileScope::Project(_)) => {
            let ProfileScope::Project(project) = prior.scope else {
                unreachable!()
            };
            store.resolve_project(paths, project, &prior.name)?
        }
        Some(prior) => store.resolve_global_for_selection(&prior.name, &selected)?,
        None => store.resolve_global_for_selection(&registry.default_profile, &selected)?,
    };
    anyhow::ensure!(
        before == inputs_digest(paths)?,
        "Configuration changed during resolution; retry this turn"
    );
    anyhow::ensure!(
        profile.is_ready(),
        "Profile is not ready: {}",
        profile.readiness.join("; ")
    );
    Ok(ExecutionConfiguration {
        version: 1,
        profile,
        inputs_digest: before,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionConfiguration {
    pub(crate) version: u32,
    pub(crate) profile: ResolvedProfile,
    /// Digest of application-owned configuration inputs, never credential values.
    pub(crate) inputs_digest: String,
}

impl ExecutionConfiguration {
    pub(crate) fn validate(&self) -> bool {
        self.version == 1
            && self.inputs_digest.len() == 64
            && self
                .inputs_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            && self.profile.is_ready()
    }

    pub(crate) fn digest(&self) -> String {
        blake3::hash(&serde_json::to_vec(self).expect("configuration is serializable"))
            .to_hex()
            .to_string()
    }
}
