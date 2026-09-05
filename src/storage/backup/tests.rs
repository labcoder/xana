use super::*;
use crate::{
    artifact::ArtifactStore,
    identity::PrincipalId,
    storage::{RecoveryIdentity, TestCustody},
};

#[test]
fn encrypted_snapshot_recovers_wal_and_objects_without_original_custody() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("data");
    let key = RecoveryIdentity::generate();
    let store = ProtectedStore::initialize(&home, &key, &TestCustody::default()).unwrap();
    store
        .set_document("memory/canary", b"private backup canary", 1024)
        .unwrap();
    let (artifact, _) = ArtifactStore::protected(store.clone())
        .put(b"private artifact canary", "text/plain", PrincipalId::new())
        .unwrap();
    let report = store.backup(&BackupPolicy::default(), 1000, false).unwrap();
    let snapshot = report.snapshot.unwrap();
    let recovered = ProtectedStore::recover(&snapshot, &key).unwrap();
    assert_eq!(
        recovered.document("memory/canary", 1024).unwrap().unwrap(),
        b"private backup canary"
    );
    assert_eq!(
        ArtifactStore::protected(recovered.clone())
            .read_bounded(&artifact, 1024)
            .unwrap(),
        b"private artifact canary"
    );
    recovered.verify_content().unwrap();
    let bytes = fs::read(snapshot.join("protected/content.sqlite")).unwrap();
    assert!(
        !bytes
            .windows(21)
            .any(|value| value == b"private backup canary")
    );
}

#[test]
fn policy_keeps_last_usable_snapshot_when_a_replacement_exceeds_the_cap() {
    let directory = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        &directory.path().join("data"),
        &super::super::RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let policy = BackupPolicy {
        max_snapshots: 2,
        ..Default::default()
    };
    let first = store
        .backup(&policy, 1000, false)
        .unwrap()
        .snapshot
        .unwrap();
    assert!(
        store
            .backup(&policy, 1100, true)
            .unwrap()
            .snapshot
            .is_none()
    );
    let too_small = BackupPolicy {
        max_bytes: 1,
        ..policy.clone()
    };
    let failed = store.backup(&too_small, 1200, false).unwrap();
    assert!(failed.snapshot.is_none());
    assert!(first.exists());
    store.backup(&policy, 2000, false).unwrap();
    let final_report = store.backup(&policy, 3000, false).unwrap();
    assert_eq!(final_report.retained_snapshots, 2);
    assert!(!first.exists());
    assert_eq!(final_report.oldest_unix_seconds, Some(2000));
}

#[test]
fn interrupted_snapshot_writes_never_replace_the_usable_recovery_copy() {
    for point in [
        "backup-database",
        "backup-bootstrap",
        "backup-object",
        "backup-verified",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let key = RecoveryIdentity::generate();
        let store = ProtectedStore::initialize(
            &directory.path().join("data"),
            &key,
            &TestCustody::default(),
        )
        .unwrap();
        ArtifactStore::protected(store.clone())
            .put(b"canary", "text/plain", PrincipalId::new())
            .unwrap();
        let snapshot = store
            .backup(&BackupPolicy::default(), 1000, false)
            .unwrap()
            .snapshot
            .unwrap();
        let partial = directory.path().join("interrupted");
        fs::create_dir(&partial).unwrap();
        let result = store.snapshot_into_with(&partial, 1024 * 1024, |stage| {
            if stage == point {
                return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
            }
            Ok(())
        });
        assert!(result.is_err(), "{point}");
        assert!(
            validate_snapshot(&partial).is_err(),
            "incomplete snapshot is not published"
        );
        ProtectedStore::recover(&snapshot, &key)
            .unwrap()
            .verify_content()
            .unwrap();
        store.verify_content().unwrap();
    }
}

#[test]
fn pruning_refuses_unrecognized_files_in_an_old_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        &directory.path().join("data"),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let policy = BackupPolicy {
        max_snapshots: 1,
        ..Default::default()
    };
    let old = store
        .backup(&policy, 1000, false)
        .unwrap()
        .snapshot
        .unwrap();
    fs::write(old.join("protected/unrelated.txt"), b"user file").unwrap();
    assert!(store.backup(&policy, 2000, false).is_err());
    assert_eq!(
        fs::read(old.join("protected/unrelated.txt")).unwrap(),
        b"user file"
    );
}
