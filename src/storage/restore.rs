//! Reviewed protected-generation replacement; ordinary files stay in place.

#[cfg(test)]
mod tests;

use super::{ProtectedStore, RecoveryIdentity, migration::journal_path};
use crate::{config::ConfigTransactionLock, paths::XanaPaths};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub(crate) struct RestorePreview {
    pub snapshot_store: Uuid,
    pub replaces_store: Option<Uuid>,
    pub bytes: u64,
    pub review: String,
    pub recall_learning_automation: &'static str,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    action: String,
    id: Uuid,
    store: Uuid,
    prior: Option<Uuid>,
}

fn sibling(data: &Path, id: Uuid, role: &str) -> PathBuf {
    data.with_file_name(format!(
        "{}.{}.{id}",
        data.file_name().unwrap_or_default().to_string_lossy(),
        role
    ))
}

pub(crate) fn preview(
    paths: &XanaPaths,
    snapshot: &Path,
    identity: &RecoveryIdentity,
) -> Result<RestorePreview> {
    ensure!(
        !journal_path(paths.data_dir()).exists(),
        "a storage transition is pending; resume it first"
    );
    let bytes = super::backup::validate_snapshot(snapshot)?;
    let source = ProtectedStore::open_recovery(snapshot, identity, false)?;
    source.verify_content()?;
    let current =
        super::read_bootstrap(&paths.data_dir().join("protected"))?.map(|bootstrap| bootstrap.id);
    ensure!(
        current.is_some()
            || !paths.data_dir().exists()
            || fs::read_dir(paths.data_dir())?.next().is_none(),
        "restore requires a protected or empty data home; migrate existing plaintext first"
    );
    let content = crate::bounded_file::read(&snapshot.join("snapshot.json"), 4096)?;
    let mut digest = blake3::Hasher::new();
    digest.update(&content);
    digest.update(
        current
            .map(|id| id.to_string())
            .unwrap_or_default()
            .as_bytes(),
    );
    digest.update(paths.data_dir().as_os_str().as_encoded_bytes());
    // Bind the review to the exact encrypted database, not only its declared stamp.
    let mut database = fs::File::open(snapshot.join("protected/content.sqlite"))?;
    digest.update_reader(&mut database)?;
    Ok(RestorePreview {
        snapshot_store: source.id(),
        replaces_store: current,
        bytes,
        review: digest.finalize().to_hex().to_string(),
        recall_learning_automation: "inactive until reviewed",
    })
}

pub(crate) fn apply(
    paths: &XanaPaths,
    snapshot: &Path,
    identity: &RecoveryIdentity,
    review: &str,
) -> Result<PathBuf> {
    apply_with(paths, snapshot, identity, review, |_| Ok(()))
}

fn apply_with(
    paths: &XanaPaths,
    snapshot: &Path,
    identity: &RecoveryIdentity,
    review: &str,
    fault: impl Fn(&str) -> Result<()>,
) -> Result<PathBuf> {
    let _config_lock = ConfigTransactionLock::acquire(paths.config_file())?;
    let plan = preview(paths, snapshot, identity)?;
    ensure!(
        plan.review == review,
        "restore source/destination changed; review a new preview"
    );
    let parent = paths
        .data_dir()
        .parent()
        .context("data parent unavailable")?;
    fs::create_dir_all(parent)?;
    ensure!(
        fs2::available_space(parent)?
            >= plan
                .bytes
                .saturating_mul(2)
                .saturating_add(32 * 1024 * 1024),
        "not enough space for verified restore; existing home preserved"
    );
    let _owner = lock_prior(paths.data_dir(), plan.replaces_store)?;
    let source = ProtectedStore::open_recovery(snapshot, identity, false)?;
    let id = Uuid::new_v4();
    let stage = sibling(paths.data_dir(), id, "restore-prepared");
    fs::create_dir(&stage)?;
    source.snapshot_into(&stage, plan.bytes.saturating_add(16 * 1024 * 1024))?;
    drop(source);
    let staged = ProtectedStore::recover(&stage, identity)?;
    staged.set_document("restore/review-required", b"Current forgetting exclusions and grant/job authority require review. Recall, learning and automation remain inactive; ordinary chat may continue.", 4096)?;
    staged.with_database(|db| db.checkpoint())?;
    drop(staged);
    let journal = Journal {
        version: 1,
        action: "restore".into(),
        id,
        store: plan.snapshot_store,
        prior: plan.replaces_store,
    };
    super::write_new_synced(
        &journal_path(paths.data_dir()),
        &serde_json::to_vec(&journal)?,
    )?;
    fault("restore-journal")?;
    super::migration::fence_config(paths)?;
    drop(_owner);
    finish(paths, identity, &journal, &fault)
}

pub(crate) fn resume(paths: &XanaPaths, identity: &RecoveryIdentity) -> Result<PathBuf> {
    let _config_lock = ConfigTransactionLock::acquire(paths.config_file())?;
    let journal: Journal = serde_json::from_slice(&crate::bounded_file::read(
        &journal_path(paths.data_dir()),
        4096,
    )?)?;
    ensure!(
        journal.version == 1 && journal.action == "restore",
        "not a supported restore journal"
    );
    super::migration::fence_config(paths)?;
    finish(paths, identity, &journal, &|_| Ok(()))
}

fn finish(
    paths: &XanaPaths,
    identity: &RecoveryIdentity,
    journal: &Journal,
    fault: &impl Fn(&str) -> Result<()>,
) -> Result<PathBuf> {
    let data = paths.data_dir();
    let stage = sibling(data, journal.id, "restore-prepared");
    let prior = sibling(data, journal.id, "retired");
    if stage.join("protected").exists() {
        let ready = ProtectedStore::recover(&stage, identity)?;
        ensure!(
            ready.id() == journal.store
                && ready.document("restore/review-required", 4096)?.is_some(),
            "prepared restore differs"
        );
        ready.verify_content()?;
        drop(ready);
        let _owner = if data.join("protected").exists() {
            ensure!(
                !prior.join("protected").exists(),
                "restore destination changed; both generations preserved"
            );
            let owner = lock_prior(data, journal.prior)?;
            drop(owner);
            fs::create_dir_all(&prior)?;
            fs::rename(data.join("protected"), prior.join("protected"))?;
            lock_prior(&prior, journal.prior)?
        } else {
            None
        };
        fault("prior-retired")?;
        fs::create_dir_all(data)?;
        fs::rename(stage.join("protected"), data.join("protected"))?;
        fault("restore-activated")?;
    }
    let verified = ProtectedStore::recover(data, identity)?;
    ensure!(
        verified.id() == journal.store
            && verified
                .document("restore/review-required", 4096)?
                .is_some(),
        "restored generation differs"
    );
    verified.verify_content()?;
    fs::remove_file(journal_path(data))?;
    Ok(prior)
}

fn lock_prior(data: &Path, expected: Option<Uuid>) -> Result<Option<fs::File>> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    let bootstrap = super::read_bootstrap(&data.join("protected"))?
        .context("prior protected generation unavailable")?;
    ensure!(bootstrap.id == expected, "prior generation changed");
    let path = data.join("protected/owner.lock");
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_file(),
        "owner lease must be a regular file"
    );
    let lock = fs::OpenOptions::new().read(true).write(true).open(path)?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("close every Xana owner before restoring protected content")?;
    Ok(Some(lock))
}
