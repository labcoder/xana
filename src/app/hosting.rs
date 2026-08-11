//! Explicit loopback host and attached-client application commands.

use super::{banner_mode, chat};
use crate::{
    cli,
    local_host::{LocalHostError, connect_observer, reconnect_controller},
    paths::XanaPaths,
};
use anyhow::{Context, Result};

pub(super) async fn run_serve(args: &cli::ServeArgs, paths: &XanaPaths) -> Result<()> {
    if !args.bind.is_loopback() {
        anyhow::bail!("Course 1 `xana serve` accepts only loopback bind addresses");
    }
    let presentation = banner_mode(paths, false, false, false, true).profile();
    chat::run(
        paths,
        chat::ChatSurface::Hosted {
            bind: args.bind,
            port: args.port,
            presentation,
        },
        None,
        false,
        false,
        None,
    )
    .await
    .map(|_| ())
}

pub(super) async fn run_attach(args: &cli::AttachArgs, paths: &XanaPaths) -> Result<()> {
    let workspace = std::env::current_dir()
        .context("could not resolve Xana workspace root")?
        .canonicalize()
        .context("could not canonicalize Xana workspace root")?;
    let mut reconnect = None;
    loop {
        let mut observer = match reconnect.take() {
            Some(capability) => {
                reconnect_controller(paths.runtime_dir(), &workspace, capability).await?
            }
            None => connect_observer(paths.runtime_dir(), &workspace).await?,
        };
        if args.control && !observer.is_controller() {
            let conversation = observer
                .snapshot()
                .controllable_conversation
                .clone()
                .context("foreground host has no controllable conversation")?;
            observer
                .acquire_control(conversation, args.takeover)
                .await?;
            eprintln!("xana attach: controller authority acquired");
            if let Some(prompt) = &args.prompt {
                let result = observer
                    .send_command(crate::native_runtime::RuntimeCommand::SubmitTurn {
                        operation_id: crate::identity::OperationId::new(),
                        input: prompt.clone(),
                    })
                    .await?;
                if !result.accepted {
                    anyhow::bail!(
                        "hosted prompt was rejected: {}",
                        result.reason.as_deref().unwrap_or("no reason provided")
                    );
                }
            }
        }
        println!("{}", serde_json::to_string(observer.snapshot())?);
        if let Some(artifact_id) = args.artifact {
            let result = observer.get_artifact(artifact_id).await?;
            println!("{}", serde_json::to_string(&result)?);
            return if result.accepted {
                Ok(())
            } else {
                anyhow::bail!(
                    "artifact retrieval was rejected: {}",
                    result.reason.as_deref().unwrap_or("no reason provided")
                )
            };
        }
        loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => {
                    signal.context("could not listen for attach cancellation")?;
                    if args.control {
                        observer.release_control().await?;
                    }
                    return Ok(());
                }
                observation = observer.next() => {
                    match observation {
                        Ok(observation) => println!("{}", serde_json::to_string(&observation)?),
                        Err(LocalHostError::SequenceGap { .. }) => {
                            eprintln!("xana attach: observation gap; reconnecting for a fresh snapshot");
                            reconnect = observer.take_controller_reconnect();
                            break;
                        }
                        Err(LocalHostError::Closed) => {
                            reconnect = observer.take_controller_reconnect();
                            if reconnect.is_some() {
                                eprintln!("xana attach: controller disconnected; reconnecting within grace");
                                break;
                            }
                            eprintln!("xana attach: foreground host closed");
                            return Ok(());
                        }
                        Err(error) => return Err(anyhow::Error::new(error)),
                    }
                }
            }
        }
    }
}
