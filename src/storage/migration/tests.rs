use super::*;
use crate::{
    config::PermissionMode,
    config::{InitialConfig, InitialConnection},
    message::{Message, Role},
    session::DurableSession,
    shell::ShellConfig,
    storage::TestCustody,
};

fn fixture() -> (tempfile::TempDir, XanaPaths, crate::identity::SessionId) {
    let directory = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
    let config = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Ollama {
            name: "local".into(),
            base_url: "http://localhost:11434/v1".into(),
        },
        model: "fixture".into(),
        max_tool_rounds: 8,
        shell: ShellConfig::default(),
        permission_mode: PermissionMode::Ask,
        reasoning_effort: None,
    })
    .unwrap()
    .replacen("version = 5", "# retained comment\nversion = 4", 1);
    fs::write(paths.config_file(), config).unwrap();
    let workspace = directory.path().join("ordinary");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("notes.txt"), b"ordinary source").unwrap();
    let mut session = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
    session
        .append_message(Message::text(Role::User, "private migration canary"))
        .unwrap();
    let id = session.session_id();
    drop(session);
    let mut managed = crate::managed::thread_store::ManagedThreadStore::open(
        paths.data_dir(),
        "codex",
        &workspace,
    )
    .unwrap();
    managed
        .set_thread(
            Some(crate::identity::ConversationId::new()),
            Some("thr_preserved".into()),
            Some("xana-identity-v1"),
        )
        .unwrap();
    (directory, paths, id)
}

#[test]
fn managed_migration_preserves_inline_image_artifacts() {
    let (_directory, paths, id) = fixture();
    let (mut session, _) = DurableSession::resume(paths.data_dir(), id).unwrap();
    let artifacts = crate::artifact::ArtifactStore::open(paths.data_dir()).unwrap();
    let (artifact, _) = artifacts
        .put(b"image evidence", "image/png", session.artifact_owner())
        .unwrap();
    session
        .append_message(Message {
            role: Role::User,
            content: vec![crate::message::ContentBlock::Image(
                crate::vision::ImageRef {
                    media_type: artifact.media_type.clone(),
                    byte_len: artifact.byte_len,
                    artifact,
                    width: Some(1),
                    height: Some(1),
                },
            )],
        })
        .unwrap();
    drop(session);
    let original = SessionStore::inspect(
        &paths
            .data_dir()
            .join("sessions")
            .join(format!("{id}.jsonl")),
    )
    .unwrap();
    let review = preview(&paths).unwrap().review;
    let custody = TestCustody::default();
    // Revisit a fully imported but unactivated generation with the same key,
    // just as setup must do after a verification failure. No history rewrite.
    let key = RecoveryIdentity::generate();
    let error = apply_inner(&paths, &key, &custody, &review, true, |stage| {
        if stage == "verified" {
            anyhow::bail!("fixture: stop before activation");
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "fixture: stop before activation");
    let retained = resume_managed(&paths, &custody).unwrap();
    let store = ProtectedStore::open(paths.data_dir(), &custody).unwrap();
    assert_eq!(
        SessionStore::inspect_protected(&store, id).unwrap().records,
        original.records
    );
    assert_eq!(
        SessionStore::inspect(&retained.join("sessions").join(format!("{id}.jsonl")))
            .unwrap()
            .records,
        original.records
    );
    assert_eq!(
        store.managed_recovery_identity().unwrap().to_public(),
        key.to_public()
    );
    store.verify_content().unwrap();
}

#[test]
fn managed_migration_accepts_created_but_empty_conversations() {
    let (_directory, paths, _) = fixture();
    let empty = DurableSession::create(paths.data_dir(), PathBuf::from("empty-workspace")).unwrap();
    let empty_id = empty.session_id();
    drop(empty);
    let custody = TestCustody::default();
    let review = preview(&paths).unwrap().review;
    let retained = apply_managed(&paths, &custody, &review)
        .unwrap_or_else(|error| panic!("migration of an empty Conversation failed: {error:#}"));
    let store = ProtectedStore::open(paths.data_dir(), &custody).unwrap();
    assert_eq!(
        SessionStore::inspect_protected(&store, empty_id)
            .unwrap()
            .records
            .len(),
        1
    );
    assert!(retained.is_dir());
}

#[test]
fn reviewed_migration_preserves_ids_and_requires_current_inventory() {
    let (_directory, paths, id) = fixture();
    let plan = preview(&paths).unwrap();
    fs::write(paths.data_dir().join("new-derived-copy"), b"keep this also").unwrap();
    let key = RecoveryIdentity::generate();
    assert!(apply(&paths, &key, &TestCustody::default(), &plan.review).is_err());
    assert!(!journal_path(paths.data_dir()).exists());
    let plan = preview(&paths).unwrap();
    let custody = TestCustody::default();
    let retained = apply(&paths, &key, &custody, &plan.review).unwrap();
    assert!(retained.join("sessions").is_dir());
    assert!(retained.join("new-derived-copy").is_file());
    assert!(!paths.data_dir().join("new-derived-copy").exists());
    let store = ProtectedStore::open(paths.data_dir(), &custody).unwrap();
    let archived = archive::list(&store, None).unwrap();
    assert_eq!(archived.len(), 1);
    let exported = paths
        .config_file()
        .parent()
        .unwrap()
        .join("explicit-derived-export");
    archive::export(&store, &archived[0].id, &exported).unwrap();
    assert_eq!(fs::read(&exported).unwrap(), b"keep this also");
    assert!(archive::export(&store, &archived[0].id, &exported).is_err());
    let loaded = SessionStore::inspect_protected(&store, id).unwrap();
    assert!(
        String::from_utf8(serde_json::to_vec(&loaded.records).unwrap())
            .unwrap()
            .contains("private migration canary")
    );
    let managed = store.document_names("managed-threads/", 10).unwrap();
    assert_eq!(managed.len(), 1);
    assert!(
        String::from_utf8(store.document(&managed[0], 8192).unwrap().unwrap())
            .unwrap()
            .contains("thr_preserved")
    );
    assert_eq!(
        fs::read(
            paths
                .config_file()
                .parent()
                .unwrap()
                .join("ordinary/notes.txt")
        )
        .unwrap(),
        b"ordinary source"
    );
    assert!(
        fs::read_to_string(paths.config_file())
            .unwrap()
            .contains("# retained comment\nversion = 5")
    );
}

#[test]
fn managed_migration_resumes_at_every_boundary_with_original_os_custody() {
    for point in [
        "journal",
        "config-fence",
        "source-renamed",
        "source-fenced",
        "imported-file",
        "verified",
        "before-activation",
        "activated",
    ] {
        let (directory, paths, id) = fixture();
        let review = preview(&paths).unwrap().review;
        let custody = TestCustody::default();
        let key = RecoveryIdentity::generate();
        assert!(
            apply_inner(&paths, &key, &custody, &review, true, |stage| {
                if stage == point {
                    anyhow::bail!("injected interruption");
                }
                Ok(())
            })
            .is_err(),
            "{point}"
        );
        let retained = resume_managed(&paths, &custody).unwrap();
        assert!(retained.is_dir());
        let store = ProtectedStore::open(paths.data_dir(), &custody).unwrap();
        assert!(SessionStore::inspect_protected(&store, id).is_ok());
        assert_eq!(
            super::super::recovery::status(paths.data_dir()).unwrap(),
            super::super::recovery::RecoveryStatus::Pending
        );
        let export = directory.path().join("auto-recovery.key");
        store.export_recovery(&export).unwrap();
        let recovered = super::super::read_recovery_identity(&export).unwrap();
        assert_eq!(recovered.to_public(), key.to_public());
    }
}

#[test]
fn every_activation_boundary_recovers_without_os_custody_or_replaying_history() {
    for point in [
        "journal",
        "config-fence",
        "source-renamed",
        "source-fenced",
        "imported-file",
        "verified",
        "before-activation",
        "activated",
    ] {
        let (_directory, paths, id) = fixture();
        let review = preview(&paths).unwrap().review;
        let key = RecoveryIdentity::generate();
        let failed = apply_with(&paths, &key, &TestCustody::default(), &review, |stage| {
            if stage == point {
                anyhow::bail!("injected interruption")
            }
            Ok(())
        });
        assert!(failed.is_err(), "{point}");
        assert!(
            ProtectedStore::status(paths.data_dir()).is_err(),
            "a pending transition must fence even a completed publication: {point}"
        );
        let source =
            resume(&paths, &key).unwrap_or_else(|error| panic!("resume {point}: {error:#}"));
        assert!(source.is_dir());
        let recovered = ProtectedStore::recover(paths.data_dir(), &key).unwrap();
        let loaded = SessionStore::inspect_protected(&recovered, id).unwrap();
        let original =
            SessionStore::inspect(&source.join("sessions").join(format!("{id}.jsonl"))).unwrap();
        assert_eq!(
            loaded.records, original.records,
            "no duplicate append at {point}"
        );
        recovered.verify_content().unwrap();
    }
}

#[test]
fn live_old_writer_and_wrong_recovery_cannot_activate_a_generation() {
    let (_directory, paths, id) = fixture();
    let plan = preview(&paths).unwrap();
    let (writer, _) = DurableSession::resume(paths.data_dir(), id).unwrap();
    assert!(
        apply(
            &paths,
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
            &plan.review
        )
        .is_err()
    );
    assert!(!journal_path(paths.data_dir()).exists());
    drop(writer);
    let key = RecoveryIdentity::generate();
    let _ = apply_with(
        &paths,
        &key,
        &TestCustody::default(),
        &plan.review,
        |stage| {
            if stage == "journal" {
                anyhow::bail!("interrupted")
            }
            Ok(())
        },
    );
    assert!(resume(&paths, &RecoveryIdentity::generate()).is_err());
    resume(&paths, &key).unwrap();
}

#[test]
fn killed_migration_process_recovers_after_rename_and_publication() {
    use age::secrecy::ExposeSecret;
    for point in ["source-renamed", "verified", "activated"] {
        let (_directory, paths, id) = fixture();
        let recovery = paths.config_file().parent().unwrap().join("recovery.key");
        let key = RecoveryIdentity::generate();
        fs::write(&recovery, key.to_string().expose_secret()).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "storage::migration::tests::migration_crash_worker",
            ])
            .env(
                "XANA_TEST_MIGRATION_HOME",
                paths.config_file().parent().unwrap(),
            )
            .env("XANA_TEST_MIGRATION_POINT", point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(47),
            "child exited without unwinding at {point}"
        );
        assert!(ProtectedStore::status(paths.data_dir()).is_err());
        let legacy =
            resume(&paths, &key).unwrap_or_else(|error| panic!("killed at {point}: {error:#}"));
        let store = ProtectedStore::recover(paths.data_dir(), &key).unwrap();
        let actual = SessionStore::inspect_protected(&store, id).unwrap();
        let original =
            SessionStore::inspect(&legacy.join("sessions").join(format!("{id}.jsonl"))).unwrap();
        assert_eq!(actual.records, original.records);
        store.verify_content().unwrap();
    }
}

#[test]
#[ignore = "subprocess-only abrupt-exit fixture, exercised by the parent test"]
fn migration_crash_worker() {
    let root = std::env::var_os("XANA_TEST_MIGRATION_HOME").expect("parent fixture home");
    let paths = XanaPaths::resolve(Some(root)).unwrap();
    let key = super::super::read_recovery_identity(
        &paths.config_file().parent().unwrap().join("recovery.key"),
    )
    .unwrap();
    let point = std::env::var("XANA_TEST_MIGRATION_POINT").unwrap();
    let review = preview(&paths).unwrap().review;
    apply_with(&paths, &key, &TestCustody::default(), &review, |stage| {
        if stage == point {
            std::process::exit(47);
        }
        Ok(())
    })
    .unwrap();
    panic!("crash stage was not reached");
}
