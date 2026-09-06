use super::*;

fn fixture() -> (SessionId, VisionReceipt) {
    let session = SessionId::new();
    (
        session,
        VisionReceipt {
            version: 1,
            conversation_id: session.to_string(),
            operation_id: OperationId::new().to_string(),
            revision: 1,
            plan_digest: "a".repeat(64),
            prompt_digest: "b".repeat(64),
            destination: VisionDestination {
                route: Some("vision".into()),
                connection: "fixture".into(),
                model: "fixture-model".into(),
                adapter: "openai.vision".into(),
                recipient: "http://127.0.0.1:1/v1/chat/completions".into(),
                recipient_digest: "c".repeat(64),
            },
            sources: (0..2)
                .map(|_| VisionSource {
                    artifact_id: uuid::Uuid::new_v4().to_string(),
                    digest: "d".repeat(64),
                    media_type: "image/png".into(),
                    byte_len: 32,
                })
                .collect(),
            status: VisionStatus::Dispatching,
            usage: VisionUsage::default(),
            derivative: None,
            untrusted_derivative: true,
        },
    )
}

#[test]
fn vision_receipt_chain_binds_every_approved_identity_and_refuses_replay() {
    let (session, start) = fixture();
    assert!(valid_transition(None, &start, session));
    let mut end = start.clone();
    end.revision = 2;
    end.status = VisionStatus::Failed;
    assert!(valid_transition(Some(&start), &end, session));
    assert!(!valid_transition(None, &end, session));
    assert!(!valid_transition(Some(&end), &end, session));
    assert!(!valid_transition(Some(&start), &start, session));
    assert!(!valid_transition(Some(&start), &end, SessionId::new()));
    for change in 0..8 {
        let mut changed = end.clone();
        match change {
            0 => changed.sources.swap(0, 1),
            1 => changed.sources[0].digest = "e".repeat(64),
            2 => changed.destination.model = "other-model".into(),
            3 => changed.destination.recipient = "http://127.0.0.1:2/v1/chat/completions".into(),
            4 => changed.plan_digest = "e".repeat(64),
            5 => changed.prompt_digest = "e".repeat(64),
            6 => changed.destination.route = Some("other-route".into()),
            _ => changed.operation_id = OperationId::new().to_string(),
        }
        assert!(
            !valid_transition(Some(&start), &changed, session),
            "changed binding {change}"
        );
    }
}

#[test]
fn vision_receipts_do_not_invent_results_usage_or_persist_unknown_as_fact() {
    let (session, start) = fixture();
    let mut end = start.clone();
    end.revision = 2;
    end.status = VisionStatus::AnalysisReady;
    assert!(!valid_transition(Some(&start), &end, session));
    end.derivative = Some(VisionSource {
        artifact_id: uuid::Uuid::new_v4().to_string(),
        digest: "e".repeat(64),
        media_type: "text/plain".into(),
        byte_len: 30,
    });
    assert!(valid_transition(Some(&start), &end, session));
    end.status = VisionStatus::NativeSubmitted;
    assert!(!end.valid_for(session));
    end.status = VisionStatus::Failed;
    assert!(!end.valid_for(session));
    end.derivative = None;
    end.status = VisionStatus::Unknown;
    assert!(!end.valid_for(session));
    end.status = VisionStatus::Cancelled;
    end.usage.input_tokens = Some(0);
    assert!(!end.valid_for(session));
    let mut duplicate = start.clone();
    duplicate.sources[1] = duplicate.sources[0].clone();
    assert!(!duplicate.valid_for(session));
    let mut wrong_kind = start.clone();
    wrong_kind.sources[0].media_type = "text/plain".into();
    assert!(!wrong_kind.valid_for(session));
}
