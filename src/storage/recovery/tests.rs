use super::*;
use crate::storage::{KeyCustody, StorageStatus, TestCustody};

#[test]
fn managed_keys_survive_restart_and_late_export_recovers_earlier_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let custody = TestCustody::default();
    let store = ProtectedStore::initialize_managed(&data, &custody).unwrap();
    store.set_document("fixture/name", b"Alice", 64).unwrap();
    assert_eq!(status(&data).unwrap(), RecoveryStatus::Pending);
    let snapshot = temp.path().join("snapshot");
    fs::create_dir(&snapshot).unwrap();
    store.snapshot_into(&snapshot, 32 * 1024 * 1024).unwrap();
    let copied_bytes = fs::read_dir(snapshot.join("protected"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum::<u64>();
    let too_small = temp.path().join("too-small");
    fs::create_dir(&too_small).unwrap();
    assert!(store.snapshot_into(&too_small, copied_bytes - 1).is_err());
    drop(store);
    let store = ProtectedStore::open(&data, &custody).unwrap();
    let export = temp.path().join("recovery.txt");
    store.export_recovery(&export).unwrap();
    assert_eq!(status(&data).unwrap(), RecoveryStatus::Exported);
    let identity = super::super::read_recovery_identity(&export).unwrap();
    assert!(store.export_recovery(&export).is_err());
    drop(store);
    assert!(ProtectedStore::open(&data, &TestCustody::default()).is_err());
    let recovered = ProtectedStore::recover(&snapshot, &identity).unwrap();
    assert_eq!(
        recovered.document("fixture/name", 64).unwrap().unwrap(),
        b"Alice"
    );
    // Managed keys are not printed or stored as an ordinary sidecar file.
    let key = identity.to_string();
    for entry in fs::read_dir(data.join("protected")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(path).unwrap();
            assert!(
                !bytes
                    .windows(key.expose_secret().len())
                    .any(|s| s == key.expose_secret().as_bytes())
            );
        }
    }
}

#[test]
fn failed_export_keeps_recovery_pending_and_lock_revokes_export() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = ProtectedStore::initialize_managed(&data, &TestCustody::default()).unwrap();
    assert!(store.export_recovery(&data.join("bad.key")).is_err());
    assert!(store.export_recovery(Path::new("relative.key")).is_err());
    assert!(
        store
            .export_recovery(&temp.path().join("missing/key"))
            .is_err()
    );
    assert_eq!(status(&data).unwrap(), RecoveryStatus::Pending);
    store.lock().unwrap();
    assert_eq!(status(&data).unwrap(), RecoveryStatus::Pending);
    assert!(
        store
            .export_recovery(&temp.path().join("locked.key"))
            .is_err()
    );
    assert!(!temp.path().join("locked.key").exists());
}

#[test]
fn unavailable_custody_never_activates_or_creates_a_plaintext_key() {
    struct Unavailable;
    impl KeyCustody for Unavailable {
        fn load(&self, _: uuid::Uuid) -> Result<Option<Zeroizing<Vec<u8>>>> {
            anyhow::bail!("unavailable")
        }
        fn store(&self, _: uuid::Uuid, _: &[u8]) -> Result<()> {
            anyhow::bail!("unavailable")
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    assert!(ProtectedStore::initialize_managed(&data, &Unavailable).is_err());
    assert!(!data.join("protected/store.json").exists());
    assert!(ProtectedStore::status(&data).is_err());
    assert!(!matches!(
        ProtectedStore::status(&data),
        Ok(StorageStatus::Legacy)
    ));
}

#[test]
fn existing_user_key_remains_required_and_is_not_replaced() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let key = RecoveryIdentity::generate();
    let store = ProtectedStore::initialize(&data, &key, &TestCustody::default()).unwrap();
    assert_eq!(status(&data).unwrap(), RecoveryStatus::UserManaged);
    assert!(
        store
            .export_recovery(&temp.path().join("replacement.key"))
            .is_err()
    );
    assert!(!temp.path().join("replacement.key").exists());
    drop(store);
    assert!(ProtectedStore::recover(&data, &key).is_ok());
}
