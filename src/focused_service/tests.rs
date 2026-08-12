use super::*;
use crate::config::XanaConfig;
use std::sync::atomic::{AtomicUsize, Ordering};

const CONFIG: &str = r#"
version = 4
default_profile = "default"
permission_mode = "ask"

[providers.chat]
kind = "ollama"

[profiles.default]
connection = "chat"
model = "qwen"
service_routes = ["fast", "quality"]
egress_policy = "images"

[service_connections.openai]
adapter = "test.images"
credential = { source = "environment", variable = "OPENAI_API_KEY" }

[service_routes.fast]
operation = "image.generate"
connection = "openai"
model = "image-fast"
description = "Fast image generation"
default = true
options = { size = "1024x1024" }
egress_policy = "images"

[service_routes.quality]
operation = "image.generate"
connection = "openai"
model = "image-quality"
egress_policy = "images"

[service_routes.private]
operation = "image.generate"
connection = "openai"
model = "image-private"
egress_policy = "images"

[egress_policies.images]
allowed = ["prompt_text", "selected_artifacts"]
"#;

struct FakeAdapter {
    descriptors: Vec<FocusedServiceDescriptor>,
    calls: Arc<AtomicUsize>,
    failure: Option<FocusedServiceError>,
}

impl FocusedServiceAdapter for FakeAdapter {
    fn descriptors(&self) -> &[FocusedServiceDescriptor] {
        &self.descriptors
    }

    fn execute<'a>(
        &'a self,
        request: FocusedServiceRequest,
        context: FocusedServiceContext,
    ) -> BoxFuture<'a, Result<FocusedServiceResult, FocusedServiceError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(FocusedServiceError::Cancelled);
            }
            if let Some(error) = &self.failure {
                return Err(error.clone());
            }
            let (artifact, _) = context
                .artifacts
                .put(b"fake image bytes", "image/png", context.owner)
                .map_err(|error| FocusedServiceError::Artifact(error.to_string()))?;
            Ok(FocusedServiceResult {
                provenance: FocusedServiceProvenance {
                    operation_id: request.operation_id,
                    route: request.route.name,
                    connection: request.route.connection,
                    adapter: request.route.adapter,
                    model: request.route.model,
                    operation: request.route.operation,
                    options: request.route.options,
                    created_unix_ms: 1,
                },
                artifacts: vec![artifact],
                derived_text: None,
                usage: FocusedServiceUsage::default(),
                usage_available: false,
                cost_available: false,
                untrusted: true,
            })
        })
    }
}

fn image_descriptor() -> FocusedServiceDescriptor {
    FocusedServiceDescriptor {
        adapter: "test.images".into(),
        operation: ServiceOperation::ImageGenerate,
        inputs: [MediaModality::Text].into_iter().collect(),
        outputs: [MediaModality::Image].into_iter().collect(),
        supports_editing: false,
        output_formats: ["image/png".into()].into_iter().collect(),
        max_prompt_bytes: 4096,
        max_input_artifacts: 0,
        max_output_artifacts: 1,
        max_artifact_bytes: 1024,
        requires_credential: true,
        supports_cancellation: true,
        reports_usage: false,
        reports_cost: false,
    }
}

fn service_registry(
    calls: Arc<AtomicUsize>,
    failure: Option<FocusedServiceError>,
) -> FocusedServiceRegistry {
    let mut services = FocusedServiceRegistry::default();
    services
        .register(Arc::new(FakeAdapter {
            descriptors: vec![image_descriptor()],
            calls,
            failure,
        }))
        .unwrap();
    services
}

#[test]
fn routes_resolve_by_exact_name_or_one_declared_default() {
    let registry = XanaConfig::parse_registry(CONFIG).unwrap();
    let profile = &registry.profiles["default"];
    let calls = Arc::new(AtomicUsize::new(0));
    let services = service_registry(calls.clone(), None);
    let default = services
        .resolve(
            &registry,
            &profile.service_routes,
            &registry.egress_policies["images"].allowed,
            ServiceOperation::ImageGenerate,
            None,
        )
        .unwrap();
    assert_eq!(default.name, "fast");
    assert_eq!(default.model, "image-fast");
    assert_eq!(default.options["size"], "1024x1024");
    assert_eq!(
        default.allowed_outbound,
        [
            OutboundDataClass::PromptText,
            OutboundDataClass::SelectedArtifacts
        ]
        .into_iter()
        .collect()
    );
    let exact = services
        .resolve(
            &registry,
            &profile.service_routes,
            &registry.egress_policies["images"].allowed,
            ServiceOperation::ImageGenerate,
            Some("quality"),
        )
        .unwrap();
    assert_eq!(exact.model, "image-quality");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn unexposed_incompatible_and_missing_default_routes_fail_before_adapter_use() {
    let registry = XanaConfig::parse_registry(CONFIG).unwrap();
    let profile = &registry.profiles["default"];
    let calls = Arc::new(AtomicUsize::new(0));
    let services = service_registry(calls.clone(), None);
    assert!(matches!(
        services.resolve(
            &registry,
            &profile.service_routes,
            &[],
            ServiceOperation::ImageGenerate,
            Some("private")
        ),
        Err(FocusedServiceError::RouteNotExposed(route)) if route == "private"
    ));
    assert!(matches!(
        services.resolve(
            &registry,
            &profile.service_routes,
            &[],
            ServiceOperation::VisionAnalyze,
            Some("fast")
        ),
        Err(FocusedServiceError::OperationMismatch { .. })
    ));
    assert!(matches!(
        services.resolve(
            &registry,
            &["quality".into()],
            &[],
            ServiceOperation::ImageGenerate,
            None
        ),
        Err(FocusedServiceError::NoDefault(
            ServiceOperation::ImageGenerate
        ))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn one_exact_adapter_executes_and_failure_never_falls_back() {
    let registry = XanaConfig::parse_registry(CONFIG).unwrap();
    let profile = &registry.profiles["default"];
    let calls = Arc::new(AtomicUsize::new(0));
    let services = service_registry(calls.clone(), Some(FocusedServiceError::RateLimited));
    let route = services
        .resolve(
            &registry,
            &profile.service_routes,
            &registry.egress_policies["images"].allowed,
            ServiceOperation::ImageGenerate,
            Some("quality"),
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let result = services
        .execute(
            FocusedServiceRequest {
                operation_id: OperationId::new(),
                route,
                prompt: "draw one safe fixture".into(),
                input_artifacts: Vec::new(),
            },
            FocusedServiceContext {
                artifacts: ArtifactStore::new(directory.path().to_owned()),
                owner: PrincipalId::new(),
                cancellation: CancellationToken::new(),
            },
        )
        .await;
    assert!(matches!(result, Err(FocusedServiceError::RateLimited)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn route_status_is_explicit_without_resolving_credentials_or_network() {
    let registry = XanaConfig::parse_registry(CONFIG).unwrap();
    let profile = &registry.profiles["default"];
    let services = service_registry(Arc::new(AtomicUsize::new(0)), None);
    assert!(
        services
            .inspect(&registry, &profile.service_routes, "fast")
            .ready
    );
    let private = services.inspect(&registry, &profile.service_routes, "private");
    assert!(!private.ready);
    assert_eq!(
        private.reason.as_deref(),
        Some("not exposed by the resolved profile")
    );
    let missing = services.inspect(&registry, &profile.service_routes, "missing");
    assert!(!missing.configured);
}
