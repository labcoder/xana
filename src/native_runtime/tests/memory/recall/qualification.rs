//! Opt-in outcome gate through the native runtime, real prompt compiler,
//! ordinary built-ins, permission broker and disposable reopened protected store.
//! All inputs/facts are synthetic. No real-home keys, paid routes or downloads.
use super::*;
use crate::provider::openai_compat::OpenAiCompatClient;
use std::time::Instant;

fn model() -> String {
    let model = std::env::var("XANA_MEMORY_QUALIFICATION_MODEL")
        .expect("select an authorized installed model explicitly");
    assert!(matches!(model.as_str(), "qwen3-vl:2b" | "qwen3:14b"));
    model
}

#[derive(Default, serde::Serialize)]
struct Observation {
    elapsed_ms: u128,
    answer: String,
    tool_names: Vec<String>,
    approvals: usize,
    completed: bool,
    timed_out: bool,
    timings: Vec<serde_json::Value>,
}

#[derive(Default)]
struct Timings(Mutex<Vec<serde_json::Value>>);
impl crate::telemetry::RuntimeTelemetry for Timings {
    fn record(&self, _: crate::telemetry::RuntimeTelemetryEvent) {}
    fn context_phase(&self, event: crate::telemetry::ContextPhaseEvent) {
        self.0.lock().unwrap().push(serde_json::json!({"phase":format!("{:?}",event.phase),"elapsed_ms":event.elapsed.as_millis()}));
    }
    fn generation_timing(&self, event: crate::telemetry::GenerationTiming) {
        self.0.lock().unwrap().push(serde_json::json!({"phase":"generation","recovery":event.recovery,"first_delta_ms":event.first_delta.map(|time| time.as_millis()),"elapsed_ms":event.elapsed.as_millis(),"succeeded":event.succeeded}));
    }
}

async fn run_turn(
    directory: &std::path::Path,
    recovery: &RecoveryIdentity,
    root: &std::path::Path,
    id: crate::identity::SessionId,
    reopen: bool,
    model: &str,
    input: &str,
) -> Observation {
    let store = ProtectedStore::recover(directory, recovery).unwrap();
    let owner = MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(id.to_string().parse().unwrap()),
            ..Default::default()
        },
    );
    let session = if reopen {
        DurableSession::resume_protected(store.clone(), id)
            .unwrap()
            .0
    } else {
        DurableSession::create_protected(store.clone(), root.to_owned(), id).unwrap()
    };
    let provider = OpenAiCompatClient::new("http://127.0.0.1:11434/v1".into(), model.into())
        .with_fixture_direct_transport()
        .with_fixture_timeouts(Duration::from_secs(15), Duration::from_secs(15));
    let tools = ToolRegistry::builtins(
        crate::shell::Shell::resolve(crate::shell::ShellConfig::default()).unwrap(),
    )
    .unwrap();
    let mut tools = tools;
    crate::memory::tools::register(&mut tools, Some(owner.clone())).unwrap();
    let assembler = PromptAssembler::new(
        tools.definitions().into_iter().cloned().collect(),
        PromptEnvironment {
            connection: "ollama".into(),
            model: model.into(),
            operating_system: std::env::consts::OS.into(),
            working_directory: root.to_owned(),
            configured_shell: "platform".into(),
            surface: PromptSurface::Cli,
        },
        None,
        ContextBudget {
            total_tokens: 32_768,
            conversation_reserve_tokens: 4096,
        },
    );
    let timings = Arc::new(Timings::default());
    let agent = Agent::new(
        Box::new(provider),
        tools,
        root.to_owned(),
        assembler.assemble(&[]).unwrap(),
        4,
    )
    .with_runtime_telemetry(timings.clone());
    // Match default Ask. There is no synthetic approval: external reads,
    // commands, broad/sensitive writes and other Ask operations fail closed.
    let policy = PermissionPolicy::new(PolicyDecision::Ask, vec![], root).unwrap();
    let mut runtime =
        RuntimeHandle::spawn_persistent(agent, policy, false, session, assembler, Some(owner))
            .unwrap();
    let operation = OperationId::new();
    let started = Instant::now();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: operation,
            input: input.into(),
        })
        .await
        .unwrap();
    let mut observation = Observation::default();
    let ended = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = runtime.next_event().await {
            match event {
                AgentEvent::AssistantMessage { message, .. } => {
                    for block in &message.content {
                        if let ContentBlock::ToolCall(call) = block {
                            observation.tool_names.push(call.name.clone());
                        }
                    }
                    let text = crate::completion_evidence::message_text(&message);
                    if !text.trim().is_empty() {
                        observation.answer = text;
                    }
                }
                AgentEvent::PermissionRequested { .. } => observation.approvals += 1,
                AgentEvent::OperationStateChanged {
                    state: OperationState::Finished(outcome),
                    ..
                } => {
                    observation.completed = outcome == OperationOutcome::Completed;
                    break;
                }
                AgentEvent::RoundBudgetReached { .. } => break,
                _ => {}
            }
        }
    })
    .await;
    observation.elapsed_ms = started.elapsed().as_millis();
    observation.timed_out = ended.is_err();
    observation.timings = timings.0.lock().unwrap().clone();
    assert!(
        runtime.shutdown_owned().await,
        "qualification runtime did not settle"
    );
    // Count policy review requirements even for a noninteractive denied request.
    let (_, restored) = DurableSession::inspect_protected(&store, id).unwrap();
    observation.approvals += restored
        .audits
        .iter()
        .filter(|fact| {
            fact.request.operation_id == operation && fact.effective != PolicyDecision::Allow
        })
        .count();
    assert_eq!(
        std::fs::read_dir(root).unwrap().count(),
        0,
        "workspace file effects are a hard failure"
    );
    observation
}

fn admits_unknown(answer: &str) -> bool {
    let answer = answer.to_lowercase().replace('’', "'");
    // A conservative test oracle, not production intent recognition. Replies
    // are retained for semantic inspection; substring matching is not proof.
    [
        "don't know",
        "do not know",
        "haven't told",
        "haven't shared",
        "haven't provided",
        "not know",
        "not been provided",
        "haven't given",
        "not shared your name",
        "not told me your name",
        "don't have your name",
        "do not have your name",
        "no saved information about your name",
    ]
    .iter()
    .any(|phrase| answer.contains(phrase))
        && !answer.contains("your name is")
        && !answer.contains("privacy protection")
        && answer.chars().count() <= 300
}

#[tokio::test]
#[ignore = "explicit installed Ollama model; isolated save and reopened recall outcome gate"]
async fn local_model_save_and_reopened_recall() {
    let model = model();
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let recovery = RecoveryIdentity::generate();
    drop(ProtectedStore::initialize(data.path(), &recovery, &TestCustody::default()).unwrap());
    let id = crate::identity::SessionId::new();
    let save = run_turn(
        data.path(),
        &recovery,
        &root,
        id,
        false,
        &model,
        "For this conversation, remember that my preferred test color is vermilion.",
    )
    .await;
    let recall = run_turn(
        data.path(),
        &recovery,
        &root,
        id,
        true,
        &model,
        "What is my preferred test color?",
    )
    .await;
    let store = ProtectedStore::recover(data.path(), &recovery).unwrap();
    let owner = MemoryOwner::new(store, MemoryContext::default());
    let records = owner.page(None, None).unwrap().records;
    let grounded = records.len() == 1
        && records[0].scope == MemoryScope::Conversation(id.to_string().parse().unwrap())
        && records[0].statement.to_lowercase().contains("vermilion");
    let success = grounded
        && save.completed
        && !save.timed_out
        && !save.answer.trim().is_empty()
        && save.tool_names == ["memory_remember"]
        && save.approvals == 0
        && recall.completed
        && !recall.timed_out
        && recall.tool_names.is_empty()
        && recall.approvals == 0
        && recall.answer.to_lowercase().contains("vermilion")
        && recall.answer.chars().count() <= 300;
    eprintln!(
        "NATIVE_MEMORY_ROUNDTRIP {}",
        serde_json::json!({"model":model,"save":save,"recall":recall,"grounded_record":grounded,"success":success,"denominator":1})
    );
    assert!(success, "save and reopened recall are not qualified");
}

#[tokio::test]
#[ignore = "explicit installed Ollama model; isolated real-model native recall quality gate"]
async fn local_model_unknown_name_recall() {
    let model = model();
    let data = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let recovery = RecoveryIdentity::generate();
    drop(ProtectedStore::initialize(data.path(), &recovery, &TestCustody::default()).unwrap());
    let id = crate::identity::SessionId::new();
    let inputs = [
        "what is my name?",
        "do you know my name?",
        "Have I told you my name?",
        "Can you tell me my name?",
    ];
    let mut successes = 0;
    for (index, input) in inputs.iter().enumerate() {
        let result = run_turn(data.path(), &recovery, &root, id, index > 0, &model, input).await;
        let store = ProtectedStore::recover(data.path(), &recovery).unwrap();
        let owner = MemoryOwner::new(store, MemoryContext::default());
        let no_writes = owner.page(None, None).unwrap().records.is_empty();
        let success = result.completed
            && !result.timed_out
            && result.tool_names.is_empty()
            && result.approvals == 0
            && no_writes
            && admits_unknown(&result.answer);
        successes += usize::from(success);
        eprintln!(
            "NATIVE_RECALL_CASE {}",
            serde_json::json!({"model":model,"input":input,"reopened":index>0,"no_writes":no_writes,"result":result,"success":success})
        );
    }
    eprintln!(
        "NATIVE_RECALL_SUMMARY {}",
        serde_json::json!({"model":model,"successes":successes,"denominator":inputs.len(),"required":"all cases; no tool calls, approvals or writes; brief useful answer; failures and timeouts remain failures"})
    );
    assert_eq!(
        successes,
        inputs.len(),
        "local-model recall is not qualified"
    );
}
