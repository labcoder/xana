use super::*;

#[derive(Default)]
struct MemoryHandler {
    calls: usize,
    cancel_after_commit: Option<CancellationToken>,
}

impl ManagedEventHandler for MemoryHandler {
    fn notification(&mut self, _: ManagedNotification) -> Result<(), CodexError> {
        Ok(())
    }
    fn approve<'a>(
        &'a mut self,
        _: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        Box::pin(async { Ok(ApprovalDecision::Decline) })
    }
    fn dynamic_tool<'a>(
        &'a mut self,
        _: ManagedToolCall,
        _: CancellationToken,
    ) -> BoxFuture<'a, Result<ManagedToolResult, CodexError>> {
        self.calls += 1;
        if let Some(token) = &self.cancel_after_commit {
            token.cancel();
        }
        Box::pin(async {
            Ok(ManagedToolResult {
                text: "{\"committed\":true}".into(),
                success: true,
            })
        })
    }
}

fn callback(id: u64, turn: &str) -> Value {
    json!({"id":id,"method":"item/tool/call","params":{
        "threadId":"thread-1","turnId":turn,"callId":"call-1",
        "namespace":null,"tool":"memory_update","arguments":{"action":"remember"}
    }})
}

fn completed() -> Value {
    json!({"method":"turn/completed","params":{"turn":{"id":"turn-1","status":"completed"}}})
}

#[tokio::test]
async fn dynamic_memory_callback_returns_a_receipt_and_deduplicates_call_ids() {
    let (client, server) = duplex(32 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let remote = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        for id in [41, 42] {
            server_write
                .write_all(format!("{}\n", callback(id, "turn-1")).as_bytes())
                .await
                .unwrap();
            let response: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(
                response,
                json!({"id":id,"result":{"contentItems":[{"type":"inputText","text":"{\"committed\":true}"}],"success":true}})
            );
        }
        server_write
            .write_all(format!("{}\n", completed()).as_bytes())
            .await
            .unwrap();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let mut handler = MemoryHandler::default();
    peer.wait_for_turn(
        "thread-1",
        "turn-1",
        None,
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
    assert_eq!(handler.calls, 1);
    remote.await.unwrap();
}

#[tokio::test]
async fn dynamic_memory_wrong_scope_or_pre_acknowledgement_never_executes() {
    for active in [true, false] {
        let frame = format!(
            "{}\n",
            callback(41, if active { "another-turn" } else { "turn-1" })
        );
        let mut peer = JsonLinePeer::new(BufReader::new(frame.as_bytes()), sink());
        let mut handler = MemoryHandler::default();
        let result = if active {
            peer.wait_for_turn("thread-1", "turn-1", None, |_| Ok(false), &mut handler)
                .await
                .map(|_| ())
        } else {
            peer.request("turn/start", json!({}), &mut handler)
                .await
                .map(|_| ())
        };
        assert!(matches!(result, Err(CodexError::Protocol(_))));
        assert_eq!(handler.calls, 0);
    }
}

#[tokio::test]
async fn dynamic_memory_cancellation_preserves_a_known_committed_callback_result() {
    let (client, server) = duplex(32 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let remote = tokio::spawn(async move {
        server_write
            .write_all(format!("{}\n", callback(41, "turn-1")).as_bytes())
            .await
            .unwrap();
        let mut lines = BufReader::new(server_read).lines();
        let interrupt: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        let result: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(result["result"]["success"], true);
        assert_eq!(
            result["result"]["contentItems"][0]["text"],
            "{\"committed\":true}"
        );
        server_write
            .write_all(
                format!(
                    "{}\n{}\n",
                    json!({"id":interrupt["id"],"result":{}}),
                    completed()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let cancellation = CancellationToken::new();
    let mut handler = MemoryHandler {
        calls: 0,
        cancel_after_commit: Some(cancellation.clone()),
    };
    assert!(
        peer.wait_for_turn(
            "thread-1",
            "turn-1",
            Some(&cancellation),
            |notification| Ok(matches!(
                notification,
                ManagedNotification::TurnCompleted { .. }
            )),
            &mut handler
        )
        .await
        .unwrap()
    );
    assert_eq!(handler.calls, 1);
    remote.await.unwrap();
}

#[test]
fn dynamic_memory_tools_have_exact_supported_schema_and_bounded_ids_arguments_results() {
    use super::super::dynamic_tools as bridge;
    let tools = bridge::definitions(crate::memory::tools::definitions()).unwrap();
    assert_eq!(tools.len(), 4);
    assert!(
        tools
            .iter()
            .all(|tool| tool["type"] == "function" && tool["inputSchema"].is_object())
    );
    let base = callback(41, "turn-1")["params"].clone();
    for (field, value) in [
        ("tool", json!("run_command")),
        ("namespace", json!("functions")),
        ("callId", json!("")),
        ("callId", json!("x".repeat(257))),
        ("arguments", json!({"text":"x".repeat(16*1024)})),
        ("arguments", json!([])),
    ] {
        let mut params = base.clone();
        params[field] = value;
        assert!(bridge::decode(&params).is_err(), "{field}");
    }
    let call = bridge::decode(&base).unwrap();
    let result = ManagedToolResult {
        text: "receipt".into(),
        success: true,
    };
    let mut receipts = bridge::TurnReceipts::default();
    receipts.record(call.clone(), result).unwrap();
    let mut changed = call;
    changed.arguments = json!({"action":"forget"});
    assert!(receipts.existing(&changed).is_err());
    assert!(
        bridge::encode(ManagedToolResult {
            text: "x".repeat(32 * 1024 + 1),
            success: true
        })
        .is_err()
    );
}

#[test]
fn dynamic_memory_lookup_receipts_recheck_current_state_and_bound_repeated_calls() {
    use super::super::dynamic_tools as bridge;
    let mut params = callback(41, "turn-1")["params"].clone();
    params["tool"] = json!("memory_lookup");
    let call = bridge::decode(&params).unwrap();
    let mut receipts = bridge::TurnReceipts::default();
    receipts
        .record(
            call.clone(),
            ManagedToolResult {
                text: "obsolete private fact".into(),
                success: true,
            },
        )
        .unwrap();
    for _ in 0..32 {
        assert!(
            receipts.existing(&call).unwrap().is_none(),
            "read results never replay private facts"
        );
    }
    assert!(
        receipts.existing(&call).is_err(),
        "duplicate calls count toward the execution bound"
    );
}
