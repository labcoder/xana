use super::*;
use crate::{
    credential::CredentialResolver,
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
struct GenerateImagePlan {
    prompt: String,
    route: ResolvedServiceRoute,
}

struct GenerateImageTool {
    registry: ConnectionRegistry,
    exposed_routes: Vec<String>,
    profile_egress: Vec<OutboundDataClass>,
    artifacts: ArtifactStore,
    owner: PrincipalId,
}

pub(crate) fn activate_profile_image_tool(
    registry: &ConnectionRegistry,
    exposed_routes: &[String],
    profile_egress: &[OutboundDataClass],
    artifacts: ArtifactStore,
    owner: PrincipalId,
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
            registry: registry.clone(),
            exposed_routes: exposed_routes.to_vec(),
            profile_egress: profile_egress.to_vec(),
            artifacts,
            owner,
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
        let route = image_descriptor_registry()
            .map_err(|error| error.to_string())?
            .resolve(
                &self.registry,
                &self.exposed_routes,
                &self.profile_egress,
                ServiceOperation::ImageGenerate,
                arguments.route.as_deref(),
            )
            .map_err(|error| error.to_string())?;
        let request = FocusedServiceRequest {
            operation_id: OperationId::new(),
            route: route.clone(),
            prompt: arguments.prompt.clone(),
            input_artifacts: Vec::new(),
        };
        request.validate().map_err(|error| error.to_string())?;
        if !route
            .allowed_outbound
            .contains(&OutboundDataClass::PromptText)
        {
            return Err(
                "the effective route/profile egress policy does not allow prompt_text".to_owned(),
            );
        }
        let identity = blake3::hash(
            format!("{}\0{}\0{}", route.connection, route.adapter, route.model).as_bytes(),
        )
        .to_hex()
        .to_string();
        Ok(PlannedToolInvocation::new(
            json!({"prompt": arguments.prompt, "route": route.name}),
            PermissionScope::External {
                recipient_identity_digest: identity,
                operation: ServiceOperation::ImageGenerate.as_str().to_owned(),
            },
            GenerateImagePlan {
                prompt: request.prompt,
                route,
            },
        ))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<GenerateImagePlan>("generate_image")?;
            let secret =
                CredentialResolver::default()
                    .resolve(plan.route.credential.as_ref().ok_or_else(|| {
                        "selected image route has no credential reference".to_owned()
                    })?)
                    .map_err(|_| "selected image credential is unavailable".to_owned())?;
            let mut execution = FocusedServiceRegistry::default();
            match plan.route.adapter.as_str() {
                "openai.images" => execution
                    .register(Arc::new(
                        openai_images::OpenAiImageAdapter::new(secret)
                            .map_err(|error| error.to_string())?,
                    ))
                    .map_err(|error| error.to_string())?,
                "openrouter.images" => execution
                    .register(Arc::new(
                        openrouter_images::OpenRouterImageAdapter::new(secret)
                            .map_err(|error| error.to_string())?,
                    ))
                    .map_err(|error| error.to_string())?,
                adapter => return Err(format!("unsupported image adapter {adapter:?}")),
            }
            let result = execution
                .execute(
                    FocusedServiceRequest {
                        operation_id: context.operation_id,
                        route: plan.route.clone(),
                        prompt: plan.prompt.clone(),
                        input_artifacts: Vec::new(),
                    },
                    FocusedServiceContext {
                        artifacts: self.artifacts.clone(),
                        owner: self.owner,
                        cancellation: CancellationToken::new(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&result)
                .map_err(|_| "image result could not be encoded".to_owned())
        })
    }
}
