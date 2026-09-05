use super::*;
use std::collections::HashMap;
use zeroize::Zeroizing;

#[test]
fn memory_corrupt_routing_and_oversized_blobs_fail_closed() {
    use crate::memory::{MemoryContext, MemoryControlEdit, MemoryOwner, MemoryScope};
    let home = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        home.path(),
        &RecoveryIdentity::generate(),
        &Custody::default(),
    )
    .unwrap();
    let owner = MemoryOwner::new(
        store,
        MemoryContext {
            profile: Some(Uuid::new_v4()),
            ..Default::default()
        },
    );
    let record = owner
        .remember(MemoryScope::User, "Synthetic fact".into(), None)
        .unwrap();
    owner
        .store
        .with_database(|db| {
            db.connection.execute(
                "UPDATE memory_entries SET scope=?1 WHERE id=?2",
                rusqlite::params![
                    format!("profile:{}", owner.context.profile.unwrap()),
                    record.id.to_string()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(owner.record(record.id).is_err());
    assert!(owner.page(None, None).is_err());
    assert!(owner.eligible().is_err());
    owner
        .store
        .with_database(|db| {
            db.connection.execute(
                "UPDATE memory_entries SET scope='user',body=zeroblob(8193)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(owner.record(record.id).is_err());
    assert!(
        owner
            .controls(
                MemoryScope::Profile(Uuid::nil()),
                MemoryControlEdit::default()
            )
            .is_err()
    );
}

#[derive(Default)]
pub(crate) struct Custody(Mutex<HashMap<Uuid, Zeroizing<Vec<u8>>>>);

impl KeyCustody for Custody {
    fn load(&self, id: Uuid) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(self.0.lock().unwrap().get(&id).cloned())
    }
    fn store(&self, id: Uuid, secret: &[u8]) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(id, Zeroizing::new(secret.to_vec()));
        Ok(())
    }
}

#[test]
fn home_reopens_through_custody_and_independent_recovery() {
    let home = tempfile::tempdir().unwrap();
    let recovery = RecoveryIdentity::generate();
    let custody = Custody::default();
    let store = ProtectedStore::initialize(home.path(), &recovery, &custody).unwrap();
    let id = store.id();
    store.verify().unwrap();
    let second = ProtectedStore::open(home.path(), &custody).unwrap();
    second.verify().unwrap();
    let session_id = crate::identity::SessionId::new();
    let writer = store.session_writer(session_id).unwrap();
    assert!(second.session_writer(session_id).is_err());
    drop(writer);
    drop(second);
    drop(store);
    let opened = ProtectedStore::open(home.path(), &custody).unwrap();
    assert_eq!(opened.id(), id);
    drop(opened);
    custody.0.lock().unwrap().clear();
    assert!(ProtectedStore::open(home.path(), &custody).is_err());
    let recovered = ProtectedStore::recover(home.path(), &recovery).unwrap();
    recovered.verify().unwrap();
    recovered.bind_custody(&custody).unwrap();
    drop(recovered);
    assert!(ProtectedStore::open(home.path(), &custody).is_ok());
}

#[test]
fn accounting_upgrade_preserves_old_snapshots_and_excludes_live_owners() {
    let home = tempfile::tempdir().unwrap();
    let key = RecoveryIdentity::generate();
    let custody = Custody::default();
    let store = ProtectedStore::initialize(home.path(), &key, &custody).unwrap();
    store.set_document("fixture", b"retained", 1024).unwrap();
    // Construct the released schema-1 format, not a different storage backend.
    store.with_database(|db| { db.connection.execute_batch("DROP TABLE usage_requests; DROP TABLE usage_counters; DROP TABLE memory_entries; DROP TABLE memory_revisions; DROP TABLE memory_controls; UPDATE store_identity SET version=1; PRAGMA user_version=1;")?; Ok(()) }).unwrap();
    let recovery = ProtectedStore::recover(home.path(), &key).unwrap();
    assert_eq!(
        recovery.document("fixture", 1024).unwrap().unwrap(),
        b"retained"
    );
    let database = store.inner.open.lock().unwrap().take().unwrap();
    assert!(
        database.prepare_schema().is_err(),
        "a recovery owner prevents exclusive upgrade"
    );
    recovery
        .with_database(|db| {
            assert_eq!(
                db.connection
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
    drop(recovery);
    let reopened = ProtectedStore::open(home.path(), &custody).unwrap();
    let database = reopened.inner.open.lock().unwrap().take().unwrap();
    assert!(
        database.prepare_schema().unwrap().is_none(),
        "upgrade closes the connection under its exclusive lease"
    );
    let upgraded = ProtectedStore::open(home.path(), &custody).unwrap();
    assert_eq!(
        upgraded.document("fixture", 1024).unwrap().unwrap(),
        b"retained"
    );
    assert!(upgraded.usage_page(None, None, None).unwrap().is_empty());
    upgraded.verify().unwrap();
}

#[test]
fn usage_corruption_is_rejected_before_owned_blob_materialization() {
    let home = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        home.path(),
        &RecoveryIdentity::generate(),
        &Custody::default(),
    )
    .unwrap();
    store.with_database(|db| { db.connection.execute("INSERT INTO usage_requests(id,operation,root,job,day,background,charge,admission) VALUES('bad','op','root','job',0,0,1,zeroblob(16385))", [])?; Ok(()) }).unwrap();
    assert!(store.usage_page(None, None, None).is_err());
    assert!(store.usage_attribution("op").is_err());
}

#[test]
fn memory_schema_upgrade_from_accounting_preserves_data_and_snapshot_inspection_is_read_only() {
    let home = tempfile::tempdir().unwrap();
    let key = RecoveryIdentity::generate();
    let custody = Custody::default();
    let store = ProtectedStore::initialize(home.path(), &key, &custody).unwrap();
    store.set_document("fixture", b"retained", 1024).unwrap();
    store.with_database(|db| {
        db.connection.execute_batch("DROP TABLE memory_entries; DROP TABLE memory_revisions; DROP TABLE memory_controls; UPDATE store_identity SET version=2; PRAGMA user_version=2;")?;
        Ok(())
    }).unwrap();
    drop(store);
    let snapshot = ProtectedStore::recover(home.path(), &key).unwrap();
    snapshot
        .with_database(|db| {
            assert_eq!(
                db.connection
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))?,
                2
            );
            Ok(())
        })
        .unwrap();
    drop(snapshot);
    let canonical = ProtectedStore::open(home.path(), &custody).unwrap();
    let database = canonical.inner.open.lock().unwrap().take().unwrap();
    assert!(database.prepare_schema().unwrap().is_none());
    let upgraded = ProtectedStore::open(home.path(), &custody).unwrap();
    assert_eq!(
        upgraded.document("fixture", 1024).unwrap().unwrap(),
        b"retained"
    );
    assert!(upgraded.usage_page(None, None, None).unwrap().is_empty());
    assert!(upgraded.memory_page(None, None).unwrap().records.is_empty());
    upgraded.verify().unwrap();
}

#[test]
fn lock_does_not_claim_success_while_an_independent_owner_retains_keys() {
    let home = tempfile::tempdir().unwrap();
    let custody = Custody::default();
    let store =
        ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody).unwrap();
    let second = ProtectedStore::open(home.path(), &custody).unwrap();
    assert!(store.lock().is_err());
    assert!(store.verify().is_err(), "the requesting handle is revoked");
    second.verify().unwrap();
    assert!(matches!(
        ProtectedStore::status(home.path()).unwrap(),
        StorageStatus::Protected { locked: false, .. }
    ));
    second.lock().unwrap();
    assert!(ProtectedStore::open(home.path(), &custody).is_err());
}

#[test]
fn locking_revokes_clones_and_requires_explicit_unlock() {
    let home = tempfile::tempdir().unwrap();
    let custody = Custody::default();
    let store =
        ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody).unwrap();
    let clone = store.clone();
    store.lock().unwrap();
    assert!(clone.verify().is_err());
    assert!(ProtectedStore::open(home.path(), &custody).is_err());
    let unlocked = ProtectedStore::unlock(home.path(), &custody).unwrap();
    unlocked.verify().unwrap();
    assert!(
        clone.verify().is_err(),
        "unlock must not revive revoked capabilities"
    );
}

#[test]
fn recovery_can_unlock_a_locked_home_after_custody_loss() {
    let home = tempfile::tempdir().unwrap();
    let recovery = RecoveryIdentity::generate();
    let custody = Custody::default();
    let store = ProtectedStore::initialize(home.path(), &recovery, &custody).unwrap();
    store.lock().unwrap();
    custody.0.lock().unwrap().clear();
    let recovered = ProtectedStore::recover(home.path(), &recovery).unwrap();
    recovered.verify().unwrap();
}

#[test]
fn existing_content_and_partial_activation_never_fall_back_to_plaintext() {
    let home = tempfile::tempdir().unwrap();
    let custody = Custody::default();
    let recipient = RecoveryIdentity::generate();
    fs::write(home.path().join("ordinary.txt"), b"leave me alone").unwrap();
    assert!(ProtectedStore::initialize(home.path(), &recipient, &custody).is_err());
    assert_eq!(
        fs::read(home.path().join("ordinary.txt")).unwrap(),
        b"leave me alone"
    );
    fs::create_dir(home.path().join("protected")).unwrap();
    assert!(ProtectedStore::status(home.path()).is_err());
    assert!(ProtectedStore::open(home.path(), &custody).is_err());
}

#[test]
fn selected_reset_requires_exclusive_ownership_and_preserves_other_records() {
    let home = tempfile::tempdir().unwrap();
    let custody = Custody::default();
    let store =
        ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody).unwrap();
    store
        .set_document("managed-threads/fixture", b"handle", 1024)
        .unwrap();
    store
        .set_document("interoperable/projects.json", b"projects", 1024)
        .unwrap();
    let second = ProtectedStore::open(home.path(), &custody).unwrap();
    assert!(store.reset_records(false, true).is_err());
    assert_eq!(
        second
            .document("managed-threads/fixture", 1024)
            .unwrap()
            .unwrap(),
        b"handle"
    );
    second.reset_records(false, true).unwrap();
    let reopened = ProtectedStore::open(home.path(), &custody).unwrap();
    assert!(
        reopened
            .document("managed-threads/fixture", 1024)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened
            .document("interoperable/projects.json", 1024)
            .unwrap()
            .unwrap(),
        b"projects"
    );
    reopened.verify().unwrap();
}

#[test]
fn encrypted_artifact_facade_deduplicates_and_rejects_off_range_tampering() {
    use crate::{artifact::ArtifactStore, identity::PrincipalId};
    let home = tempfile::tempdir().unwrap();
    let custody = Custody::default();
    let store =
        ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody).unwrap();
    let artifacts = ArtifactStore::protected(store.clone());
    let mut bytes = vec![b'x'; 192 * 1024];
    bytes[..24].copy_from_slice(b"private artifact canary!");
    let (first, created) = artifacts
        .put(&bytes, "text/plain", PrincipalId::new())
        .unwrap();
    assert!(created);
    let (second, created) = artifacts
        .put(&bytes, "text/plain", PrincipalId::new())
        .unwrap();
    assert!(!created);
    assert_ne!(first.reference.id, second.reference.id);
    assert_eq!(first.reference.content_hash, second.reference.content_hash);
    let range = artifacts
        .read_verified_range(&first, 190 * 1024, 1024, 256 * 1024)
        .unwrap();
    assert_eq!(range.bytes, bytes[190 * 1024..191 * 1024]);
    assert_eq!(artifacts.read_bounded(&first, 256 * 1024).unwrap(), bytes);
    let export = tempfile::tempdir().unwrap();
    let destination = export.path().join("explicit-export.txt");
    artifacts
        .copy_verified_create_new(&first, &destination, 256 * 1024)
        .unwrap();
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(
        artifacts
            .copy_verified_create_new(&first, &destination, 256 * 1024)
            .is_err()
    );
    let path = fs::read_dir(home.path().join("protected/objects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut cipher = fs::read(&path).unwrap();
    assert!(
        !cipher
            .windows(24)
            .any(|part| part == b"private artifact canary!")
    );
    cipher[80 * 1024] ^= 1;
    fs::write(path, cipher).unwrap();
    let failed_export = export.path().join("invalid.txt");
    assert!(
        artifacts
            .copy_verified_create_new(&first, &failed_export, 256 * 1024)
            .is_err()
    );
    assert!(
        !failed_export.exists(),
        "a failed export must not leave partial plaintext"
    );
    assert!(
        artifacts
            .read_verified_range(&first, 190 * 1024, 1024, 256 * 1024)
            .is_err(),
        "tampering outside selected range must fail"
    );
    store.lock().unwrap();
    assert!(artifacts.read_bounded(&first, 256 * 1024).is_err());
}
