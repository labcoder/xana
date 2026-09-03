use super::*;
use tempfile::tempdir;

#[test]
fn same_bytes_share_content_but_keep_logical_identity_and_owner() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().to_owned());
    let first_owner = PrincipalId::new();
    let second_owner = PrincipalId::new();

    let (first, first_created) = store
        .put(b"same bytes", "text/plain", first_owner)
        .expect("first artifact");
    let (second, second_created) = store
        .put(b"same bytes", "text/plain", second_owner)
        .expect("second artifact");

    assert!(first_created);
    assert!(!second_created);
    assert_eq!(first.reference.content_hash, second.reference.content_hash);
    assert_ne!(first.reference.id, second.reference.id);
    assert_eq!(first.owner, first_owner);
    assert_eq!(second.owner, second_owner);
}

#[test]
fn existing_incorrect_digest_path_is_visible_corruption() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().to_owned());
    fs::create_dir_all(directory.path()).expect("artifact directory");
    let expected = ContentHash::for_bytes(b"expected");
    fs::write(store.path_for(&expected), b"wrong").expect("plant corrupt bytes");

    assert!(matches!(
        store.put(b"expected", "text/plain", PrincipalId::new()),
        Err(ArtifactError::CorruptContent { .. })
    ));
}

#[test]
fn bounded_reads_verify_length_and_digest() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().to_owned());
    let (artifact, _) = store
        .put(b"verified", "application/octet-stream", PrincipalId::new())
        .expect("put artifact");

    assert!(matches!(
        store.read_bounded(&artifact, 2),
        Err(ArtifactError::TooLarge { .. })
    ));
    assert_eq!(
        store.read_bounded(&artifact, 32).expect("verified read"),
        b"verified"
    );

    fs::write(
        store.path_for(&artifact.reference.content_hash),
        b"tampered",
    )
    .expect("tamper artifact");
    assert!(matches!(
        store.read_bounded(&artifact, 32),
        Err(ArtifactError::CorruptContent { .. })
    ));
}

#[test]
fn verified_copy_is_streamed_and_never_overwrites() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().join("store"));
    let (artifact, _) = store
        .put(b"verified export", "text/plain", PrincipalId::new())
        .expect("put artifact");
    let destination = directory.path().join("export.txt");

    store
        .copy_verified_create_new(&artifact, &destination, MAX_ARTIFACT_BYTES)
        .expect("copy artifact");
    assert_eq!(fs::read(&destination).unwrap(), b"verified export");
    assert!(matches!(
        store.copy_verified_create_new(&artifact, &destination, MAX_ARTIFACT_BYTES),
        Err(ArtifactError::Io { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists
    ));
    assert_eq!(fs::read(&destination).unwrap(), b"verified export");
}

#[test]
fn failed_verified_copy_removes_only_its_partial_destination() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().join("store"));
    let (artifact, _) = store
        .put(b"original bytes", "text/plain", PrincipalId::new())
        .expect("put artifact");
    fs::write(
        store.path_for(&artifact.reference.content_hash),
        b"tampered bytes",
    )
    .unwrap();
    let destination = directory.path().join("partial.txt");

    assert!(matches!(
        store.copy_verified_create_new(&artifact, &destination, MAX_ARTIFACT_BYTES),
        Err(ArtifactError::CorruptContent { .. })
    ));
    assert!(!destination.exists());
}

#[test]
fn publishing_leaves_no_partial_temporary_file() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().to_owned());
    let (artifact, created) = store
        .put(b"published", "text/plain", PrincipalId::new())
        .expect("put artifact");

    assert!(created);
    assert!(store.path_for(&artifact.reference.content_hash).is_file());
    assert!(
        fs::read_dir(directory.path())
            .expect("list artifacts")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with('.'))
    );
}

#[test]
fn recovery_removes_only_abandoned_recognized_staging_files() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().to_owned());
    fs::create_dir_all(directory.path()).unwrap();
    let abandoned = directory
        .path()
        .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let unrelated = directory.path().join("notes.tmp");
    fs::write(&abandoned, b"partial").unwrap();
    fs::write(&unrelated, b"keep").unwrap();

    let report = store.reconcile_partials().unwrap();

    assert_eq!(report.removed, 1);
    assert_eq!(report.retained_active, 0);
    assert_eq!(report.ignored, 1);
    assert!(!abandoned.exists());
    assert!(unrelated.exists());
    assert_eq!(store.reconcile_partials().unwrap().removed, 0);
}

#[test]
fn recovery_preserves_a_staging_file_owned_by_a_live_writer() {
    let directory = tempdir().expect("artifact tempdir");
    let store = ArtifactStore::new(directory.path().to_owned());
    fs::create_dir_all(directory.path()).unwrap();
    let active = directory
        .path()
        .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let writer = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&active)
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&writer).unwrap();

    let report = store.reconcile_partials().unwrap();

    assert_eq!(report.removed, 0);
    assert_eq!(report.retained_active, 1);
    assert!(active.exists());
    fs2::FileExt::unlock(&writer).unwrap();
}

#[test]
fn content_hash_deserialization_rejects_paths_and_uppercase() {
    for invalid in ["../bad", &"A".repeat(64), &"g".repeat(64)] {
        let json = serde_json::to_string(invalid).expect("hash json");
        assert!(serde_json::from_str::<ContentHash>(&json).is_err());
    }
}

#[test]
fn resource_publication_accepts_only_explicit_limits_below_the_ceiling() {
    let directory = tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().to_owned());

    let (artifact, _) = store
        .put_bounded(b"five", "application/octet-stream", PrincipalId::new(), 5)
        .unwrap();
    assert_eq!(artifact.byte_len, 4);
    assert!(matches!(
        store.put_bounded(b"five", "application/octet-stream", PrincipalId::new(), 3),
        Err(ArtifactError::TooLarge { .. })
    ));
    assert!(matches!(
        store.put_bounded(b"x", "application/octet-stream", PrincipalId::new(), 0),
        Err(ArtifactError::InvalidLimit { .. })
    ));
}

#[test]
fn file_publication_streams_content_and_reuses_verified_identity() {
    let directory = tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().join("store"));
    let source = directory.path().join("source.bin");
    let bytes = vec![0x5a; 128 * 1024 + 7];
    fs::write(&source, &bytes).unwrap();
    let owner = PrincipalId::new();

    let (first, created) = store
        .put_file_bounded(&source, "application/octet-stream", owner, bytes.len())
        .unwrap();
    let (second, created_again) = store
        .put_file_bounded(&source, "application/octet-stream", owner, bytes.len())
        .unwrap();

    assert!(created);
    assert!(!created_again);
    assert_eq!(first.byte_len, bytes.len() as u64);
    assert_eq!(first.reference.content_hash, second.reference.content_hash);
    assert_eq!(store.read_bounded(&first, bytes.len()).unwrap(), bytes,);
    assert!(matches!(
        store.put_file_bounded(&source, "application/octet-stream", owner, bytes.len() - 1,),
        Err(ArtifactError::TooLarge { .. })
    ));
}

#[test]
fn verified_ranges_are_bounded_and_hash_the_complete_artifact() {
    let directory = tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().to_owned());
    let bytes = b"0123456789";
    let (artifact, _) = store
        .put(bytes, "application/octet-stream", PrincipalId::new())
        .unwrap();

    let range = store
        .read_verified_range(&artifact, 3, 4, MAX_ARTIFACT_BYTES)
        .unwrap();
    assert_eq!(range.offset, 3);
    assert_eq!(range.total_byte_len, 10);
    assert_eq!(range.bytes, b"3456");
    assert!(range.truncated_after);
    assert!(matches!(
        store.read_verified_range(&artifact, 11, 1, MAX_ARTIFACT_BYTES),
        Err(ArtifactError::InvalidRange { .. })
    ));

    fs::write(
        store.path_for(&artifact.reference.content_hash),
        b"0123tamper",
    )
    .unwrap();
    assert!(matches!(
        store.read_verified_range(&artifact, 0, 2, MAX_ARTIFACT_BYTES),
        Err(ArtifactError::CorruptContent { .. })
    ));
}

#[cfg(unix)]
#[test]
fn verified_ranges_reject_symlinked_artifact_entries() {
    use std::os::unix::fs::symlink;

    let directory = tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().join("store"));
    let bytes = b"outside";
    let hash = ContentHash::for_bytes(bytes);
    fs::create_dir_all(&store.root).unwrap();
    let outside = directory.path().join("outside");
    fs::write(&outside, bytes).unwrap();
    symlink(&outside, store.path_for(&hash)).unwrap();
    let artifact = ArtifactRecord {
        reference: ArtifactRef {
            id: ArtifactId::new(),
            content_hash: hash,
        },
        media_type: "application/octet-stream".into(),
        byte_len: bytes.len() as u64,
        owner: PrincipalId::new(),
    };

    assert!(matches!(
        store.read_verified_range(&artifact, 0, 4, MAX_ARTIFACT_BYTES),
        Err(ArtifactError::NotRegular { .. })
    ));
}
