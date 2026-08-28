use super::*;
use tokio::sync::oneshot;

struct PendingController {
    started: Arc<Notify>,
    decision: Option<oneshot::Receiver<ApprovalDecision>>,
    approvals: usize,
}

impl ManagedEventHandler for PendingController {
    fn notification(&mut self, _: ManagedNotification) -> Result<(), CodexError> {
        Ok(())
    }

    fn approve<'a>(
        &'a mut self,
        _: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        self.approvals += 1;
        self.started.notify_one();
        let decision = self
            .decision
            .take()
            .expect("only the original request prompts");
        Box::pin(async move {
            decision
                .await
                .map_err(|_| CodexError::RequestCancelled("test controller disconnected"))
        })
    }
}

#[tokio::test]
async fn active_approval_is_cancelled_without_authorizing_late_requests() {
    let (client, server) = duplex(32 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let cancellation = CancellationToken::new();
    let started = Arc::new(Notify::new());
    let (reply, decision) = oneshot::channel();
    let mut handler = PendingController {
        started: Arc::clone(&started),
        decision: Some(decision),
        approvals: 0,
    };
    let cancel = cancellation.clone();
    let server_task = tokio::spawn(async move {
        let request = json!({
            "id":99,
            "method":"item/commandExecution/requestApproval",
            "params":{
                "threadId":"thread-1", "turnId":"turn-1", "itemId":"command-1",
                "command":"echo first", "availableDecisions":["accept","decline","cancel"]
            }
        });
        server_write
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        started.notified().await;
        cancel.cancel();
        let mut lines = BufReader::new(server_read).lines();
        let interrupt: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(
            interrupt["params"],
            json!({"threadId":"thread-1","turnId":"turn-1"})
        );
        let cancelled: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(cancelled, json!({"id":99,"result":{"decision":"cancel"}}));
        assert!(
            reply.send(ApprovalDecision::AcceptOnce).is_err(),
            "a stale controller cannot answer"
        );
        let late = json!({
            "id":100,
            "method":"item/fileChange/requestApproval",
            "params":{
                "threadId":"thread-1", "turnId":"turn-1", "itemId":"patch-1",
                "availableDecisions":["accept","decline"]
            }
        });
        server_write
            .write_all(format!("{late}\n").as_bytes())
            .await
            .unwrap();
        let declined: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(declined, json!({"id":100,"result":{"decision":"decline"}}));
        server_write
            .write_all(format!("{}\n", json!({"id":interrupt["id"],"result":{}})).as_bytes())
            .await
            .unwrap();
        server_write.write_all(b"{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread-1\",\"turn\":{\"id\":\"turn-1\",\"status\":\"interrupted\"}}}\n").await.unwrap();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let interrupted = timeout(
        Duration::from_secs(2),
        peer.wait_for_turn(
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
        ),
    )
    .await
    .expect("cancellation must not wait for a controller answer")
    .unwrap();
    assert!(interrupted);
    assert_eq!(handler.approvals, 1);
    server_task.await.unwrap();
}

#[tokio::test]
async fn completion_before_interrupt_reply_is_drained_before_connection_reuse() {
    let (client, server) = duplex(32 * 1024);
    let (client_read, client_write) = split(client);
    let (server_read, mut server_write) = split(server);
    let server_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let interrupt: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        server_write.write_all(b"{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread-1\",\"turn\":{\"id\":\"turn-1\",\"status\":\"interrupted\"}}}\n").await.unwrap();
        server_write
            .write_all(format!("{}\n", json!({"id":interrupt["id"],"result":{}})).as_bytes())
            .await
            .unwrap();
        let next: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(next["method"], "account/read");
        server_write
            .write_all(
                format!("{}\n", json!({"id":next["id"],"result":{"account":null}})).as_bytes(),
            )
            .await
            .unwrap();
    });
    let mut peer = JsonLinePeer::new(BufReader::new(client_read), BufWriter::new(client_write));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut handler = TestHandler::default();
    assert!(
        peer.wait_for_turn(
            "thread-1",
            "turn-1",
            Some(&cancellation),
            |notification| Ok(matches!(
                notification,
                ManagedNotification::TurnCompleted { .. }
            )),
            &mut handler,
        )
        .await
        .unwrap()
    );
    assert_eq!(
        peer.request("account/read", json!({}), &mut handler)
            .await
            .unwrap(),
        json!({"account":null})
    );
    server_task.await.unwrap();
}
