use super::*;
use crate::mcp::McpPrimitiveAllowlist;
use crate::{
    identity::{OperationId, ToolInvocationId},
    message::{ToolCall, ToolResultStatus},
    native_runtime::AgentEvent,
    permission::{PermissionBroker, PermissionPolicy, PolicyDecision},
    tool::ToolContext,
};
use serde_json::json;
use std::sync::{Mutex, atomic::AtomicUsize};
use tempfile::tempdir;

#[derive(Default)]
struct FakeTransport {
    calls: AtomicUsize,
    methods: Mutex<Vec<String>>,
}

impl FakeTransport {
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn methods(&self) -> Vec<String> {
        self.methods.lock().expect("methods lock").clone()
    }
}

impl McpPrimitiveTransport for FakeTransport {
    fn request<'a>(
        &'a self,
        method: &'a str,
        _params: Value,
        _tool_schema: Option<Value>,
        _operation_id: OperationId,
        _cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpTransportResponse, McpApplicationError>> {
        Box::pin(async move {
            let id = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.methods
                .lock()
                .expect("methods lock")
                .push(method.into());
            let result = match method {
                "server/discover" => json!({
                    "supportedVersions":[super::super::MCP_PROTOCOL_VERSION],
                    "capabilities":{"tools":{},"resources":{},"prompts":{}}
                }),
                "tools/list" => json!({"tools":[{
                    "name":"lookup","description":"Look up one value",
                    "inputSchema":{"type":"object","required":["q"],"properties":{"q":{"type":"string"}}}
                }]}),
                "resources/list" => json!({"resources":[{
                    "uri":"file:///guide","name":"guide","mimeType":"text/plain"
                }]}),
                "resources/templates/list" => json!({"resourceTemplates":[]}),
                "prompts/list" => json!({"prompts":[{
                    "name":"review","description":"Review text",
                    "arguments":[{"name":"text","required":true}]
                }]}),
                "tools/get" => json!({"tool":{
                    "name":"lookup","description":"Look up one value",
                    "inputSchema":{"type":"object","required":["q"],"properties":{"q":{"type":"string"}}}
                }}),
                "tools/call" => json!({
                    "content":[{"type":"text","text":"remote result"}],"isError":false
                }),
                "resources/read" => json!({
                    "contents":[{"uri":"file:///guide","mimeType":"text/plain","text":"remote guidance"}]
                }),
                "prompts/get" => json!({
                    "description":"Review text",
                    "messages":[{"role":"user","content":{"type":"text","text":"review this"}}]
                }),
                other => {
                    return Err(McpApplicationError::Transport(format!(
                        "unexpected {other}"
                    )));
                }
            };
            let bytes = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result}))
                .expect("fixture response");
            Ok(McpTransportResponse {
                request_id: McpRequestId::new(id as u64).expect("request id"),
                bytes,
                notifications: Vec::new(),
            })
        })
    }
}

fn application(fake: Arc<FakeTransport>) -> McpApplication {
    let mut application = McpApplication::new(["read_file".to_owned()]);
    application
        .add_server(
            McpServerExposure {
                server: "docs".into(),
                configured_identity_digest: "a".repeat(64),
                enabled: true,
                profile_selected: true,
                allowlist: McpPrimitiveAllowlist {
                    tools: ["lookup".to_owned()].into_iter().collect(),
                    resources: ["file:///guide".to_owned()].into_iter().collect(),
                    resource_templates: Default::default(),
                    prompts: ["review".to_owned()].into_iter().collect(),
                },
            },
            fake,
        )
        .expect("server is valid");
    application
}

#[tokio::test]
async fn allowlisted_primitives_refresh_and_keep_distinct_semantics() {
    let fake = Arc::new(FakeTransport::default());
    let application = application(fake.clone());
    let cancellation = CancellationToken::new();
    application
        .refresh("docs", &cancellation)
        .await
        .expect("catalog refreshes");

    let resource = application
        .read_resource("docs", "file:///guide", &cancellation)
        .await
        .expect("explicit resource read");
    assert!(resource.untrusted);
    assert_eq!(resource.server, "docs");

    let prompt = application
        .invoke_prompt(
            "docs",
            "review",
            [("text".to_owned(), "sample".to_owned())]
                .into_iter()
                .collect(),
            &cancellation,
        )
        .await
        .expect("explicit prompt preview");
    assert!(prompt.untrusted);
    assert_eq!(prompt.messages[0].role, "user");
    assert!(!fake.methods().iter().any(|method| method == "system"));
}

#[tokio::test]
async fn remote_tool_does_zero_transport_io_when_permission_denies() {
    let fake = Arc::new(FakeTransport::default());
    let application = application(fake.clone());
    let cancellation = CancellationToken::new();
    application.refresh("docs", &cancellation).await.unwrap();
    let mut registry = ToolRegistry::new();
    application
        .activate_tools(&mut registry, &cancellation)
        .await
        .expect("tool activates");
    let before = fake.call_count();
    let workspace = tempdir().unwrap();
    let policy =
        PermissionPolicy::new(PolicyDecision::Deny, Vec::new(), workspace.path()).expect("policy");
    let (events, _receiver) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let (permissions, task) = PermissionBroker::spawn(policy, false, events);
    let result = registry
        .invoke(
            &ToolCall {
                id: "call-1".into(),
                name: "mcp.docs.lookup".into(),
                arguments: json!({"q":"value"}),
            },
            ToolContext {
                workspace_root: workspace.path(),
                operation_id: OperationId::new(),
                invocation_id: ToolInvocationId::new(),
                permissions: &permissions,
                events: None,
            },
        )
        .await;
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(fake.call_count(), before);
    permissions.shutdown();
    task.await.unwrap();
}

#[tokio::test]
async fn remote_tool_runs_after_exact_permission_and_reports_provenance() {
    let fake = Arc::new(FakeTransport::default());
    let application = application(fake.clone());
    let cancellation = CancellationToken::new();
    application.refresh("docs", &cancellation).await.unwrap();
    let mut registry = ToolRegistry::new();
    application
        .activate_tools(&mut registry, &cancellation)
        .await
        .unwrap();
    let workspace = tempdir().unwrap();
    let policy =
        PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), workspace.path()).unwrap();
    let (events, _receiver) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let (permissions, task) = PermissionBroker::spawn(policy, false, events);
    let result = registry
        .invoke(
            &ToolCall {
                id: "call-2".into(),
                name: "mcp.docs.lookup".into(),
                arguments: json!({"q":"value"}),
            },
            ToolContext {
                workspace_root: workspace.path(),
                operation_id: OperationId::new(),
                invocation_id: ToolInvocationId::new(),
                permissions: &permissions,
                events: None,
            },
        )
        .await;
    assert_eq!(result.status, ToolResultStatus::Success);
    assert!(result.output.contains("\"server\":\"docs\""));
    assert!(result.output.contains("\"untrusted\":true"));
    permissions.shutdown();
    task.await.unwrap();
}

#[tokio::test]
async fn unallowlisted_direct_access_fails_before_transport_io() {
    let fake = Arc::new(FakeTransport::default());
    let application = application(fake.clone());
    let cancellation = CancellationToken::new();
    application.refresh("docs", &cancellation).await.unwrap();
    let before = fake.call_count();
    let error = application
        .read_resource("docs", "file:///secret", &cancellation)
        .await
        .expect_err("unallowlisted resource is denied");
    assert!(matches!(
        error,
        McpApplicationError::Catalog(McpCatalogError::NotAuthorized)
    ));
    assert_eq!(fake.call_count(), before);
}

#[tokio::test]
async fn outbound_policy_denial_prevents_discovery_transport_io() {
    let fake = Arc::new(FakeTransport::default());
    let root = tempdir().unwrap();
    let paths = crate::paths::XanaPaths::resolve(Some(root.path().as_os_str().to_owned())).unwrap();
    let recipient = RecipientIdentity::new(
        crate::outbound::RecipientKind::McpStdio,
        "docs",
        "fake",
        b"fake-server",
    )
    .unwrap();
    let guarded = Arc::new(McpGuardedTransport::new(
        fake.clone(),
        OutboundGuard::open(&paths).unwrap(),
        recipient,
        OutboundPolicyLayers {
            connection_allowed: Default::default(),
            user_ceiling: OutboundDataClass::ALL.into_iter().collect(),
            profile_allowed: OutboundDataClass::ALL.into_iter().collect(),
            conversation_allowed: None,
        },
    ));
    let mut application = McpApplication::new(std::iter::empty());
    application
        .add_server(
            McpServerExposure {
                server: "docs".into(),
                configured_identity_digest: "b".repeat(64),
                enabled: true,
                profile_selected: true,
                allowlist: McpPrimitiveAllowlist::default(),
            },
            guarded,
        )
        .unwrap();
    let error = application
        .refresh("docs", &CancellationToken::new())
        .await
        .expect_err("egress ceiling denies discovery");
    assert!(matches!(error, McpApplicationError::Egress(_)));
    assert_eq!(fake.call_count(), 0);
}
