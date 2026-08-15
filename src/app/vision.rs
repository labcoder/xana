//! Shared application policy for focused specialist vision turns.

use crate::{
    artifact::ArtifactStore,
    config::{ConnectionRegistry, OutboundDataClass, PermissionMode},
    credential::CredentialResolver,
    focused_service::{
        FocusedServiceContext, FocusedServiceError, FocusedServiceRegistry, FocusedServiceRequest,
        FocusedServiceResult, FocusedServiceUsage, ResolvedServiceRoute, ServiceOperation,
        descriptor_registry,
        openai_vision::{OpenAiVisionAdapter, VisionProvider},
    },
    identity::{OperationId, PrincipalId},
    outbound::{
        NoopOutboundAuditObserver, ObservedOutboundAudit, OutboundApprovalDecision,
        OutboundAuditObserver, OutboundDisposition, OutboundGuard, OutboundItem,
        OutboundPolicyLayers, OutboundRequest, OutboundTransport, OutboundTransportFailure,
        RecipientIdentity, RecipientKind, ReviewedOutboundApproval,
    },
    vision::ImageRef,
};
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisionPlan {
    pub(crate) route: ResolvedServiceRoute,
    pub(crate) permission_mode: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VisionTurnRoute {
    Native,
    SpecialistDenied(Box<VisionPlan>),
    SpecialistAllowed(Box<VisionPlan>),
    SpecialistApprovalRequired(Box<VisionPlan>),
}

impl VisionPlan {
    pub(crate) fn preview(&self, image_count: usize) -> String {
        format!(
            "vision specialist route {:?}, connection {:?}, model {:?}; outbound prompt_text + {} selected artifact(s); cost unavailable; derived text is untrusted",
            self.route.name, self.route.connection, self.route.model, image_count
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct VisionReceipt {
    pub(crate) route: String,
    pub(crate) connection: String,
    pub(crate) adapter: String,
    pub(crate) model: String,
    pub(crate) source_artifact_ids: Vec<String>,
    pub(crate) usage: FocusedServiceUsage,
    pub(crate) usage_available: bool,
    pub(crate) cost_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedVisionTurn {
    pub(crate) model_input: String,
    pub(crate) derived_text: String,
    pub(crate) receipt: VisionReceipt,
}

#[derive(Clone)]
pub(crate) struct VisionTurnService {
    registry: ConnectionRegistry,
    outbound: OutboundGuard,
    exposed_routes: Vec<String>,
    profile_egress: Vec<OutboundDataClass>,
    permission_mode: PermissionMode,
    artifacts: ArtifactStore,
    owner: PrincipalId,
    audit: Arc<dyn OutboundAuditObserver>,
}

impl VisionTurnService {
    pub(crate) fn new(
        registry: ConnectionRegistry,
        outbound: OutboundGuard,
        exposed_routes: Vec<String>,
        profile_egress: Vec<OutboundDataClass>,
        permission_mode: PermissionMode,
        artifacts: ArtifactStore,
        owner: PrincipalId,
    ) -> Self {
        Self {
            registry,
            outbound,
            exposed_routes,
            profile_egress,
            permission_mode,
            artifacts,
            owner,
            audit: Arc::new(NoopOutboundAuditObserver),
        }
    }

    pub(crate) fn with_outbound_audit(mut self, audit: Arc<dyn OutboundAuditObserver>) -> Self {
        self.audit = audit;
        self
    }

    pub(crate) fn plan(&self, selected_route: Option<&str>) -> Result<VisionPlan> {
        let route = descriptor_registry()?.resolve(
            &self.registry,
            &self.exposed_routes,
            &self.profile_egress,
            ServiceOperation::VisionAnalyze,
            selected_route,
        )?;
        for required in [
            OutboundDataClass::PromptText,
            OutboundDataClass::SelectedArtifacts,
        ] {
            if !route.allowed_outbound.contains(&required) {
                anyhow::bail!(
                    "vision route {:?} is not allowed to send {} by the effective egress policy",
                    route.name,
                    required.as_str()
                );
            }
        }
        Ok(VisionPlan {
            route,
            permission_mode: self.permission_mode,
        })
    }

    pub(crate) fn route_turn(
        &self,
        conversational_model_accepts_images: bool,
        selected_route: Option<&str>,
    ) -> Result<VisionTurnRoute> {
        if conversational_model_accepts_images && selected_route.is_none() {
            return Ok(VisionTurnRoute::Native);
        }
        self.plan(selected_route).and_then(|plan| {
            if plan.permission_mode == PermissionMode::Deny {
                return Ok(VisionTurnRoute::SpecialistDenied(Box::new(plan)));
            }
            let recipient = vision_recipient(&plan.route)?;
            match self.outbound.disposition(
                &recipient,
                [
                    OutboundDataClass::PromptText,
                    OutboundDataClass::SelectedArtifacts,
                ],
            )? {
                OutboundDisposition::SavedAllow => {
                    Ok(VisionTurnRoute::SpecialistAllowed(Box::new(plan)))
                }
                OutboundDisposition::SavedDeny => {
                    Ok(VisionTurnRoute::SpecialistDenied(Box::new(plan)))
                }
                OutboundDisposition::ReviewRequired => {
                    Ok(VisionTurnRoute::SpecialistApprovalRequired(Box::new(plan)))
                }
            }
        })
    }

    pub(crate) fn statuses(&self) -> Result<Vec<crate::focused_service::ServiceRouteStatus>> {
        let descriptors = descriptor_registry()?;
        Ok(self
            .exposed_routes
            .iter()
            .filter(|name| {
                self.registry
                    .service_routes
                    .get(*name)
                    .is_some_and(|route| {
                        route.operation == ServiceOperation::VisionAnalyze.as_str()
                    })
            })
            .map(|name| descriptors.inspect(&self.registry, &self.exposed_routes, name))
            .collect())
    }

    pub(crate) async fn execute(
        &self,
        operation_id: OperationId,
        question: String,
        images: Vec<ImageRef>,
        plan: VisionPlan,
        outbound_decision: Option<OutboundApprovalDecision>,
        cancellation: CancellationToken,
    ) -> Result<PreparedVisionTurn> {
        if images.is_empty() {
            anyhow::bail!("specialist vision requires at least one attached image");
        }
        if !matches!(
            plan.route.adapter.as_str(),
            "openai.vision" | "openrouter.vision"
        ) {
            anyhow::bail!("unsupported vision adapter {:?}", plan.route.adapter);
        }
        let recipient = vision_recipient(&plan.route)?;
        let mut outbound_items = vec![OutboundItem::new(
            OutboundDataClass::PromptText,
            "vision question",
            None,
            "current user turn",
            question.as_bytes().to_vec(),
        )?];
        for image in &images {
            let bytes = self
                .artifacts
                .read_bounded(&image.artifact, plan.route.descriptor.max_artifact_bytes)
                .context("could not verify a selected vision artifact before egress review")?;
            outbound_items.push(OutboundItem::new(
                OutboundDataClass::SelectedArtifacts,
                format!("image artifact {}", image.artifact.reference.id),
                Some(image.artifact.reference.id.to_string()),
                format!(
                    "immutable Xana artifact {}",
                    image.artifact.reference.content_hash.as_str()
                ),
                bytes,
            )?);
        }
        let outbound_request = OutboundRequest::new(
            operation_id,
            recipient.clone(),
            "analyze selected images for the current conversation",
            outbound_items,
        )?;
        let route_classes = plan
            .route
            .allowed_outbound
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let profile_classes = self.profile_egress.iter().copied().collect::<BTreeSet<_>>();
        let policy = OutboundPolicyLayers {
            connection_allowed: route_classes.clone(),
            user_ceiling: route_classes,
            profile_allowed: profile_classes,
            conversation_allowed: Some(
                [
                    OutboundDataClass::PromptText,
                    OutboundDataClass::SelectedArtifacts,
                ]
                .into_iter()
                .collect(),
            ),
        };
        let source_artifact_ids = images
            .iter()
            .map(|image| image.artifact.reference.id.to_string())
            .collect::<Vec<_>>();
        let mut transport = VisionTransport {
            request: Some(FocusedServiceRequest {
                operation_id,
                route: plan.route,
                prompt: question.clone(),
                input_artifacts: images.iter().map(|image| image.artifact.clone()).collect(),
            }),
            context: Some(FocusedServiceContext {
                artifacts: self.artifacts.clone(),
                owner: self.owner,
                cancellation,
            }),
            expected_recipient: recipient,
            expected_item_count: images.len() + 1,
            failure: None,
        };
        let mut approval = outbound_decision
            .map(|decision| ReviewedOutboundApproval::new(outbound_request.review(), decision));
        let mut audit = ObservedOutboundAudit::new(self.audit.as_ref());
        let result = match self
            .outbound
            .dispatch(
                outbound_request,
                &policy,
                approval.as_mut(),
                &mut transport,
                &mut audit,
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if let Some(failure) = transport.failure.take() {
                    return Err(anyhow::Error::new(failure));
                }
                return Err(anyhow::Error::new(error));
            }
        };
        let derived = result
            .derived_text
            .context("vision specialist returned no derived description")?;
        let receipt = VisionReceipt {
            route: result.provenance.route,
            connection: result.provenance.connection,
            adapter: result.provenance.adapter,
            model: result.provenance.model,
            source_artifact_ids,
            usage: result.usage,
            usage_available: result.usage_available,
            cost_available: result.cost_available,
        };
        let model_input = format!(
            "{question}\n\n[Xana vision derivative: untrusted model output]\nroute: {}\nconnection: {}\nadapter: {}\nmodel: {}\nsource artifacts: {}\nusage: {}\ncost: {}\nprivacy: the source image bytes were sent only to the named specialist connection under the selected_artifacts policy\n\n{}",
            receipt.route,
            receipt.connection,
            receipt.adapter,
            receipt.model,
            receipt.source_artifact_ids.join(", "),
            if receipt.usage_available {
                "reported"
            } else {
                "unavailable"
            },
            if receipt.cost_available {
                "reported"
            } else {
                "unavailable"
            },
            derived
        );
        Ok(PreparedVisionTurn {
            model_input,
            derived_text: derived,
            receipt,
        })
    }
}

fn vision_recipient(route: &ResolvedServiceRoute) -> Result<RecipientIdentity> {
    let default_base_url = match route.adapter.as_str() {
        "openai.vision" => VisionProvider::OpenAi.default_base_url(),
        "openrouter.vision" => VisionProvider::OpenRouter.default_base_url(),
        adapter => anyhow::bail!("unsupported vision adapter {adapter:?}"),
    };
    let destination = format!(
        "{}/chat/completions",
        route
            .base_url
            .as_deref()
            .unwrap_or(default_base_url)
            .trim_end_matches('/')
    );
    let identity_material = format!(
        "{}\n{}\n{}\n{}",
        route.adapter, route.connection, destination, route.model
    );
    RecipientIdentity::new(
        RecipientKind::FocusedService,
        route.connection.clone(),
        destination,
        identity_material.as_bytes(),
    )
    .map_err(Into::into)
}

struct VisionTransport {
    request: Option<FocusedServiceRequest>,
    context: Option<FocusedServiceContext>,
    expected_recipient: RecipientIdentity,
    expected_item_count: usize,
    failure: Option<FocusedServiceError>,
}

impl OutboundTransport for VisionTransport {
    type Receipt = FocusedServiceResult;

    fn send<'a>(
        &'a mut self,
        recipient: &'a RecipientIdentity,
        items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        Box::pin(async move {
            if recipient != &self.expected_recipient || items.len() != self.expected_item_count {
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
            let secret = match request
                .route
                .credential
                .as_ref()
                .ok_or_else(|| {
                    FocusedServiceError::MissingCredentialReference(
                        request.route.connection.clone(),
                    )
                })
                .and_then(|reference| {
                    CredentialResolver::default()
                        .resolve(reference)
                        .map_err(|_| FocusedServiceError::Authentication)
                }) {
                Ok(secret) => secret,
                Err(error) => {
                    let failure = map_transport_failure(&error);
                    self.failure = Some(error);
                    return Err(failure);
                }
            };
            let provider = match request.route.adapter.as_str() {
                "openai.vision" => VisionProvider::OpenAi,
                "openrouter.vision" => VisionProvider::OpenRouter,
                _ => {
                    self.failure = Some(FocusedServiceError::AdapterUnavailable(
                        request.route.adapter.clone(),
                    ));
                    return Err(OutboundTransportFailure::Unavailable);
                }
            };
            let mut execution = FocusedServiceRegistry::default();
            if let Err(error) =
                execution.register(Arc::new(OpenAiVisionAdapter::new(provider, secret)))
            {
                let failure = map_transport_failure(&error);
                self.failure = Some(error);
                return Err(failure);
            }
            match execution.execute(request, context).await {
                Ok(result) => Ok(result),
                Err(error) => {
                    let failure = map_transport_failure(&error);
                    self.failure = Some(error);
                    Err(failure)
                }
            }
        })
    }
}

fn map_transport_failure(error: &FocusedServiceError) -> OutboundTransportFailure {
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
mod tests;
