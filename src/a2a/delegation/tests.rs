use super::*;
use crate::{
    message::{ToolCall, ToolResultStatus},
    permission::{PermissionBroker, PermissionPolicy, PolicyDecision},
    tool::ToolContext,
};
use std::ffi::OsString;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

fn trusted(interface_url: String) -> ExternalAgentStateRecord {
    ExternalAgentStateRecord {
        connection: "research".into(),
        configured_endpoint: "https://card.example/.well-known/agent-card.json".into(),
        identity_digest: "a".repeat(64),
        agent_name: "Research Agent".into(),
        description: "A fake A2A test peer".into(),
        owner: None,
        agent_version: "1.0.0".into(),
        interface_url,
        protocol_binding: "JSONRPC".into(),
        protocol_version: A2A_PROTOCOL_VERSION.into(),
        streaming: true,
        input_modes: vec!["text/plain".into()],
        output_modes: vec!["text/plain".into()],
        security_schemes: Vec::new(),
        skills: Vec::new(),
        refreshed_unix_ms: 1,
        trusted_identity_digest: Some("a".repeat(64)),
        trusted_unix_ms: Some(1),
    }
}

async fn streaming_server() -> (String, oneshot::Receiver<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, received) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap();
        while request.len() - header_end < content_length {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
        }
        let body: Value =
            serde_json::from_slice(&request[header_end..header_end.saturating_add(content_length)])
                .unwrap();
        let id = body["id"].as_str().unwrap();
        sent.send(body.clone()).unwrap();
        let events = [
            json!({"jsonrpc":"2.0","id":id,"result":{"task":{
                "id":"task-1","contextId":"context-1",
                "status":{"state":"TASK_STATE_WORKING"}
            }}}),
            json!({"jsonrpc":"2.0","id":id,"result":{"statusUpdate":{
                "taskId":"task-1","contextId":"context-1",
                "status":{"state":"TASK_STATE_WORKING","message":{
                    "role":"ROLE_AGENT","parts":[{"text":"Reviewing the bounded input"}]
                }}
            }}}),
            json!({"jsonrpc":"2.0","id":id,"result":{"artifactUpdate":{
                "taskId":"task-1","contextId":"context-1","lastChunk":false,
                "artifact":{"artifactId":"artifact-1","name":"answer.txt",
                    "parts":[{"text":"remote "}]}
            }}}),
            json!({"jsonrpc":"2.0","id":id,"result":{"artifactUpdate":{
                "taskId":"task-1","contextId":"context-1","append":true,"lastChunk":true,
                "artifact":{"artifactId":"artifact-1","name":"answer.txt",
                    "parts":[{"text":"answer"}]}
            }}}),
            json!({"jsonrpc":"2.0","id":id,"result":{"statusUpdate":{
                "taskId":"task-1","contextId":"context-1",
                "status":{"state":"TASK_STATE_COMPLETED"}
            }}}),
        ]
        .into_iter()
        .map(|event| format!("data: {}\n\n", serde_json::to_string(&event).unwrap()))
        .collect::<String>();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            events.len(),
            events
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (format!("http://{address}/a2a"), received)
}

fn all_policy() -> OutboundPolicyLayers {
    let all = OutboundDataClass::ALL.into_iter().collect::<BTreeSet<_>>();
    OutboundPolicyLayers {
        connection_allowed: all.clone(),
        user_ceiling: all.clone(),
        profile_allowed: all,
        conversation_allowed: None,
    }
}

#[tokio::test]
async fn trusted_tool_sends_only_selected_classes_streams_and_ingests_artifact() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("selected.txt"), b"selected bytes").unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(home.path()))).unwrap();
    let (interface, request) = streaming_server().await;
    let tool = A2aDelegationTool {
        connection: "research".into(),
        declaration_credential: None,
        trusted: trusted(interface),
        paths: paths.clone(),
        workspace: workspace.path().canonicalize().unwrap(),
        artifacts: ArtifactStore::new(paths.data_dir().join("artifacts")),
        owner: PrincipalId::new(),
        policy: all_policy(),
        security: McpHttpSecurity::default(),
        audit: Arc::new(crate::outbound::NoopOutboundAuditObserver),
    }
    .allow_loopback();
    let mut registry = ToolRegistry::new();
    registry.register(tool).unwrap();
    let policy =
        PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), workspace.path()).unwrap();
    let (broker_events, mut broker_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (permissions, broker_task) = PermissionBroker::spawn(policy, true, broker_events);
    let approval = {
        let permissions = permissions.clone();
        tokio::spawn(async move {
            while let Some(event) = broker_receiver.recv().await {
                if let AgentEvent::PermissionRequested { request } = event {
                    permissions
                        .decide(
                            request.operation_id,
                            request.invocation_id,
                            crate::permission::ControllerDecision::AllowOnce,
                        )
                        .await
                        .unwrap();
                    break;
                }
            }
        })
    };
    let (event_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    let event_sender = AgentEventSender::from(event_sender);
    let canonical_workspace = workspace.path().canonicalize().unwrap();
    let result = registry
        .invoke(
            &ToolCall {
                id: "call-1".into(),
                name: "a2a__research__delegate".into(),
                arguments: json!({
                    "task":"Answer one question",
                    "selected_messages":["Only this prior message"],
                    "selected_files":["selected.txt"],
                    "include_workspace_metadata":true
                }),
            },
            ToolContext {
                workspace_root: &canonical_workspace,
                operation_id: OperationId::new(),
                invocation_id: crate::identity::ToolInvocationId::new(),
                permissions: &permissions,
                events: Some(&event_sender),
            },
        )
        .await;
    approval.await.unwrap();
    permissions.shutdown();
    broker_task.await.unwrap();
    assert_eq!(
        result.status,
        ToolResultStatus::Success,
        "{}",
        result.output
    );
    let receipt: Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(receipt["task_id"], "task-1");
    assert_eq!(receipt["terminal_state"], "completed");
    assert_eq!(receipt["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["artifacts"][0]["record"]["byte_len"], 13);
    let request = request.await.unwrap();
    assert_eq!(request["method"], "SendStreamingMessage");
    let parts = request["params"]["message"]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 4);
    assert!(!request.to_string().contains("selected bytes"));
    assert!(
        request
            .to_string()
            .contains(&STANDARD.encode(b"selected bytes"))
    );
    let mut saw_status = false;
    let mut saw_artifact = false;
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::ExternalAgentActivity { activity, .. } = event {
            saw_status |= matches!(activity.activity, ExternalAgentActivityKind::Status { .. });
            saw_artifact |= matches!(
                activity.activity,
                ExternalAgentActivityKind::Artifact { .. }
            );
        }
    }
    assert!(saw_status && saw_artifact);
    let task = ExternalAgentManager::open(&paths)
        .task("research", "task-1")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, "completed");
}

#[tokio::test]
async fn denied_outbound_scope_sends_zero_network_bytes() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(home.path()))).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let interface = format!("http://{}/a2a", listener.local_addr().unwrap());
    let tool = A2aDelegationTool {
        connection: "research".into(),
        declaration_credential: None,
        trusted: trusted(interface),
        paths,
        workspace: workspace.path().canonicalize().unwrap(),
        artifacts: ArtifactStore::new(home.path().join("artifacts")),
        owner: PrincipalId::new(),
        policy: all_policy(),
        security: McpHttpSecurity::default(),
        audit: Arc::new(crate::outbound::NoopOutboundAuditObserver),
    }
    .allow_loopback();
    let mut registry = ToolRegistry::new();
    registry.register(tool).unwrap();
    let policy = PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), workspace.path()).unwrap();
    let (broker_events, _broker_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (permissions, broker_task) = PermissionBroker::spawn(policy, false, broker_events);
    let canonical_workspace = workspace.path().canonicalize().unwrap();
    let result = registry
        .invoke(
            &ToolCall {
                id: "call-denied".into(),
                name: "a2a__research__delegate".into(),
                arguments: json!({"task":"do not send"}),
            },
            ToolContext {
                workspace_root: &canonical_workspace,
                operation_id: OperationId::new(),
                invocation_id: crate::identity::ToolInvocationId::new(),
                permissions: &permissions,
                events: None,
            },
        )
        .await;
    permissions.shutdown();
    broker_task.await.unwrap();
    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn planner_rejects_escape_and_oversized_task_without_network_work() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(home.path()))).unwrap();
    let tool = A2aDelegationTool {
        connection: "research".into(),
        declaration_credential: None,
        trusted: trusted("http://127.0.0.1:9/a2a".into()),
        paths: paths.clone(),
        workspace: workspace.path().canonicalize().unwrap(),
        artifacts: ArtifactStore::new(paths.data_dir().join("artifacts")),
        owner: PrincipalId::new(),
        policy: all_policy(),
        security: McpHttpSecurity {
            allow_loopback_http: true,
        },
        audit: Arc::new(crate::outbound::NoopOutboundAuditObserver),
    };
    assert!(
        tool.plan(
            &json!({"task":"ok","selected_files":["../secret"]}),
            &workspace.path().canonicalize().unwrap()
        )
        .is_err()
    );
    assert!(
        tool.plan(
            &json!({"task":"x".repeat(MAX_TASK_BYTES + 1)}),
            &workspace.path().canonicalize().unwrap()
        )
        .is_err()
    );
}
