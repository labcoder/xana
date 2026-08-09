use super::*;
use std::sync::Arc;
use tokio::io::{BufReader, duplex, sink, split};
use tokio::sync::Notify;

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
    fn approve<'a>(
        &'a mut self,
        _: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        self.approvals += 1;
        Box::pin(async { Ok(ApprovalDecision::AcceptOnce) })
    }
}

struct BlockingApprovalHandler {
    started: Arc<Notify>,
}

impl ManagedEventHandler for BlockingApprovalHandler {
    fn notification(&mut self, _: ManagedNotification) -> Result<(), CodexError> {
        Ok(())
    }

    fn approve<'a>(
        &'a mut self,
        _: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            started.notify_one();
            std::future::pending().await
        })
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
async fn cancellable_turn_wait_sends_one_exact_interrupt_request() {
    let (client, server) = duplex(32 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let interrupt: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(interrupt["params"]["threadId"], "thread-1");
        assert_eq!(interrupt["params"]["turnId"], "turn-1");
        let id = interrupt["id"].clone();
        server_write
            .write_all(format!("{{\"id\":{id},\"result\":{{}}}}\n").as_bytes())
            .await
            .unwrap();
        server_write
            .write_all(
                b"{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"turn-1\",\"status\":\"interrupted\"}}}\n",
            )
            .await
            .unwrap();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut handler = TestHandler::default();

    let interrupted = peer
        .wait_for_turn(
            "thread-1",
            "turn-1",
            Some(&cancellation),
            |notification| {
                Ok(matches!(
                    notification,
                    ManagedNotification::TurnCompleted { .. }
                ))
            },
            &mut handler,
        )
        .await
        .unwrap();

    assert!(interrupted);
    assert_eq!(
        handler
            .notifications
            .iter()
            .filter(|event| matches!(event, ManagedNotification::TurnCompleted { .. }))
            .count(),
        1
    );
    server_task.await.unwrap();
}

#[tokio::test]
async fn unsupported_interrupt_is_a_typed_remote_error() {
    let (client, server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let interrupt: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let id = interrupt["id"].clone();
        server_write
            .write_all(
                format!(
                    "{{\"id\":{id},\"error\":{{\"code\":-32601,\"message\":\"method not found\"}}}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut handler = TestHandler::default();

    assert!(matches!(
        peer.wait_for_turn(
            "thread-1",
            "turn-1",
            Some(&cancellation),
            |_| Ok(false),
            &mut handler,
        )
        .await,
        Err(CodexError::Remote {
            code: Some(-32601),
            ..
        })
    ));
    server_task.await.unwrap();
}

#[tokio::test]
async fn pending_turn_start_request_observes_cancellation() {
    let (client, server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, _server_write) = split(server);
    let cancellation = CancellationToken::new();
    let cancel_after_request = cancellation.clone();
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let request: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "turn/start");
        cancel_after_request.cancel();
        std::future::pending::<()>().await;
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let mut handler = TestHandler::default();

    assert!(matches!(
        peer.request_cancellable(
            "turn/start",
            json!({"threadId":"thread-1"}),
            &cancellation,
            &mut handler,
        )
        .await,
        Err(CodexError::RequestCancelled("turn/start"))
    ));
    server_task.abort();
    let _ = server_task.await;
}

#[tokio::test]
async fn backpressured_turn_start_write_observes_cancellation() {
    let (client, _server) = duplex(8);
    let (client_read, client_write) = split(client);
    let cancellation = CancellationToken::new();
    let cancel_write = cancellation.clone();
    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        cancel_write.cancel();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let mut handler = TestHandler::default();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        peer.request_cancellable(
            "turn/start",
            json!({"input":"x".repeat(4 * 1024)}),
            &cancellation,
            &mut handler,
        ),
    )
    .await
    .expect("cancellation must beat a blocked JSONL write");
    assert!(matches!(
        result,
        Err(CodexError::RequestCancelled("turn/start"))
    ));
    cancel_task.await.expect("cancellation task");
}

#[tokio::test]
async fn pending_approval_callback_observes_turn_start_cancellation() {
    let (client, server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let request: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "turn/start");
        server_write
            .write_all(b"{\"method\":\"item/commandExecution/requestApproval\",\"id\":99,\"params\":{\"command\":\"echo hi\"}}\n")
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let cancellation = CancellationToken::new();
    let started = Arc::new(Notify::new());
    let cancel_approval = cancellation.clone();
    let approval_started = Arc::clone(&started);
    let cancel_task = tokio::spawn(async move {
        approval_started.notified().await;
        cancel_approval.cancel();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let mut handler = BlockingApprovalHandler { started };

    assert!(matches!(
        peer.request_cancellable(
            "turn/start",
            json!({"threadId":"thread-1"}),
            &cancellation,
            &mut handler,
        )
        .await,
        Err(CodexError::RequestCancelled("turn/start"))
    ));
    cancel_task.await.expect("cancellation task");
    server_task.abort();
    let _ = server_task.await;
}

#[tokio::test]
async fn cancelled_partial_receive_preserves_the_frame_prefix() {
    let (client, server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let (_server_read, mut server_write) = split(server);
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    server_write
        .write_all(b"{\"method\":\"test/no")
        .await
        .expect("first frame fragment");
    let cancellation = CancellationToken::new();
    let cancel_receive = cancellation.clone();
    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        cancel_receive.cancel();
    });

    {
        let receive = peer.receive();
        tokio::pin!(receive);
        tokio::select! {
            _ = cancellation.cancelled() => {}
            result = &mut receive => panic!("partial frame unexpectedly completed: {result:?}"),
        }
    }
    server_write
        .write_all(b"ise\",\"params\":{}}\n")
        .await
        .expect("remaining frame fragment");
    let message = peer.receive().await.expect("complete retained frame");
    assert_eq!(message["method"], "test/noise");
    cancel_task.await.expect("cancellation task");
}

#[tokio::test]
async fn continuous_ready_input_cannot_starve_request_cancellation() {
    let (client, server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let cancellation = CancellationToken::new();
    let cancel_noise = cancellation.clone();
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let _: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        for index in 0usize.. {
            if index == 10 {
                cancel_noise.cancel();
            }
            if server_write
                .write_all(b"{\"method\":\"test/noise\",\"params\":{}}\n")
                .await
                .is_err()
            {
                return;
            }
        }
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let mut handler = TestHandler::default();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        peer.request_cancellable(
            "turn/start",
            json!({"threadId":"thread-1"}),
            &cancellation,
            &mut handler,
        ),
    )
    .await
    .expect("ready input must not starve cancellation");
    assert!(matches!(
        result,
        Err(CodexError::RequestCancelled("turn/start"))
    ));
    server_task.abort();
    let _ = server_task.await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn noisy_server_cannot_replenish_the_interruption_grace_bound() {
    let (client, server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let interrupt: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let id = interrupt["id"].clone();
        server_write
            .write_all(format!("{{\"id\":{id},\"result\":{{}}}}\n").as_bytes())
            .await
            .unwrap();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if server_write
                .write_all(b"{\"method\":\"test/noise\",\"params\":{}}\n")
                .await
                .is_err()
            {
                return;
            }
        }
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut handler = TestHandler::default();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        peer.wait_for_turn(
            "thread-1",
            "turn-1",
            Some(&cancellation),
            |_| Ok(false),
            &mut handler,
        ),
    )
    .await
    .expect("absolute interruption deadline must beat the outer test bound");
    assert!(matches!(
        result,
        Err(CodexError::Timeout("turn interruption"))
    ));
    server_task.abort();
    let _ = server_task.await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn silent_server_after_interrupt_hits_the_interruption_grace_bound() {
    let (client, _server) = duplex(16 * 1024);
    let (client_read, client_write) = split(client);
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut handler = TestHandler::default();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        peer.wait_for_turn(
            "thread-1",
            "turn-1",
            Some(&cancellation),
            |_| Ok(false),
            &mut handler,
        ),
    )
    .await
    .expect("test timeout should outlast the adapter interruption bound");
    assert!(matches!(
        result,
        Err(CodexError::Timeout("turn interruption"))
    ));
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
fn thread_and_usage_notifications_keep_managed_correlation() {
    assert_eq!(
        normalize_notification("thread/started".into(), json!({"thread":{"id":"thread-1"}}),)
            .unwrap(),
        ManagedNotification::ThreadStarted {
            thread_id: "thread-1".into(),
        }
    );
    assert_eq!(
        normalize_notification(
            "thread/tokenUsage/updated".into(),
            json!({
                "threadId":"thread-1",
                "turnId":"turn-1",
                "tokenUsage":{
                    "total":{"inputTokens":30,"outputTokens":7,"totalTokens":37},
                    "last":{"inputTokens":10,"outputTokens":2,"totalTokens":12}
                }
            }),
        )
        .unwrap(),
        ManagedNotification::TokenUsageUpdated {
            thread_id: "thread-1".into(),
            turn_id: "turn-1".into(),
            input_tokens: 30,
            output_tokens: 7,
            total_tokens: 37,
        }
    );

    assert!(matches!(
        normalize_notification(
            "thread/tokenUsage/updated".into(),
            json!({
                "threadId":"thread-1",
                "turnId":"turn-1",
                "tokenUsage":{"total":{"inputTokens":30,"totalTokens":37}}
            }),
        ),
        Err(CodexError::Protocol(_))
    ));
}

#[test]
fn thread_lifecycle_preserves_codex_base_and_supplies_xana_identity() {
    let workspace = Path::new("C:/work");
    let developer_instructions = "You are Xana, a personal AI agent.";
    let start = thread_start_params(
        "gpt-5.6-sol",
        workspace,
        developer_instructions,
        ManagedThreadPolicy::default(),
    );
    assert_eq!(start["sandbox"], "workspace-write");
    assert_eq!(start["approvalPolicy"], "on-request");
    assert_eq!(start["serviceName"], "xana");
    assert_eq!(start["developerInstructions"], developer_instructions);
    assert!(start.get("baseInstructions").is_none());

    let resume = thread_resume_params(
        "thr_123",
        "gpt-5.6-sol",
        workspace,
        developer_instructions,
        ManagedThreadPolicy::default(),
    );
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
