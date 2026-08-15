//! One-shot CLI surface for focused vision analysis.

use super::vision::VisionTurnService;
use crate::{
    artifact::ArtifactStore,
    cli::VisionCommand,
    config::{ConnectionRegistry, OutboundDataClass, PermissionMode, XanaConfig},
    identity::{OperationId, PrincipalId},
    paths::XanaPaths,
    vision::{
        DroppedImagePath, ImageIngestor, ImageLimits, MAX_IMAGE_BYTES_PER_TURN,
        classify_dropped_image_path,
    },
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::io::{Read, Write};
use tokio_util::sync::CancellationToken;

const MAX_QUESTION_BYTES: u64 = 32 * 1024;

pub(crate) async fn run(
    command: VisionCommand,
    paths: &XanaPaths,
    input: &mut dyn Read,
    output: &mut dyn Write,
) -> Result<()> {
    let registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load focused-service configuration")?;
    let (routes, egress, permission) = default_profile(&registry)?;
    let owner = PrincipalId::new();
    let service = VisionTurnService::new(
        registry,
        crate::outbound::OutboundGuard::open(paths)
            .context("could not open the outbound policy gate for vision")?,
        routes,
        egress,
        permission,
        ArtifactStore::new(paths.data_dir().join("artifacts")),
        owner,
    )
    .with_outbound_audit(crate::diagnostics::outbound_audit(paths)?);
    match command {
        VisionCommand::List { json } => {
            let statuses = service.statuses()?;
            if json {
                serde_json::to_writer_pretty(&mut *output, &statuses)?;
                writeln!(output)?;
            } else if statuses.is_empty() {
                writeln!(
                    output,
                    "No vision routes are exposed by the default profile."
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
        VisionCommand::Inspect { route, json } => {
            let status = service
                .statuses()?
                .into_iter()
                .find(|status| status.route == route)
                .unwrap_or(crate::focused_service::ServiceRouteStatus {
                    route,
                    operation: None,
                    configured: false,
                    exposed: false,
                    compatible: false,
                    credential_declared: false,
                    ready: false,
                    reason: Some("not exposed by the default profile".to_owned()),
                });
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
        VisionCommand::Analyze {
            images,
            question,
            route,
            yes,
            json,
        } => {
            let question = question.map_or_else(|| read_question(input), Ok)?;
            let plan = service.plan(route.as_deref())?;
            if plan.permission_mode == PermissionMode::Deny {
                anyhow::bail!("vision analysis is denied by the default profile permission mode");
            }
            if !yes {
                anyhow::bail!(
                    "vision analysis requires exact noninteractive approval; review {}, then rerun with --yes",
                    plan.preview(images.len())
                );
            }
            eprintln!("Analyzing via {}...", plan.preview(images.len()));
            let workspace = std::env::current_dir()
                .context("could not resolve the current workspace for image ingestion")?;
            let ingestor = ImageIngestor::new(
                ArtifactStore::new(paths.data_dir().join("artifacts")),
                ImageLimits::default(),
            );
            let mut total = 0_u64;
            let mut staged = Vec::with_capacity(images.len());
            for path in images {
                let value = path.to_string_lossy().into_owned();
                let attachment = match classify_dropped_image_path(&workspace, &value)? {
                    DroppedImagePath::Workspace { relative } => {
                        ingestor.ingest_path(&workspace, &relative, owner)?
                    }
                    DroppedImagePath::External { .. } => {
                        ingestor.ingest_approved_dropped_path(&workspace, &value, owner)?
                    }
                };
                total = total.saturating_add(attachment.image.byte_len);
                if total > MAX_IMAGE_BYTES_PER_TURN {
                    anyhow::bail!("image inputs exceed the 20 MiB per-turn budget");
                }
                staged.push(attachment.image);
            }
            let cancellation = CancellationToken::new();
            let execution = service.execute(
                OperationId::new(),
                question,
                staged,
                plan,
                Some(crate::outbound::OutboundApprovalDecision::AllowOnce),
                cancellation.clone(),
            );
            tokio::pin!(execution);
            let prepared = tokio::select! {
                result = &mut execution => result?,
                signal = tokio::signal::ctrl_c() => {
                    signal.context("could not listen for cancellation")?;
                    cancellation.cancel();
                    execution.await?
                }
            };
            if json {
                #[derive(Serialize)]
                struct Output<'a> {
                    receipt: &'a super::vision::VisionReceipt,
                    derived_text: &'a str,
                }
                serde_json::to_writer_pretty(
                    &mut *output,
                    &Output {
                        receipt: &prepared.receipt,
                        derived_text: &prepared.derived_text,
                    },
                )?;
                writeln!(output)?;
            } else {
                writeln!(output, "Vision analysis complete.")?;
                writeln!(output, "  Route: {}", prepared.receipt.route)?;
                writeln!(output, "  Model: {}", prepared.receipt.model)?;
                writeln!(
                    output,
                    "  Source artifacts: {}",
                    prepared.receipt.source_artifact_ids.join(", ")
                )?;
                writeln!(
                    output,
                    "  Derived text is untrusted:\n{}",
                    prepared.derived_text
                )?;
            }
            Ok(())
        }
    }
}

fn default_profile(
    registry: &ConnectionRegistry,
) -> Result<(Vec<String>, Vec<OutboundDataClass>, PermissionMode)> {
    let profile = registry
        .profiles
        .get(&registry.default_profile)
        .context("default profile is missing")?;
    let egress = profile
        .egress_policy
        .as_deref()
        .and_then(|name| registry.egress_policies.get(name))
        .map(|policy| policy.allowed.clone())
        .unwrap_or_default();
    Ok((
        profile.service_routes.clone(),
        egress,
        profile.permission_mode.unwrap_or(registry.permission_mode),
    ))
}

fn read_question(input: &mut dyn Read) -> Result<String> {
    let mut bytes = Vec::new();
    input.take(MAX_QUESTION_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_QUESTION_BYTES {
        anyhow::bail!("stdin question exceeds the 32 KiB limit");
    }
    String::from_utf8(bytes).context("stdin question must be UTF-8")
}
