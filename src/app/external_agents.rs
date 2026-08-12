//! Typed application commands for A2A declaration, discovery, and trust.

use crate::{
    a2a::ExternalAgentManager,
    cli::ExternalAgentCommand,
    config::{CredentialReference, NewExternalAgent, XanaConfig},
    paths::XanaPaths,
};
use anyhow::{Context, Result};
use std::io::Write;
use tokio_util::sync::CancellationToken;

pub(super) async fn run(
    command: ExternalAgentCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let manager = ExternalAgentManager::open(paths);
    match command {
        ExternalAgentCommand::Add {
            name,
            endpoint,
            env,
            credential_id,
            egress_policy,
        } => {
            let credential = env
                .map(|variable| CredentialReference::Environment { variable })
                .or_else(|| credential_id.map(|id| CredentialReference::Stored { id }));
            XanaConfig::add_external_agent(
                paths.config_file(),
                NewExternalAgent {
                    id: name.clone(),
                    endpoint,
                    credential,
                    egress_policy,
                },
            )?;
            writeln!(
                output,
                "External agent {name:?} declared. No network request was made.\nNext: xana external-agent refresh {name}"
            )?;
        }
        ExternalAgentCommand::List => {
            let registry = XanaConfig::load_registry_from(paths.config_file())?;
            let views = manager.list(&registry)?;
            if views.is_empty() {
                writeln!(output, "No external A2A agents are configured.")?;
            }
            for view in views {
                writeln!(
                    output,
                    "{}\t{}\t{}",
                    view.name,
                    view.readiness.as_str(),
                    view.endpoint
                )?;
            }
        }
        ExternalAgentCommand::Show { name } => {
            let registry = XanaConfig::load_registry_from(paths.config_file())?;
            let declaration = registry
                .external_agents
                .get(&name)
                .with_context(|| format!("unknown external agent {name:?}"))?;
            let view = manager.show(&name, declaration)?;
            serde_json::to_writer_pretty(
                &mut *output,
                &serde_json::json!({
                    "name":view.name,
                    "configured_endpoint":view.endpoint,
                    "enabled":view.enabled,
                    "readiness":view.readiness.as_str(),
                    "card":view.cached,
                }),
            )?;
            writeln!(output)?;
        }
        ExternalAgentCommand::Refresh { name } => {
            let registry = XanaConfig::load_registry_from(paths.config_file())?;
            let declaration = registry
                .external_agents
                .get(&name)
                .with_context(|| format!("unknown external agent {name:?}"))?;
            let prior = manager.show(&name, declaration)?.cached;
            let refreshed = manager
                .refresh(&name, declaration, &CancellationToken::new())
                .await?;
            writeln!(
                output,
                "Agent Card refreshed for {name:?}: {} ({}, {})",
                refreshed.agent_name, refreshed.protocol_binding, refreshed.protocol_version
            )?;
            match prior {
                Some(prior) if prior.identity_digest == refreshed.identity_digest => {
                    writeln!(output, "Identity unchanged: {}", refreshed.identity_digest)?;
                }
                Some(prior) => {
                    writeln!(output, "Identity changed; trust is blocked pending review.")?;
                    writeln!(output, "Previous: {}", prior.identity_digest)?;
                    writeln!(output, "Current:  {}", refreshed.identity_digest)?;
                }
                None => writeln!(
                    output,
                    "New identity requires review: {}\nNext: xana external-agent show {name}",
                    refreshed.identity_digest
                )?,
            }
        }
        ExternalAgentCommand::Trust { name, yes } => {
            if !yes {
                anyhow::bail!("trust requires --yes after `xana external-agent show {name}`");
            }
            let registry = XanaConfig::load_registry_from(paths.config_file())?;
            let declaration = registry
                .external_agents
                .get(&name)
                .with_context(|| format!("unknown external agent {name:?}"))?;
            let before = manager.show(&name, declaration)?;
            if before
                .cached
                .as_ref()
                .is_some_and(|record| record.configured_endpoint != declaration.endpoint)
            {
                anyhow::bail!("endpoint changed; refresh the Agent Card before trusting");
            }
            let record = manager.trust(&name)?;
            writeln!(
                output,
                "Trusted external agent {name:?} at identity {}.",
                record.identity_digest
            )?;
        }
        ExternalAgentCommand::Untrust { name } => {
            manager.untrust(&name)?;
            writeln!(output, "External agent {name:?} is no longer trusted.")?;
        }
        ExternalAgentCommand::Tasks { name } => {
            let registry = XanaConfig::load_registry_from(paths.config_file())?;
            if !registry.external_agents.contains_key(&name) {
                anyhow::bail!("unknown external agent {name:?}");
            }
            let tasks = manager.tasks_for(&name)?;
            if tasks.is_empty() {
                writeln!(output, "No tracked tasks for external agent {name:?}.")?;
            }
            for task in tasks {
                writeln!(
                    output,
                    "{}\t{}\tcancel={}\tupdated={}",
                    task.task_id, task.state, task.remote_cancel, task.updated_unix_ms
                )?;
            }
        }
        ExternalAgentCommand::Cancel { name, task_id, yes } => {
            if !yes {
                anyhow::bail!("remote cancellation requires --yes");
            }
            let registry = XanaConfig::load_registry_from(paths.config_file())?;
            let declaration = registry
                .external_agents
                .get(&name)
                .with_context(|| format!("unknown external agent {name:?}"))?;
            let task =
                crate::a2a::cancel_tracked_task(&manager, &name, declaration, &task_id).await?;
            writeln!(
                output,
                "Remote cancellation for {name:?} task {task_id}: {} ({})",
                task.remote_cancel, task.state
            )?;
        }
        ExternalAgentCommand::Remove { name, yes } => {
            if !yes {
                anyhow::bail!("removal requires --yes");
            }
            XanaConfig::remove_external_agent(paths.config_file(), &name)?;
            manager.remove_cache(&name)?;
            writeln!(
                output,
                "Removed external agent {name:?} and its cached Card."
            )?;
        }
    }
    Ok(())
}
