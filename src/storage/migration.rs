//! Reviewed generation migration with an intact source and a fail-closed restart journal.

pub(crate) mod archive;
mod inventory;
#[cfg(test)]
mod tests;

use super::{KeyCustody, ProtectedStore, RecoveryIdentity};
use crate::{
    config::{ConfigTransactionLock, XanaConfig},
    paths::XanaPaths,
    session::SessionStore,
};
use anyhow::{Context, Result, ensure};
use inventory::{Entry, Kind};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub(crate) struct MigrationPreview {
    pub files: usize,
    pub bytes: u64,
    pub encrypted_archive_files: usize,
    pub required_free_bytes: u64,
    pub review: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    id: Uuid,
    store: Uuid,
    inventory_hash: String,
    fenced_config_hash: String,
}

pub(crate) fn journal_path(data: &Path) -> PathBuf {
    data.with_file_name(format!(
        "{}.storage-transition.json",
        data.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn sibling(data: &Path, id: Uuid, role: &str) -> PathBuf {
    data.with_file_name(format!(
        "{}.{}.{id}",
        data.file_name().unwrap_or_default().to_string_lossy(),
        role
    ))
}

pub(crate) fn preview(paths: &XanaPaths) -> Result<MigrationPreview> {
    ensure!(
        !journal_path(paths.data_dir()).exists(),
        "storage migration is pending; use --resume"
    );
    let entries = inventory::scan(paths.data_dir())?;
    let config = crate::bounded_file::read(paths.config_file(), 1024 * 1024)?;
    // Validate the config and the complete bounded inventory before any mutation.
    XanaConfig::migrate_to_current(std::str::from_utf8(&config)?)?;
    for record in crate::private_state::inspect_interoperable_records(paths) {
        ensure!(
            matches!(
                record.status,
                crate::private_state::PrivateRecordStatus::Healthy
                    | crate::private_state::PrivateRecordStatus::Missing
            ),
            "{} is {}; run xana config migrate/doctor before storage migration",
            record.name,
            record.status.as_str()
        );
    }
    let bytes = entries.iter().try_fold(0u64, |total, entry| {
        total
            .checked_add(entry.length)
            .context("migration size overflow")
    })?;
    let serialized = inventory_bytes(&entries)?;
    let mut digest = blake3::Hasher::new();
    digest.update(&config);
    digest.update(&serialized);
    Ok(MigrationPreview {
        files: entries.len(),
        bytes,
        encrypted_archive_files: entries
            .iter()
            .filter(|entry| entry.kind == Kind::Archive)
            .count(),
        required_free_bytes: bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(32 * 1024 * 1024))
            .context("migration size overflow")?,
        review: digest.finalize().to_hex().to_string(),
    })
}

pub(crate) fn apply(
    paths: &XanaPaths,
    identity: &RecoveryIdentity,
    custody: &dyn KeyCustody,
    review: &str,
) -> Result<PathBuf> {
    apply_with(paths, identity, custody, review, |_| Ok(()))
}

fn apply_with(
    paths: &XanaPaths,
    identity: &RecoveryIdentity,
    custody: &dyn KeyCustody,
    review: &str,
    fault: impl Fn(&str) -> Result<()>,
) -> Result<PathBuf> {
    let _config_lock = ConfigTransactionLock::acquire(paths.config_file())?;
    let plan = preview(paths)?;
    ensure!(
        plan.review == review,
        "migration inventory/config changed; review a new preview"
    );
    ensure!(
        fs2::available_space(paths.data_dir())? >= plan.required_free_bytes,
        "not enough free space for verified migration; source unchanged"
    );
    let entries = inventory::scan(paths.data_dir())?;
    let _writers = inventory::lock_writers(paths.data_dir(), &entries)?;
    ensure!(
        preview(paths)?.review == review,
        "migration source changed before fencing"
    );
    let original = crate::bounded_file::read(paths.config_file(), 1024 * 1024)?;
    let fenced = XanaConfig::migrate_to_current(std::str::from_utf8(&original)?)?;
    let id = Uuid::new_v4();
    let stage = sibling(paths.data_dir(), id, "prepared");
    // Recovery and custody are actually exercised before fencing the existing home.
    let store = ProtectedStore::initialize(&stage, identity, custody)?;
    store.set_document("migration/original-config", &original, 1024 * 1024)?;
    store.set_document(
        "migration/inventory",
        &inventory_bytes(&entries)?,
        16 * 1024 * 1024,
    )?;
    let journal = Journal {
        version: 1,
        id,
        store: store.id(),
        inventory_hash: blake3::hash(&inventory_bytes(&entries)?)
            .to_hex()
            .to_string(),
        fenced_config_hash: blake3::hash(fenced.as_bytes()).to_hex().to_string(),
    };
    drop(store);
    super::write_new_synced(
        &journal_path(paths.data_dir()),
        &serde_json::to_vec(&journal)?,
    )?;
    fault("journal")?;
    write_atomic(paths.config_file(), fenced.as_bytes())?;
    fault("config-fence")?;
    // The journal blocks new current writers; format 5 blocks older binaries.
    drop(_writers);
    finish(paths, identity, &journal, &fault)
}

pub(crate) fn resume(paths: &XanaPaths, identity: &RecoveryIdentity) -> Result<PathBuf> {
    let _config_lock = ConfigTransactionLock::acquire(paths.config_file())?;
    let journal: Journal = serde_json::from_slice(&crate::bounded_file::read(
        &journal_path(paths.data_dir()),
        4096,
    )?)?;
    ensure!(
        journal.version == 1,
        "unsupported storage migration journal"
    );
    finish(paths, identity, &journal, &|_| Ok(()))
}

fn finish(
    paths: &XanaPaths,
    identity: &RecoveryIdentity,
    journal: &Journal,
    fault: &impl Fn(&str) -> Result<()>,
) -> Result<PathBuf> {
    let data = paths.data_dir();
    let stage = sibling(data, journal.id, "prepared");
    let source = sibling(data, journal.id, "legacy");
    let fence = format!("Xana storage transition {}\n", journal.id);
    if !stage.exists() {
        // The atomic directory publication succeeded; only journal retirement was interrupted.
        ensure!(
            source.is_dir(),
            "migration source generation is unavailable"
        );
        let store = ProtectedStore::recover(data, identity)?;
        ensure!(store.id() == journal.store, "activated generation differs");
        store.verify_content()?;
        fs::remove_file(journal_path(data))?;
        return Ok(source);
    }
    let store = ProtectedStore::recover(&stage, identity)?;
    ensure!(store.id() == journal.store, "prepared generation differs");
    let inventory = store
        .document("migration/inventory", 16 * 1024 * 1024)?
        .context("migration inventory unavailable")?;
    ensure!(
        blake3::hash(&inventory).to_hex().as_str() == journal.inventory_hash,
        "migration inventory differs"
    );
    let entries: Vec<Entry> = serde_json::from_slice(&inventory)?;
    let original = store
        .document("migration/original-config", 1024 * 1024)?
        .context("source config unavailable")?;
    let current = crate::bounded_file::read(paths.config_file(), 1024 * 1024)?;
    if current == original {
        let fenced = XanaConfig::migrate_to_current(std::str::from_utf8(&original)?)?;
        ensure!(
            blake3::hash(fenced.as_bytes()).to_hex().as_str() == journal.fenced_config_hash,
            "config fence differs"
        );
        write_atomic(paths.config_file(), fenced.as_bytes())?;
    } else {
        ensure!(
            blake3::hash(&current).to_hex().as_str() == journal.fenced_config_hash,
            "config changed during migration; preserve both generations and review"
        );
    }
    let _writers = if source.exists() {
        inventory::lock_writers(&source, &entries)?
    } else {
        let locks = inventory::lock_writers(data, &entries)?;
        ensure!(
            inventory::scan(data)? == entries,
            "source changed since migration review"
        );
        // Windows may refuse a directory rename with byte-range locked children.
        // New writers are fenced by config/journal; reacquire and revalidate the
        // moved generation before reading any content or activating it.
        drop(locks);
        fs::rename(data, &source)
            .context("could not fence the old generation; close all Xana processes")?;
        fault("source-renamed")?;
        inventory::lock_writers(&source, &entries)?
    };
    if !data.exists() {
        super::write_new_synced(data, fence.as_bytes())?;
    }
    ensure!(
        crate::bounded_file::read(data, 4096)? == fence.as_bytes(),
        "migration destination changed; no files overwritten"
    );
    fault("source-fenced")?;
    ensure!(
        inventory::scan(&source)? == entries,
        "fenced source differs; no activation"
    );
    for entry in &entries {
        import(&store, &source, &stage, entry)?;
        fault("imported-file")?;
    }
    ensure!(
        inventory::scan(&source)? == entries,
        "source changed during conversion; no activation"
    );
    store.verify_content()?;
    store.set_document(
        "migration/receipt",
        b"Verified migration; plaintext prior generation retained; no automatic effect replay.",
        1024,
    )?;
    store.with_database(|db| db.checkpoint())?;
    fault("verified")?;
    drop(store);
    // Current binaries remain fenced by the journal throughout this recoverable gap.
    ensure!(
        crate::bounded_file::read(data, 4096)? == fence.as_bytes(),
        "migration fence changed"
    );
    fs::remove_file(data)?;
    fault("before-activation")?;
    fs::rename(&stage, data).context("could not activate prepared generation; rerun --resume")?;
    fault("activated")?;
    fs::remove_file(journal_path(data))?;
    Ok(source)
}

fn import(store: &ProtectedStore, source: &Path, stage: &Path, entry: &Entry) -> Result<()> {
    let path = source.join(&entry.name);
    match entry.kind {
        Kind::Lock => (),
        Kind::History => {
            let loaded = SessionStore::inspect(&path)?;
            let id = loaded
                .records
                .first()
                .context("empty native history")?
                .session_id;
            if let Ok(parsed) = SessionStore::inspect_protected(store, id) {
                ensure!(
                    parsed.records == loaded.records,
                    "prepared history differs from source"
                );
            } else {
                store.create_history(&loaded.records)?;
            }
            store.with_database(|db| {
                db.connection.execute(
                    "UPDATE native_sessions SET modified=?1 WHERE id=?2",
                    rusqlite::params![i64::try_from(entry.modified_unix_millis)?, id.to_string()],
                )?;
                Ok(())
            })?;
            if loaded.repair.is_some() {
                // Retain the original torn tail as an inspectable encrypted
                // archive; only validated complete records become live history.
                let (hash, length, _) = store.put_artifact(
                    &mut fs::File::open(&path)?,
                    crate::resource::MAX_RESOURCE_SOURCE_BYTES,
                    |_| Ok(()),
                )?;
                ensure!(hash.as_str() == entry.hash, "torn history source changed");
                store.set_document(
                    &format!("migration/archive/{}", blake3::hash(entry.name.as_bytes())),
                    &serde_json::to_vec(&(entry, hash, length))?,
                    4096,
                )?;
            }
        }
        Kind::Document => {
            let body = crate::bounded_file::read(&path, 16 * 1024 * 1024)?;
            store.set_document(&entry.name, &body, 16 * 1024 * 1024)?;
        }
        Kind::Artifact | Kind::Archive => {
            let (hash, length, _) = store.put_artifact(
                &mut fs::File::open(&path)?,
                crate::resource::MAX_RESOURCE_SOURCE_BYTES,
                |length| {
                    ensure!(length == entry.length, "migration file length changed");
                    Ok(())
                },
            )?;
            ensure!(hash.as_str() == entry.hash, "migration file hash changed");
            if entry.kind == Kind::Artifact {
                ensure!(
                    entry.name.strip_prefix("artifacts/") == Some(hash.as_str()),
                    "legacy artifact identity differs"
                );
            } else {
                let name_hash = blake3::hash(entry.name.as_bytes());
                store.set_document(
                    &format!("migration/archive/{name_hash}"),
                    &serde_json::to_vec(&(entry, hash, length))?,
                    4096,
                )?;
            }
        }
        Kind::Ordinary => {
            let target = stage.join(&entry.name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            if !target.exists() {
                let mut output = atomic_write_file::AtomicWriteFile::open(&target)?;
                std::io::copy(
                    &mut fs::File::open(&path)?.take(entry.length + 1),
                    &mut output,
                )?;
                output.commit()?;
            }
            ensure!(
                inventory::hash_file(&target, entry.length)? == entry.hash,
                "ordinary migration copy differs; source preserved"
            );
        }
    }
    Ok(())
}

fn inventory_bytes(entries: &[Entry]) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(entries)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "migration inventory exceeds protected document bound"
    );
    Ok(bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut output = atomic_write_file::AtomicWriteFile::open(path)?;
    output.write_all(bytes)?;
    output.commit()?;
    Ok(())
}

/// Caller holds the config transaction lock. Ordinary configuration remains
/// readable text; only its supported-format fence changes.
pub(crate) fn fence_config(paths: &XanaPaths) -> Result<()> {
    if !paths.config_file().exists() {
        return Ok(());
    }
    let original = crate::bounded_file::read(paths.config_file(), 1024 * 1024)?;
    let fenced = XanaConfig::migrate_to_current(std::str::from_utf8(&original)?)?;
    if original != fenced.as_bytes() {
        write_atomic(paths.config_file(), fenced.as_bytes())?;
    }
    Ok(())
}
