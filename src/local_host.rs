//! Authenticated loopback projection of Xana's repository-private frontend contract.

mod artifact_access;
mod descriptor;
mod execution;
mod hub;
mod protocol;
mod transport;

use crate::{
    frontend::{ClientSnapshotSeed, EmbeddedClient},
    native_runtime::RuntimeHandle,
    plain_terminal::ChatHeader,
    workspace_host::{ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use std::{io::Write as _, net::IpAddr, path::Path, sync::Arc};

#[cfg(test)]
pub(crate) use descriptor::write_stale_for_test;
pub(crate) use descriptor::{
    DescriptorHealth, inspect_health as inspect_descriptor_health,
    remove_stale as remove_stale_descriptor,
};
pub(crate) use protocol::HostSnapshotSeed;
pub(crate) use transport::{
    ControlledExecution, LocalHostError, LocalHostServer, connect_observer, reconnect_controller,
};

pub(crate) async fn run_native_host(
    runtime_root: &Path,
    bind: IpAddr,
    port: u16,
    runtime: RuntimeHandle,
    header: ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
) -> Result<()> {
    let artifacts = header.artifact_store.clone();
    let workspace_host = Arc::new(workspace_host);
    let seed = HostSnapshotSeed::from_workspace(&workspace_host.snapshot()?);
    let frontend = EmbeddedClient::from_runtime(
        runtime,
        ClientSnapshotSeed {
            session_id: header.session_id,
            connection: header.provider_name,
            execution_owner: "native".into(),
            model: header.model,
            reasoning_effort: None,
            children: header.children,
        },
    );
    let snapshot = frontend.snapshot().clone();
    let controlled_host = Arc::clone(&workspace_host);
    let controlled_conversation = conversation.clone();
    let server = LocalHostServer::bind_controlled(
        runtime_root,
        workspace_host.workspace(),
        bind,
        port,
        seed,
        ControlledExecution::new(
            conversation.to_string(),
            Some(snapshot),
            Some(artifacts),
            move |hub| {
                execution::spawn_native_execution(
                    frontend,
                    controlled_host,
                    controlled_conversation,
                    hub,
                )
            },
        ),
    )
    .await?;
    run_server(server).await
}

pub(crate) async fn run_managed_host(
    runtime_root: &Path,
    bind: IpAddr,
    port: u16,
    execution: ManagedHostExecution,
) -> Result<()> {
    let ManagedHostExecution {
        server,
        models,
        config,
        workspace_host,
        conversation,
    } = execution;
    let workspace_host = Arc::new(workspace_host);
    let seed = HostSnapshotSeed::from_workspace(&workspace_host.snapshot()?);
    let artifacts = config.artifact_store.clone();
    let driver = crate::managed_execution::ManagedTuiDriver::start(
        server,
        models,
        config,
        Arc::clone(&workspace_host),
        conversation.clone(),
    )
    .await?;
    let server = LocalHostServer::bind_controlled(
        runtime_root,
        workspace_host.workspace(),
        bind,
        port,
        seed,
        ControlledExecution::new(
            conversation.to_string(),
            None,
            Some(artifacts),
            move |hub| execution::spawn_managed_execution(driver, hub),
        ),
    )
    .await?;
    run_server(server).await
}

pub(crate) struct ManagedHostExecution {
    pub(crate) server: crate::managed::codex::CodexAppServer,
    pub(crate) models: crate::model_catalog::ModelManager,
    pub(crate) config: crate::managed_execution::ManagedChatConfig,
    pub(crate) workspace_host: WorkspaceHost,
    pub(crate) conversation: ConversationRef,
}

async fn run_server(server: LocalHostServer) -> Result<()> {
    let shutdown = server.shutdown_token();
    writeln!(
        anstream::stderr().lock(),
        "xana serve: foreground loopback host ready\n  endpoint: {}\n  workspace: {}\n  descriptor: {}\n  clients: 0\nattach from this workspace with: xana attach",
        server.endpoint(),
        serde_json::to_string(
            std::env::current_dir()
                .ok()
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("workspace")
        )?,
        server.descriptor_path().display(),
    )?;
    let signal = shutdown.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    let result = server.run().await;
    signal_task.abort();
    writeln!(anstream::stderr().lock(), "xana serve: stopped")?;
    result
        .map_err(anyhow::Error::new)
        .context("local host failed")
}

#[cfg(test)]
mod tests;
