use super::*;
use futures::StreamExt;

#[tokio::test]
async fn oversized_frame_fails_closed_without_a_second_dispatch() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        let _ = socket
            .send(Message::Text("x".repeat(MAX_WIRE_BYTES + 1).into()))
            .await;
    });
    let owner = CdpOwner::connect(
        &format!("ws://{address}/devtools/browser/test"),
        EgressPolicy::fixture("https://example.com", None),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            owner.connection.call("fixture-read", json!({}), None)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(
        owner
            .connection
            .call("fixture-effect", json!({}), None)
            .await
            .is_err()
    );
    drop(owner);
    server.await.unwrap();
}

#[tokio::test]
async fn oversized_command_is_rejected_before_transport_write() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["method"], "safe-read");
        socket
            .send(Message::Text(
                json!({"id":value["id"],"result":{"ok":true}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let owner = CdpOwner::connect(
        &format!("ws://{address}/devtools/browser/test"),
        EgressPolicy::fixture("https://example.com", None),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        owner
            .connection
            .call(
                "fixture-effect",
                json!({"text":"x".repeat(MAX_COMMAND_BYTES)}),
                None
            )
            .await,
        Err(BrowserError::Limit)
    );
    assert_eq!(
        owner
            .connection
            .call("safe-read", json!({}), None)
            .await
            .unwrap(),
        json!({"ok":true})
    );
    server.await.unwrap();
}

#[tokio::test]
async fn lost_response_poisoning_prevents_a_second_effect_dispatch() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let value = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(value.contains("Runtime.callFunctionOn"));
        socket.close(None).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    let owner = CdpOwner::connect(
        &format!("ws://{address}/devtools/browser/test"),
        EgressPolicy::fixture("https://example.com", None),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        owner
            .connection
            .call("Runtime.callFunctionOn", json!({}), None)
            .await
            .is_err()
    );
    assert!(
        owner
            .connection
            .call("Runtime.callFunctionOn", json!({}), None)
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancelled_command_revokes_transport_without_replaying() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (seen, received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        let _ = seen.send(());
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
    let stop = CancellationToken::new();
    let owner = CdpOwner::connect(
        &format!("ws://{address}/devtools/browser/test"),
        EgressPolicy::fixture("https://example.com", None),
        stop.clone(),
    )
    .await
    .unwrap();
    let connection = owner.connection.clone();
    let command = tokio::spawn(async move { connection.call("effect", json!({}), None).await });
    received.await.unwrap();
    command.abort();
    let _ = command.await;
    assert!(stop.is_cancelled());
    assert!(
        owner
            .connection
            .call("effect", json!({}), None)
            .await
            .is_err()
    );
    server.await.unwrap();
}
