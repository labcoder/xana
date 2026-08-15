use super::*;
use crate::{
    paths::XanaPaths,
    permission::PermissionScope,
    tool::{
        EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition,
        ToolExecutionContext, ToolRegistry,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerateImageArgs {
    prompt: String,
    #[serde(default)]
    route: Option<String>,
}

#[derive(Clone)]
struct GenerateImageTool {
    service: ImageGenerationService,
}

pub(crate) fn activate_profile_image_tool(
    registry: &ConnectionRegistry,
    exposed_routes: &[String],
    profile_egress: &[OutboundDataClass],
    artifacts: ArtifactStore,
    owner: PrincipalId,
    paths: &XanaPaths,
    tools: &mut ToolRegistry,
) -> Result<(), String> {
    let descriptors = image_descriptor_registry().map_err(|error| error.to_string())?;
    let has_image_route = exposed_routes.iter().any(|name| {
        descriptors
            .inspect(registry, exposed_routes, name)
            .operation
            == Some(ServiceOperation::ImageGenerate)
    });
    if !has_image_route {
        return Ok(());
    }
    tools
        .register(GenerateImageTool {
            service: ImageGenerationService::new(
                registry.clone(),
                exposed_routes.to_vec(),
                profile_egress.to_vec(),
                crate::outbound::OutboundGuard::open(paths).map_err(|error| error.to_string())?,
                artifacts,
                owner,
            )
            .with_outbound_audit(
                crate::diagnostics::outbound_audit(paths).map_err(|error| error.to_string())?,
            ),
        })
        .map_err(|error| error.to_string())
}

impl Tool for GenerateImageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "generate_image".to_owned(),
            contract_version: 1,
            description: "Generate one immutable image artifact using an image route explicitly exposed by the active Xana profile. This is a paid external effect.".to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_FOCUSED_PROMPT_BYTES},
                    "route": {"type": "string", "description": "Exact exposed route; omit only when one route is declared default"}
                },
                "required": ["prompt"]
            }),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        let arguments: GenerateImageArgs = serde_json::from_value(arguments.clone())
            .map_err(|error| format!("invalid generate_image arguments: {error}"))?;
        let plan = self
            .service
            .plan(
                OperationId::new(),
                arguments.prompt.clone(),
                arguments.route.as_deref(),
            )
            .map_err(|error| error.to_string())?;
        let review = plan.outbound_review().map_err(|error| error.to_string())?;
        Ok(PlannedToolInvocation::new(
            json!({
                "prompt": arguments.prompt,
                "route": plan.route().name,
                "outbound_classes": ["prompt_text"],
            }),
            PermissionScope::External {
                recipient_identity_digest: plan.recipient_identity_digest().to_owned(),
                operation: ServiceOperation::ImageGenerate.as_str().to_owned(),
            },
            plan,
        )
        .with_outbound_review(review))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<ImageGenerationPlan>("generate_image")?;
            let plan = plan.clone().for_operation(context.operation_id);
            let cancellation = CancellationToken::new();
            let result = self
                .service
                .execute(plan, context.outbound_approval, cancellation)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&result)
                .map_err(|_| "image result could not be encoded".to_owned())
        })
    }

    fn outbound_disposition(
        &self,
        planned: &PlannedToolInvocation,
    ) -> Result<Option<crate::outbound::OutboundDisposition>, String> {
        let plan = planned.executable::<ImageGenerationPlan>("generate_image")?;
        self.service
            .disposition(plan)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}
