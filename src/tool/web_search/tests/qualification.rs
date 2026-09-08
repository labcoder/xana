//! Opt-in full native answer check: local model, real search adapter/broker,
//! synthetic sources and no paid route or real-home state.
use super::*;
use crate::{
    agent::Agent,
    context::ContextBudget,
    message::{ContentBlock, Message, Role},
    native_runtime::AgentEvent,
    permission::{ControllerDecision, PermissionBroker, PermissionPolicy, PolicyDecision},
    prompt::{PromptAssembler, PromptEnvironment, PromptSurface},
    provider::openai_compat::OpenAiCompatClient,
    tool::ToolRegistry,
};
use std::time::Instant;

#[tokio::test]
#[ignore = "explicit installed Ollama model; synthetic complete-answer qualification"]
async fn local_model_current_fact_after_unrelated_website() {
    let model = std::env::var("XANA_WEB_QUALIFICATION_MODEL").expect("select an installed model");
    assert!(matches!(model.as_str(), "qwen3:14b" | "qwen3-vl:2b"));
    let mut outcomes = Vec::new();
    for attempt in 1..=3 {
        let (home, mut search) = fixture();
        search.config.connections.get_mut("exa").unwrap().provider = SearchProvider::Exa;
        let source = "https://example.com/synthetic-marlins-schedule";
        let body = json!({"results":[{"url":source,"title":"Synthetic official schedule",
            "text":"Fixture as of September 7, 2026: Miami Marlins next game is September 8, 2026, at 7:10 PM ET. This is synthetic evidence, not a live sports schedule."}]});
        let (endpoint, requests) =
            server(vec![("200 OK", "application/json", body.to_string())]).await;
        search.fixture = Some(endpoint);
        let mut tools =
            ToolRegistry::builtins(crate::shell::Shell::resolve(Default::default()).unwrap())
                .unwrap();
        tools.register(search).unwrap();
        let root = home.path().canonicalize().unwrap();
        let assembler = PromptAssembler::new(
            tools.definitions().into_iter().cloned().collect(),
            PromptEnvironment {
                connection: "ollama".into(),
                model: model.clone(),
                operating_system: std::env::consts::OS.into(),
                working_directory: root.clone(),
                configured_shell: "platform".into(),
                surface: PromptSurface::Cli,
            },
            None,
            ContextBudget {
                total_tokens: 32768,
                conversation_reserve_tokens: 4096,
            },
        );
        let provider = OpenAiCompatClient::new("http://127.0.0.1:11434/v1".into(), model.clone())
            .with_fixture_direct_transport()
            .with_fixture_timeouts(Duration::from_secs(35), Duration::from_secs(35));
        let agent = Agent::new(
            Box::new(provider),
            tools,
            root.clone(),
            assembler.assemble(&[]).unwrap(),
            4,
        );
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (permissions, broker_task) = PermissionBroker::spawn(
            PermissionPolicy::new(PolicyDecision::Ask, vec![], &root).unwrap(),
            true,
            events.clone(),
        );
        let controller = permissions.clone();
        let approvals = tokio::spawn(async move {
            let mut count = 0;
            while let Some(event) = receiver.recv().await {
                if let AgentEvent::PermissionRequested { request } = event {
                    count += 1;
                    // Only the synthetic search adapter can send; never grant a
                    // model-proposed command or real public-page request here.
                    let choice = if request.tool_name == "web_search" {
                        ControllerDecision::AllowPublicWebTurn
                    } else {
                        ControllerDecision::Deny
                    };
                    controller
                        .decide(request.operation_id, request.invocation_id, choice)
                        .await
                        .unwrap();
                }
            }
            count
        });
        let mut history = vec![
            Message::text(
                Role::User,
                "Summarize https://oscarsanchez.com in one sentence.",
            ),
            Message::text(
                Role::Assistant,
                "Oscar Sanchez's personal website covers software, games, and tools.",
            ),
            Message::text(
                Role::User,
                "What about using web search? Tell me the next Miami Marlins game time as of September 7, 2026. Give the date, time zone and source. This test uses a synthetic schedule.",
            ),
        ];
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(100),
            agent.run_turn(
                OperationId::new(),
                &mut history,
                permissions.clone(),
                events.clone(),
            ),
        )
        .await;
        let elapsed = started.elapsed().as_millis();
        permissions.shutdown();
        broker_task.await.unwrap();
        drop(agent);
        drop(events);
        let approvals = tokio::time::timeout(Duration::from_secs(2), approvals)
            .await
            .unwrap()
            .unwrap();
        let answer = result
            .as_ref()
            .ok()
            .and_then(|r| r.as_ref().ok())
            .map(crate::completion_evidence::message_text)
            .unwrap_or_default();
        let calls: Vec<_> = history
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| {
                if let ContentBlock::ToolCall(call) = b {
                    Some(json!({"name":call.name,"arguments":call.arguments}))
                } else {
                    None
                }
            })
            .collect();
        let wire = if requests.is_finished() {
            requests.await.unwrap()
        } else {
            requests.abort();
            Vec::new()
        };
        let success = result.is_ok_and(|r| r.is_ok())
            && answer.contains("7:10")
            && answer.contains("8")
            && (answer.contains("ET") || answer.contains("Eastern"))
            && answer.contains(source)
            && wire.len() == 1
            && approvals == 1
            && calls.iter().all(|c| c["name"] == "web_search");
        outcomes.push(json!({"attempt":attempt,"success":success,"elapsed_ms":elapsed,"answer":answer,"tool_calls":calls,"approval_requests":approvals,"http_requests":wire.len(),"request_bytes":wire.iter().map(String::len).sum::<usize>()}));
    }
    eprintln!(
        "WEB_QUALIFICATION {}",
        json!({"model":model,"endpoint":"loopback Ollama","synthetic":true,"outcomes":outcomes})
    );
    assert!(
        outcomes.iter().all(|o| o["success"] == true),
        "not qualified: inspect all attempts, not just successful ones"
    );
}
