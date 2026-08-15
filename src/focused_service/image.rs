use super::*;
use crate::{
    credential::CredentialResolver,
    outbound::{
        NoopOutboundAuditObserver, ObservedOutboundAudit, OutboundAuditObserver, OutboundGuard,
        OutboundItem, OutboundPolicyLayers, OutboundRequest, OutboundTransport,
        OutboundTransportFailure, RecipientIdentity, RecipientKind, ReviewedOutboundApproval,
    },
};
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct ImageGenerationService {
    registry: ConnectionRegistry,
    routes: Vec<String>,
    profile_egress: Vec<OutboundDataClass>,
    outbound: OutboundGuard,
    artifacts: ArtifactStore,
    owner: PrincipalId,
    audit: Arc<dyn OutboundAuditObserver>,
}

#[derive(Debug, Clone)]
pub(crate) struct ImageGenerationPlan {
    request: FocusedServiceRequest,
    recipient: RecipientIdentity,
    policy: OutboundPolicyLayers,
    item: OutboundItem,
}

impl ImageGenerationPlan {
    pub(crate) fn route(&self) -> &ResolvedServiceRoute {
        &self.request.route
    }

    pub(crate) fn recipient_identity_digest(&self) -> &str {
        &self.recipient.identity_digest
    }

    pub(crate) fn for_operation(mut self, operation_id: OperationId) -> Self {
        self.request.operation_id = operation_id;
        self
    }

    pub(crate) fn outbound_review(&self) -> Result<crate::outbound::OutboundApprovalRequest> {
        Ok(OutboundRequest::new(
            self.request.operation_id,
            self.recipient.clone(),
            "generate one image with the selected focused service",
            vec![self.item.clone()],
        )?
        .review())
    }
}

impl ImageGenerationService {
    pub(crate) fn new(
        registry: ConnectionRegistry,
        routes: Vec<String>,
        profile_egress: Vec<OutboundDataClass>,
        outbound: OutboundGuard,
        artifacts: ArtifactStore,
        owner: PrincipalId,
    ) -> Self {
        Self {
            registry,
            routes,
            profile_egress,
            outbound,
            artifacts,
            owner,
            audit: Arc::new(NoopOutboundAuditObserver),
        }
    }

    pub(crate) fn with_outbound_audit(mut self, audit: Arc<dyn OutboundAuditObserver>) -> Self {
        self.audit = audit;
        self
    }

    pub(crate) fn plan(
        &self,
        operation_id: OperationId,
        prompt: String,
        route: Option<&str>,
    ) -> Result<ImageGenerationPlan> {
        let route = image_descriptor_registry()?.resolve(
            &self.registry,
            &self.routes,
            &self.profile_egress,
            ServiceOperation::ImageGenerate,
            route,
        )?;
        let request = FocusedServiceRequest {
            operation_id,
            route: route.clone(),
            prompt: prompt.clone(),
            input_artifacts: Vec::new(),
        };
        request.validate()?;
        if !route
            .allowed_outbound
            .contains(&OutboundDataClass::PromptText)
        {
            anyhow::bail!(
                "route {:?} is not allowed to send prompt_text by the effective egress policy",
                route.name
            );
        }
        let destination = image_destination(&route)?;
        let identity_material = format!(
            "{}\n{}\n{}\n{}",
            route.adapter, route.connection, destination, route.model
        );
        let recipient = RecipientIdentity::new(
            RecipientKind::FocusedService,
            route.connection.clone(),
            destination,
            identity_material.as_bytes(),
        )?;
        let classes = route.allowed_outbound.clone();
        let profile = self.profile_egress.iter().copied().collect();
        Ok(ImageGenerationPlan {
            request,
            recipient,
            policy: OutboundPolicyLayers {
                connection_allowed: classes.clone(),
                user_ceiling: classes,
                profile_allowed: profile,
                conversation_allowed: Some([OutboundDataClass::PromptText].into_iter().collect()),
            },
            item: OutboundItem::new(
                OutboundDataClass::PromptText,
                "image generation prompt",
                None,
                "current explicit image generation request",
                prompt.into_bytes(),
            )?,
        })
    }

    pub(crate) async fn execute(
        &self,
        plan: ImageGenerationPlan,
        approval: Option<ReviewedOutboundApproval>,
        cancellation: CancellationToken,
    ) -> Result<FocusedServiceResult> {
        let request = OutboundRequest::new(
            plan.request.operation_id,
            plan.recipient.clone(),
            "generate one image with the selected focused service",
            vec![plan.item],
        )?;
        let expected_recipient = plan.recipient;
        let mut transport = ImageTransport {
            request: Some(plan.request),
            context: Some(FocusedServiceContext {
                artifacts: self.artifacts.clone(),
                owner: self.owner,
                cancellation,
            }),
            expected_recipient,
            failure: None,
        };
        let mut approval = approval;
        let mut audit = ObservedOutboundAudit::new(self.audit.as_ref());
        match self
            .outbound
            .dispatch(
                request,
                &plan.policy,
                approval.as_mut(),
                &mut transport,
                &mut audit,
            )
            .await
        {
            Ok(result) => Ok(result),
            Err(error) => {
                if let Some(failure) = transport.failure {
                    Err(anyhow::Error::new(failure))
                } else {
                    Err(anyhow::Error::new(error))
                }
            }
        }
    }

    pub(crate) fn disposition(
        &self,
        plan: &ImageGenerationPlan,
    ) -> Result<crate::outbound::OutboundDisposition> {
        self.outbound
            .disposition(&plan.recipient, [OutboundDataClass::PromptText])
            .map_err(anyhow::Error::new)
    }
}

fn image_destination(route: &ResolvedServiceRoute) -> Result<String> {
    let (default, suffix) = match route.adapter.as_str() {
        "openai.images" => ("https://api.openai.com/v1", "images/generations"),
        "openrouter.images" => ("https://openrouter.ai/api/v1", "images"),
        adapter => anyhow::bail!("unsupported image adapter {adapter:?}"),
    };
    let mut base = reqwest::Url::parse(route.base_url.as_deref().unwrap_or(default))
        .context("image route has an invalid base URL")?;
    if base.scheme() != "https"
        || base.username() != ""
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.host_str().is_none()
    {
        anyhow::bail!(
            "image route base URL must be an exact HTTPS origin/path without credentials, query, or fragment"
        );
    }
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    Ok(base.join(suffix)?.to_string())
}

struct ImageTransport {
    request: Option<FocusedServiceRequest>,
    context: Option<FocusedServiceContext>,
    expected_recipient: RecipientIdentity,
    failure: Option<FocusedServiceError>,
}

impl OutboundTransport for ImageTransport {
    type Receipt = FocusedServiceResult;

    fn send<'a>(
        &'a mut self,
        recipient: &'a RecipientIdentity,
        items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        Box::pin(async move {
            if recipient != &self.expected_recipient || items.len() != 1 {
                return Err(OutboundTransportFailure::Protocol);
            }
            let request = self
                .request
                .take()
                .ok_or(OutboundTransportFailure::Protocol)?;
            let context = self
                .context
                .take()
                .ok_or(OutboundTransportFailure::Protocol)?;
            let result = execute_image_adapter(request, context).await;
            match result {
                Ok(result) => Ok(result),
                Err(error) => {
                    let failure = map_failure(&error);
                    self.failure = Some(error);
                    Err(failure)
                }
            }
        })
    }
}

async fn execute_image_adapter(
    request: FocusedServiceRequest,
    context: FocusedServiceContext,
) -> Result<FocusedServiceResult, FocusedServiceError> {
    let secret = CredentialResolver::default()
        .resolve(request.route.credential.as_ref().ok_or_else(|| {
            FocusedServiceError::MissingCredentialReference(request.route.connection.clone())
        })?)
        .map_err(|_| FocusedServiceError::Authentication)?;
    let mut execution = FocusedServiceRegistry::default();
    match request.route.adapter.as_str() {
        "openai.images" => {
            execution.register(Arc::new(openai_images::OpenAiImageAdapter::new(secret)?))?
        }
        "openrouter.images" => execution.register(Arc::new(
            openrouter_images::OpenRouterImageAdapter::new(secret)?,
        ))?,
        adapter => return Err(FocusedServiceError::AdapterUnavailable(adapter.to_owned())),
    }
    execution.execute(request, context).await
}

fn map_failure(error: &FocusedServiceError) -> OutboundTransportFailure {
    match error {
        FocusedServiceError::Cancelled => OutboundTransportFailure::Cancelled,
        FocusedServiceError::Timeout => OutboundTransportFailure::TimedOut,
        FocusedServiceError::Authentication
        | FocusedServiceError::RateLimited
        | FocusedServiceError::Quota
        | FocusedServiceError::ContentPolicy => OutboundTransportFailure::Rejected,
        FocusedServiceError::Transport | FocusedServiceError::AdapterUnavailable(_) => {
            OutboundTransportFailure::Unavailable
        }
        _ => OutboundTransportFailure::Protocol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::XanaConfig, outbound::OutboundAuditEvent, paths::XanaPaths};
    use std::sync::Mutex;
    use tempfile::tempdir;

    const CONFIG: &str = r#"
version = 4
default_profile = "default"
permission_mode = "ask"

[providers.chat]
kind = "ollama"

[profiles.default]
connection = "chat"
model = "text-model"
service_routes = ["draw"]
egress_policy = "images"

[service_connections.images]
adapter = "openai.images"
credential = { source = "environment", variable = "XANA_TEST_MISSING_IMAGE_KEY" }

[service_routes.draw]
operation = "image.generate"
connection = "images"
model = "gpt-image-1"
default = true
egress_policy = "images"

[egress_policies.images]
allowed = ["prompt_text"]
"#;

    #[derive(Default)]
    struct Audit(Mutex<Vec<OutboundAuditEvent>>);

    impl OutboundAuditObserver for Audit {
        fn observe(
            &self,
            event: &OutboundAuditEvent,
        ) -> std::result::Result<(), crate::private_state::PrivateStateError> {
            self.0.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    fn service() -> (tempfile::TempDir, ImageGenerationService, Arc<Audit>) {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
        let registry = XanaConfig::parse_registry(CONFIG).unwrap();
        let profile = &registry.profiles["default"];
        let audit = Arc::new(Audit::default());
        let service = ImageGenerationService::new(
            registry.clone(),
            profile.service_routes.clone(),
            registry.egress_policies["images"].allowed.clone(),
            OutboundGuard::open(&paths).unwrap(),
            ArtifactStore::new(paths.data_dir().join("artifacts")),
            PrincipalId::new(),
        )
        .with_outbound_audit(audit.clone());
        (directory, service, audit)
    }

    #[tokio::test]
    async fn saved_denial_prevents_credential_resolution_and_transport_dispatch() {
        let (_directory, service, audit) = service();
        let plan = service
            .plan(OperationId::new(), "draw a bounded fixture".into(), None)
            .unwrap();
        let approval = ReviewedOutboundApproval::new(
            plan.outbound_review().unwrap(),
            crate::outbound::OutboundApprovalDecision::SaveDeny,
        );
        let error = service
            .execute(plan, Some(approval), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("denied"));

        let retry = service
            .plan(OperationId::new(), "draw a second fixture".into(), None)
            .unwrap();
        let error = service
            .execute(retry, None, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("denied"));
        let events = audit.0.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.stage, crate::outbound::OutboundAuditStage::Sending))
                .count(),
            0
        );
    }
}
