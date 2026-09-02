use super::*;
use crate::outbound::{OutboundApprovalDecision, ReviewedOutboundApproval};
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

fn fixture() -> (TempDir, WebFetch) {
    let home = tempfile::tempdir().expect("temporary Xana home");
    let paths = XanaPaths::resolve(Some(home.path().join("home").into_os_string()))
        .expect("absolute fixture paths");
    (home, WebFetch::for_tests(paths))
}

async fn serve_once(response: Vec<u8>) -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = vec![0_u8; 4096];
        let _ = stream.read(&mut request).await;
        stream.write_all(&response).await.expect("response");
        stream.shutdown().await.expect("shutdown");
    });
    (
        Url::parse(&format!("http://{address}/document")).expect("fixture URL"),
        task,
    )
}

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("HTTP/1.1 {status}\r\nConnection: close\r\n").into_bytes();
    for (name, value) in headers {
        bytes.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    bytes.extend_from_slice(body);
    bytes
}

async fn execute(tool: &WebFetch, arguments: Value) -> Result<Value, String> {
    let planned = tool.plan(&arguments, Path::new(".")).expect("plan");
    let operation_id = OperationId::new();
    let review = planned
        .outbound_review
        .clone()
        .expect("outbound review")
        .for_operation(operation_id);
    let context = ToolExecutionContext {
        operation_id,
        events: None,
        outbound_approval: Some(ReviewedOutboundApproval::new(
            review,
            OutboundApprovalDecision::AllowOnce,
        )),
        cleanup: crate::tool::DeferredCleanup::default(),
    };
    tool.execute(&planned, context)
        .await
        .and_then(|output| serde_json::from_str(&output).map_err(|error| error.to_string()))
}

#[tokio::test]
async fn plain_text_is_bounded_attributed_and_untrusted() {
    let (_home, tool) = fixture();
    let (url, server) = serve_once(response(
        "200 OK",
        &[("Content-Type", "text/plain; charset=utf-8")],
        b"public evidence",
    ))
    .await;

    let result = execute(&tool, json!({"url":url.to_string()}))
        .await
        .expect("fetch");

    assert_eq!(result["text"], "public evidence");
    assert_eq!(result["untrusted"], true);
    assert_eq!(result["response_bytes"], 15);
    assert_eq!(result["media_type"], "text/plain");
    assert_eq!(result["cache_status"], "fresh_not_cached");
    assert_eq!(result["artifact"], Value::Null);
    server.await.expect("server");
}

#[tokio::test]
async fn html_parser_ignores_active_content_and_control_bytes() {
    let (_home, tool) = fixture();
    let body = br#"<html><head><script>steal-secret()</script><style>x{}</style></head><body><h1>Safe title</h1><p>Useful text</p></body></html>"#;
    let (url, server) =
        serve_once(response("200 OK", &[("Content-Type", "text/html")], body)).await;

    let result = execute(&tool, json!({"url":url.to_string()}))
        .await
        .expect("fetch");
    let text = result["text"].as_str().expect("text");

    assert!(text.contains("Safe title"));
    assert!(text.contains("Useful text"));
    assert!(!text.contains("steal-secret"));
    assert!(!text.contains("x{}"));
    server.await.expect("server");
}

#[tokio::test]
async fn overflow_preserves_the_complete_source_as_an_immutable_artifact() {
    let (home, tool) = fixture();
    let body = vec![b'a'; MAX_INLINE_BYTES + 1024];
    let (url, server) =
        serve_once(response("200 OK", &[("Content-Type", "text/plain")], &body)).await;

    let result = execute(&tool, json!({"url":url.to_string()}))
        .await
        .expect("fetch");
    let hash = result["artifact"]["reference"]["content_hash"]
        .as_str()
        .expect("artifact hash");

    assert_eq!(result["text"].as_str().unwrap().len(), MAX_INLINE_BYTES);
    assert_eq!(result["text_truncated"], true);
    assert_eq!(
        std::fs::read(home.path().join("home/data/artifacts").join(hash)).unwrap(),
        body
    );
    server.await.expect("server");
}

#[tokio::test]
async fn redirect_is_reported_before_any_unreviewed_destination_request() {
    let (_home, tool) = fixture();
    let (destination, destination_server) = serve_once(response(
        "200 OK",
        &[("Content-Type", "text/plain")],
        b"redirected",
    ))
    .await;
    let location = destination.to_string();
    let (origin, origin_server) = serve_once(response(
        "302 Found",
        &[("Location", &location), ("Content-Type", "text/plain")],
        b"",
    ))
    .await;

    let planned = tool
        .plan(&json!({"url":origin.to_string()}), Path::new("."))
        .unwrap();
    let operation_id = OperationId::new();
    let review = planned
        .outbound_review
        .clone()
        .unwrap()
        .for_operation(operation_id);
    let error = tool
        .execute(
            &planned,
            ToolExecutionContext {
                operation_id,
                events: None,
                outbound_approval: Some(ReviewedOutboundApproval::new(
                    review,
                    OutboundApprovalDecision::AllowOnce,
                )),
                cleanup: crate::tool::DeferredCleanup::default(),
            },
        )
        .await
        .unwrap_err();

    assert!(error.contains("unreviewed redirect"));
    assert!(error.contains(destination.as_str()));
    origin_server.await.expect("origin server");
    destination_server.abort();
}

#[tokio::test]
async fn exact_reviewed_redirect_chain_succeeds() {
    let (_home, tool) = fixture();
    let (destination, destination_server) = serve_once(response(
        "200 OK",
        &[("Content-Type", "text/plain")],
        b"redirected",
    ))
    .await;
    let location = destination.to_string();
    let (origin, origin_server) = serve_once(response(
        "307 Temporary Redirect",
        &[("Location", &location), ("Content-Type", "text/plain")],
        b"",
    ))
    .await;

    let result = execute(
        &tool,
        json!({"url":origin.to_string(),"redirects":[destination.to_string()]}),
    )
    .await
    .expect("reviewed redirect");

    assert_eq!(result["text"], "redirected");
    assert_eq!(result["redirects"].as_array().unwrap().len(), 1);
    origin_server.await.expect("origin server");
    destination_server.await.expect("destination server");
}

#[tokio::test]
async fn oversized_compressed_unsupported_and_malformed_responses_fail_closed() {
    let cases = [
        (
            response(
                "200 OK",
                &[("Content-Type", "text/plain")],
                b"sixteen bytes???",
            ),
            json!({"max_response_bytes":4}),
            "exceeds its byte limit",
        ),
        (
            response(
                "200 OK",
                &[("Content-Type", "text/plain"), ("Content-Encoding", "gzip")],
                b"compressed",
            ),
            json!({}),
            "rejects compressed responses",
        ),
        (
            response(
                "200 OK",
                &[("Content-Type", "application/octet-stream")],
                b"binary",
            ),
            json!({}),
            "supports only UTF-8",
        ),
        (
            response(
                "200 OK",
                &[("Content-Type", "text/plain; charset=iso-8859-1")],
                b"text",
            ),
            json!({}),
            "supports only UTF-8 response text",
        ),
        (
            response("200 OK", &[("Content-Type", "text/plain")], &[0xff, 0xfe]),
            json!({}),
            "not valid UTF-8",
        ),
    ];

    for (response, options, expected) in cases {
        let (_home, tool) = fixture();
        let (url, server) = serve_once(response).await;
        let mut arguments = options.as_object().unwrap().clone();
        arguments.insert("url".into(), json!(url.to_string()));
        let planned = tool
            .plan(&Value::Object(arguments), Path::new("."))
            .expect("plan");
        let operation_id = OperationId::new();
        let review = planned
            .outbound_review
            .clone()
            .unwrap()
            .for_operation(operation_id);
        let error = tool
            .execute(
                &planned,
                ToolExecutionContext {
                    operation_id,
                    events: None,
                    outbound_approval: Some(ReviewedOutboundApproval::new(
                        review,
                        OutboundApprovalDecision::AllowOnce,
                    )),
                    cleanup: crate::tool::DeferredCleanup::default(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            error.contains(expected),
            "{error:?} did not contain {expected:?}"
        );
        server.await.expect("server");
    }
}

#[tokio::test]
async fn cancellation_and_timeout_do_not_wait_for_a_slow_peer() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/slow", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
    });

    let error = fetch_chain(
        &[url],
        McpHttpSecurity {
            allow_loopback_http: true,
        },
        1024,
        Duration::from_secs(2),
        &cancellation,
    )
    .await
    .unwrap_err();

    assert_eq!(error, FetchError::Cancelled);
    server.abort();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/slow", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    });

    let error = fetch_chain(
        &[url],
        McpHttpSecurity {
            allow_loopback_http: true,
        },
        1024,
        Duration::from_millis(20),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();

    assert_eq!(error, FetchError::TimedOut);
    server.abort();
}

#[tokio::test]
async fn oversized_headers_fail_before_body_collection() {
    let (_home, tool) = fixture();
    let oversized = "a".repeat(MAX_RESPONSE_HEADERS_BYTES + 1);
    let (url, server) = serve_once(response(
        "200 OK",
        &[("Content-Type", "text/plain"), ("X-Oversized", &oversized)],
        b"not observed",
    ))
    .await;

    let error = execute(&tool, json!({"url":url.to_string()}))
        .await
        .unwrap_err();

    assert!(error.contains("headers exceed"));
    server.await.expect("server");
}

#[tokio::test]
async fn unavailable_policy_state_prevents_any_network_attempt() {
    let (home, tool) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://{}/document",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let data = home.path().join("home/data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("interoperable"), b"not a directory").unwrap();

    let planned = tool
        .plan(&json!({"url":url.to_string()}), Path::new("."))
        .unwrap();
    let operation_id = OperationId::new();
    let review = planned
        .outbound_review
        .clone()
        .unwrap()
        .for_operation(operation_id);
    let error = tool
        .execute(
            &planned,
            ToolExecutionContext {
                operation_id,
                events: None,
                outbound_approval: Some(ReviewedOutboundApproval::new(
                    review,
                    OutboundApprovalDecision::AllowOnce,
                )),
                cleanup: crate::tool::DeferredCleanup::default(),
            },
        )
        .await
        .unwrap_err();

    assert!(error.contains("interoperable"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "policy persistence failure must stop before a network connection"
    );
}

#[tokio::test]
async fn private_addresses_credentials_downgrades_and_cycles_are_rejected() {
    let (_home, test_tool) = fixture();
    assert!(
        test_tool
            .plan(
                &json!({"url":"http://user:password@127.0.0.1/private"}),
                Path::new(".")
            )
            .err()
            .unwrap()
            .contains("credentials")
    );
    assert!(
        test_tool
            .plan(
                &json!({"url":"https://example.com/","redirects":["http://127.0.0.1/"]}),
                Path::new(".")
            )
            .err()
            .unwrap()
            .contains("HTTPS-to-HTTP")
    );
    assert!(
        test_tool
            .plan(
                &json!({"url":"http://127.0.0.1/a","redirects":["http://127.0.0.1/a"]}),
                Path::new(".")
            )
            .err()
            .unwrap()
            .contains("cycle")
    );

    let cancellation = CancellationToken::new();
    let error = fetch_chain(
        &[Url::parse("https://127.0.0.1/private").unwrap()],
        McpHttpSecurity::default(),
        1024,
        Duration::from_secs(1),
        &cancellation,
    )
    .await
    .unwrap_err();
    assert_eq!(error, FetchError::DnsOrAddressPolicy);
}

#[test]
fn review_is_exact_content_free_and_structurally_stable() {
    let (_home, tool) = fixture();
    let planned = tool
        .plan(
            &json!({
                "url":"http://127.0.0.1:3000/a?q=one",
                "redirects":["http://127.0.0.1:3001/b?q=two"]
            }),
            Path::new("."),
        )
        .expect("plan");
    let review = planned.outbound_review.clone().expect("review");

    assert_eq!(review.classes, vec![OutboundDataClass::PromptText]);
    assert_eq!(review.items.len(), 1);
    assert_eq!(review.recipient.kind, RecipientKind::WebFetch);
    assert!(review.recipient.destination.contains(" -> "));
    assert!(review.render().contains("explicit GET URL chain"));
    assert!(!review.render().contains("password"));
}
