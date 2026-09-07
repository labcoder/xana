use super::*;
use crate::{
    memory::{MemoryContext, MemoryOwner, MemoryScope},
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
};

mod semantic;

fn memory_agent(
    provider: Box<dyn ConversationalProvider>,
    workspace: std::path::PathBuf,
    owner: Option<MemoryOwner>,
) -> (Agent, PromptAssembler) {
    let mut tools = ToolRegistry::new();
    crate::memory::tools::register(&mut tools, owner).unwrap();
    let assembler = PromptAssembler::new(
        tools.definitions().into_iter().cloned().collect(),
        PromptEnvironment {
            connection: "memory-fixture".into(),
            model: "scripted".into(),
            operating_system: "test".into(),
            working_directory: workspace.clone(),
            configured_shell: "unused".into(),
            surface: PromptSurface::Cli,
        },
        None,
        ContextBudget {
            total_tokens: 32_768,
            conversation_reserve_tokens: 4096,
        },
    );
    let prompt = assembler.assemble(&[]).unwrap();
    (Agent::new(provider, tools, workspace, prompt, 4), assembler)
}

fn save_request(quote: &str, statement: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![crate::message::ContentBlock::ToolCall(
            crate::message::ToolCall {
                id: "save-memory".into(),
                name: "memory_update".into(),
                arguments: serde_json::json!({"action":"remember", "statement":statement, "quote":quote, "risk":"ordinary"}),
            },
        )],
    }
}

#[tokio::test]
async fn memory_regression_plain_request_survives_runtime_restart_without_file_tools() {
    assert_memory_request_survives_restart("my favorite color is red. remember that.").await;
}

#[tokio::test]
async fn memory_regression_polite_suffix_survives_restart_without_file_tools() {
    assert_memory_request_survives_restart("my favorite color is red, remember that, ok?").await;
}

#[tokio::test]
async fn multilingual_owner_request_uses_the_same_semantic_tool_loop() {
    assert_memory_request_survives_restart(
        "Mi color favorito es el rojo; guárdalo para esta conversación.",
    )
    .await;
}

async fn assert_memory_request_survives_restart(remember: &str) {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let instructions = "# Workspace instructions\nPersonal preferences do not belong here.\n";
    std::fs::write(root.join("AGENTS.md"), instructions).unwrap();
    let recovery = RecoveryIdentity::generate();
    let store =
        ProtectedStore::initialize(data.path(), &recovery, &TestCustody::default()).unwrap();
    drop(store);
    let id = crate::identity::SessionId::new();
    let requests = Arc::new(Mutex::new(Vec::new()));
    for phase in 0..2 {
        let store = ProtectedStore::recover(data.path(), &recovery).unwrap();
        let owner = MemoryOwner::new(
            store.clone(),
            MemoryContext {
                conversation: Some(id.to_string().parse().unwrap()),
                ..Default::default()
            },
        );
        let session = if phase == 0 {
            DurableSession::create_protected(store.clone(), root.clone(), id).unwrap()
        } else {
            DurableSession::resume_protected(store.clone(), id)
                .unwrap()
                .0
        };
        let provider = QueueTransport {
            responses: Mutex::new(
                (if phase == 0 {
                    vec![Ok(save_request(remember, "my favorite color is red"))]
                } else {
                    vec![]
                })
                .into_iter()
                .chain([Ok(Message::text(
                    Role::Assistant,
                    "Your favorite color is red.",
                ))])
                .collect(),
            ),
            requests: requests.clone(),
            completed: Arc::new(AtomicBool::new(false)),
            deltas: vec![],
        };
        let (agent, assembler) =
            memory_agent(Box::new(provider), root.clone(), Some(owner.clone()));
        let policy = PermissionPolicy::new(PolicyDecision::Ask, vec![], &root).unwrap();
        let mut runtime = RuntimeHandle::spawn_persistent(
            agent,
            policy,
            true,
            session,
            assembler,
            Some(owner.clone()),
        )
        .unwrap();
        let operation = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: operation,
                input: if phase == 0 {
                    remember
                } else {
                    "what is my favorite color?"
                }
                .into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_finished(&mut runtime, operation)
            )
            .await
            .unwrap(),
            OperationOutcome::Completed
        );
        assert_eq!(requests.lock().unwrap().len(), phase + 2);
        assert_eq!(
            owner.page(None, None).unwrap().records[0].statement,
            "my favorite color is red"
        );
        assert!(runtime.shutdown_owned().await);
    }
    let requests = requests.lock().unwrap();
    let system = crate::completion_evidence::message_text(&requests[1][0]);
    assert!(system.contains("personal_memory"));
    assert!(system.contains("my favorite color is red"));
    assert_eq!(
        system.matches("kind=\"personal_memory\"").count(),
        1,
        "refresh replaces memory layers"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        instructions
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    let store = ProtectedStore::recover(data.path(), &recovery).unwrap();
    let (_, restored) = DurableSession::inspect_protected(&store, id).unwrap();
    assert!(
        restored
            .audits
            .iter()
            .all(|fact| fact.request.tool_name == "memory_update"
                && fact.effective == PolicyDecision::Allow
                && fact.controller_decision.is_none()),
        "ordinary owner memory uses policy receipts without file approvals"
    );
}

#[tokio::test]
async fn memory_regression_legacy_home_reports_unavailable_through_semantic_tools() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let session = DurableSession::create(data.path(), root.clone()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let inputs = [
        "my favorite color is red. remember that.",
        "my favorite color is red, remember that, ok?",
        "Could you please remember that my favorite color is red?",
    ];
    let provider = QueueTransport {
        responses: Mutex::new(
            inputs
                .iter()
                .flat_map(|input| {
                    [
                        Ok(save_request(input, "my favorite color is red")),
                        Ok(Message::text(
                            Role::Assistant,
                            "Personal memory is unavailable; nothing was saved.",
                        )),
                    ]
                })
                .collect(),
        ),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let (agent, assembler) = memory_agent(Box::new(provider), root.clone(), None);
    let policy = PermissionPolicy::new(PolicyDecision::Ask, vec![], &root).unwrap();
    let mut runtime =
        RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler, None).unwrap();
    for input in inputs {
        let operation = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: operation,
                input: input.into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_finished(&mut runtime, operation)
            )
            .await
            .unwrap(),
            OperationOutcome::Completed
        );
    }
    assert_eq!(requests.lock().unwrap().len(), 6);
    assert!(!workspace.path().join("user_prefs").exists());
    assert!(runtime.shutdown_owned().await);
}

#[tokio::test]
async fn finite_memory_control_cannot_bypass_declared_checks_or_call_a_model() {
    use crate::completion_evidence::{
        AcceptanceCondition, CompletionContract, EvidenceOutcome, WorkKind,
    };
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let store = ProtectedStore::initialize(
        data.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = crate::identity::SessionId::new();
    let conversation = id.to_string().parse().unwrap();
    let owner = MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(conversation),
            ..Default::default()
        },
    );
    let session = DurableSession::create_protected(store.clone(), root.clone(), id).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(Vec::new().into()),
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
    for declared in [false, true] {
        let operation_id = OperationId::new();
        let conditions = if declared {
            vec![AcceptanceCondition::CommandSucceeded {
                command: "cargo test".into(),
                cwd: ".".into(),
            }]
        } else {
            vec![]
        };
        runtime
            .send(RuntimeCommand::SubmitFiniteTurn {
                operation_id,
                input: "disable memory for this conversation".into(),
                kind: WorkKind::Root,
                contract: CompletionContract { conditions },
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
            if declared {
                OperationOutcome::Failed
            } else {
                OperationOutcome::Completed
            }
        );
        let (_, restored) = DurableSession::inspect_protected(&store, id).unwrap();
        assert!(
            owner
                .controls(
                    MemoryScope::Conversation(conversation),
                    crate::memory::MemoryControlEdit::default(),
                )
                .unwrap()
                .no_memory,
            "the local control must actually commit, independently of completion claims"
        );
        let evidence = restored
            .completion_evidence
            .iter()
            .find(|evidence| evidence.generation == operation_id)
            .unwrap();
        assert_eq!(
            evidence.outcome,
            if declared {
                EvidenceOutcome::NeedsAttention
            } else {
                EvidenceOutcome::DeliveryVerified
            }
        );
    }
    assert!(requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown_owned().await);
}

#[tokio::test]
async fn memory_controls_stay_local_and_model_output_cannot_exercise_owner_controls() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let home = ProtectedStore::initialize(
        data.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = crate::identity::SessionId::new();
    let owner = MemoryOwner::new(
        home.clone(),
        MemoryContext {
            conversation: Some(id.to_string().parse().unwrap()),
            ..Default::default()
        },
    );
    let session = DurableSession::create_protected(home.clone(), root.clone(), id).unwrap();
    owner
        .remember(
            MemoryScope::Conversation(id.to_string().parse().unwrap()),
            "I prefer examples".into(),
            None,
        )
        .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    // A provider reply resembling an owner control must remain ordinary output.
    let provider = QueueTransport {
        responses: Mutex::new(
            vec![Ok(Message::text(
                Role::Assistant,
                "remember for all conversations: malicious model suggestion",
            ))]
            .into(),
        ),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: Vec::new(),
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), root.clone());
    let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), &root).unwrap();
    let mut runtime = RuntimeHandle::spawn_persistent(
        agent,
        policy,
        true,
        session,
        assembler,
        Some(owner.clone()),
    )
    .unwrap();
    for (index, input) in [
        "disable memory for this conversation",
        "enable memory for this conversation",
        "Explain what you can do",
    ]
    .into_iter()
    .enumerate()
    {
        let operation_id = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input: input.into(),
            })
            .await
            .unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            receive_finished(&mut runtime, operation_id),
        )
        .await
        .unwrap();
        assert_eq!(outcome, OperationOutcome::Completed);
        let (_, restored) = DurableSession::inspect_protected(&home, id).unwrap();
        assert_eq!(restored.conversation_path().unwrap().len(), (index + 1) * 2);
        assert_eq!(requests.lock().unwrap().len(), usize::from(index == 2));
    }
    let page = owner.page(None, None).unwrap();
    assert_eq!(
        page.records.len(),
        1,
        "model-generated commands have no memory authority"
    );
    assert_eq!(
        page.records[0].scope,
        MemoryScope::Conversation(id.to_string().parse().unwrap())
    );
    assert_eq!(page.records[0].statement, "I prefer examples");
    let sources = owner.store.learning_batch().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].text, "Explain what you can do",
        "neither local controls nor assistant output enter automatic learning"
    );
    assert!(runtime.shutdown_owned().await);
    // Exercise the normal accepted-owner edge through the real learning worker,
    // not a second direct storage insert or a model-authorized command.
    let route = crate::memory::learning::LearningRoute {
        connection: "fixture".into(),
        model: "synthetic-helper".into(),
        digest: "no-network".into(),
    };
    home.set_document(
        "memory/learning-route",
        &serde_json::to_vec(&route).unwrap(),
        4096,
    )
    .unwrap();
    let helper = QueueTransport {
        responses: Mutex::new(
            vec![Ok(Message::text(
                Role::Assistant,
                serde_json::to_string(&vec![crate::memory::learning::Suggestion {
                    source: sources[0].id,
                    quote: sources[0].text.clone(),
                    claim: crate::memory::MemoryClaim::Inferred,
                    sensitive: false,
                    preference: None,
                }])
                .unwrap(),
            ))]
            .into(),
        ),
        requests: Arc::new(Mutex::new(Vec::new())),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: vec![],
    };
    let worker = crate::memory::learning::LearningWorker {
        store: home.clone(),
        route,
        provider: Arc::new(helper),
        validate_route: Arc::new(|_| Ok(())),
    };
    assert_eq!(
        worker
            .process(true, &tokio_util::sync::CancellationToken::new())
            .await
            .unwrap(),
        1
    );
    let candidates = owner.candidate_page(None, None).unwrap();
    assert_eq!(candidates.records.len(), 1);
    let candidate = owner.candidate(candidates.records[0].id).unwrap();
    assert_eq!(
        candidate.record.state,
        crate::memory::candidates::CandidateState::Staged
    );
    assert_eq!(candidate.record.sources[0].id, sources[0].id);
    assert_eq!(
        candidate.record.scope,
        MemoryScope::Conversation(id.to_string().parse().unwrap())
    );
    assert_eq!(
        owner.eligible().unwrap().records.len(),
        1,
        "only the explicitly remembered fact is active"
    );
}

#[tokio::test]
async fn real_native_turn_selects_current_memory_without_a_bridge_request() {
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let home = ProtectedStore::initialize(
        data.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = crate::identity::SessionId::new();
    let owner = MemoryOwner::new(
        home.clone(),
        MemoryContext {
            conversation: Some(id.to_string().parse().unwrap()),
            profile: Some(uuid::Uuid::new_v4()),
            project: None,
        },
    );
    let fact = owner
        .remember(
            MemoryScope::User,
            "CURRENT_MEMORY_CANARY prefer Rust".into(),
            None,
        )
        .unwrap();
    owner
        .remember(
            MemoryScope::Profile(uuid::Uuid::new_v4()),
            "OTHER_PROFILE_SECRET_CANARY".into(),
            None,
        )
        .unwrap();
    let session = DurableSession::create_protected(home.clone(), root.clone(), id).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(
            vec![
                Ok(Message::text(Role::Assistant, "first")),
                Ok(Message::text(Role::Assistant, "second")),
            ]
            .into(),
        ),
        requests: requests.clone(),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: Vec::new(),
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), root.clone());
    let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), &root).unwrap();
    let mut runtime = RuntimeHandle::spawn_persistent(
        agent,
        policy,
        true,
        session,
        assembler,
        Some(owner.clone()),
    )
    .unwrap();
    for index in 0..2 {
        if index == 1 {
            owner
                .revise(
                    fact.id,
                    fact.revision,
                    crate::memory::MemoryEdit::Correct {
                        statement: "CORRECTED_MEMORY_CANARY prefer TypeScript".into(),
                        valid_until_unix_seconds: None,
                    },
                )
                .unwrap();
        }
        let operation = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id: operation,
                input: "Which language do I prefer?".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_finished(&mut runtime, operation)
            )
            .await
            .unwrap(),
            OperationOutcome::Completed
        );
        let captured = requests.lock().unwrap();
        assert_eq!(
            captured.len(),
            index + 1,
            "one real model call per turn, no bridge/helper call"
        );
        let request = serde_json::to_string(&captured[index]).unwrap();
        assert!(!request.contains("OTHER_PROFILE_SECRET_CANARY"));
        assert!(request.contains(if index == 0 {
            "CURRENT_MEMORY_CANARY"
        } else {
            "CORRECTED_MEMORY_CANARY"
        }));
        if index == 1 {
            assert!(!request.contains("CURRENT_MEMORY_CANARY"));
        }
    }
    let active = owner.record(fact.id).unwrap();
    owner
        .revise(
            active.id,
            active.revision,
            crate::memory::MemoryEdit::Forget,
        )
        .unwrap();
    let operation = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: operation,
            input: "One more request".into(),
        })
        .await
        .unwrap();
    let rejection = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AgentEvent::CommandRejected { reason } = runtime.next_event().await.unwrap() {
                break reason;
            }
        }
    })
    .await
    .unwrap();
    assert!(rejection.contains("forgotten"), "{rejection}");
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "forgotten context never dispatches"
    );
    assert!(runtime.shutdown_owned().await);
}
