//! Shared application policy for focused specialist vision turns.

use crate::{
    artifact::ArtifactStore,
    config::{ConnectionRegistry, OutboundDataClass, PermissionMode},
    credential::CredentialResolver,
    focused_service::{
        FocusedServiceContext, FocusedServiceRegistry, FocusedServiceRequest, FocusedServiceUsage,
        ResolvedServiceRoute, ServiceOperation, descriptor_registry,
        openai_vision::{OpenAiVisionAdapter, VisionProvider},
    },
    identity::{OperationId, PrincipalId},
    vision::ImageRef,
};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisionPlan {
    pub(crate) route: ResolvedServiceRoute,
    pub(crate) permission_mode: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VisionTurnRoute {
    Native,
    Specialist(Box<VisionPlan>),
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
    exposed_routes: Vec<String>,
    profile_egress: Vec<OutboundDataClass>,
    permission_mode: PermissionMode,
    artifacts: ArtifactStore,
    owner: PrincipalId,
}

impl VisionTurnService {
    pub(crate) fn new(
        registry: ConnectionRegistry,
        exposed_routes: Vec<String>,
        profile_egress: Vec<OutboundDataClass>,
        permission_mode: PermissionMode,
        artifacts: ArtifactStore,
        owner: PrincipalId,
    ) -> Self {
        Self {
            registry,
            exposed_routes,
            profile_egress,
            permission_mode,
            artifacts,
            owner,
        }
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
        self.plan(selected_route)
            .map(Box::new)
            .map(VisionTurnRoute::Specialist)
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
        cancellation: CancellationToken,
    ) -> Result<PreparedVisionTurn> {
        if images.is_empty() {
            anyhow::bail!("specialist vision requires at least one attached image");
        }
        let secret = CredentialResolver::default().resolve(
            plan.route
                .credential
                .as_ref()
                .context("selected vision route has no credential reference")?,
        )?;
        let provider = match plan.route.adapter.as_str() {
            "openai.vision" => VisionProvider::OpenAi,
            "openrouter.vision" => VisionProvider::OpenRouter,
            adapter => anyhow::bail!("unsupported vision adapter {adapter:?}"),
        };
        let mut execution = FocusedServiceRegistry::default();
        execution.register(Arc::new(OpenAiVisionAdapter::new(provider, secret)))?;
        let source_artifact_ids = images
            .iter()
            .map(|image| image.artifact.reference.id.to_string())
            .collect::<Vec<_>>();
        let result = execution
            .execute(
                FocusedServiceRequest {
                    operation_id,
                    route: plan.route,
                    prompt: question.clone(),
                    input_artifacts: images.iter().map(|image| image.artifact.clone()).collect(),
                },
                FocusedServiceContext {
                    artifacts: self.artifacts.clone(),
                    owner: self.owner,
                    cancellation,
                },
            )
            .await?;
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

#[cfg(test)]
mod tests;
