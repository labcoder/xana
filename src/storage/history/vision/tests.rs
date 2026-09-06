use super::*;
use crate::{
    artifact::ArtifactStore,
    identity::{OperationId, PrincipalId},
    session::DurableSession,
    storage::{RecoveryIdentity, TestCustody},
    vision::receipt::*,
};

fn fixture() -> (
    tempfile::TempDir,
    ProtectedStore,
    DurableSession,
    VisionReceipt,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let mut session =
        DurableSession::create_protected(store.clone(), directory.path().into(), SessionId::new())
            .unwrap();
    let artifacts = ArtifactStore::protected(store.clone());
    let sources = (0..2)
        .map(|index| {
            let (artifact, _) = artifacts
                .put(
                    format!("synthetic-image-{index}").as_bytes(),
                    "image/png",
                    PrincipalId::new(),
                )
                .unwrap();
            session
                .append_record(SessionRecord::ArtifactRegistered {
                    artifact: artifact.clone(),
                })
                .unwrap();
            VisionSource {
                artifact_id: artifact.reference.id.to_string(),
                digest: artifact.reference.content_hash.as_str().into(),
                media_type: artifact.media_type,
                byte_len: artifact.byte_len,
            }
        })
        .collect();
    let receipt = VisionReceipt {
        version: 1,
        conversation_id: session.session_id().to_string(),
        operation_id: OperationId::new().to_string(),
        revision: 1,
        plan_digest: "a".repeat(64),
        prompt_digest: "b".repeat(64),
        sources,
        destination: VisionDestination {
            route: Some("fixture".into()),
            connection: "fixture".into(),
            model: "fixture".into(),
            adapter: "openai.vision".into(),
            recipient: "http://127.0.0.1:1/v1/chat/completions".into(),
            recipient_digest: "c".repeat(64),
        },
        status: VisionStatus::Dispatching,
        usage: VisionUsage::default(),
        derivative: None,
        untrusted_derivative: true,
    };
    (directory, store, session, receipt)
}

#[test]
fn protected_vision_writer_and_exact_reader_enforce_immutable_attempts() {
    for variant in 0..5 {
        let (_root, store, mut session, start) = fixture();
        let operation = start.operation().unwrap();
        session
            .append_record(SessionRecord::VisionReceiptRecorded {
                receipt: start.clone(),
            })
            .unwrap();
        let mut invalid = start.clone();
        invalid.revision = 2;
        invalid.status = VisionStatus::Failed;
        match variant {
            0 => invalid.sources.swap(0, 1),
            1 => invalid.destination.recipient = "http://127.0.0.1:2/v1/chat/completions".into(),
            2 => invalid.plan_digest = "d".repeat(64),
            3 => invalid.status = VisionStatus::AnalysisReady,
            _ => invalid.sources[0].digest = "e".repeat(64),
        }
        assert!(
            session
                .append_record(SessionRecord::VisionReceiptRecorded { receipt: invalid })
                .is_err()
        );
        assert_eq!(
            store
                .history_vision_receipt(session.session_id(), operation)
                .unwrap(),
            Some(start)
        );
        store.verify_content().unwrap();
    }
}

#[test]
fn protected_vision_reopen_inspection_and_backup_verify_reject_changed_terminal_evidence() {
    let (_root, store, mut session, start) = fixture();
    let operation = start.operation().unwrap();
    let id = session.session_id();
    session
        .append_record(SessionRecord::VisionReceiptRecorded {
            receipt: start.clone(),
        })
        .unwrap();
    let mut end = start.clone();
    end.revision = 2;
    end.status = VisionStatus::Failed;
    session
        .append_record(SessionRecord::VisionReceiptRecorded {
            receipt: end.clone(),
        })
        .unwrap();
    assert_eq!(
        store.history_vision_receipt(id, operation).unwrap(),
        Some(end)
    );
    store.verify_content().unwrap();
    let mut record = store
        .history_records_for(id, HistorySubject::Vision(operation))
        .unwrap()
        .remove(1);
    let SessionRecord::VisionReceiptRecorded { receipt } = &mut record.record else {
        unreachable!()
    };
    receipt.sources.swap(0, 1);
    store
        .with_database(|db| {
            db.connection.execute(
                "UPDATE native_records SET body=?3 WHERE session=?1 AND id=?2",
                params![
                    id.to_string(),
                    record.record_id.to_string(),
                    serde_json::to_vec(&record)?
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(store.history_vision_receipt(id, operation).is_err());
    assert!(store.verify_content().is_err());
}

#[test]
fn protected_vision_never_claims_native_submission_without_durable_acceptance() {
    let (_root, _store, mut session, mut start) = fixture();
    start.destination.route = None;
    session
        .append_record(SessionRecord::VisionReceiptRecorded {
            receipt: start.clone(),
        })
        .unwrap();
    start.revision = 2;
    start.status = VisionStatus::NativeSubmitted;
    assert!(
        session
            .append_record(SessionRecord::VisionReceiptRecorded { receipt: start })
            .is_err()
    );
}

#[test]
fn protected_vision_provenance_survives_actual_encrypted_backup_and_restore() {
    let directory = tempfile::tempdir().unwrap();
    let paths =
        crate::paths::XanaPaths::resolve(Some(directory.path().join("home").into_os_string()))
            .unwrap();
    let identity = RecoveryIdentity::generate();
    let store =
        ProtectedStore::initialize(paths.data_dir(), &identity, &TestCustody::default()).unwrap();
    let mut session =
        DurableSession::create_protected(store.clone(), directory.path().into(), SessionId::new())
            .unwrap();
    let artifacts = ArtifactStore::protected(store.clone());
    let owner = session.artifact_owner();
    let mut registered = Vec::new();
    for (bytes, mime) in [
        (b"synthetic source".as_slice(), "image/png"),
        (b"untrusted derivative".as_slice(), "text/plain"),
    ] {
        let (artifact, _) = artifacts.put(bytes, mime, owner).unwrap();
        session
            .append_record(SessionRecord::ArtifactRegistered {
                artifact: artifact.clone(),
            })
            .unwrap();
        registered.push(VisionSource {
            artifact_id: artifact.reference.id.to_string(),
            digest: artifact.reference.content_hash.as_str().into(),
            media_type: artifact.media_type,
            byte_len: artifact.byte_len,
        });
    }
    let receipt = VisionReceipt {
        version: 1,
        conversation_id: session.session_id().to_string(),
        operation_id: OperationId::new().to_string(),
        revision: 1,
        plan_digest: "a".repeat(64),
        prompt_digest: "b".repeat(64),
        sources: vec![registered[0].clone()],
        destination: VisionDestination {
            route: Some("fixture".into()),
            connection: "fixture".into(),
            model: "fixture".into(),
            adapter: "openai.vision".into(),
            recipient: "http://127.0.0.1:1/v1/chat/completions".into(),
            recipient_digest: "c".repeat(64),
        },
        status: VisionStatus::Dispatching,
        usage: VisionUsage::default(),
        derivative: None,
        untrusted_derivative: true,
    };
    session
        .append_record(SessionRecord::VisionReceiptRecorded {
            receipt: receipt.clone(),
        })
        .unwrap();
    let mut terminal = receipt.clone();
    terminal.revision = 2;
    terminal.status = VisionStatus::AnalysisReady;
    terminal.derivative = Some(registered[1].clone());
    terminal.usage.input_tokens = Some(12);
    session
        .append_record(SessionRecord::VisionReceiptRecorded {
            receipt: terminal.clone(),
        })
        .unwrap();
    let snapshot = store
        .backup(
            &crate::storage::backup::BackupPolicy {
                directory: Some(directory.path().join("backups")),
                ..Default::default()
            },
            1000,
            false,
        )
        .unwrap()
        .snapshot
        .unwrap();
    let destination =
        crate::paths::XanaPaths::resolve(Some(directory.path().join("restored").into_os_string()))
            .unwrap();
    let preview = crate::storage::restore::preview(&destination, &snapshot, &identity).unwrap();
    crate::storage::restore::apply(&destination, &snapshot, &identity, &preview.review).unwrap();
    let restored = ProtectedStore::open_recovery(destination.data_dir(), &identity, false).unwrap();
    assert_eq!(
        restored
            .history_vision_receipt(session.session_id(), receipt.operation().unwrap())
            .unwrap(),
        Some(terminal)
    );
    restored.verify_content().unwrap();
}
