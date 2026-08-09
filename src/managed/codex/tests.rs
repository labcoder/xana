use super::*;
use tokio::io::{BufReader, duplex, sink, split};

#[derive(Default)]
struct TestHandler {
    notifications: Vec<ManagedNotification>,
    approvals: usize,
}
impl ManagedEventHandler for TestHandler {
    fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError> {
        self.notifications.push(notification);
        Ok(())
    }
    fn approve(&mut self, _: ApprovalRequest) -> Result<ApprovalDecision, CodexError> {
        self.approvals += 1;
        Ok(ApprovalDecision::AcceptOnce)
    }
}

#[tokio::test]
async fn fake_jsonl_child_maps_notification_and_approval() {
    let (client, server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let request: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let id = request["id"].clone();
        server_write
                .write_all(
                    b"{\"method\":\"item/agentMessage/delta\",\"params\":{\"itemId\":\"answer\",\"delta\":\"hi\"}}\n",
                )
                .await
                .unwrap();
        server_write.write_all(b"{\"method\":\"item/commandExecution/requestApproval\",\"id\":99,\"params\":{\"command\":\"echo hi\"}}\n").await.unwrap();
        let approval: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(approval["result"]["decision"], "accept");
        server_write
            .write_all(format!("{{\"id\":{id},\"result\":{{\"ok\":true}}}}\n").as_bytes())
            .await
            .unwrap();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let mut handler = TestHandler::default();
    let result = peer.request("test", json!({}), &mut handler).await.unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(handler.approvals, 1);
    assert_eq!(
        handler.notifications,
        vec![ManagedNotification::AssistantDelta {
            item_id: Some("answer".into()),
            delta: "hi".into(),
        }]
    );
    server_task.await.unwrap();
}

#[tokio::test]
async fn oversized_incoming_frame_fails_before_json_decoding() {
    let bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
    let reader = BufReader::new(bytes.as_slice());
    let mut peer = JsonLinePeer::new(reader, sink());

    assert!(matches!(
        peer.receive().await,
        Err(CodexError::FrameTooLarge)
    ));
}

#[tokio::test]
async fn remote_error_response_is_typed_and_bounded_by_the_frame_reader() {
    let input = br#"{"id":1,"error":{"code":401,"message":"authentication failed"}}
"#;
    let mut peer = JsonLinePeer::new(BufReader::new(input.as_slice()), sink());
    let mut handler = TestHandler::default();

    let error = peer
        .request("account/read", json!({}), &mut handler)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CodexError::Remote {
            code: Some(401),
            ..
        }
    ));
}

#[test]
fn item_started_keeps_a_bounded_approval_summary() {
    let notification = normalize_notification(
        "item/started".into(),
        json!({
            "item": {
                "id": "item-1",
                "type": "fileChange",
                "changes": [{"path": "src/lib.rs", "kind": "update"}]
            }
        }),
    )
    .unwrap();

    let ManagedNotification::ItemStarted(item) = notification else {
        panic!("expected item-start notification");
    };
    assert_eq!(item.id, "item-1");
    assert_eq!(item.kind, "fileChange");
    assert!(item.label.contains("src/lib.rs"));
    assert!(item.details.contains("src/lib.rs"));
    assert!(item.details.len() <= MAX_ITEM_DETAIL_BYTES + 64);
}

#[test]
fn reasoning_plan_tool_and_completion_events_are_typed() {
    assert_eq!(
        normalize_notification(
            "item/reasoning/summaryTextDelta".into(),
            json!({"itemId":"reasoning-1","summaryIndex":2,"delta":"Checking tests"}),
        )
        .unwrap(),
        ManagedNotification::ReasoningSummaryDelta {
            item_id: Some("reasoning-1".into()),
            summary_index: Some(2),
            delta: "Checking tests".into(),
        }
    );
    assert!(matches!(
        normalize_notification(
            "turn/plan/updated".into(),
            json!({"explanation":"Implementation","plan":[{"step":"Patch","status":"inProgress"}]}),
        )
        .unwrap(),
        ManagedNotification::PlanUpdated { steps, .. }
            if steps == vec![ManagedPlanStep { step: "Patch".into(), status: "inProgress".into() }]
    ));
    assert_eq!(
        normalize_notification(
            "item/commandExecution/outputDelta".into(),
            json!({"itemId":"command-1","delta":"ok\n"}),
        )
        .unwrap(),
        ManagedNotification::CommandOutputDelta {
            item_id: Some("command-1".into()),
            delta: "ok\n".into(),
        }
    );
    assert!(matches!(
        normalize_notification(
            "item/completed".into(),
            json!({"item":{"id":"child-1","type":"collabAgentToolCall","tool":"spawnAgent","status":"completed","receiverThreadIds":["thr_child"]}}),
        )
        .unwrap(),
        ManagedNotification::ItemCompleted(item)
            if item.kind == "collabAgentToolCall" && item.label.contains("spawnAgent")
    ));
    assert!(matches!(
        normalize_notification(
            "item/started".into(),
            json!({"item":{"id":"child-activity","type":"subAgentActivity","agentPath":"researcher","agentThreadId":"thr_child","kind":"started"}}),
        )
        .unwrap(),
        ManagedNotification::ItemStarted(item)
            if item.kind == "subAgentActivity" && item.label.contains("researcher")
    ));
    assert_eq!(
        normalize_notification(
            "turn/completed".into(),
            json!({"turn":{"id":"turn-1","status":"failed","error":{"message":"boom"}}}),
        )
        .unwrap(),
        ManagedNotification::TurnCompleted {
            turn_id: "turn-1".into(),
            status: "failed".into(),
            error: Some("boom".into()),
        }
    );
}

#[test]
fn thread_lifecycle_preserves_codex_base_and_supplies_xana_identity() {
    let workspace = Path::new("C:/work");
    let developer_instructions = "You are Xana, a personal AI agent.";
    let start = thread_start_params("gpt-5.6-sol", workspace, developer_instructions);
    assert_eq!(start["sandbox"], "workspace-write");
    assert_eq!(start["approvalPolicy"], "on-request");
    assert_eq!(start["serviceName"], "xana");
    assert_eq!(start["developerInstructions"], developer_instructions);
    assert!(start.get("baseInstructions").is_none());

    let resume = thread_resume_params("thr_123", "gpt-5.6-sol", workspace, developer_instructions);
    assert_eq!(resume["threadId"], "thr_123");
    assert_eq!(resume["model"], "gpt-5.6-sol");
    assert_eq!(resume["sandbox"], "workspace-write");
    assert_eq!(resume["developerInstructions"], developer_instructions);
    assert!(resume.get("baseInstructions").is_none());

    let turn = turn_start_params(
        "thr_123",
        "gpt-5.6-sol",
        &ManagedTurnOptions {
            reasoning_effort: Some("xhigh".into()),
            reasoning_summary: Some(ReasoningSummary::Detailed),
        },
        vec![json!({"type":"text","text":"Implement it"})],
    );
    assert_eq!(turn["effort"], "xhigh");
    assert_eq!(turn["summary"], "detailed");
    assert_eq!(turn["model"], "gpt-5.6-sol");
}

#[test]
fn codex_model_descriptor_preserves_reasoning_catalog_options() {
    let descriptor = model_descriptor_from_wire(&json!({
        "id":"gpt-5.6-sol",
        "displayName":"GPT-5.6-Sol",
        "defaultReasoningEffort":"low",
        "supportedReasoningEfforts":[
            {"reasoningEffort":"low","description":"Fast"},
            {"reasoningEffort":"xhigh","description":"Deep"}
        ],
        "inputModalities":["text","image"],
        "isDefault":true
    }))
    .unwrap();
    assert_eq!(descriptor.default_reasoning_effort.as_deref(), Some("low"));
    assert_eq!(
        descriptor
            .reasoning_efforts
            .iter()
            .map(|effort| effort.id.as_str())
            .collect::<Vec<_>>(),
        vec!["low", "xhigh"]
    );
    assert!(descriptor.input_modalities.contains("image"));
    assert!(descriptor.is_default);

    let legacy = model_descriptor_from_wire(&json!({"id":"legacy"})).unwrap();
    assert_eq!(
        legacy.input_modalities,
        ["text".to_owned(), "image".to_owned()]
            .into_iter()
            .collect()
    );
}

#[test]
fn account_and_error_debug_paths_contain_no_tokens() {
    let error = CodexError::Remote {
        code: Some(401),
        message: "authentication failed".into(),
    };
    assert!(!format!("{error:?}").contains("access_token"));
    assert_eq!(
        AccountStatus::ChatGpt {
            plan: "plus".into()
        },
        AccountStatus::ChatGpt {
            plan: "plus".into()
        }
    );
}
