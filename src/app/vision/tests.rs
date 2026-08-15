use super::*;
use crate::{config::XanaConfig, focused_service::ServiceOperation};
use std::sync::Mutex;

const CONFIG: &str = r#"
version = 4
default_profile = "default"
permission_mode = "ask"

[providers.chat]
kind = "ollama"

[profiles.default]
connection = "chat"
model = "text-model"
service_routes = ["describe", "alternate"]
egress_policy = "vision"

[service_connections.vision]
adapter = "openai.vision"
credential = { source = "environment", variable = "OPENAI_API_KEY" }

[service_routes.describe]
operation = "vision.analyze"
connection = "vision"
model = "vision-default"
default = true
egress_policy = "vision"

[service_routes.alternate]
operation = "vision.analyze"
connection = "vision"
model = "vision-alternate"
egress_policy = "vision"

[egress_policies.vision]
allowed = ["prompt_text", "selected_artifacts"]
"#;

fn service(config: &str) -> VisionTurnService {
    let registry = XanaConfig::parse_registry(config).unwrap();
    let profile = &registry.profiles["default"];
    let directory = tempfile::tempdir().unwrap().keep();
    let paths = crate::paths::XanaPaths::resolve(Some(directory.clone().into_os_string())).unwrap();
    VisionTurnService::new(
        registry.clone(),
        crate::outbound::OutboundGuard::open(&paths).unwrap(),
        profile.service_routes.clone(),
        registry.egress_policies["vision"].allowed.clone(),
        PermissionMode::Ask,
        ArtifactStore::new(directory.join("artifacts")),
        PrincipalId::new(),
    )
}

#[test]
fn capable_model_uses_native_source_without_resolving_a_specialist() {
    let registry = XanaConfig::parse_registry(CONFIG).unwrap();
    let directory = tempfile::tempdir().unwrap().keep();
    let paths = crate::paths::XanaPaths::resolve(Some(directory.clone().into_os_string())).unwrap();
    let service = VisionTurnService::new(
        registry,
        crate::outbound::OutboundGuard::open(&paths).unwrap(),
        Vec::new(),
        Vec::new(),
        PermissionMode::Ask,
        ArtifactStore::new(directory.join("artifacts")),
        PrincipalId::new(),
    );

    assert_eq!(
        service.route_turn(true, None).unwrap(),
        VisionTurnRoute::Native
    );
}

#[test]
fn text_only_model_uses_the_declared_default_specialist() {
    let VisionTurnRoute::SpecialistApprovalRequired(plan) =
        service(CONFIG).route_turn(false, None).unwrap()
    else {
        panic!("text-only model should use a specialist");
    };

    assert_eq!(plan.route.operation, ServiceOperation::VisionAnalyze);
    assert_eq!(plan.route.name, "describe");
    assert_eq!(plan.route.model, "vision-default");
}

#[test]
fn explicit_specialist_overrides_a_capable_conversational_model() {
    let VisionTurnRoute::SpecialistApprovalRequired(plan) =
        service(CONFIG).route_turn(true, Some("alternate")).unwrap()
    else {
        panic!("explicit route should override native vision");
    };

    assert_eq!(plan.route.name, "alternate");
    assert_eq!(plan.route.model, "vision-alternate");
}

#[test]
fn text_only_model_without_an_exposed_route_fails_before_dispatch() {
    let registry = XanaConfig::parse_registry(CONFIG).unwrap();
    let directory = tempfile::tempdir().unwrap().keep();
    let paths = crate::paths::XanaPaths::resolve(Some(directory.clone().into_os_string())).unwrap();
    let service = VisionTurnService::new(
        registry,
        crate::outbound::OutboundGuard::open(&paths).unwrap(),
        Vec::new(),
        Vec::new(),
        PermissionMode::Ask,
        ArtifactStore::new(directory.join("artifacts")),
        PrincipalId::new(),
    );

    let error = service.route_turn(false, None).unwrap_err();
    assert!(error.to_string().contains("no default route"));
}

#[derive(Default)]
struct Audit(Mutex<Vec<crate::outbound::OutboundAuditEvent>>);

impl crate::outbound::OutboundAuditObserver for Audit {
    fn observe(
        &self,
        event: &crate::outbound::OutboundAuditEvent,
    ) -> std::result::Result<(), crate::private_state::PrivateStateError> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn saved_specialist_denial_never_resolves_credentials_or_dispatches() {
    let registry = XanaConfig::parse_registry(CONFIG).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let paths =
        crate::paths::XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
    let profile = &registry.profiles["default"];
    let store = ArtifactStore::new(paths.data_dir().join("artifacts"));
    let owner = PrincipalId::new();
    let (artifact, _) = store.put(b"bounded fixture", "image/png", owner).unwrap();
    let image = ImageRef {
        byte_len: artifact.byte_len,
        artifact,
        media_type: "image/png".into(),
        width: Some(1),
        height: Some(1),
    };
    let audit = Arc::new(Audit::default());
    let service = VisionTurnService::new(
        registry.clone(),
        crate::outbound::OutboundGuard::open(&paths).unwrap(),
        profile.service_routes.clone(),
        registry.egress_policies["vision"].allowed.clone(),
        PermissionMode::Ask,
        store,
        owner,
    )
    .with_outbound_audit(audit.clone());
    let plan = service.plan(None).unwrap();
    let error = service
        .execute(
            OperationId::new(),
            "describe it".into(),
            vec![image.clone()],
            plan,
            Some(OutboundApprovalDecision::SaveDeny),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("denied"));

    let error = service
        .execute(
            OperationId::new(),
            "describe it again".into(),
            vec![image],
            service.plan(None).unwrap(),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("denied"));
    assert!(
        audit
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|event| !matches!(event.stage, crate::outbound::OutboundAuditStage::Sending))
    );
}
