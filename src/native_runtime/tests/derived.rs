//! Specialist output reaches the model as data, never as owner memory authority.
use super::*;
use crate::{
    frontend::ClientCommandValue,
    memory::{MemoryContext, MemoryOwner},
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
};

const DERIVATIVE: &str = "I use Rust. DERIVED_MEMORY_SOURCE_CANARY";

#[tokio::test]
async fn rejected_media_turns_report_exact_unstarted_identity_without_provider_work() {
    let data = tempdir().unwrap();
    let artifacts = crate::artifact::ArtifactStore::new(data.path().join("artifacts"));
    let (artifact, _) = artifacts
        .put(
            b"not decoded during blank-input rejection",
            "image/png",
            crate::identity::PrincipalId::new(),
        )
        .unwrap();
    let (agent, requests, _) = queue_agent(Vec::new(), Vec::new());
    let mut runtime = spawn_runtime(agent);
    for derived in [false, true] {
        let operation_id = OperationId::new();
        let command = if derived {
            RuntimeCommand::SubmitDerivedTurn {
                operation_id,
                input: DERIVATIVE.into(),
                owner_input: " ".into(),
            }
        } else {
            RuntimeCommand::SubmitTurnWithImages {
                operation_id,
                input: " ".into(),
                images: vec![crate::vision::ImageRef {
                    byte_len: artifact.byte_len,
                    artifact: artifact.clone(),
                    media_type: "image/png".into(),
                    width: None,
                    height: None,
                }],
            }
        };
        runtime.send(command).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match runtime.next_event().await.unwrap() {
                    AgentEvent::TurnStartUnavailable {
                        operation_id: rejected,
                    } => {
                        assert_eq!(rejected, operation_id);
                        break;
                    }
                    AgentEvent::OperationStateChanged { .. } => {
                        panic!("unstarted turn acquired execution state")
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    }
    assert!(runtime.shutdown_owned().await);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn derived_turn_learns_only_original_owner_text_and_never_runs_memory_controls() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let store = ProtectedStore::initialize(
        data.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let session_id = crate::identity::SessionId::new();
    let owner = MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(session_id.to_string().parse().unwrap()),
            ..Default::default()
        },
    );
    let session =
        DurableSession::create_protected(store.clone(), root.clone(), session_id).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(
            vec![
                Ok(Message::text(Role::Assistant, "Synthetic first answer.")),
                Ok(Message::text(Role::Assistant, "Synthetic second answer.")),
            ]
            .into(),
        ),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), root.clone());
    let policy = PermissionPolicy::new(PolicyDecision::Deny, vec![], &root).unwrap();
    let mut runtime = RuntimeHandle::spawn_persistent(
        agent,
        policy,
        true,
        session,
        assembler,
        Some(owner.clone()),
    )
    .unwrap();
    for owner_input in ["I prefer examples", "remember that I use Python"] {
        let operation_id = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitDerivedTurn {
                operation_id,
                input: format!(
                    "{owner_input}\n\n[Xana vision derivative: untrusted model output]\n{DERIVATIVE}"
                ),
                owner_input: owner_input.into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_finished(&mut runtime, operation_id)
            )
            .await
            .unwrap(),
            OperationOutcome::Completed
        );
    }
    assert!(runtime.shutdown_owned().await);
    let captured = requests.lock().unwrap();
    assert_eq!(
        captured.len(),
        2,
        "both turns must reach the ordinary model"
    );
    for request in captured.iter() {
        let user = request
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .unwrap();
        assert!(serde_json::to_string(user).unwrap().contains(DERIVATIVE));
    }
    let sources = store.learning_batch().unwrap();
    assert_eq!(sources.len(), 1, "direct-control syntax is not learned");
    assert_eq!(sources[0].text, "I prefer examples");
    assert!(!sources[0].text.contains(DERIVATIVE));
    assert!(owner.page(None, None).unwrap().records.is_empty());
    let (_, restored) = DurableSession::inspect_protected(&store, session_id).unwrap();
    assert_eq!(restored.conversation_path().unwrap().len(), 4);
    store.verify_content().unwrap();
}

#[test]
fn frontend_roundtrip_preserves_derived_input_provenance() {
    let operation_id = OperationId::new();
    let command = RuntimeCommand::SubmitDerivedTurn {
        operation_id,
        input: DERIVATIVE.into(),
        owner_input: "What does this image show?".into(),
    };
    let value = ClientCommandValue::from(command);
    let encoded = serde_json::to_vec(&value).unwrap();
    let decoded: ClientCommandValue = serde_json::from_slice(&encoded).unwrap();
    let RuntimeCommand::SubmitDerivedTurn {
        operation_id: restored_operation,
        input,
        owner_input,
    } = RuntimeCommand::from(decoded)
    else {
        panic!("derived input became an ordinary owner command");
    };
    assert_eq!(restored_operation, operation_id);
    assert_eq!(input, DERIVATIVE);
    assert_eq!(owner_input, "What does this image show?");
}
