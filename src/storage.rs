//! Explicit ownership of Xana-managed encrypted content, never ordinary files.
//!
//! The composition root supplies custody or a recovery key. A handle owns one
//! connection; clones share its revocable unlock state, not a process-global key.

pub(crate) mod backup;
mod database;
mod documents;
mod encrypted_artifacts;
mod history;
pub(crate) use history::HistorySubject;
mod autonomy;
mod forgetting;
mod keys;
mod learning;
mod memory;
mod priority;
mod recall;
pub(crate) use priority::{ForegroundJobLease, ForegroundLease};
pub(crate) mod migration;
mod private_file;
mod reset;
pub(crate) mod restore;
mod usage;
mod verification;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::Custody as TestCustody;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

use keys::Secrets;
pub(crate) use keys::{
    KeyCustody, OsCustody, RecoveryIdentity, RecoveryOnlyCustody, read_recovery_identity,
};
pub(crate) use private_file::create_private_file;

const FORMAT: u32 = 1;
const MAX_BOOTSTRAP: usize = 4096;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    version: u32,
    id: Uuid,
}

/// Non-content location and format inspection does not unlock the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StorageStatus {
    Legacy,
    Protected { id: Uuid, locked: bool },
}

/// Cloning a capability does not create another key owner or writer.
#[derive(Clone)]
pub(crate) struct ProtectedStore {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    id: Uuid,
    open: Mutex<Option<database::Database>>,
}

impl std::fmt::Debug for ProtectedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedStore")
            .field("id", &self.inner.id)
            .finish_non_exhaustive()
    }
}

impl ProtectedStore {
    /// Only for an explicitly prepared restore destination, never snapshot inspection.
    fn prepare_restored_schema(data: &Path, identity: &RecoveryIdentity) -> Result<Self> {
        for _ in 0..2 {
            let store = Self::open_recovery(data, identity, false)?;
            {
                let mut guard = store
                    .inner
                    .open
                    .lock()
                    .map_err(|_| anyhow::anyhow!("protected owner failed"))?;
                let database = guard.take().context("protected storage is locked")?;
                let Some(database) = database.prepare_schema()? else {
                    continue;
                };
                *guard = Some(database);
            }
            return Ok(store);
        }
        bail!("restore schema changed repeatedly")
    }
    /// Explicit managed-home selection. An unavailable/locked protected home
    /// is an error, never a reason to select the legacy plaintext backend.
    pub(crate) fn configured(data_dir: &Path) -> Result<Option<Self>> {
        for _ in 0..2 {
            match Self::status(data_dir)? {
                StorageStatus::Legacy => return Ok(None),
                StorageStatus::Protected { locked: true, .. } => {
                    bail!("protected storage is locked; explicitly unlock it before continuing")
                }
                StorageStatus::Protected { .. } => {
                    // Explicit application configuration, not a hidden fallback or
                    // a key cache. Provider/Agent objects never read this variable.
                    let store = if let Some(path) = std::env::var_os("XANA_STORAGE_RECOVERY_KEY") {
                        let identity = read_recovery_identity(Path::new(&path))?;
                        Self::open_recovery(data_dir, &identity, false)?
                    } else {
                        Self::open(data_dir, &OsCustody)?
                    };
                    {
                        let mut guard = store
                            .inner
                            .open
                            .lock()
                            .map_err(|_| anyhow::anyhow!("protected owner failed"))?;
                        let database = guard.take().context("protected storage is locked")?;
                        let Some(database) = database.prepare_schema()? else {
                            continue;
                        };
                        *guard = Some(database);
                    }
                    return Ok(Some(store));
                }
            }
        }
        bail!(
            "protected schema changed repeatedly during upgrade; reopen after lifecycle work finishes"
        )
    }

    pub(crate) fn status(data_dir: &Path) -> Result<StorageStatus> {
        ensure!(
            !migration::journal_path(data_dir).exists(),
            "storage transition is pending; resume the reviewed migration/restore before opening Xana"
        );
        let root = data_dir.join("protected");
        let Some(bootstrap) = read_bootstrap(&root)? else {
            return Ok(StorageStatus::Legacy);
        };
        Ok(StorageStatus::Protected {
            id: bootstrap.id,
            locked: root.join("locked").exists(),
        })
    }

    /// Only a fresh data directory is accepted. Existing homes use reviewed migration.
    pub(crate) fn initialize(
        data_dir: &Path,
        recovery: &RecoveryIdentity,
        custody: &dyn KeyCustody,
    ) -> Result<Self> {
        fs::create_dir_all(data_dir).context("could not create Xana data directory")?;
        ensure!(
            fs::read_dir(data_dir)?.next().is_none(),
            "existing Xana data requires reviewed storage migration; no changes made"
        );
        let root = data_dir.join("protected");
        fs::create_dir(&root)?;
        let id = Uuid::new_v4();
        let bootstrap = serde_json::to_vec(&Bootstrap {
            version: FORMAT,
            id,
        })?;
        write_new_synced(&root.join("pending.json"), &bootstrap)?;
        let secrets = Secrets::generate()?;
        let result = (|| {
            let database = database::Database::create(&root, id, secrets)?;
            let envelope = database
                .secrets
                .recovery_envelope(id, &recovery.to_public())?;
            write_new_synced(&root.join("recovery.age"), &envelope)?;
            let recovered = Secrets::recover(id, &envelope, recovery)?;
            ensure!(
                *recovered.database == *database.secrets.database,
                "recovery verification differs"
            );
            database.verify()?;
            custody.store(id, &database.secrets.encode()?)?;
            fs::rename(root.join("pending.json"), root.join("store.json"))?;
            Ok(Self::from_database(root.clone(), id, database))
        })();
        if result.is_err() {
            // Keep interrupted ciphertext for explicit recovery, never activate
            // or fall through to a fresh plaintext home after partial creation.
            let _ = write_new_synced(&root.join("incomplete"), b"initialization interrupted\n");
        }
        result
    }

    pub(crate) fn open(data_dir: &Path, custody: &dyn KeyCustody) -> Result<Self> {
        let root = data_dir.join("protected");
        let bootstrap = read_bootstrap(&root)?.context("protected storage is not initialized")?;
        ensure!(
            !root.join("locked").exists(),
            "protected storage is locked; unlock it before opening a conversation"
        );
        let secret = custody
            .load(bootstrap.id)?
            .context("protected storage key is unavailable; use independent recovery")?;
        let secrets = Secrets::decode(&secret)?;
        let database = database::Database::open(&root, bootstrap.id, secrets)?;
        // Recheck after acquiring the live-owner lease: a concurrent lock may
        // have completed between the first check and acquisition.
        ensure!(!root.join("locked").exists(), "protected storage is locked");
        Ok(Self::from_database(root, bootstrap.id, database))
    }

    pub(crate) fn recover(data_dir: &Path, identity: &RecoveryIdentity) -> Result<Self> {
        Self::open_recovery(data_dir, identity, true)
    }

    fn open_recovery(
        data_dir: &Path,
        identity: &RecoveryIdentity,
        explicit_unlock: bool,
    ) -> Result<Self> {
        let root = data_dir.join("protected");
        let bootstrap = read_bootstrap(&root)?.context("protected storage is not initialized")?;
        let envelope = crate::bounded_file::read(&root.join("recovery.age"), MAX_BOOTSTRAP)?;
        let secrets = Secrets::recover(bootstrap.id, &envelope, identity)?;
        let database = database::Database::open(&root, bootstrap.id, secrets)?;
        // Recovery is an explicit unlock. Do not revive any revoked old handle.
        let locked = root.join("locked");
        ensure!(
            explicit_unlock || !locked.exists(),
            "protected storage is locked"
        );
        if explicit_unlock && locked.exists() {
            fs::remove_file(locked)?;
        }
        Ok(Self::from_database(root, bootstrap.id, database))
    }

    fn from_database(root: PathBuf, id: Uuid, database: database::Database) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                id,
                open: Mutex::new(Some(database)),
            }),
        }
    }

    pub(crate) fn id(&self) -> Uuid {
        self.inner.id
    }

    pub(crate) fn database_path(&self) -> PathBuf {
        self.inner.root.join("content.sqlite")
    }

    pub(crate) fn data_dir(&self) -> &Path {
        self.inner
            .root
            .parent()
            .expect("protected store has data parent")
    }

    pub(crate) fn session_writer(&self, id: crate::identity::SessionId) -> Result<fs::File> {
        self.with_database(|_| {
            let path = self.inner.root.join(format!("{id}.writer"));
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            fs2::FileExt::try_lock_exclusive(&file)
                .context("this Conversation already has a writer")?;
            Ok(file)
        })
    }

    pub(crate) fn verify(&self) -> Result<()> {
        self.with_database(|db| db.verify())
    }

    /// Call only after the host has stopped admission, settled work and detached
    /// plaintext projections. This final step revokes all clones and checkpoints.
    pub(crate) fn lock(&self) -> Result<()> {
        let mut open = self
            .inner
            .open
            .lock()
            .map_err(|_| anyhow::anyhow!("protected store owner failed"))?;
        if let Some(db) = open.take() {
            db.seal(&self.inner.root)?;
        }
        Ok(())
    }

    pub(crate) fn unlock(data_dir: &Path, custody: &dyn KeyCustody) -> Result<Self> {
        let root = data_dir.join("protected");
        let bootstrap = read_bootstrap(&root)?.context("protected storage is not initialized")?;
        let secret = custody
            .load(bootstrap.id)?
            .context("protected storage key is unavailable; use independent recovery")?;
        let db = database::Database::open(&root, bootstrap.id, Secrets::decode(&secret)?)?;
        let locked = root.join("locked");
        if locked.exists() {
            fs::remove_file(locked)?;
        }
        Ok(Self::from_database(root, bootstrap.id, db))
    }

    pub(crate) fn bind_custody(&self, custody: &dyn KeyCustody) -> Result<()> {
        self.with_database(|db| custody.store(self.id(), &db.secrets.encode()?))
    }

    fn with_database<T>(
        &self,
        action: impl FnOnce(&mut database::Database) -> Result<T>,
    ) -> Result<T> {
        let mut open = self
            .inner
            .open
            .lock()
            .map_err(|_| anyhow::anyhow!("protected store owner failed"))?;
        let db = open.as_mut().context("protected storage is locked")?;
        // A lock requested through another owner stops the next admission too.
        ensure!(
            !self.inner.root.join("locked").exists(),
            "protected storage is locked"
        );
        action(db)
    }

    /// Offline lifecycle operations consume this unlocked owner even on error.
    /// This prevents a failed shared-to-exclusive lease change from leaving an
    /// apparently usable connection without its live-owner protection.
    fn with_exclusive_database<T>(
        &self,
        action: impl FnOnce(&mut database::Database) -> Result<T>,
    ) -> Result<T> {
        let mut guard = self
            .inner
            .open
            .lock()
            .map_err(|_| anyhow::anyhow!("protected owner failed"))?;
        let mut database = guard.take().context("protected storage is locked")?;
        database.exclusive()?;
        action(&mut database)
    }
}

fn read_bootstrap(root: &Path) -> Result<Option<Bootstrap>> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context("protected storage is unavailable; no plaintext fallback");
        }
    };
    ensure!(
        metadata.file_type().is_dir(),
        "protected storage must be a real directory"
    );
    let path = root.join("store.json");
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_file(),
        "protected bootstrap must be a regular file"
    );
    let bytes = crate::bounded_file::read(&path, MAX_BOOTSTRAP).context("protected storage activation is incomplete or unavailable; do not recreate plaintext state")?;
    let bootstrap: Bootstrap =
        serde_json::from_slice(&bytes).context("invalid protected storage bootstrap")?;
    if bootstrap.version != FORMAT {
        bail!(
            "unsupported protected store version {}; expected {FORMAT}",
            bootstrap.version
        );
    }
    Ok(Some(bootstrap))
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = fs::File::create_new(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
