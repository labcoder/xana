//! Shared application commands for focused image generation.

use crate::{
    artifact::ArtifactStore,
    cli::ImageCommand,
    config::{ConnectionRegistry, OutboundDataClass, PermissionMode, XanaConfig},
    credential::CredentialResolver,
    focused_service::{
        FocusedServiceContext, FocusedServiceRegistry, FocusedServiceRequest, ServiceOperation,
        image_descriptor_registry, openai_images::OpenAiImageAdapter,
        openrouter_images::OpenRouterImageAdapter,
    },
    identity::{OperationId, PrincipalId},
    paths::XanaPaths,
};
use anyhow::{Context, Result};
use std::{io::Read, io::Write, sync::Arc};
use tokio_util::sync::CancellationToken;

const MAX_STDIN_PROMPT_BYTES: u64 = 64 * 1024;

pub(crate) async fn run(
    command: ImageCommand,
    paths: &XanaPaths,
    input: &mut dyn Read,
    output: &mut dyn Write,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load focused-service configuration")?;
    let (routes, egress, permission) = default_profile(&registry)?;
    let descriptors = image_descriptor_registry()?;
    match command {
        ImageCommand::List { json } => {
            let statuses = routes
                .iter()
                .map(|name| descriptors.inspect(&registry, routes, name))
                .collect::<Vec<_>>();
            if json {
                serde_json::to_writer_pretty(&mut *output, &statuses)?;
                writeln!(output)?;
            } else if statuses.is_empty() {
                writeln!(
                    output,
                    "No image routes are exposed by the default profile."
                )?;
            } else {
                for status in statuses {
                    writeln!(
                        output,
                        "{}\t{}\t{}",
                        status.route,
                        if status.ready { "ready" } else { "unavailable" },
                        status.reason.as_deref().unwrap_or("configured")
                    )?;
                }
            }
            Ok(())
        }
        ImageCommand::Inspect { route, json } => {
            let status = descriptors.inspect(&registry, routes, &route);
            if json {
                serde_json::to_writer_pretty(&mut *output, &status)?;
                writeln!(output)?;
            } else {
                writeln!(output, "Route: {}", status.route)?;
                writeln!(output, "Ready: {}", status.ready)?;
                writeln!(
                    output,
                    "Reason: {}",
                    status.reason.as_deref().unwrap_or("configured and exposed")
                )?;
            }
            Ok(())
        }
        ImageCommand::Generate {
            prompt,
            route,
            yes,
            json,
        } => {
            let prompt = prompt.map_or_else(|| read_prompt(input), Ok)?;
            let resolved = descriptors.resolve(
                &registry,
                routes,
                egress,
                ServiceOperation::ImageGenerate,
                route.as_deref(),
            )?;
            if !resolved
                .allowed_outbound
                .contains(&OutboundDataClass::PromptText)
            {
                anyhow::bail!(
                    "route {:?} is not allowed to send prompt_text by the effective egress policy",
                    resolved.name
                );
            }
            if permission == PermissionMode::Deny {
                anyhow::bail!("image generation is denied by the default profile permission mode");
            }
            if !yes {
                anyhow::bail!(
                    "image generation requires exact noninteractive approval; review route {:?}, provider {:?}, model {:?}, outbound [prompt_text], cost [unknown], then rerun with --yes",
                    resolved.name,
                    resolved.connection,
                    resolved.model
                );
            }
            eprintln!(
                "Generating via route {:?}, connection {:?}, model {:?}; outbound prompt_text; cost unknown...",
                resolved.name, resolved.connection, resolved.model
            );
            let secret = CredentialResolver::default().resolve(
                resolved
                    .credential
                    .as_ref()
                    .context("selected image route has no credential reference")?,
            )?;
            let mut execution = FocusedServiceRegistry::default();
            match resolved.adapter.as_str() {
                "openai.images" => {
                    execution.register(Arc::new(OpenAiImageAdapter::new(secret)?))?
                }
                "openrouter.images" => {
                    execution.register(Arc::new(OpenRouterImageAdapter::new(secret)?))?
                }
                adapter => anyhow::bail!("unsupported image adapter {adapter:?}"),
            }
            let cancellation = CancellationToken::new();
            let request = FocusedServiceRequest {
                operation_id: OperationId::new(),
                route: resolved,
                prompt,
                input_artifacts: Vec::new(),
            };
            let context = FocusedServiceContext {
                artifacts: ArtifactStore::new(paths.data_dir().join("artifacts")),
                owner: PrincipalId::new(),
                cancellation: cancellation.clone(),
            };
            let execution = execution.execute(request, context);
            tokio::pin!(execution);
            let result = tokio::select! {
                result = &mut execution => result?,
                signal = tokio::signal::ctrl_c() => {
                    signal.context("could not listen for cancellation")?;
                    cancellation.cancel();
                    execution.await?
                }
            };
            if json {
                serde_json::to_writer_pretty(&mut *output, &result)?;
                writeln!(output)?;
            } else {
                writeln!(output, "Image generated.")?;
                writeln!(output, "  Route: {}", result.provenance.route)?;
                writeln!(output, "  Model: {}", result.provenance.model)?;
                for artifact in result.artifacts {
                    writeln!(
                        output,
                        "  Artifact: {} ({}; {} bytes; {})",
                        artifact.reference.id,
                        artifact.media_type,
                        artifact.byte_len,
                        artifact.reference.content_hash.as_str()
                    )?;
                }
                if let Some(cost) = result.usage.cost_microusd {
                    writeln!(output, "  Cost: {cost} micro-USD")?;
                } else {
                    writeln!(output, "  Cost: unavailable")?;
                }
            }
            Ok(())
        }
    }
}

fn default_profile(
    registry: &ConnectionRegistry,
) -> Result<(&[String], &[OutboundDataClass], PermissionMode)> {
    let profile = registry
        .profiles
        .get(&registry.default_profile)
        .context("default profile is missing")?;
    let egress = profile
        .egress_policy
        .as_deref()
        .and_then(|name| registry.egress_policies.get(name))
        .map(|policy| policy.allowed.as_slice())
        .unwrap_or_default();
    Ok((
        &profile.service_routes,
        egress,
        profile.permission_mode.unwrap_or(registry.permission_mode),
    ))
}

fn read_prompt(input: &mut dyn Read) -> Result<String> {
    let mut bytes = Vec::new();
    input
        .take(MAX_STDIN_PROMPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STDIN_PROMPT_BYTES {
        anyhow::bail!("stdin prompt exceeds the 64 KiB limit");
    }
    String::from_utf8(bytes).context("stdin prompt must be UTF-8")
}
