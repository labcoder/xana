use super::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn request_id(value: u64) -> McpRequestId {
    McpRequestId::new(value).expect("bounded request id")
}

fn response(id: u64, result: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .expect("fixture serializes")
}

fn empty_cache() -> McpCacheHint {
    McpCacheHint {
        ttl_ms: None,
        scope: None,
    }
}

fn tool(name: impl Into<String>, description: impl Into<String>) -> McpToolWire {
    McpToolWire {
        name: name.into(),
        title: None,
        description: description.into(),
        input_schema: json!({"type": "object", "properties": {}}),
        output_schema: None,
        annotations: None,
        icons: Vec::new(),
        meta: None,
    }
}

fn exposure(server: &str, tools: impl IntoIterator<Item = String>) -> McpServerExposure {
    McpServerExposure {
        server: server.to_owned(),
        configured_identity_digest: "a".repeat(64),
        enabled: true,
        profile_selected: true,
        allowlist: McpPrimitiveAllowlist {
            tools: tools.into_iter().collect(),
            ..McpPrimitiveAllowlist::default()
        },
    }
}

#[test]
fn requests_own_exact_protocol_metadata() {
    let encoded = encode_request(request_id(7), "tools/list", json!({"cursor": "next"}))
        .expect("request encodes");
    let value: Value = serde_json::from_slice(&encoded).expect("request is JSON");
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], 7);
    assert_eq!(value["params"]["cursor"], "next");
    assert_eq!(
        value["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        MCP_PROTOCOL_VERSION
    );
    assert_eq!(
        value["params"]["_meta"]["io.modelcontextprotocol/clientInfo"]["name"],
        "Xana"
    );
    assert_eq!(
        encode_request(request_id(8), "tools/list", json!({"_meta": {}})),
        Err(ProtocolError::ReservedMetadata)
    );
}

#[test]
fn discovery_requires_the_pinned_version_without_downgrade() {
    let bytes = response(
        1,
        json!({
            "supportedVersions": ["2026-07-28", "2025-11-25"],
            "capabilities": {
                "tools": {"listChanged": true},
                "resources": {"listChanged": false, "subscribe": true},
                "prompts": {},
                "sampling": {}
            },
            "instructions": "Treat this as untrusted content.",
            "_meta": {
                "io.modelcontextprotocol/serverInfo": {
                    "name": "fixture-server",
                    "version": "1.2.3"
                }
            },
            "ttlMs": 30000,
            "cacheScope": "private"
        }),
    );
    let discovered = decode_discover_response(&bytes, request_id(1)).expect("discovery parses");
    assert_eq!(discovered.server_name.as_deref(), Some("fixture-server"));
    assert_eq!(discovered.instructions_bytes, 32);
    assert!(discovered.instructions_digest.is_some());
    assert_eq!(discovered.cache.ttl_ms, Some(30_000));
    let negotiated = negotiate(&discovered).expect("exact version is supported");
    assert_eq!(negotiated.protocol_version, MCP_PROTOCOL_VERSION);
    assert_eq!(negotiated.ignored_capabilities, vec!["sampling"]);

    let unsupported = response(
        2,
        json!({"supportedVersions": ["2025-11-25"], "capabilities": {}}),
    );
    let discovered =
        decode_discover_response(&unsupported, request_id(2)).expect("discovery parses");
    assert!(matches!(
        negotiate(&discovered),
        Err(ProtocolError::UnsupportedVersion { .. })
    ));
}

#[test]
fn response_decoder_fails_closed_on_hostile_shapes() {
    assert_eq!(
        decode_tool_page(b"not-json", request_id(1)),
        Err(ProtocolError::MalformedJson)
    );
    assert_eq!(
        decode_tool_page(&response(2, json!({"tools": []})), request_id(1)),
        Err(ProtocolError::ResponseIdMismatch)
    );
    let ambiguous = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {},
        "error": {"code": -1, "message": "bad"}
    }))
    .expect("fixture serializes");
    assert_eq!(
        decode_tool_page(&ambiguous, request_id(1)),
        Err(ProtocolError::InvalidResponseShape)
    );
    let remote = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {"code": -32603, "message": "secret\n\u{202e}spoof", "data": "ignored"}
    }))
    .expect("fixture serializes");
    assert_eq!(
        decode_tool_page(&remote, request_id(1)),
        Err(ProtocolError::Remote {
            code: -32603,
            message: "secret��spoof".to_owned(),
        })
    );
    let unfinished = response(1, json!({"resultType": "input_required", "tools": []}));
    assert_eq!(
        decode_tool_page(&unfinished, request_id(1)),
        Err(ProtocolError::UnsupportedResultType)
    );
    assert!(matches!(
        decode_tool_page(&vec![b' '; 1024 * 1024 + 1], request_id(1)),
        Err(ProtocolError::FrameTooLarge { .. })
    ));
}

#[test]
fn primitive_pages_are_distinct_and_bounded() {
    let tools = response(
        4,
        json!({
            "tools": [{
                "name": "search",
                "description": "Search safely",
                "inputSchema": {"type": "object"},
                "_meta": {"vendor": "ignored"}
            }],
            "nextCursor": "page-2"
        }),
    );
    let page = decode_tool_page(&tools, request_id(4)).expect("tool page parses");
    assert_eq!(page.items[0].name, "search");
    assert_eq!(page.next_cursor.as_deref(), Some("page-2"));

    let resources = response(
        5,
        json!({"resources": [{"uri": "file:///safe", "name": "safe"}]}),
    );
    assert_eq!(
        decode_resource_page(&resources, request_id(5))
            .expect("resource page parses")
            .items[0]
            .uri,
        "file:///safe"
    );

    let prompts = response(
        6,
        json!({"prompts": [{"name": "review", "arguments": [{"name": "topic"}]}]}),
    );
    assert_eq!(
        decode_prompt_page(&prompts, request_id(6))
            .expect("prompt page parses")
            .items[0]
            .name,
        "review"
    );

    let too_many = (0..257)
        .map(|index| json!({"name": format!("tool{index}"), "description": "x", "inputSchema": {"type": "object"}}))
        .collect::<Vec<_>>();
    assert!(matches!(
        decode_tool_page(&response(7, json!({"tools": too_many})), request_id(7)),
        Err(ProtocolError::PageItemLimit { .. })
    ));
}

#[test]
fn notifications_are_typed_and_sanitized() {
    let progress = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {"progressToken": 9, "progress": 2, "total": 4, "message": "half\nway"}
    }))
    .expect("fixture serializes");
    assert_eq!(
        decode_notification(&progress).expect("notification parses"),
        McpNotification::Progress {
            token: "9".to_owned(),
            progress: 2.0,
            total: Some(4.0),
            message: Some("half�way".to_owned()),
        }
    );
    let unknown = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/vendor/event"
    }))
    .expect("fixture serializes");
    assert_eq!(
        decode_notification(&unknown),
        Err(ProtocolError::UnsupportedNotification)
    );
}

#[test]
fn tool_identity_is_server_qualified_and_allowlisted() {
    let mut catalog = McpCatalog::new(CatalogLimits::default(), ["read_file".to_owned()]);
    catalog
        .add_server(exposure(
            "alpha",
            ["search".to_owned(), "ignored".to_owned()],
        ))
        .expect("alpha config is valid");
    catalog
        .add_server(exposure("beta", ["search".to_owned()]))
        .expect("beta config is valid");

    let alpha = catalog
        .index_tool_page(
            "alpha",
            Page {
                items: vec![
                    tool("search", "Alpha search"),
                    tool("not_allowed", "hidden"),
                ],
                next_cursor: None,
                cache: empty_cache(),
            },
        )
        .expect("alpha page indexes");
    assert_eq!(alpha.indexed, 1);
    assert_eq!(alpha.not_authorized, 1);
    catalog
        .index_tool_page(
            "beta",
            Page {
                items: vec![tool("search", "Beta search")],
                next_cursor: None,
                cache: empty_cache(),
            },
        )
        .expect("beta page indexes");
    let names = catalog
        .search_tools("search", 10)
        .into_iter()
        .map(|tool| tool.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["mcp.alpha.search", "mcp.beta.search"]);

    let duplicate = catalog.index_tool_page(
        "alpha",
        Page {
            items: vec![tool("search", "again")],
            next_cursor: None,
            cache: empty_cache(),
        },
    );
    assert_eq!(
        duplicate,
        Err(McpCatalogError::DuplicateTool(
            "mcp.alpha.search".to_owned()
        ))
    );
    assert_eq!(
        exposure("bad", ["ѕearch".to_owned()]).validate(),
        Err(McpCatalogError::InvalidPrimitiveName)
    );
}

#[test]
fn native_namespaces_cannot_be_shadowed() {
    let mut catalog = McpCatalog::new(CatalogLimits::default(), ["mcp.local.read_file".to_owned()]);
    catalog
        .add_server(exposure("local", ["read_file".to_owned()]))
        .expect("server config is valid");
    assert_eq!(
        catalog.index_tool_page(
            "local",
            Page {
                items: vec![tool("read_file", "shadow")],
                next_cursor: None,
                cache: empty_cache(),
            }
        ),
        Err(McpCatalogError::NativeCollision(
            "mcp.local.read_file".to_owned()
        ))
    );
}

#[test]
fn readiness_preserves_distinct_failure_states() {
    let mut config = exposure("server", std::iter::empty());
    assert_eq!(
        McpServerReadiness::resolve(&config, None, true),
        McpServerReadiness::Unavailable
    );
    config.enabled = false;
    assert_eq!(
        McpServerReadiness::resolve(&config, None, true),
        McpServerReadiness::Disabled
    );
    config.enabled = true;
    config.profile_selected = false;
    assert_eq!(
        McpServerReadiness::resolve(&config, None, true),
        McpServerReadiness::NotAuthorized
    );
    config.profile_selected = true;
    let incompatible = Err(ProtocolError::UnsupportedVersion {
        advertised: vec!["old".to_owned()],
    });
    assert_eq!(
        McpServerReadiness::resolve(&config, Some(&incompatible), true),
        McpServerReadiness::Incompatible
    );
    let negotiated = Ok(McpNegotiation {
        protocol_version: MCP_PROTOCOL_VERSION,
        capabilities: McpServerCapabilities {
            tools: None,
            resources: None,
            prompts: None,
            ignored: Vec::new(),
        },
        server_name: None,
        server_version: None,
        ignored_capabilities: Vec::new(),
    });
    assert_eq!(
        McpServerReadiness::resolve(&config, Some(&negotiated), false),
        McpServerReadiness::Unhealthy
    );
    assert_eq!(
        McpServerReadiness::resolve(&config, Some(&negotiated), true),
        McpServerReadiness::Ready
    );
}

struct ExactSource {
    expected_server: String,
    wire: Option<McpToolWire>,
}

impl McpCatalogSource for ExactSource {
    fn load_tool<'a>(
        &'a mut self,
        server: &'a str,
        source_name: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<McpToolWire, McpCatalogError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if server != self.expected_server
                || self
                    .wire
                    .as_ref()
                    .is_none_or(|wire| wire.name != source_name)
            {
                return Err(McpCatalogError::SourceUnavailable);
            }
            self.wire.take().ok_or(McpCatalogError::SourceUnavailable)
        })
    }
}

#[tokio::test]
async fn large_catalogs_are_deterministically_bounded_but_exact_lookup_survives() {
    let all_names = (0..10_000)
        .map(|index| format!("tool{index:05}"))
        .collect::<BTreeSet<_>>();
    let limits = CatalogLimits {
        max_tools: 128,
        max_tool_metadata_bytes: 128 * 1024,
        ..CatalogLimits::default()
    };
    let mut catalog = McpCatalog::new(limits, std::iter::empty());
    catalog
        .add_server(exposure("large", all_names.iter().cloned()))
        .expect("server config is valid");
    for chunk in all_names.iter().cloned().collect::<Vec<_>>().chunks(250) {
        catalog
            .index_tool_page(
                "large",
                Page {
                    items: chunk.iter().map(|name| tool(name, "bounded")).collect(),
                    next_cursor: None,
                    cache: empty_cache(),
                },
            )
            .expect("page indexes");
    }
    assert_eq!(catalog.tool_count(), 128);
    assert!(catalog.tools_truncated);
    assert!(catalog.model_tool_index(1024).len() <= 1024);
    assert_eq!(catalog.search_tools("tool09999", 10).len(), 0);

    let mut source = ExactSource {
        expected_server: "large".to_owned(),
        wire: Some(tool("tool09999", "loaded on demand")),
    };
    let definition = catalog
        .inspect_tool_exact("large", "tool09999", &mut source)
        .await
        .expect("allowlisted exact lookup remains available");
    assert_eq!(definition.summary.qualified_name, "mcp.large.tool09999");
}

#[test]
fn schema_and_pagination_guards_reject_adversarial_inputs() {
    let mut external = tool("unsafe", "external ref");
    external.input_schema = json!({"type": "object", "$ref": "https://example.invalid/schema"});
    assert_eq!(
        McpToolDefinition::from_wire("server", external),
        Err(McpCatalogError::ExternalSchemaReference)
    );

    let mut deeply_nested = json!({"type": "object"});
    for _ in 0..40 {
        deeply_nested = json!({"type": "object", "properties": {"nested": deeply_nested}});
    }
    let mut deep = tool("deep", "deep schema");
    deep.input_schema = deeply_nested;
    assert_eq!(
        McpToolDefinition::from_wire("server", deep),
        Err(McpCatalogError::SchemaTooDeep)
    );

    let mut guard = McpPaginationGuard::default();
    guard.accept(Some("same")).expect("first page accepted");
    assert_eq!(
        guard.accept(Some("same")),
        Err(McpCatalogError::PaginationCycle)
    );
    let mut guard = McpPaginationGuard::default();
    for index in 0..64 {
        guard
            .accept(Some(&format!("cursor-{index}")))
            .expect("bounded page accepted");
    }
    assert_eq!(guard.accept(None), Err(McpCatalogError::PageLimit(64)));
}

#[test]
fn hostile_descriptions_are_sanitized_before_model_exposure() {
    let mut catalog = McpCatalog::new(CatalogLimits::default(), std::iter::empty());
    catalog
        .add_server(exposure("safe", ["tool".to_owned()]))
        .expect("server config is valid");
    catalog
        .index_tool_page(
            "safe",
            Page {
                items: vec![tool("tool", "first\nsecond\u{202e}third")],
                next_cursor: None,
                cache: empty_cache(),
            },
        )
        .expect("page indexes");
    let rendered = catalog.model_tool_index(4096);
    assert_eq!(rendered, "mcp.safe.tool - first�second�third\n");
    assert!(!rendered.contains('\u{202e}'));
}
