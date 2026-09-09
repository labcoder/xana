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
    let store = ProfileStore::open(paths);
    let profile = match prior {
        Some(prior) if matches!(prior.scope, ProfileScope::Project(_)) => {
            let ProfileScope::Project(project) = prior.scope else {
                unreachable!()
            };
            let portable = crate::portable_project::PortableProjectStore::open(paths)
                .resolve(paths, project)?;
            let name = portable
                .manifest
                .profiles
                .iter()
                .find(|(name, profile)| {
                    !profile.archived
                        && crate::portable_project::PortableProjectStore::profile_id(
                            &portable.manifest,
                            name,
                            profile,
                        ) == prior.profile_id
                })
                .map(|(name, _)| name)
                .ok_or_else(|| {
                    anyhow::anyhow!(RetiredProfile {
                        name: prior.name.clone()
                    })
                })?;
            store.resolve_project(paths, project, name)?
        }
        Some(prior) => {
            let profile = registry
                .profiles
                .values()
                .find(|profile| profile.profile_id == prior.profile_id)
                .filter(|profile| !profile.archived)
                .ok_or_else(|| {
                    anyhow::anyhow!(RetiredProfile {
                        name: prior.name.clone()
                    })
                })?;
            store.resolve_global_for_selection(
                &profile.id,
                &manager.selected_for_profile(&profile.id)?,
            )?
        }
        None => {
            store.resolve_global_for_selection(&registry.default_profile, &manager.selected()?)?
        }
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

#[derive(Debug)]
pub(crate) struct RetiredProfile {
    name: String,
}

impl std::fmt::Display for RetiredProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Profile {:?} was removed or archived. History and private memory are retained; restore that profile, or use `xana profile continue NAME CONVERSATION` to explicitly choose a linked continuation. No replacement was selected automatically.",
            self.name
        )
    }
}

impl std::error::Error for RetiredProfile {}

/// Retired Conversations remain viewable. Submission still resolves current authority and fails closed.
pub(crate) fn resolve_for_startup(
    paths: &XanaPaths,
    prior: Option<&ResolvedProfile>,
) -> Result<ExecutionConfiguration> {
    match resolve_current(paths, prior) {
        Ok(configuration) => Ok(configuration),
        Err(error) if error.is::<RetiredProfile>() => Ok(ExecutionConfiguration {
            version: 1,
            profile: prior.expect("retirement requires a saved profile").clone(),
            inputs_digest: inputs_digest(paths)?,
        }),
        Err(error) => Err(error),
    }
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
