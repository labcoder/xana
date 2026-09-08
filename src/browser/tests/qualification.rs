//! Complete local-model browser reading against the owned synthetic recipient.
use super::*;
use crate::browser::session::BrowserSnapshot;
use crate::{
    agent::Agent,
    context::ContextBudget,
    message::{ContentBlock, Message, Role, ToolResultStatus},
    native_runtime::AgentEvent,
    permission::{ControllerDecision, PermissionBroker, PermissionPolicy, PolicyDecision},
    prompt::{PromptAssembler, PromptEnvironment, PromptSurface},
    provider::openai_compat::OpenAiCompatClient,
    tool::ToolRegistry,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore = "explicit installed Ollama model and disposable native HTTPS browser fixture"]
async fn local_model_opens_reads_and_leaves_browser_for_takeover() {
    if std::env::var_os("XANA_BROWSER_MODEL_CHILD").is_none() {
        use age::secrecy::ExposeSecret;
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let key = directory.path().join("recovery.key");
        let identity = crate::storage::RecoveryIdentity::generate();
        let mut file = crate::storage::create_private_file(&key).unwrap();
        file.write_all(identity.to_string().expose_secret().as_bytes())
            .unwrap();
        file.sync_all().unwrap();
        drop(file);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "browser::tests::qualification::local_model_opens_reads_and_leaves_browser_for_takeover",
                "--ignored",
                "--nocapture",
            ])
            .env("XANA_BROWSER_MODEL_CHILD", "1")
            .env("XANA_STORAGE_RECOVERY_KEY", &key)
            .env("XANA_HOME", directory.path().join("unused-home"))
            .output()
            .unwrap();
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "browser outcome qualification failed"
        );
        return;
    }
    let model = std::env::var("XANA_WEB_QUALIFICATION_MODEL").expect("select installed model");
    assert!(matches!(model.as_str(), "qwen3:14b" | "qwen3-vl:2b"));
    let file = std::env::var_os("XANA_BROWSER_FIXTURE").expect("synthetic fixture path");
    let facts: Value = serde_json::from_slice(
        &crate::bounded_file::read(std::path::Path::new(&file), 4096).unwrap(),
    )
    .unwrap();
    let origin = facts["origin"].as_str().unwrap();
    let address: std::net::SocketAddr = facts["address"].as_str().unwrap().parse().unwrap();
    assert!(address.ip().is_loopback());
    assert_eq!(
        reqwest::Url::parse(origin).unwrap().host_str(),
        Some("localhost")
    );
    let mut outcomes = Vec::new();
    for attempt in 1..=3 {
        let root = tempfile::tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(root.path().as_os_str().to_owned())).unwrap();
        let recovery = std::env::var_os("XANA_STORAGE_RECOVERY_KEY").unwrap();
        let identity =
            crate::storage::read_recovery_identity(std::path::Path::new(&recovery)).unwrap();
        let store = ProtectedStore::initialize(
            paths.data_dir(),
            &identity,
            &crate::storage::RecoveryOnlyCustody,
        )
        .unwrap();
        let owner = BrowserOwner::with_executable(
            paths,
            store,
            PrincipalId::new(),
            crate::identity::SessionId::new(),
            None,
            true,
        );
        let owner = owner.native_fixture(origin, address, facts["spki"].as_str().unwrap().into());
        let workspace = root.path().canonicalize().unwrap();
        let mut tools =
            ToolRegistry::builtins(crate::shell::Shell::resolve(Default::default()).unwrap())
                .unwrap();
        register_tools(
            &mut tools,
            owner.clone(),
            [crate::config::OutboundDataClass::PromptText].into(),
        )
        .unwrap();
        let prompt = PromptAssembler::new(
            tools.definitions().into_iter().cloned().collect(),
            PromptEnvironment {
                connection: "ollama".into(),
                model: model.clone(),
                operating_system: std::env::consts::OS.into(),
                working_directory: workspace.clone(),
                configured_shell: "platform".into(),
                surface: PromptSurface::Cli,
            },
            None,
            ContextBudget {
                total_tokens: 32768,
                conversation_reserve_tokens: 4096,
            },
        )
        .assemble(&[])
        .unwrap();
        let provider = OpenAiCompatClient::new("http://127.0.0.1:11434/v1".into(), model.clone())
            .with_fixture_direct_transport()
            .with_fixture_timeouts(Duration::from_secs(40), Duration::from_secs(40));
        let agent = Agent::new(Box::new(provider), tools, workspace.clone(), prompt, 4);
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (permissions, broker) = PermissionBroker::spawn(
            PermissionPolicy::new(PolicyDecision::Ask, vec![], &workspace).unwrap(),
            true,
            events.clone(),
        );
        let controller = permissions.clone();
        let exact_url = format!("{origin}/");
        let approved_url = exact_url.clone();
        let approvals = tokio::spawn(async move {
            let mut requests = 0;
            while let Some(event) = receiver.recv().await {
                if let AgentEvent::PermissionRequested { request } = event {
                    requests += 1;
                    let browser = &request.final_arguments["request"];
                    let allowed = request.tool_name == "browser"
                        && (browser["op"] == "open" && browser["url"] == approved_url
                            || matches!(browser["op"].as_str(), Some("observe" | "takeover")));
                    controller
                        .decide(
                            request.operation_id,
                            request.invocation_id,
                            if allowed {
                                ControllerDecision::AllowOnce
                            } else {
                                ControllerDecision::Deny
                            },
                        )
                        .await
                        .unwrap();
                }
            }
            requests
        });
        let mut history = vec![Message::text(
            Role::User,
            format!(
                "Use the browser tool to open {exact_url}. Summarize the page in one sentence. Do not follow links, submit anything, or click controls. Leave the browser open for manual takeover."
            ),
        )];
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(100),
            agent.run_turn(
                crate::identity::OperationId::new(),
                &mut history,
                permissions.clone(),
                events.clone(),
            ),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis();
        let answer = result
            .as_ref()
            .ok()
            .and_then(|value| value.as_ref().ok())
            .map(crate::completion_evidence::message_text)
            .unwrap_or_default();
        let calls: Vec<_> = history
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => {
                    Some(json!({"name":call.name,"arguments":call.arguments}))
                }
                _ => None,
            })
            .collect();
        let snapshot = owner.snapshot();
        let success = result.is_ok_and(|value| value.is_ok())
            && grounded_read(&snapshot, &history, &answer)
            && calls
                .first()
                .is_some_and(|call| call["name"] == "browser" && call["arguments"]["op"] == "open")
            && calls.iter().all(|call| {
                call["name"] == "browser"
                    && matches!(
                        call["arguments"]["op"].as_str(),
                        Some("open" | "observe" | "takeover")
                    )
            });
        permissions.shutdown();
        broker.await.unwrap();
        drop(agent);
        drop(events);
        let approvals = tokio::time::timeout(Duration::from_secs(2), approvals)
            .await
            .unwrap()
            .unwrap();
        owner.shutdown().await.unwrap();
        let rejected_calls = history.iter().flat_map(|m| &m.content).filter(|block| matches!(block, ContentBlock::ToolResult(result) if result.status == ToolResultStatus::Error)).count();
        outcomes.push(json!({"attempt":attempt,"success":success,"elapsed_ms":elapsed_ms,"answer":answer,"calls":calls,"rejected_calls":rejected_calls,"approvals":approvals,"left_open":snapshot.task.is_some(),"state":snapshot.state}));
    }
    eprintln!(
        "BROWSER_MODEL_QUALIFICATION {}",
        json!({"model":model,"synthetic":true,"outcomes":outcomes})
    );
    assert!(outcomes.iter().all(|outcome| outcome["success"] == true));
}

// This is a minimum grounded-evidence check, not a universal semantic judge.
// Retain final prose for separate content adjudication in the evidence record.
fn grounded_read(snapshot: &BrowserSnapshot, history: &[Message], answer: &str) -> bool {
    let answer = answer.to_ascii_lowercase();
    snapshot.task.is_some()
        && matches!(snapshot.state.as_str(), "ready" | "manual_takeover")
        && !snapshot.receipt_error
        && answer.contains("production adapter fixture")
        && ["count effect", "synthetic value", "submit fixture"].iter().filter(|fact| answer.contains(**fact)).count() >= 2
        && history.iter().flat_map(|m| &m.content).any(|block| {
            let ContentBlock::ToolResult(result) = block else { return false };
            if result.status != ToolResultStatus::Success || !history.iter().flat_map(|m| &m.content).any(|block| matches!(block, ContentBlock::ToolCall(call) if call.id == result.call_id && call.name == "browser" && matches!(call.arguments["op"].as_str(), Some("open" | "observe")))) {
                return false;
            }
            serde_json::from_str::<BrowserReceipt>(&result.output).is_ok_and(|receipt| receipt.acknowledged && receipt.task == snapshot.task && receipt.observation.is_some_and(|observation| observation.to_string().contains("Production adapter fixture")))
        })
}

#[test]
fn a_refusal_or_open_task_without_read_evidence_never_qualifies() {
    let (_root, owner) = fixture();
    let mut snapshot = owner.snapshot();
    snapshot.task = Some(uuid::Uuid::new_v4());
    snapshot.state = "ready".into();
    assert!(!grounded_read(
        &snapshot,
        &[],
        "I could not read the fixture"
    ));
    let summary = "Production adapter fixture has Count effect and Submit fixture controls.";
    assert!(!grounded_read(&snapshot, &[], summary));
    let receipt = BrowserReceipt {
        id: uuid::Uuid::new_v4(),
        task: snapshot.task,
        operation: crate::identity::OperationId::new(),
        outcome: "acknowledged".into(),
        acknowledged: true,
        snapshot: snapshot.clone(),
        evidence: None,
        observation: Some(json!({"text":"Production adapter fixture"})),
    };
    let history = [
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(crate::message::ToolCall {
                id: "call".into(),
                name: "browser".into(),
                arguments: json!({"op":"open","url":"https://example.com/"}),
            })],
        },
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(
                crate::message::ToolResult::success(
                    "call",
                    serde_json::to_string(&receipt).unwrap(),
                ),
            )],
        },
    ];
    assert!(grounded_read(&snapshot, &history, summary));
    snapshot.state = "cleanup_failed".into();
    assert!(!grounded_read(&snapshot, &history, summary));
    snapshot.state = "ready".into();
    snapshot.receipt_error = true;
    assert!(!grounded_read(&snapshot, &history, summary));
}
