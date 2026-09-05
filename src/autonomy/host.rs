//! Explicit detached process lifecycle. The existing authenticated loopback
//! descriptor lease is keyed by protected-home identity, not a frontend window.
#[cfg(windows)]
mod windows;

use super::{
    HostPolicy, JobState,
    runner::{NativeExecutor, TaskExecutor, tick},
};
use crate::{
    local_host::{HostSnapshotSeed, LocalHostServer},
    paths::XanaPaths,
    storage::ProtectedStore,
    workspace_identity::WorkspaceIdentity,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobSummary {
    pub(crate) id: Uuid,
    pub(crate) conversation: Uuid,
    pub(crate) state: JobState,
    pub(crate) next_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopImpact {
    pub(crate) policy_revision: u64,
    pub(crate) requested_at: i64,
    pub(crate) active_job: Option<StopJob>,
    /// None means the owning host has not recorded its client snapshot.
    pub(crate) attached_clients: Option<StopClients>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopJob {
    pub(crate) id: Uuid,
    pub(crate) conversation: Uuid,
    pub(crate) name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopClients {
    pub(crate) host_id: Uuid,
    pub(crate) host_generation: u64,
    pub(crate) count: usize,
    pub(crate) identities: Vec<crate::controller::ControllerClientId>,
}

pub(crate) fn summaries(store: &ProtectedStore) -> Result<Vec<JobSummary>> {
    Ok(store
        .autonomy_page(0)?
        .into_iter()
        .map(|(_, job)| JobSummary {
            id: job.id,
            conversation: job.conversation,
            state: job.state,
            next_at: job.next.at,
        })
        .collect())
}

pub(crate) async fn run(
    paths: &XanaPaths,
    store: ProtectedStore,
    from_startup: bool,
    learner: Option<std::sync::Arc<crate::memory::learning::LearningWorker>>,
) -> Result<()> {
    let policy = store.autonomy_policy()?;
    ensure!(
        policy.detached_enabled && !policy.stop_requested,
        "detached host is disabled or stopped; explicitly start it through owner controls"
    );
    ensure!(
        !from_startup || policy.startup_enabled,
        "OS startup has not been explicitly enabled"
    );
    let executor = NativeExecutor {
        paths: paths.clone(),
        store: store.clone(),
    };
    run_owned(paths, &store, &executor, &super::now, learner.as_deref()).await
}

/// Custody and execution are composed once by the process entry point. Tests
/// use this same host/observer lifecycle with a synthetic home and fake provider.
pub(super) async fn run_owned(
    paths: &XanaPaths,
    store: &ProtectedStore,
    executor: &dyn TaskExecutor,
    clock: &dyn Fn() -> Result<i64>,
    learner: Option<&crate::memory::learning::LearningWorker>,
) -> Result<()> {
    let identity = WorkspaceIdentity::resolve(paths.data_dir())?;
    let server = LocalHostServer::bind(
        paths.runtime_dir(),
        paths.data_dir(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        HostSnapshotSeed {
            workspace_id: identity.collision_key().to_owned(),
            workspace_name: "Protected-home background host".into(),
            conversations: vec![],
            conversations_truncated: false,
            active_conversation: None,
        },
    )
    .await?;
    let server = std::sync::Arc::new(server);
    // Descriptor ownership precedes recovery; a competing launch cannot mark
    // the actual owner's live run as interrupted.
    store.autonomy_recover(clock()?)?;
    let observer_shutdown = server.shutdown_token();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let publisher = server.autonomy_publisher();
    publisher
        .publish(summaries(store)?)
        .map_err(anyhow::Error::msg)?;
    let serving_owner = server.clone();
    let mut serving = tokio::spawn(async move { serving_owner.run_borrowed().await });
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next_learning = tokio::time::Instant::now() + Duration::from_secs(30);
    let scheduler = async {
        loop {
            timer.tick().await;
            let policy = store.autonomy_policy()?;
            if policy.stop_requested || !policy.detached_enabled || shutdown.is_cancelled() {
                if policy.stop_requested {
                    store.autonomy_record_stop_clients(
                        policy.revision,
                        publisher.stop_clients().map_err(anyhow::Error::msg)?,
                    )?;
                }
                break;
            }
            let execution = async {
                let completed = tick(store, executor, clock, &shutdown).await?;
                if completed.is_none() && tokio::time::Instant::now() >= next_learning {
                    next_learning = tokio::time::Instant::now() + Duration::from_secs(30);
                    if let Some(worker) = learner {
                        let _ = worker.process(false, &shutdown).await;
                    }
                }
                Ok::<_, anyhow::Error>(())
            };
            tokio::pin!(execution);
            let mut updates = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    result=&mut execution=>{result?;break;}
                    _=updates.tick()=>{
                        if let Ok(policy)=store.autonomy_policy()
                            && (policy.stop_requested || !policy.detached_enabled) {
                            let recorded=publisher.stop_clients().map_err(anyhow::Error::msg).and_then(|clients|store.autonomy_record_stop_clients(policy.revision,clients));
                            shutdown.cancel();
                            if let Err(error)=recorded {let _=execution.await;return Err(error);}
                        }
                        let published=summaries(store).and_then(|jobs|publisher.publish(jobs).map_err(anyhow::Error::msg));
                        if let Err(error)=published {shutdown.cancel();let _=execution.await;return Err(error);}
                    }
                }
            }
            publisher
                .publish(summaries(store)?)
                .map_err(anyhow::Error::msg)?;
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::pin!(scheduler);
    let mut serving_joined = false;
    let result = tokio::select! {
        result=&mut scheduler=>result,
        result=&mut serving=>{serving_joined=true;shutdown.cancel();let drained=scheduler.await;result.context("background observer host task failed")??;drained},
        result=tokio::signal::ctrl_c()=>{shutdown.cancel();let drained=scheduler.await;result?;drained},
    };
    shutdown.cancel();
    observer_shutdown.cancel();
    if !serving_joined {
        match tokio::time::timeout(Duration::from_secs(8), &mut serving).await {
            Ok(value) => {
                value.context("observer host shutdown failed")??;
            }
            Err(_) => {
                serving.abort();
                let _ = serving.await;
                anyhow::bail!("background observer host did not acknowledge shutdown");
            }
        }
    }
    result?;
    let lock = store.autonomy_policy()?.lock_requested;
    if lock {
        store.lock()?;
    }
    Ok(())
}

/// Spawn is explicit, without a shell or visible Windows console. Descriptor
/// acquisition in the child chooses the sole owner; process id is diagnostic.
pub(crate) fn detach(paths: &XanaPaths) -> Result<String> {
    ensure!(
        XanaPaths::resolve(std::env::var_os("XANA_HOME"))? == *paths,
        "detached launch requires the same explicit XANA_HOME as its owner"
    );
    let store = ProtectedStore::configured(paths.data_dir())?
        .context("detached host requires protected storage")?;
    let policy = store.autonomy_policy()?;
    ensure!(
        policy.detached_enabled && !policy.stop_requested,
        "enable and start detached work explicitly"
    );
    if matches!(
        crate::local_host::inspect_descriptor_health(paths.runtime_dir(), paths.data_dir())
            .map_err(anyhow::Error::msg)?,
        crate::local_host::DescriptorHealth::Active { .. }
    ) {
        return Ok("Existing same-home host retained; use autonomy host observe to attach".into());
    }
    let executable = cli_executable()?;
    launch_detached(&executable)?;
    Ok("Detached launch requested; host observe/status confirms readiness. Closing clients does not stop the host".into())
}

#[cfg(windows)]
fn launch_detached(executable: &std::path::Path) -> Result<()> {
    windows::detach(executable)
}

#[cfg(not(windows))]
fn launch_detached(executable: &std::path::Path) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .args(["autonomy", "host", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; the post-fork closure performs no
        // allocation, lock acquisition, environment lookup or Rust destructor.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    let mut child = command.spawn().context("could not launch detached host")?;
    // Retain the child until it exits on Unix, avoiding zombies while this client
    // remains alive. This monitor owns no work authority or shutdown signal.
    std::thread::Builder::new()
        .name("xana-detached-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

pub(super) fn cli_executable() -> Result<std::path::PathBuf> {
    let executable = std::env::current_exe()?;
    Ok(
        if executable.file_stem().and_then(|name| name.to_str()) == Some("xana-desktop") {
            let sibling =
                executable.with_file_name(if cfg!(windows) { "xana.exe" } else { "xana" });
            ensure!(
                sibling.is_file(),
                "detached host needs the xana CLI executable beside xana-desktop"
            );
            sibling
        } else {
            executable
        },
    )
}

pub(crate) fn policy_edit(
    store: &ProtectedStore,
    revision: u64,
    enabled: Option<bool>,
    startup: Option<bool>,
    stop: bool,
    lock: bool,
) -> Result<HostPolicy> {
    store.autonomy_edit_policy(revision, |policy| {
        if let Some(enabled) = enabled {
            policy.detached_enabled = enabled;
            policy.stop_requested = !enabled;
            policy.lock_requested = false;
            if !enabled {
                policy.startup_enabled = false;
            }
        }
        if let Some(startup) = startup {
            policy.startup_enabled = startup;
        }
        if stop || lock {
            policy.stop_requested = true;
            policy.lock_requested = lock;
        }
    })
}
