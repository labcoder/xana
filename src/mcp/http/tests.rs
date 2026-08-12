use super::*;
use crate::mcp::MCP_PROTOCOL_VERSION;
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn request(id: u64, method: &str, params: Value) -> Vec<u8> {
    let mut params = params.as_object().cloned().unwrap();
    params.insert(
        "_meta".into(),
        json!({"io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION}),
    );
    serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .unwrap()
}

async fn server_once(response: Vec<u8>) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        stream.write_all(&response).await.unwrap();
        stream.shutdown().await.unwrap();
        request
    });
    (format!("http://{address}/mcp"), task)
}

async fn client_for(url: &str) -> McpHttpClient {
    let endpoint = McpHttpEndpoint::parse(
        url,
        McpHttpSecurity {
            allow_loopback_http: true,
        },
    )
    .unwrap();
    McpHttpClient::connect(endpoint).await.unwrap()
}

fn json_response(status: &str, body: &[u8], extra_headers: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}\r\n",
        body.len()
    )
    .bytes()
    .chain(body.iter().copied())
    .collect()
}

#[tokio::test]
async fn posts_required_metadata_auth_and_tool_headers() {
    let body = br#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
    let (url, observed) = server_once(json_response("200 OK", body, "")).await;
    let client = client_for(&url).await;
    let schema = json!({
        "type": "object",
        "properties": {"tenant": {"type": "string", "x-mcp-header": "Tenant"}}
    });
    let headers =
        McpHttpToolHeaders::from_schema_and_arguments(&schema, &json!({"tenant": "alpha"}))
            .unwrap();
    let bearer = SecretString::new("test-secret".into()).unwrap();
    let response = client
        .request(
            &request(7, "tools/call", json!({"name": "weather", "arguments": {}})),
            Some(&bearer),
            &headers,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.final_response, body);

    let wire = String::from_utf8(observed.await.unwrap()).unwrap();
    let lower = wire.to_ascii_lowercase();
    assert!(lower.contains("mcp-protocol-version: 2026-07-28"));
    assert!(lower.contains("mcp-method: tools/call"));
    assert!(lower.contains("mcp-name: weather"));
    assert!(lower.contains("mcp-param-tenant: alpha"));
    assert!(lower.contains("authorization: bearer test-secret"));
    assert!(
        wire.ends_with(
            &String::from_utf8(request(
                7,
                "tools/call",
                json!({"name": "weather", "arguments": {}})
            ))
            .unwrap()
        )
    );
}

#[tokio::test]
async fn collects_request_scoped_sse_progress_and_final_response() {
    let events = concat!(
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":\"p\",\"progress\":1,\"total\":2}}\n\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"ok\":true}}\n\n"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{events}",
        events.len()
    )
    .into_bytes();
    let (url, server) = server_once(response).await;
    let received = client_for(&url)
        .await
        .request(
            &request(9, "tools/call", json!({"name": "slow", "arguments": {}})),
            None,
            &McpHttpToolHeaders::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(received.notifications.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&received.final_response).unwrap()["id"],
        9
    );
}

#[tokio::test]
async fn rejects_redirects_and_classifies_auth_and_rate_errors() {
    for (status, extra, expected) in [
        (
            "302 Found",
            "Location: http://127.0.0.1:1/steal\r\n",
            "redirect",
        ),
        (
            "401 Unauthorized",
            "WWW-Authenticate: Bearer resource_metadata=\"https://auth.example/resource\"\r\n",
            "auth",
        ),
        ("403 Forbidden", "", "scope"),
        ("429 Too Many Requests", "", "rate"),
    ] {
        let (url, server) = server_once(json_response(status, b"{}", extra)).await;
        let error = client_for(&url)
            .await
            .request(
                &request(1, "tools/list", json!({})),
                None,
                &McpHttpToolHeaders::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(matches!(
            (expected, error),
            ("redirect", McpHttpError::RedirectRejected)
                | ("auth", McpHttpError::Unauthorized(Some(_)))
                | ("scope", McpHttpError::InsufficientScope)
                | ("rate", McpHttpError::RateLimited)
        ));
    }
}

#[tokio::test]
async fn cancellation_drops_an_open_request_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0_u8; 4096];
        let _ = stream.read(&mut bytes).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
            .await
            .unwrap();
        let mut one = [0_u8; 1];
        tokio::time::timeout(Duration::from_secs(2), stream.read(&mut one))
            .await
            .unwrap()
            .unwrap()
    });
    let cancellation = CancellationToken::new();
    let child = cancellation.clone();
    let client = client_for(&url).await;
    let body = request(2, "tools/list", json!({}));
    let headers = McpHttpToolHeaders::default();
    let request_future = client.request(&body, None, &headers, &cancellation);
    tokio::pin!(request_future);
    tokio::select! {
        result = &mut request_future => panic!("request ended early: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(50)) => child.cancel(),
    }
    assert!(matches!(request_future.await, Err(McpHttpError::Cancelled)));
    assert_eq!(server.await.unwrap(), 0);
}

#[tokio::test]
async fn address_policy_blocks_local_network_by_default() {
    let endpoint =
        McpHttpEndpoint::parse("https://127.0.0.1/mcp", McpHttpSecurity::default()).unwrap();
    assert!(matches!(
        McpHttpClient::connect(endpoint).await,
        Err(McpHttpError::AddressPolicy)
    ));
    assert!(matches!(
        McpHttpEndpoint::parse("http://example.com/mcp", McpHttpSecurity::default()),
        Err(McpHttpError::HttpsRequired)
    ));
}

#[test]
fn tool_header_schema_is_bounded_and_null_is_omitted() {
    let schema = json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "x-mcp-header": "Name"},
            "nested": {"type": "object", "properties": {
                "count": {"type": "integer", "x-mcp-header": "Count"}
            }}
        }
    });
    let headers = McpHttpToolHeaders::from_schema_and_arguments(
        &schema,
        &json!({"name": "line\nbreak", "nested": {"count": null}}),
    )
    .unwrap();
    assert!(headers.headers["Mcp-Param-name"].starts_with("=?base64?"));
    assert!(!headers.headers.contains_key("Mcp-Param-count"));

    let hostile = json!({
        "type": "object",
        "properties": {"bad": {"type": "object", "x-mcp-header": "Bad"}}
    });
    assert!(matches!(
        McpHttpToolHeaders::from_schema_and_arguments(&hostile, &json!({"bad": {}})),
        Err(McpHttpError::ToolHeaderSchema)
    ));
}

#[test]
fn endpoint_and_auth_identity_invalidate_saved_recipient_decisions() {
    let security = McpHttpSecurity::default();
    let endpoint = McpHttpEndpoint::parse("https://example.com/mcp", security).unwrap();
    let first =
        mcp_http_recipient("docs", &endpoint, Some("https://issuer.one"), Some("x")).unwrap();
    let second =
        mcp_http_recipient("docs", &endpoint, Some("https://issuer.two"), Some("x")).unwrap();
    assert_ne!(first.identity_digest, second.identity_digest);
}

#[tokio::test]
async fn rejects_oversized_or_malformed_responses() {
    let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
    let (url, server) = server_once(json_response("200 OK", &body, "")).await;
    assert!(matches!(
        client_for(&url)
            .await
            .request(
                &request(3, "tools/list", json!({})),
                None,
                &McpHttpToolHeaders::default(),
                &CancellationToken::new(),
            )
            .await,
        Err(McpHttpError::ResponseTooLarge)
    ));
    server.await.unwrap();

    let wrong = br#"{"jsonrpc":"2.0","id":4,"result":{},"error":{}}"#;
    let (url, server) = server_once(json_response("200 OK", wrong, "")).await;
    assert!(matches!(
        client_for(&url)
            .await
            .request(
                &request(4, "tools/list", json!({})),
                None,
                &McpHttpToolHeaders::default(),
                &CancellationToken::new(),
            )
            .await,
        Err(McpHttpError::ProtocolShape)
    ));
    server.await.unwrap();
}
