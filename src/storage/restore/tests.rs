use super::*;
use crate::storage::{TestCustody, backup::BackupPolicy};

fn fixture() -> (tempfile::TempDir, XanaPaths, RecoveryIdentity, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
    let key = RecoveryIdentity::generate();
    let store =
        ProtectedStore::initialize(paths.data_dir(), &key, &TestCustody::default()).unwrap();
    store
        .set_document("memory/fixture", b"prior fact", 1024)
        .unwrap();
    let snapshot = store
        .backup(&BackupPolicy::default(), 1000, false)
        .unwrap()
        .snapshot
        .unwrap();
    store
        .set_document("memory/fixture", b"later correction", 1024)
        .unwrap();
    fs::write(
        paths.data_dir().join("selection.toml"),
        b"ordinary preferences",
    )
    .unwrap();
    drop(store);
    (directory, paths, key, snapshot)
}

#[test]
fn restore_preserves_prior_generation_and_requires_memory_authority_review() {
    let (_directory, paths, key, snapshot) = fixture();
    let plan = preview(&paths, &snapshot, &key).unwrap();
    let prior = apply(&paths, &snapshot, &key, &plan.review).unwrap();
    let restored = ProtectedStore::recover(paths.data_dir(), &key).unwrap();
    assert_eq!(
        restored.document("memory/fixture", 1024).unwrap().unwrap(),
        b"prior fact"
    );
    assert!(
        restored
            .document("restore/review-required", 4096)
            .unwrap()
            .is_some()
    );
    let retired = ProtectedStore::recover(&prior, &key).unwrap();
    assert_eq!(
        retired.document("memory/fixture", 1024).unwrap().unwrap(),
        b"later correction"
    );
    assert_eq!(
        fs::read(paths.data_dir().join("selection.toml")).unwrap(),
        b"ordinary preferences"
    );
}

#[test]
fn restore_faults_recover_without_activating_background_authority() {
    for point in ["restore-journal", "prior-retired", "restore-activated"] {
        let (_directory, paths, key, snapshot) = fixture();
        let review = preview(&paths, &snapshot, &key).unwrap().review;
        assert!(
            apply_with(&paths, &snapshot, &key, &review, |stage| {
                if stage == point {
                    anyhow::bail!("injected restore interruption")
                }
                Ok(())
            })
            .is_err()
        );
        assert!(ProtectedStore::status(paths.data_dir()).is_err());
        resume(&paths, &key).unwrap_or_else(|error| panic!("resume {point}: {error:#}"));
        let restored = ProtectedStore::recover(paths.data_dir(), &key).unwrap();
        assert!(
            restored
                .document("restore/review-required", 4096)
                .unwrap()
                .is_some()
        );
        restored.verify_content().unwrap();
    }
}

#[test]
fn restore_rejects_live_owners_and_tampered_backups_without_replacing_current_data() {
    let (_directory, paths, key, snapshot) = fixture();
    let owner = ProtectedStore::recover(paths.data_dir(), &key).unwrap();
    let review = preview(&paths, &snapshot, &key).unwrap().review;
    assert!(apply(&paths, &snapshot, &key, &review).is_err());
    assert_eq!(
        owner.document("memory/fixture", 1024).unwrap().unwrap(),
        b"later correction"
    );
    drop(owner);
    let database = snapshot.join("protected/content.sqlite");
    let mut bytes = fs::read(&database).unwrap();
    bytes[100] ^= 1;
    fs::write(database, bytes).unwrap();
    assert!(preview(&paths, &snapshot, &key).is_err());
    assert!(!journal_path(paths.data_dir()).exists());
}
