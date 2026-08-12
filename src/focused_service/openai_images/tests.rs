use super::*;
use crate::{
    config::CredentialReference,
    identity::{OperationId, PrincipalId},
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn route(base_url: String, options: BTreeMap<String, String>) -> ResolvedServiceRoute {
    ResolvedServiceRoute {
        name: "illustrate".into(),
        description: Some("fixture".into()),
        connection: "openai-image".into(),
        adapter: ADAPTER_ID.into(),
        base_url: Some(base_url),
        credential: Some(CredentialReference::Environment {
            variable: "OPENAI_API_KEY".into(),
        }),
        model: "gpt-image-2".into(),
        operation: ServiceOperation::ImageGenerate,
        options,
        descriptor: descriptor(),
        allowed_outbound: BTreeSet::new(),
        is_default: true,
    }
}

async fn fake_server(
    status: &str,
    content_type: &str,
    body: Value,
) -> (String, tokio::sync::oneshot::Receiver<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    let status = status.to_owned();
    let content_type = content_type.to_owned();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&chunk[..count]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap();
        while request.len() - header_end < length {
            let count = stream.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..count]);
        }
        let request: Value =
            serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
        let _ = send.send(request);
        let body = serde_json::to_vec(&body).unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
    });
    (format!("http://{address}/v1"), receive)
}

fn context(directory: &tempfile::TempDir) -> FocusedServiceContext {
    FocusedServiceContext {
        artifacts: ArtifactStore::new(directory.path().to_owned()),
        owner: PrincipalId::new(),
        cancellation: CancellationToken::new(),
    }
}

#[tokio::test]
async fn fixture_generation_sends_exact_request_and_publishes_one_artifact() {
    let png = b"\x89PNG\r\n\x1a\nfixture";
    let (base, captured) = fake_server(
        "200 OK",
        "application/json",
        json!({
            "created":1,
            "output_format":"png",
            "data":[{"b64_json":STANDARD.encode(png)}],
            "usage":{"input_tokens":12,"output_tokens":34}
        }),
    )
    .await;
    let adapter = OpenAiImageAdapter::new(SecretString::new("test-key".into()).unwrap())
        .unwrap()
        .allow_http();
    let directory = tempfile::tempdir().unwrap();
    let result = adapter
        .execute(
            FocusedServiceRequest {
                operation_id: OperationId::new(),
                route: route(
                    base,
                    BTreeMap::from([
                        ("quality".into(), "high".into()),
                        ("size".into(), "1024x1024".into()),
                    ]),
                ),
                prompt: "A tiny test image".into(),
                input_artifacts: Vec::new(),
            },
            context(&directory),
        )
        .await
        .unwrap();
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(result.artifacts[0].media_type, "image/png");
    assert_eq!(result.provenance.route, "illustrate");
    assert_eq!(result.usage.input_units, Some(12));
    assert!(result.usage_available);
    assert!(!result.cost_available);
    let request = captured.await.unwrap();
    assert_eq!(request["model"], "gpt-image-2");
    assert_eq!(request["n"], 1);
    assert_eq!(request["quality"], "high");
    assert_eq!(request["prompt"], "A tiny test image");
    assert!(!request.to_string().contains("test-key"));
}

#[tokio::test]
async fn unsupported_options_fail_before_network_io() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let adapter = OpenAiImageAdapter::new(SecretString::new("test-key".into()).unwrap())
        .unwrap()
        .allow_http();
    let directory = tempfile::tempdir().unwrap();
    let result = adapter
        .execute(
            FocusedServiceRequest {
                operation_id: OperationId::new(),
                route: route(base, BTreeMap::from([("n".into(), "2".into())])),
                prompt: "fixture".into(),
                input_artifacts: Vec::new(),
            },
            context(&directory),
        )
        .await;
    assert!(matches!(
        result,
        Err(FocusedServiceError::InvalidRequest(_))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn provider_errors_are_typed_without_exposing_response_bodies() {
    for (status, code, expected) in [
        (
            "401 Unauthorized",
            "secret leaked body",
            FocusedServiceError::Authentication,
        ),
        (
            "429 Too Many Requests",
            "insufficient_quota",
            FocusedServiceError::Quota,
        ),
        (
            "400 Bad Request",
            "content_policy_violation",
            FocusedServiceError::ContentPolicy,
        ),
    ] {
        let (base, _) = fake_server(
            status,
            "application/json",
            json!({"error":{"code":code,"message":"private provider text"}}),
        )
        .await;
        let adapter = OpenAiImageAdapter::new(SecretString::new("test-key".into()).unwrap())
            .unwrap()
            .allow_http();
        let directory = tempfile::tempdir().unwrap();
        let result = adapter
            .execute(
                FocusedServiceRequest {
                    operation_id: OperationId::new(),
                    route: route(base, BTreeMap::new()),
                    prompt: "fixture".into(),
                    input_artifacts: Vec::new(),
                },
                context(&directory),
            )
            .await;
        assert_eq!(result.unwrap_err(), expected);
        assert!(!expected.to_string().contains("private provider text"));
    }
}

#[tokio::test]
async fn cancellation_before_dispatch_sends_zero_bytes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let adapter = OpenAiImageAdapter::new(SecretString::new("test-key".into()).unwrap())
        .unwrap()
        .allow_http();
    let directory = tempfile::tempdir().unwrap();
    let context = context(&directory);
    context.cancellation.cancel();
    let result = adapter
        .execute(
            FocusedServiceRequest {
                operation_id: OperationId::new(),
                route: route(base, BTreeMap::new()),
                prompt: "fixture".into(),
                input_artifacts: Vec::new(),
            },
            context,
        )
        .await;
    assert_eq!(result.unwrap_err(), FocusedServiceError::Cancelled);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn gpt_image_two_size_rules_are_bounded_and_explicit() {
    assert!(valid_size("1024x1024"));
    assert!(valid_size("1536x864"));
    assert!(valid_size("auto"));
    assert!(!valid_size("1025x1024"));
    assert!(!valid_size("4096x4096"));
    assert!(!valid_size("100x100"));
}
