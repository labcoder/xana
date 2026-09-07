use super::*;
use crate::{
    artifact::ArtifactStore,
    message::{ContentBlock, ToolResult},
    vision::ImageRef,
};

fn image(store: &ProtectedStore, session: &DurableSession) -> ImageRef {
    // Verification authenticates stored bytes and references, not pixel decoding.
    let (artifact, _) = ArtifactStore::protected(store.clone())
        .put(
            b"image fixture bytes",
            "image/png",
            session.artifact_owner(),
        )
        .unwrap();
    ImageRef {
        media_type: artifact.media_type.clone(),
        byte_len: artifact.byte_len,
        artifact,
        width: Some(1),
        height: Some(1),
    }
}

#[test]
fn inline_images_verify_content_without_a_separate_registration() {
    for damage in [
        "none",
        "missing hash",
        "wrong length",
        "image length",
        "image type",
        "corrupt bytes",
    ] {
        let (_directory, store, mut session) = fixture();
        let mut image = image(&store, &session);
        match damage {
            "missing hash" => {
                image.artifact.reference.content_hash = ContentHash::for_bytes(b"absent")
            }
            "wrong length" => {
                image.artifact.byte_len += 1;
                image.byte_len += 1;
            }
            "image length" => image.byte_len += 1,
            "image type" => image.media_type = "image/jpeg".into(),
            "corrupt bytes" => {
                let (_, file, _) = &store.object_inventory().unwrap()[0];
                std::fs::write(
                    store.inner.root.join("objects").join(format!("{file}.age")),
                    b"corrupt",
                )
                .unwrap();
            }
            _ => (),
        }
        session
            .append_message(Message {
                role: Role::User,
                content: vec![ContentBlock::Image(image)],
            })
            .unwrap();
        let id = session.session_id();
        drop(session);
        let result = store.verify_content();
        if damage == "none" {
            result.unwrap();
            let (mut resumed, _) = DurableSession::resume_protected(store.clone(), id).unwrap();
            resumed
                .append_message(Message::text(Role::User, "continue after reopening"))
                .unwrap();
            drop(resumed);
            store.verify_content().unwrap();
        } else {
            assert!(result.is_err(), "accepted {damage}");
        }
    }
}

#[test]
fn tool_evidence_still_needs_exact_preceding_registration() {
    for registration in ["missing", "mismatched", "valid"] {
        let (_directory, store, mut session) = fixture();
        let artifact = image(&store, &session).artifact;
        if registration != "missing" {
            session
                .append_record(SessionRecord::ArtifactRegistered {
                    artifact: artifact.clone(),
                })
                .unwrap();
        }
        let mut reference = artifact;
        if registration == "mismatched" {
            reference.owner = crate::identity::PrincipalId::new();
        }
        let mut result = ToolResult::success("fixture", "evidence");
        result.artifact = Some(Box::new(reference));
        let id = session.session_id();
        let entry = crate::session::ConversationEntry {
            id: ConversationEntryId::new(),
            parent: None,
            agent_id: session.agent_id(),
            message: Message::tool_result(result),
        };
        // Insert original bytes below the reducer to exercise independent
        // verification of a syntactically valid but invalid imported history.
        drop(session);
        store
            .append_history(
                id,
                store.history_metadata(id).unwrap().revision,
                &RecordEnvelope::new(id, SessionRecord::ConversationEntryAppended { entry }),
            )
            .unwrap();
        let result = store.verify_content();
        match registration {
            "valid" => result.unwrap(),
            "missing" => assert!(
                format!("{:#}", result.unwrap_err()).contains("has no registration before record")
            ),
            _ => assert!(
                format!("{:#}", result.unwrap_err())
                    .contains("attachment differs from its registered artifact")
            ),
        }
    }
}
