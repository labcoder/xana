use super::*;
mod qualification;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn server(
    responses: Vec<(&str, &str, String)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/search", listener.local_addr().unwrap());
    let responses = responses
        .into_iter()
        .map(|(status, mime, body)| (status.to_owned(), mime.to_owned(), body))
        .collect::<Vec<_>>();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, mime, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                assert!(bytes.len() < 16384);
                if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let len = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
        requests
    });
    (url, task)
}

async fn authorized(
    tool: &WebSearch,
    operation: OperationId,
    query: &str,
) -> Result<String, String> {
    use crate::outbound::{OutboundApprovalDecision, ReviewedOutboundApproval};
    let plan = tool.plan(&json!({"query":query}), Path::new(".")).unwrap();
    let review = plan
        .outbound_review
        .clone()
        .unwrap()
        .for_operation(operation);
    tool.execute(
        &plan,
        ToolExecutionContext {
            operation_id: operation,
            events: None,
            outbound_approval: Some(ReviewedOutboundApproval::new(
                review,
                OutboundApprovalDecision::AllowPublicWebTurn,
            )),
            cleanup: crate::tool::DeferredCleanup::default(),
        },
    )
    .await
}

#[tokio::test]
async fn direct_api_wire_contracts_and_duplicate_receipts() {
    for provider in [SearchProvider::Exa, SearchProvider::Brave] {
        let (_home, mut tool) = fixture();
        tool.config.connections.get_mut("exa").unwrap().provider = provider;
        let result = json!({"url":"https://example.com/schedule", "title":"Schedule", "text":"Sep 8 at 7:10 PM ET", "description":"Sep 8 at 7:10 PM ET"});
        let body = if provider == SearchProvider::Exa {
            json!({"results":[result]})
        } else {
            json!({"web":{"results":[result]}})
        };
        let (endpoint, task) = server(vec![("200 OK", "application/json", body.to_string())]).await;
        tool.fixture = Some(endpoint);
        let operation = OperationId::new();
        let first = authorized(&tool, operation, "next game").await.unwrap();
        let requests = task.await.unwrap();
        assert_eq!(
            authorized(&tool, operation, "next game").await.unwrap(),
            first,
            "no second socket for a duplicate"
        );
        assert!(first.contains("7:10 PM"));
        let wire = &requests[0];
        assert!(!wire.contains("answers"));
        assert!(!wire.contains("deep"));
        if provider == SearchProvider::Exa {
            assert!(wire.starts_with("POST /search "));
            assert!(
                wire.to_ascii_lowercase()
                    .contains("x-api-key: fixture-only-key")
            );
            let payload: Value =
                serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(payload["query"], "next game");
            assert_eq!(payload["type"], "auto");
            assert!(payload.get("summary").is_none());
        } else {
            assert!(wire.starts_with("GET /search?q=next+game&count=5 "));
            assert!(
                wire.to_ascii_lowercase()
                    .contains("x-subscription-token: fixture-only-key")
            );
        }
        for _ in 0..7 {
            tool.runtime.turn(operation).attempt().unwrap();
        }
        assert_eq!(
            tool.runtime.turn(operation).attempt(),
            Err(WebFailure::Budget)
        );
    }
}

#[tokio::test]
async fn terminal_provider_failures_are_not_replayed_or_treated_as_empty() {
    for (status, body, category) in [
        ("429 Too Many Requests", "{}", "RateLimited"),
        ("401 Unauthorized", "{}", "Authentication"),
        ("200 OK", "{bad", "InvalidResponse"),
    ] {
        let (_home, mut tool) = fixture();
        tool.config.connections.get_mut("exa").unwrap().provider = SearchProvider::Exa;
        let (endpoint, task) = server(vec![(status, "application/json", body.into())]).await;
        tool.fixture = Some(endpoint);
        let operation = OperationId::new();
        let first = authorized(&tool, operation, "weather").await.unwrap_err();
        task.await.unwrap();
        assert!(first.contains(category), "{first}");
        assert_eq!(
            authorized(&tool, operation, "weather").await.unwrap_err(),
            first
        );
    }
}

#[tokio::test]
async fn mcp_search_counts_handshake_and_sse_without_fabricating_sources() {
    let (_home, mut tool) = fixture();
    let initialized = json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":crate::mcp::EXA_SEARCH_PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}});
    let result = json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"Schedule: https://example.com/game — September 8"}]}});
    let sse = format!("data: {result}\n\n");
    let (endpoint, task) = server(vec![
        ("200 OK", "application/json", initialized.to_string()),
        ("202 Accepted", "application/json", String::new()),
        ("200 OK", "text/event-stream", sse),
    ])
    .await;
    tool.fixture = Some(endpoint);
    let operation = OperationId::new();
    let value: Value =
        serde_json::from_str(&authorized(&tool, operation, "game").await.unwrap()).unwrap();
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].contains("initialize"));
    assert!(requests[1].contains("notifications/initialized"));
    assert!(requests[2].contains("web_search_exa"));
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("fixture-only-key"))
    );
    assert_eq!(value["sources"], json!([]));
    assert!(
        value["unstructured_evidence"]
            .as_str()
            .unwrap()
            .contains("September 8")
    );
    for _ in 0..5 {
        tool.runtime.turn(operation).attempt().unwrap();
    }
    assert_eq!(
        tool.runtime.turn(operation).attempt(),
        Err(WebFailure::Budget)
    );
}

#[test]
fn api_results_are_sources_not_synthetic_answers() {
    let value = normalize(SearchProvider::Exa, "exa", 5, json!({"results":[{
        "url":"https://example.com/game","title":"Next game","text":"First pitch 7:10 PM","publishedDate":"2026-09-07"}],
        "answer":"invented answer must not be used"}), 2).unwrap();
    assert!(!value.contains("invented answer"));
    let value: Value = serde_json::from_str(&value).unwrap();
    assert_eq!(value["sources"][0]["text"], "First pitch 7:10 PM");
    assert_eq!(value["untrusted"], true);
    assert_eq!(value["empty"], false);
    assert_eq!(value["provider_internal_work"], "unknown");
}

#[test]
fn empty_drift_and_hostile_source_urls_are_distinct() {
    let empty = normalize(
        SearchProvider::Brave,
        "brave",
        5,
        json!({"web":{"results":[]}}),
        0,
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&empty).unwrap()["empty"],
        true
    );
    assert_eq!(
        normalize(SearchProvider::Brave, "brave", 5, json!({"answer":"hi"}), 0),
        Err(WebFailure::InvalidResponse)
    );
    for url in [
        "file:///secret",
        "javascript:alert(1)",
        "https://user:password@example.com",
    ] {
        assert_eq!(
            normalize(
                SearchProvider::Exa,
                "exa",
                5,
                json!({"results":[{"url":url}]}),
                0
            ),
            Err(WebFailure::InvalidResponse)
        );
    }
}

#[test]
fn mcp_text_remains_unstructured_and_output_is_bounded_after_json_escaping() {
    let text = "URL: pretend\nPublished: unknown\n\"\\".repeat(2000);
    let normalized = normalize(
        SearchProvider::ExaMcp,
        "mcp",
        5,
        json!({"result":{"content":[{"type":"text","text":text}]}}),
        0,
    )
    .unwrap();
    assert!(normalized.len() <= MAX_OUTPUT_BYTES);
    let value: Value = serde_json::from_str(&normalized).unwrap();
    assert_eq!(value["sources"], json!([]));
    assert_eq!(value["truncated"], true);
    assert!(!value["unstructured_evidence"].as_str().unwrap().is_empty());
}

fn fixture() -> (tempfile::TempDir, WebSearch) {
    let root = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(root.path().join("home").into_os_string())).unwrap();
    let config: WebConfig =
        toml::from_str("default_connection='exa'\n[connections.exa]\nprovider='exa_mcp'").unwrap();
    let runtime = Arc::new(WebRuntime::new(config.limits.clone()));
    (
        root,
        WebSearch {
            paths,
            config,
            runtime,
            fixture: None,
        },
    )
}

#[tokio::test]
async fn denied_search_is_zero_transport_and_zero_search_budget() {
    let (_root, tool) = fixture();
    let plan = tool
        .plan(&json!({"query":"next game"}), Path::new("."))
        .unwrap();
    let operation = OperationId::new();
    let result = tool
        .execute(
            &plan,
            ToolExecutionContext {
                operation_id: operation,
                events: None,
                outbound_approval: None,
                cleanup: super::super::DeferredCleanup::default(),
            },
        )
        .await;
    assert!(result.is_err());
    // All eight attempt slots remain after denial; no provider handshake or DNS.
    let turn = tool.runtime.turn(operation);
    for _ in 0..8 {
        turn.attempt().unwrap();
    }
}

#[tokio::test]
async fn frozen_profile_ceiling_wins_over_explicit_public_web_approval() {
    let (_root, mut tool) = fixture();
    tool.runtime = Arc::new(WebRuntime::new(Default::default()).with_profile(&[]));
    let operation = OperationId::new();
    assert!(authorized(&tool, operation, "game").await.is_err());
    for _ in 0..8 {
        tool.runtime.turn(operation).attempt().unwrap();
    }
}

#[test]
fn planning_does_not_accept_unbounded_queries_or_select_a_provider_for_the_user() {
    let (_root, mut tool) = fixture();
    for args in [
        json!({"query":" "}),
        json!({"query":"hello","count":11}),
        json!({"query":"x".repeat(2049)}),
        json!({"query":"hi","endpoint":"https://evil.test"}),
    ] {
        assert!(tool.plan(&args, Path::new(".")).is_err());
    }
    tool.config.default_connection = None;
    assert!(
        tool.plan(&json!({"query":"hi"}), Path::new("."))
            .err()
            .unwrap()
            .contains("not configured")
    );
}
