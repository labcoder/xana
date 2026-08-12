use super::*;
use crate::config::ExternalAgentDeclaration;
use std::ffi::OsString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn card(interface: &str, name: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name":name,
        "description":"A bounded test agent",
        "supportedInterfaces":[{
            "url":interface,
            "protocolBinding":"JSONRPC",
            "protocolVersion":"1.0"
        }],
        "provider":{"url":"https://provider.example","organization":"Example"},
        "version":"2.0.0",
        "capabilities":{"streaming":true},
        "securitySchemes":{},
        "securityRequirements":[],
        "defaultInputModes":["text/plain"],
        "defaultOutputModes":["text/plain"],
        "skills":[{
            "id":"research",
            "name":"Research",
            "description":"Research a bounded question",
            "tags":["research"]
        }]
    }))
    .unwrap()
}

async fn one_response_server(body: Vec<u8>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 8192];
        let _ = stream.read(&mut request).await.unwrap();
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
    });
    format!("http://{address}/.well-known/agent-card.json")
}

#[tokio::test]
async fn explicit_refresh_trust_identity_change_and_untrust_are_separate() {
    let home = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(home.path()))).unwrap();
    let interface = one_response_server(Vec::new()).await;
    // The first listener above only supplies an address for the interface. No
    // request is made to it; discovery uses its own exact endpoint below.
    let endpoint = one_response_server(card(&interface, "Research Agent")).await;
    let declaration = ExternalAgentDeclaration {
        endpoint: endpoint.clone(),
        credential: None,
        enabled: true,
        egress_policy: None,
    };
    let manager = ExternalAgentManager::open(&paths);
    let record = manager
        .refresh_with_security(
            "research",
            &declaration,
            McpHttpSecurity {
                allow_loopback_http: true,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(record.agent_name, "Research Agent");
    assert_eq!(
        manager.show("research", &declaration).unwrap().readiness,
        ExternalAgentReadiness::ReviewRequired
    );
    manager.trust("research").unwrap();
    assert_eq!(
        manager.show("research", &declaration).unwrap().readiness,
        ExternalAgentReadiness::Ready
    );

    let changed_endpoint = one_response_server(card(&interface, "Changed Agent")).await;
    let changed = ExternalAgentDeclaration {
        endpoint: changed_endpoint,
        ..declaration
    };
    manager
        .refresh_with_security(
            "research",
            &changed,
            McpHttpSecurity {
                allow_loopback_http: true,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        manager.show("research", &changed).unwrap().readiness,
        ExternalAgentReadiness::ReviewRequired
    );
    manager.untrust("research").unwrap();
    assert!(
        manager
            .show("research", &changed)
            .unwrap()
            .cached
            .unwrap()
            .trusted_identity_digest
            .is_none()
    );
}

#[test]
fn cards_reject_required_extensions_hostile_bounds_and_bad_protocol() {
    let good = card("https://agent.example/a2a", "Agent");
    assert!(
        normalize_card(
            "agent",
            "https://card.example/card",
            &good,
            McpHttpSecurity::default()
        )
        .is_ok()
    );

    let mut required: Value = serde_json::from_slice(&good).unwrap();
    required["capabilities"]["extensions"] =
        json!([{"uri":"https://extensions.example/required","required":true}]);
    assert!(matches!(
        normalize_card(
            "agent",
            "https://card.example/card",
            &serde_json::to_vec(&required).unwrap(),
            McpHttpSecurity::default()
        ),
        Err(A2aError::UnsupportedCapability)
    ));

    let mut incompatible: Value = serde_json::from_slice(&good).unwrap();
    incompatible["supportedInterfaces"][0]["protocolVersion"] = json!("0.3");
    assert!(matches!(
        normalize_card(
            "agent",
            "https://card.example/card",
            &serde_json::to_vec(&incompatible).unwrap(),
            McpHttpSecurity::default()
        ),
        Err(A2aError::IncompatibleProtocol)
    ));
    assert!(matches!(
        normalize_card(
            "agent",
            "https://card.example/card",
            &vec![b'x'; MAX_CARD_BYTES + 1],
            McpHttpSecurity::default()
        ),
        Err(A2aError::CardTooLarge)
    ));
}

#[test]
fn zero_selected_agents_does_not_touch_private_state() {
    let home = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(home.path()))).unwrap();
    let registry = ConnectionRegistry {
        default_profile: "default".into(),
        default_child_route: None,
        permission_mode: crate::config::PermissionMode::Ask,
        permission_rules: Vec::new(),
        connections: BTreeMap::new(),
        profiles: BTreeMap::new(),
        routes: BTreeMap::new(),
        plugins: BTreeMap::new(),
        mcp_servers: BTreeMap::new(),
        external_agents: BTreeMap::new(),
        service_connections: BTreeMap::new(),
        service_routes: BTreeMap::new(),
        egress_policies: BTreeMap::new(),
    };
    assert!(
        ExternalAgentManager::open(&paths)
            .readiness(&[], &registry)
            .unwrap()
            .is_empty()
    );
    assert!(!paths.external_agent_state_file().exists());
}
