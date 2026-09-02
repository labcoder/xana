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
