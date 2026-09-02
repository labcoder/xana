use super::*;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn connection(kind: ProviderKind, base_url: Option<String>) -> ConnectionConfig {
    ConnectionConfig {
        id: kind.as_str().to_owned(),
        kind,
        base_url,
        credential: None,
        models: Default::default(),
        codex_program: None,
        codex_home: None,
    }
}

#[derive(Clone)]
struct Response {
    status: u16,
    body: String,
    declared_length: Option<usize>,
}

async fn fixture_server(
    responses: Vec<Response>,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1_024];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            captured
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request).into_owned());
            let reason = match response.status {
                200 => "OK",
                401 => "Unauthorized",
                429 => "Too Many Requests",
                _ => "Error",
            };
            let length = response.declared_length.unwrap_or(response.body.len());
            let wire = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.status, reason, length, response.body
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}/api/v1"), requests, task)
}

#[tokio::test]
async fn openrouter_key_and_management_credit_facts_are_normalized_and_redacted() {
    let (base, requests, server) = fixture_server(vec![
        Response {
            status: 200,
            body: serde_json::json!({
                "data": {
                    "usage": 1.25,
                    "limit": 10.0,
                    "limit_remaining": 8.75,
                    "limit_reset": "monthly",
                    "is_management_key": true
                }
            })
            .to_string(),
            declared_length: None,
        },
        Response {
            status: 200,
            body: serde_json::json!({
                "data": {"total_credits": 20.0, "total_usage": 3.5}
            })
            .to_string(),
            declared_length: None,
        },
    ])
    .await;
    let connection = connection(ProviderKind::OpenRouter, Some(base));
    let secret = SecretString::new("secret-sentinel".into()).unwrap();
    let observations = LiveAccountUsageSource::new()
        .fetch_openrouter_authorized(&connection, 1_000, &CancellationToken::new(), &secret)
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].amounts.cost_microunits, Some(1_250_000));
    assert_eq!(
        observations[0]
            .quota
            .as_ref()
            .and_then(|value| value.used_percent_basis_points),
        Some(1_250)
    );
    assert_eq!(
        observations[1]
            .credits
            .as_ref()
            .and_then(|value| value.remaining_microunits),
        Some(16_500_000)
    );
    assert!(observations.iter().all(|value| value.validate().is_ok()));
    let requests = requests.lock().unwrap();
    assert!(requests[0].starts_with("GET /api/v1/key "));
    assert!(requests[1].starts_with("GET /api/v1/credits "));
    assert!(requests.iter().all(|request| {
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-sentinel")
    }));
    let serialized = serde_json::to_string(&observations).unwrap();
    assert!(!serialized.contains("secret-sentinel"));
}

#[tokio::test]
async fn openrouter_inference_key_does_not_gain_management_authority() {
    let (base, requests, server) = fixture_server(vec![Response {
        status: 200,
        body: serde_json::json!({
            "data": {"usage": 2.0, "is_management_key": false}
        })
        .to_string(),
        declared_length: None,
    }])
    .await;
    let connection = connection(ProviderKind::OpenRouter, Some(base));
    let secret = SecretString::new("inference-only".into()).unwrap();
    let observations = LiveAccountUsageSource::new()
        .fetch_openrouter_authorized(&connection, 5, &CancellationToken::new(), &secret)
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(matches!(
        observations[1].availability,
        AvailabilityV1::PermissionRequired { ref code }
            if code == "usage.account_management_credential_required"
    ));
}

#[tokio::test]
async fn openrouter_rejects_oversized_and_malformed_responses_without_body_disclosure() {
    let (base, _, server) = fixture_server(vec![Response {
        status: 200,
        body: String::new(),
        declared_length: Some(MAX_ACCOUNT_RESPONSE_BYTES + 1),
    }])
    .await;
    let connection = connection(ProviderKind::OpenRouter, Some(base));
    let secret = SecretString::new("not-in-error".into()).unwrap();
    let error = LiveAccountUsageSource::new()
        .fetch_openrouter_authorized(&connection, 5, &CancellationToken::new(), &secret)
        .await
        .unwrap_err();
    server.await.unwrap();

    assert_eq!(error.code, "usage.response_too_large");
    assert!(!error.retryable);
}

#[tokio::test]
async fn openrouter_distinguishes_permission_retryable_and_malformed_failures() {
    for (response, expected_code, retryable, permission) in [
        (
            Response {
                status: 401,
                body: "denied".into(),
                declared_length: None,
            },
            "usage.credential_rejected",
            false,
            true,
        ),
        (
            Response {
                status: 429,
                body: "slow down".into(),
                declared_length: None,
            },
            "usage.provider_retryable",
            true,
            false,
        ),
        (
            Response {
                status: 200,
                body: "not-json".into(),
                declared_length: None,
            },
            "usage.invalid_response",
            false,
            false,
        ),
    ] {
        let (base, _, server) = fixture_server(vec![response]).await;
        let connection = connection(ProviderKind::OpenRouter, Some(base));
        let secret = SecretString::new("redacted".into()).unwrap();
        let result = LiveAccountUsageSource::new()
            .fetch_openrouter_authorized(&connection, 1, &CancellationToken::new(), &secret)
            .await;
        server.await.unwrap();
        if permission {
            let observations = result.unwrap();
            assert!(matches!(
                observations[0].availability,
                AvailabilityV1::PermissionRequired { ref code } if code == expected_code
            ));
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, expected_code);
            assert_eq!(error.retryable, retryable);
        }
    }
}

#[tokio::test]
async fn unsupported_and_management_gated_providers_never_contact_an_account_endpoint() {
    let source = LiveAccountUsageSource::new();
    for (kind, expected) in [
        (ProviderKind::Ollama, AvailabilityV1::Unsupported),
        (ProviderKind::OpenAiCompat, AvailabilityV1::Unsupported),
    ] {
        let observations = source
            .fetch(&connection(kind, None), 10, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(observations[0].availability, expected);
    }
    for kind in [ProviderKind::OpenAi, ProviderKind::Anthropic] {
        let observations = source
            .fetch(&connection(kind, None), 10, &CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            observations[0].availability,
            AvailabilityV1::PermissionRequired { ref code }
                if code == "usage.account_management_credential_required"
        ));
    }
}

#[test]
fn codex_rate_limit_windows_have_stable_scopes_and_reset_periods() {
    let connection = connection(ProviderKind::Codex, None);
    let observations = parse_codex_limits(
        &connection,
        9_000,
        &serde_json::json!({
            "rateLimitsByLimitId": {
                "codex": {
                    "primary": {
                        "usedPercent": 12.5,
                        "windowDurationMins": 300,
                        "resetsAt": 1234
                    },
                    "secondary": null
                }
            }
        }),
    );

    assert_eq!(observations.len(), 1);
    assert!(matches!(
        &observations[0].scope,
        UsageScopeV1::RateLimitBucket { connection, bucket }
            if connection == "codex" && bucket == "codex-primary"
    ));
    assert_eq!(observations[0].period, "reset-1234000");
    let limit = observations[0].rate_limit.as_ref().unwrap();
    assert_eq!(limit.used_percent_basis_points, Some(1_250));
    assert_eq!(limit.remaining, Some(8_750));
    assert_eq!(limit.window_millis, Some(18_000_000));
    observations[0].validate().unwrap();
}

#[test]
fn malformed_codex_limit_payload_is_explicitly_unavailable() {
    let connection = connection(ProviderKind::Codex, None);
    let observations = parse_codex_limits(&connection, 1, &serde_json::json!({"unexpected": true}));

    assert!(matches!(
        observations[0].availability,
        AvailabilityV1::Unavailable { ref code }
            if code == "usage.codex_rate_limits_unavailable"
    ));
    assert_eq!(observations[0].source, FactSourceV1::ManagedRuntime);
}

#[test]
fn credits_can_be_exhausted_without_fabricating_a_negative_balance() {
    let connection = connection(ProviderKind::OpenRouter, None);
    let observation = openrouter_credit_observation(
        &connection,
        1,
        &serde_json::json!({"data": {"total_credits": 1.0, "total_usage": 2.0}}),
    );

    assert_eq!(
        observation
            .credits
            .as_ref()
            .and_then(|value| value.remaining_microunits),
        None
    );
    observation.validate().unwrap();
}
