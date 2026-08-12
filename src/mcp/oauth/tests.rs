use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Default)]
struct FakeStore {
    values: Arc<std::sync::Mutex<BTreeMap<String, String>>>,
    fail_writes: Arc<AtomicBool>,
}

impl SecretStore for FakeStore {
    fn get(&self, id: &str) -> Result<Option<String>, CredentialError> {
        Ok(self.values.lock().unwrap().get(id).cloned())
    }

    fn set(&self, id: &str, secret: &str) -> Result<(), CredentialError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(CredentialError::StoreUnavailable("fixture".into()));
        }
        self.values
            .lock()
            .unwrap()
            .insert(id.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<bool, CredentialError> {
        Ok(self.values.lock().unwrap().remove(id).is_some())
    }
}

fn reference() -> McpOAuthReference {
    McpOAuthReference {
        credential_id: "mcp.docs.oauth".into(),
        issuer: "https://issuer.example/".into(),
        client_id: "xana-local".into(),
        resource: "https://mcp.example/service".into(),
        scopes: BTreeSet::from(["tools.read".into()]),
    }
}

fn token(access: &str, refresh: Option<&str>, expires_at_unix: u64) -> McpOAuthToken {
    let reference = reference();
    McpOAuthToken {
        access_token: access.into(),
        refresh_token: refresh.map(str::to_owned),
        token_type: "Bearer".into(),
        expires_at_unix,
        scopes: reference.scopes.clone(),
        issuer: reference.issuer,
        client_id: reference.client_id,
        resource: reference.resource,
    }
}

fn metadata() -> McpOAuthMetadata {
    McpOAuthMetadata {
        issuer: Url::parse("https://issuer.example/").unwrap(),
        authorization_endpoint: Url::parse("https://issuer.example/authorize").unwrap(),
        token_endpoint: Url::parse("https://issuer.example/token").unwrap(),
        scopes_supported: BTreeSet::from(["tools.read".into()]),
        code_challenge_s256: true,
        authorization_response_iss_parameter_supported: true,
    }
}

#[tokio::test]
async fn discovers_exact_protected_resource_and_authorization_metadata() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let issuer = format!("http://{address}/");
    let resource = format!("http://{address}/mcp");
    let resource_document = format!(
        "{{\"resource\":\"{resource}\",\"authorization_servers\":[\"{issuer}\"],\"scopes_supported\":[\"tools.read\"]}}"
    );
    let authorization_document = format!(
        "{{\"issuer\":\"{issuer}\",\"authorization_endpoint\":\"{issuer}authorize\",\"token_endpoint\":\"{issuer}token\",\"scopes_supported\":[\"tools.read\"],\"code_challenge_methods_supported\":[\"S256\"],\"authorization_response_iss_parameter_supported\":true}}"
    );
    let server = tokio::spawn(async move {
        let mut paths = Vec::new();
        for body in [resource_document, authorization_document] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 2048];
            let count = stream.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..count]);
            paths.push(
                request
                    .lines()
                    .next()
                    .unwrap()
                    .split_ascii_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned(),
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        paths
    });
    let challenge = McpAuthChallenge {
        resource_metadata: Url::parse(&format!(
            "http://{address}/.well-known/oauth-protected-resource"
        ))
        .unwrap(),
        scopes: BTreeSet::from(["tools.read".into()]),
        insufficient_scope: false,
    };
    let discovery = McpOAuthClient::allowing_loopback_http()
        .discover(
            &challenge,
            &Url::parse(&resource).unwrap(),
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(discovery.protected_resource.resource.as_str(), resource);
    assert_eq!(discovery.authorization_server.issuer.as_str(), issuer);
    assert_eq!(
        server.await.unwrap(),
        vec![
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-authorization-server"
        ]
    );
}

#[test]
fn parses_bounded_bearer_challenge_and_metadata() {
    let challenge = McpAuthChallenge::parse(
        "Bearer resource_metadata=\"https://mcp.example/.well-known/oauth-protected-resource\", scope=\"tools.read resources.read\", error=\"insufficient_scope\"",
    )
    .unwrap();
    assert!(challenge.insufficient_scope);
    assert_eq!(challenge.scopes.len(), 2);

    let bytes = br#"{
        "issuer":"https://issuer.example/",
        "authorization_endpoint":"https://issuer.example/authorize",
        "token_endpoint":"https://issuer.example/token",
        "scopes_supported":["tools.read"],
        "code_challenge_methods_supported":["S256"],
        "authorization_response_iss_parameter_supported":true,
        "extension_field":"ignored"
    }"#;
    let parsed =
        McpOAuthMetadata::parse(bytes, &Url::parse("https://issuer.example/").unwrap()).unwrap();
    assert!(parsed.code_challenge_s256);
    assert!(parsed.authorization_response_iss_parameter_supported);

    assert!(matches!(
        McpOAuthMetadata::parse(bytes, &Url::parse("https://other.example/").unwrap()),
        Err(McpOAuthError::IssuerMismatch)
    ));
}

#[tokio::test]
async fn loopback_flow_uses_pkce_resource_state_and_issuer() {
    let flow = McpOAuthFlow::begin(
        &metadata(),
        "xana-local",
        "https://mcp.example/service",
        &BTreeSet::from(["tools.read".into()]),
    )
    .await
    .unwrap();
    let authorization = flow.authorization_url().clone();
    let redirect = Url::parse(flow.redirect_uri()).unwrap();
    let query = authorization
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["resource"], "https://mcp.example/service");
    assert_eq!(query["scope"], "tools.read");
    assert_ne!(query["code_challenge"], query["state"]);

    let cancellation = CancellationToken::new();
    let waiter = tokio::spawn(async move { flow.wait(&cancellation).await });
    let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    let path = format!(
        "/oauth/callback?code=opaque-code&state={}&iss={}",
        query["state"], "https%3A%2F%2Fissuer.example%2F"
    );
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(
        String::from_utf8(response)
            .unwrap()
            .contains("Authorization complete")
    );
    let callback = waiter.await.unwrap().unwrap();
    assert_eq!(callback.code, "opaque-code");
    assert_eq!(callback.redirect_uri, redirect.as_str());
    assert!(!callback.verifier.is_empty());
}

#[tokio::test]
async fn loopback_flow_rejects_state_and_honors_cancellation() {
    let flow = McpOAuthFlow::begin(
        &metadata(),
        "xana-local",
        "https://mcp.example/service",
        &BTreeSet::new(),
    )
    .await
    .unwrap();
    let redirect = Url::parse(flow.redirect_uri()).unwrap();
    let cancellation = CancellationToken::new();
    let waiter = tokio::spawn(async move { flow.wait(&cancellation).await });
    let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    stream
        .write_all(b"GET /oauth/callback?code=x&state=wrong HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(McpOAuthError::StateMismatch)
    ));

    let flow = McpOAuthFlow::begin(
        &metadata(),
        "xana-local",
        "https://mcp.example/service",
        &BTreeSet::new(),
    )
    .await
    .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        flow.wait(&cancellation).await,
        Err(McpOAuthError::Cancelled)
    ));
}

#[test]
fn token_store_round_trip_logout_binding_and_redaction() {
    let directory = tempfile::tempdir().unwrap();
    let store = McpOAuthStore::new(FakeStore::default(), directory.path().join("refresh.lock"));
    let reference = reference();
    let token = token("access-secret", Some("refresh-secret"), 10_000);
    store.save(&reference, &token).unwrap();
    let loaded = store.load(&reference).unwrap();
    assert_eq!(loaded.access_secret().unwrap().expose(), "access-secret");
    let rendered = format!("{loaded:?}");
    assert!(!rendered.contains("access-secret"));
    assert!(!rendered.contains("refresh-secret"));

    let mut wrong = reference.clone();
    wrong.resource = "https://other.example/".into();
    assert!(matches!(
        store.load(&wrong),
        Err(McpOAuthError::TokenBinding)
    ));
    assert!(store.logout(&reference).unwrap());
    assert!(matches!(
        store.load(&reference),
        Err(McpOAuthError::CredentialMissing)
    ));
}

#[tokio::test]
async fn concurrent_expiry_refreshes_once_and_persists_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(McpOAuthStore::new(
        FakeStore::default(),
        directory.path().join("refresh.lock"),
    ));
    let reference = Arc::new(reference());
    store
        .save(&reference, &token("old-access", Some("old-refresh"), 1))
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let store = Arc::clone(&store);
        let reference = Arc::clone(&reference);
        let calls = Arc::clone(&calls);
        tasks.push(tokio::spawn(async move {
            store
                .access_or_refresh(&reference, 1000, move |refresh| async move {
                    assert_eq!(refresh, "old-refresh");
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    Ok(token("new-access", Some("new-refresh"), 10_000))
                })
                .await
                .unwrap()
                .expose()
                .to_owned()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), "new-access");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load(&reference).unwrap().refresh_token.as_deref(),
        Some("new-refresh")
    );
}

#[tokio::test]
async fn refresh_persistence_failure_never_returns_unstored_token() {
    let directory = tempfile::tempdir().unwrap();
    let backend = FakeStore::default();
    let fail_writes = Arc::clone(&backend.fail_writes);
    let store = McpOAuthStore::new(backend.clone(), directory.path().join("refresh.lock"));
    let reference = reference();
    store
        .save(&reference, &token("old-access", Some("old-refresh"), 1))
        .unwrap();
    fail_writes.store(true, Ordering::SeqCst);
    assert!(matches!(
        store
            .access_or_refresh(&reference, 1000, |_| async {
                Ok(token("new-access", Some("new-refresh"), 10_000))
            })
            .await,
        Err(McpOAuthError::StoreUnavailable)
    ));
    fail_writes.store(false, Ordering::SeqCst);
    assert_eq!(
        store
            .load(&reference)
            .unwrap()
            .access_secret()
            .unwrap()
            .expose(),
        "old-access"
    );
}

async fn token_server(
    response_status: &str,
    response_body: &'static str,
) -> (Url, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let status = response_status.to_owned();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..count]);
            let Some(end) = request.windows(4).position(|value| value == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{response_body}",
            response_body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    (
        Url::parse(&format!("http://{address}/token")).unwrap(),
        task,
    )
}

#[tokio::test]
async fn token_exchange_binds_resource_and_accepts_extensions() {
    let (token_endpoint, observed) = token_server(
        "200 OK",
        r#"{"access_token":"new-access","refresh_token":"new-refresh","token_type":"Bearer","expires_in":3600,"scope":"tools.read","extension":"ok"}"#,
    )
    .await;
    let mut metadata = metadata();
    metadata.token_endpoint = token_endpoint;
    let reference = reference();
    let callback = OAuthCallback {
        code: "opaque-code".into(),
        verifier: "verifier".into(),
        redirect_uri: "http://127.0.0.1:4444/oauth/callback".into(),
    };
    let token = McpOAuthClient::allowing_loopback_http()
        .exchange(&metadata, &reference, callback, None)
        .await
        .unwrap();
    assert_eq!(token.access_secret().unwrap().expose(), "new-access");
    let request = observed.await.unwrap();
    assert!(request.contains("resource=https%3A%2F%2Fmcp.example%2Fservice"));
    assert!(request.contains("code_verifier=verifier"));
}

#[tokio::test]
async fn revoked_refresh_is_distinct_and_does_not_fallback() {
    let (token_endpoint, observed) =
        token_server("400 Bad Request", r#"{"error":"invalid_grant"}"#).await;
    let mut metadata = metadata();
    metadata.token_endpoint = token_endpoint;
    let error = McpOAuthClient::allowing_loopback_http()
        .refresh(&metadata, &reference(), "revoked-secret".into(), None)
        .await
        .unwrap_err();
    assert!(matches!(error, McpOAuthError::InvalidGrant));
    let wire = observed.await.unwrap();
    assert!(wire.contains("refresh_token=revoked-secret"));
    assert!(!format!("{error:?} {error}").contains("revoked-secret"));
}
