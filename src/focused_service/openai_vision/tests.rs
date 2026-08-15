use super::*;
use crate::{
    artifact::ArtifactStore,
    config::CredentialReference,
    identity::{OperationId, PrincipalId},
};
use base64::Engine as _;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn route(base_url: String) -> ResolvedServiceRoute {
    ResolvedServiceRoute {
        name: "describe".into(),
        description: Some("fixture vision route".into()),
        connection: "openai-vision".into(),
        adapter: OPENAI_ADAPTER_ID.into(),
        base_url: Some(base_url),
        credential: Some(CredentialReference::Environment {
            variable: "OPENAI_API_KEY".into(),
        }),
        model: "gpt-vision-fixture".into(),
        operation: ServiceOperation::VisionAnalyze,
        options: BTreeMap::new(),
        descriptor: descriptor(OPENAI_ADAPTER_ID),
        allowed_outbound: [
            crate::config::OutboundDataClass::PromptText,
            crate::config::OutboundDataClass::SelectedArtifacts,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>(),
        is_default: true,
    }
}

async fn fake_stream_server() -> (String, tokio::sync::oneshot::Receiver<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
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
            assert!(count > 0);
            request.extend_from_slice(&chunk[..count]);
        }
        let request: Value =
            serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
        let _ = send.send(request);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a red square\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (format!("http://{address}/v1"), receive)
}

#[tokio::test]
async fn analysis_sends_one_bounded_multimodal_request_and_returns_attributed_text() {
    let (base_url, captured) = fake_stream_server().await;
    let directory = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().join("artifacts"));
    let owner = PrincipalId::new();
    let image_bytes = b"fixture-image-bytes";
    let (artifact, _) = store.put(image_bytes, "image/png", owner).unwrap();
    let operation_id = OperationId::new();
    let adapter = OpenAiVisionAdapter::new(
        VisionProvider::OpenAi,
        SecretString::new("test-secret".into()).unwrap(),
    );

    let result = adapter
        .execute(
            FocusedServiceRequest {
                operation_id,
                route: route(base_url),
                prompt: "What is shown?".into(),
                input_artifacts: vec![artifact],
            },
            FocusedServiceContext {
                artifacts: store,
                owner,
                cancellation: CancellationToken::new(),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.derived_text.as_deref(), Some("a red square"));
    assert_eq!(result.provenance.operation_id, operation_id);
    assert_eq!(result.provenance.route, "describe");
    assert_eq!(result.provenance.model, "gpt-vision-fixture");
    assert!(result.untrusted);
    assert!(result.artifacts.is_empty());
    assert_eq!(result.usage.input_units, Some(12));
    assert_eq!(result.usage.output_units, Some(3));
    assert!(result.usage_available);
    assert!(!result.cost_available);

    let request = captured.await.unwrap();
    assert_eq!(request["model"], "gpt-vision-fixture");
    assert_eq!(request["stream"], true);
    assert_eq!(request["stream_options"]["include_usage"], true);
    let parts = request["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert!(
        parts[0]["text"]
            .as_str()
            .unwrap()
            .contains("What is shown?")
    );
    assert_eq!(
        parts[1]["image_url"]["url"],
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(image_bytes)
        )
    );
    let encoded = request.to_string();
    assert!(!encoded.contains("test-secret"));
    assert!(!encoded.contains(directory.path().to_str().unwrap()));
}

#[tokio::test]
async fn cancellation_before_dispatch_performs_no_network_request() {
    let directory = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().join("artifacts"));
    let owner = PrincipalId::new();
    let (artifact, _) = store.put(b"image", "image/png", owner).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let adapter = OpenAiVisionAdapter::new(
        VisionProvider::OpenAi,
        SecretString::new("test-secret".into()).unwrap(),
    );

    let result = adapter
        .execute(
            FocusedServiceRequest {
                operation_id: OperationId::new(),
                route: route("http://127.0.0.1:9/v1".into()),
                prompt: "What is shown?".into(),
                input_artifacts: vec![artifact],
            },
            FocusedServiceContext {
                artifacts: store,
                owner,
                cancellation,
            },
        )
        .await;

    assert!(matches!(result, Err(FocusedServiceError::Cancelled)));
}

#[tokio::test]
async fn focused_vision_refuses_redirects_to_an_unreviewed_destination() {
    let redirected = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirected_address = redirected.local_addr().unwrap();
    let (observed_send, observed_receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let observed =
            tokio::time::timeout(std::time::Duration::from_millis(500), redirected.accept())
                .await
                .is_ok();
        let _ = observed_send.send(observed);
    });

    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_address = origin.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        let response = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{redirected_address}/v1/chat/completions\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let directory = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().join("artifacts"));
    let owner = PrincipalId::new();
    let (artifact, _) = store.put(b"image", "image/png", owner).unwrap();
    let adapter = OpenAiVisionAdapter::new(
        VisionProvider::OpenAi,
        SecretString::new("test-secret".into()).unwrap(),
    );

    let result = adapter
        .execute(
            FocusedServiceRequest {
                operation_id: OperationId::new(),
                route: route(format!("http://{origin_address}/v1")),
                prompt: "What is shown?".into(),
                input_artifacts: vec![artifact],
            },
            FocusedServiceContext {
                artifacts: store,
                owner,
                cancellation: CancellationToken::new(),
            },
        )
        .await;

    assert!(result.is_err());
    assert!(!observed_receive.await.unwrap());
}
