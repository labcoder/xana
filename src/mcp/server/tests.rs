use super::*;
use crate::{
    mcp::{McpRequestId, encode_notification, encode_request},
    permission::PolicyDecision,
    shell::{Shell, ShellConfig},
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};

fn server(workspace: &std::path::Path, decision: PolicyDecision) -> McpLocalServer {
    let shell = Shell::resolve(ShellConfig::default()).expect("platform shell");
    let names = ["xana_docs".to_owned()].into_iter().collect();
    let tools = ToolRegistry::builtins_from_names(shell, &names).expect("bounded tools");
    let policy = PermissionPolicy::new(decision, Vec::new(), workspace).expect("policy");
    McpLocalServer::new(
        workspace.to_owned(),
        "server-profile".to_owned(),
        tools,
        policy,
    )
}

async fn send_frame(writer: &mut (impl AsyncWrite + Unpin), frame: &[u8]) {
    writer.write_all(frame).await.expect("write request");
    writer.write_all(b"\n").await.expect("write newline");
    writer.flush().await.expect("flush request");
}

async fn read_response(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");
    serde_json::from_str(&line).expect("response JSON")
}

#[tokio::test]
async fn hand_written_client_discovers_lists_calls_and_closes_cleanly() {
    let workspace = tempfile::tempdir().unwrap();
    let (client, server_io) = tokio::io::duplex(2 * MAX_FRAME_BYTES);
    let (server_read, server_write) = tokio::io::split(server_io);
    let task = tokio::spawn(
        server(workspace.path(), PolicyDecision::Allow).run_io(server_read, server_write),
    );
    let (client_read, mut client_write) = tokio::io::split(client);
    let mut client_read = BufReader::new(client_read);

    send_frame(
        &mut client_write,
        &encode_request(McpRequestId::new(1).unwrap(), "server/discover", json!({})).unwrap(),
    )
    .await;
    let discover = read_response(&mut client_read).await;
    assert_eq!(
        discover["result"]["supportedVersions"][0],
        MCP_PROTOCOL_VERSION
    );
    assert_eq!(
        discover["result"]["capabilities"]["tools"]["listChanged"],
        false
    );

    send_frame(
        &mut client_write,
        &encode_request(McpRequestId::new(2).unwrap(), "tools/list", json!({})).unwrap(),
    )
    .await;
    let listed = read_response(&mut client_read).await;
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(listed["result"]["tools"][0]["name"], "xana_docs");

    send_frame(
        &mut client_write,
        &encode_request(
            McpRequestId::new(3).unwrap(),
            "tools/call",
            json!({"name":"xana_docs","arguments":{"op":"list","topic":"configuration"}}),
        )
        .unwrap(),
    )
    .await;
    let started = read_response(&mut client_read).await;
    assert_eq!(started["method"], "notifications/progress");
    assert_eq!(started["params"]["progress"], 0.0);
    let finished = read_response(&mut client_read).await;
    assert_eq!(finished["params"]["progress"], 1.0);
    let called = read_response(&mut client_read).await;
    assert_eq!(called["result"]["isError"], false);
    assert!(
        called["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("configuration")
    );

    client_write.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn deny_policy_and_unallowlisted_calls_fail_without_execution() {
    let workspace = tempfile::tempdir().unwrap();
    let request = ClientRequest {
        id: 7,
        method: "tools/call".to_owned(),
        params: serde_json::from_value(json!({
            "name":"xana_docs",
            "arguments":{"op":"list","topic":null}
        }))
        .unwrap(),
    };
    let server = server(workspace.path(), PolicyDecision::Deny);
    let denied = handle_request(
        request,
        Arc::clone(&server.tools),
        server.workspace.clone(),
        server.profile.clone(),
        server.permissions.clone(),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(denied.value["result"]["isError"], true);

    let request = ClientRequest {
        id: 8,
        method: "tools/call".to_owned(),
        params: serde_json::from_value(json!({"name":"read_file","arguments":{"path":"secret"}}))
            .unwrap(),
    };
    let rejected = handle_request(
        request,
        Arc::clone(&server.tools),
        server.workspace.clone(),
        server.profile.clone(),
        server.permissions.clone(),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(rejected.value["error"]["code"], -32602);
    server.permissions.shutdown();
}

#[test]
fn cancellation_notification_requires_exact_metadata_and_numeric_id() {
    let frame = encode_notification(
        "notifications/cancelled",
        json!({"requestId":42,"reason":"client stopped"}),
    )
    .unwrap();
    assert!(matches!(
        parse_client_message(&frame),
        Ok(ClientMessage::Cancel(42))
    ));
}

#[tokio::test]
async fn malformed_partial_and_oversized_frames_are_bounded() {
    assert!(matches!(
        parse_client_message(b"not json"),
        Err((None, -32700, _))
    ));
    assert_eq!(
        McpLocalServerError::FrameTooLarge.to_string(),
        format!("MCP frame exceeds {MAX_FRAME_BYTES} bytes")
    );
    let mut oversized =
        ServerLineReader::new(std::io::Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 2]));
    assert!(matches!(
        oversized.read_frame().await,
        Err(McpLocalServerError::FrameTooLarge)
    ));
    let mut partial = ServerLineReader::new(std::io::Cursor::new(b"partial".to_vec()));
    assert!(matches!(
        partial.read_frame().await,
        Err(McpLocalServerError::PartialFrame)
    ));
}
