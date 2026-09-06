//! Immutable scope snapshots and current authority checks for retained work.
use super::{RetainedWorker, WorkerState};
use crate::{
    config::XanaConfig, identity::SessionId, paths::XanaPaths, storage::ProtectedStore,
    workspace_identity::WorkspaceIdentity,
};
use anyhow::{Context, Result, ensure};

pub(crate) fn configuration_digest(paths: &XanaPaths, session: SessionId) -> Result<String> {
    let _registry = XanaConfig::load_registry_from(paths.config_file())?;
    let profile = crate::profile::ProfileStore::open(paths)
        .snapshot(&session.to_string())?
        .context("retained work requires a frozen Conversation Profile")?;
    let mut digest = blake3::Hasher::new();
    digest.update(&crate::bounded_file::read(
        paths.config_file(),
        1024 * 1024,
    )?);
    digest.update(&serde_json::to_vec(&profile)?);
    Ok(digest.finalize().to_hex().to_string())
}

pub(crate) fn check_scope(
    paths: &XanaPaths,
    store: &ProtectedStore,
    worker: &RetainedWorker,
) -> Result<()> {
    ensure!(
        worker.expires_at > crate::autonomy::now()?
            && !matches!(
                worker.state,
                WorkerState::Stopped | WorkerState::Expired | WorkerState::NeedsReview
            ),
        "retained authority expired, stopped, or needs review"
    );
    ensure!(
        !store.memory_requires_review()?
            && store.source_reuse_allowed(worker.session.to_string().parse()?)?,
        "retained source is unavailable, forgotten, or awaiting restore review"
    );
    ensure!(
        store.privacy_generation()? == worker.privacy_generation,
        "privacy changed; retained sources require fresh owner review"
    );
    ensure!(
        (crate::recall::RecallOwner {
            store: store.clone(),
            paths: paths.clone(),
            conversation: worker.session
        })
        .scope(worker.session)?
            == worker.scope,
        "retained Project/Profile scope changed"
    );
    let workspace = store.history_metadata(worker.session)?.workspace;
    let projects = crate::project::ProjectStore::open(paths)?;
    if let Some(project) = projects.membership(&worker.session.to_string())? {
        let inspection = projects.inspect(project)?;
        ensure!(
            inspection.project.lifecycle == crate::private_state::ProjectLifecycle::Active
                && inspection.workspace_status == crate::project::WorkspaceStatus::Available,
            "retained Project is unavailable or archived"
        );
        ensure!(
            WorkspaceIdentity::resolve(&workspace)?
                .matches(&inspection.project.canonical_workspace)?,
            "retained Project workspace changed"
        );
    }
    ensure!(
        WorkspaceIdentity::resolve(&workspace)?.collision_key() == worker.workspace_identity,
        "retained workspace identity changed"
    );
    ensure!(
        configuration_digest(paths, worker.session)? == worker.configuration_digest,
        "retained route or permission configuration changed; retain newly reviewed work"
    );
    Ok(())
}
