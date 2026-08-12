use super::*;
use crate::identity::{OperationId, PrincipalId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn route(base_url: String) -> ResolvedServiceRoute {
    ResolvedServiceRoute {
        name: "fast-art".into(),
        description: None,
        connection: "openrouter-image".into(),
        adapter: ADAPTER_ID.into(),
        base_url: Some(base_url),
        credential: None,
        model: "recraft/recraft-v4".into(),
        operation: ServiceOperation::ImageGenerate,
        options: BTreeMap::from([
            ("provider_only".into(), "recraft".into()),
            ("aspect_ratio".into(), "16:9".into()),
        ]),
        descriptor: descriptor(),
        allowed_outbound: BTreeSet::new(),
        is_default: false,
    }
}

#[tokio::test]
async fn dedicated_api_pins_provider_disables_fallback_and_records_cost() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut chunk).await.unwrap();
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
        send.send(request).unwrap();
        let response = serde_json::to_vec(&json!({
            "data":[{"b64_json":STANDARD.encode(b"\x89PNG\r\n\x1a\nopenrouter"),"media_type":"image/png"}],
            "usage":{"prompt_tokens":11,"completion_tokens":22,"cost":0.04}
        })).unwrap();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).as_bytes()).await.unwrap();
        stream.write_all(&response).await.unwrap();
    });
    let adapter = OpenRouterImageAdapter::new(SecretString::new("router-key".into()).unwrap())
        .unwrap()
        .allow_http();
    let directory = tempfile::tempdir().unwrap();
    let result = adapter
        .execute(
            FocusedServiceRequest {
                operation_id: OperationId::new(),
                route: route(format!("http://{address}/api/v1")),
                prompt: "a bounded fixture".into(),
                input_artifacts: Vec::new(),
            },
            FocusedServiceContext {
                artifacts: ArtifactStore::new(directory.path().to_owned()),
                owner: PrincipalId::new(),
                cancellation: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.usage.cost_microusd, Some(40_000));
    assert!(result.cost_available);
    assert_eq!(result.artifacts[0].media_type, "image/png");
    let request = receive.await.unwrap();
    assert_eq!(request["provider"]["only"], json!(["recraft"]));
    assert_eq!(request["provider"]["allow_fallbacks"], false);
    assert_eq!(request["aspect_ratio"], "16:9");
    assert!(!request.to_string().contains("router-key"));
}

#[test]
fn provider_pin_and_options_are_required_and_bounded() {
    let mut missing = route("https://openrouter.ai/api/v1".into());
    missing.options.remove("provider_only");
    let request = FocusedServiceRequest {
        operation_id: OperationId::new(),
        route: missing,
        prompt: "fixture".into(),
        input_artifacts: Vec::new(),
    };
    assert!(matches!(
        OpenRouterImageAdapter::body(&request),
        Err(FocusedServiceError::InvalidRequest(_))
    ));
    assert!(valid_aspect_ratio("21:9"));
    assert!(!valid_aspect_ratio("100:1"));
    assert_eq!(cost_microusd(0.04), Some(40_000));
}
