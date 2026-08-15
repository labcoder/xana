//! Shared application commands for focused image generation.

use crate::{
    artifact::ArtifactStore,
    cli::ImageCommand,
    config::{ConnectionRegistry, OutboundDataClass, PermissionMode, XanaConfig},
    focused_service::{ImageGenerationService, image_descriptor_registry},
    identity::{OperationId, PrincipalId},
    paths::XanaPaths,
};
use anyhow::{Context, Result};
use std::{io::Read, io::Write};
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
            if permission == PermissionMode::Deny {
                anyhow::bail!("image generation is denied by the default profile permission mode");
            }
            let service = ImageGenerationService::new(
                registry.clone(),
                routes.to_vec(),
                egress.to_vec(),
                crate::outbound::OutboundGuard::open(paths)?,
                ArtifactStore::new(paths.data_dir().join("artifacts")),
                PrincipalId::new(),
            )
            .with_outbound_audit(crate::diagnostics::outbound_audit(paths)?);
            let plan = service.plan(OperationId::new(), prompt, route.as_deref())?;
            let resolved = plan.route();
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
            let cancellation = CancellationToken::new();
            let approval = crate::outbound::ReviewedOutboundApproval::new(
                plan.outbound_review()?,
                crate::outbound::OutboundApprovalDecision::AllowOnce,
            );
            let execution = service.execute(plan, Some(approval), cancellation.clone());
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
