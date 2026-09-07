//! Explicit opt-in against one generated production-service credential only.
use super::*;
use crate::storage::ProtectedStore;
use std::sync::Mutex;

#[derive(Default)]
struct FixtureCustody {
    owned: Mutex<Option<Uuid>>,
}

impl KeyCustody for FixtureCustody {
    fn load(&self, id: Uuid) -> Result<Option<Zeroizing<Vec<u8>>>> {
        ensure!(
            *self
                .owned
                .lock()
                .map_err(|_| anyhow::anyhow!("fixture custody owner failed"))?
                == Some(id),
            "unexpected fixture identity"
        );
        OsCustody.load(id)
    }

    fn store(&self, id: Uuid, secret: &[u8]) -> Result<()> {
        let mut owned = self
            .owned
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture custody owner failed"))?;
        ensure!(owned.is_none(), "fixture must allocate only one credential");
        let entry = OsCustody::entry(id)?;
        ensure!(
            matches!(entry.get_secret(), Err(keyring_core::Error::NoEntry)),
            "new fixture identity already exists or native custody is unavailable"
        );
        // Track before writing: partial native failure still requires cleanup.
        *owned = Some(id);
        OsCustody.store(id, secret)
    }
}

impl FixtureCustody {
    fn remove_exact(&self) -> Result<()> {
        // A panic during a native write may poison this mutex after the exact
        // identity was recorded. Cleanup still owns that identity, never others.
        let Some(id) = *self
            .owned
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        else {
            return Ok(());
        };
        let entry = OsCustody::entry(id)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => {}
            Err(_) => anyhow::bail!(
                "remove only the fixture credential dev.xana.protected-storage / {id}; cleanup failed"
            ),
        }
        ensure!(
            matches!(entry.get_secret(), Err(keyring_core::Error::NoEntry)),
            "fixture credential absence could not be verified: {id}"
        );
        Ok(())
    }
}

impl Drop for FixtureCustody {
    fn drop(&mut self) {
        if self.remove_exact().is_err() {
            let owned = self
                .owned
                .get_mut()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(id) = *owned {
                eprintln!(
                    "native custody fixture cleanup remains unverified; inspect only dev.xana.protected-storage / {id}"
                );
            } else {
                eprintln!(
                    "native custody fixture cleanup failed before a credential identity was recorded"
                );
            }
        }
    }
}

#[test]
#[ignore = "explicit disposable production credential; requires an ordinary unlocked native user session"]
fn production_os_custody_unlock_loss_lock_and_independent_recovery() -> Result<()> {
    exercise_custody(false)
}

#[test]
#[ignore = "writes and removes one generated native credential; explicit disposable qualification only"]
fn automatic_keys_restart_late_export_and_os_loss_recovery() -> Result<()> {
    exercise_custody(true)
}

fn exercise_custody(managed: bool) -> Result<()> {
    let scratch = tempfile::Builder::new()
        .prefix("xana-m6-native-custody-")
        .tempdir()?;
    let canonical = scratch.path().canonicalize()?;
    let identity = same_file::Handle::from_path(&canonical)?;
    let custody = FixtureCustody::default();
    let mut recovery = RecoveryIdentity::generate();
    let data = canonical.join("data");
    let outcome = (|| -> Result<()> {
        let store = if managed {
            ProtectedStore::initialize_managed(&data, &custody)?
        } else {
            ProtectedStore::initialize(&data, &recovery, &custody)?
        };
        let id = store.id();
        store.set_document(
            "native-custody-fixture",
            b"synthetic native custody record",
            1024,
        )?;
        drop(store);
        let opened = ProtectedStore::open(&data, &custody)?;
        if managed {
            let destination = canonical.join("recovery.key");
            opened.export_recovery(&destination)?;
            recovery = crate::storage::read_recovery_identity(&destination)?;
        }
        ensure!(opened.id() == id, "native reopened identity differs");
        opened.verify()?;
        opened.lock()?;
        drop(opened);
        ensure!(
            ProtectedStore::open(&data, &custody).is_err(),
            "explicit lock was bypassed"
        );
        let unlocked = ProtectedStore::unlock(&data, &custody)?;
        unlocked.verify()?;
        drop(unlocked);
        custody.remove_exact()?;
        ensure!(
            ProtectedStore::open(&data, &custody).is_err(),
            "missing native credential did not fail closed"
        );
        let recovered = ProtectedStore::recover(&data, &recovery)?;
        ensure!(
            recovered.id() == id,
            "independently recovered identity differs"
        );
        ensure!(
            recovered
                .document("native-custody-fixture", 1024)?
                .as_deref()
                == Some(b"synthetic native custody record".as_slice()),
            "independently recovered content differs"
        );
        recovered.verify()?;
        Ok(())
    })();
    let credential_cleanup = custody.remove_exact();
    // Never recursively remove a substituted path. Retain it for inspection.
    let unchanged = canonical.canonicalize().is_ok_and(|path| path == canonical)
        && std::fs::symlink_metadata(&canonical)
            .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        && same_file::Handle::from_path(&canonical).is_ok_and(|current| current == identity);
    drop(identity);
    if !unchanged {
        let _ = scratch.keep();
        anyhow::bail!(
            "native custody fixture directory changed; retained for exact manual inspection"
        );
    }
    scratch.close()?;
    credential_cleanup?;
    outcome
}
