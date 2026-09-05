//! Thin owner CLI adapter; indexing and citation policy stay in the shared core.
use crate::{
    cli::{NotesCommand, RecallArgs, RecallCommand},
    paths::XanaPaths,
    recall::RecallOwner,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;

pub(super) async fn run(args: RecallArgs, paths: &XanaPaths) -> Result<()> {
    let owner = RecallOwner::open(paths, args.conversation)?;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let worker_cancel = cancellation.clone();
    let paths = paths.clone();
    let mut worker = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
        Ok(match args.command {
            RecallCommand::Search { query } => {
                json!({"evidence":owner.search(&query,None)?,"notice":"No match means no fresh eligible indexed evidence; refresh history or selected notes explicitly."})
            }
            RecallCommand::Refresh => json!(owner.refresh_history(&worker_cancel)?),
            RecallCommand::Rebuild => {
                owner.rebuild()?;
                json!({"reset":"derived index only","originals":"unchanged","next":"refresh history and each selected notes root"})
            }
            RecallCommand::Include { source, remove } => {
                owner.include(source, !remove)?;
                json!({"included":owner.inclusions()?})
            }
            RecallCommand::Notes { command } => match command {
                NotesCommand::Select { path } => json!(owner.select_root(&path)?),
                NotesCommand::List => json!(owner.roots()?),
                NotesCommand::Revoke { root } => {
                    owner.revoke_root(root)?;
                    json!({"revoked":root,"original_files":"unchanged"})
                }
                NotesCommand::Refresh { root, restart } => json!(if restart {
                    owner.refresh_root_with_restart(root, &worker_cancel, true)?
                } else {
                    owner.refresh_root(root, &worker_cancel)?
                }),
                NotesCommand::Create => json!({"ordinary_notes_directory":owner.create_notes()?}),
                NotesCommand::Export { root, destination } => {
                    json!({"exported":owner.export_notes(root,&destination,&worker_cancel)?,"destination":destination})
                }
                NotesCommand::Disclose {
                    root,
                    connection,
                    model,
                    confirm,
                    remove,
                } => {
                    ensure!(
                        confirm,
                        "--confirm explicitly authorizes this notes root's disclosure to the selected native connection/model"
                    );
                    let registry =
                        crate::config::XanaConfig::load_registry_from(paths.config_file())?;
                    let connection = registry
                        .connections
                        .get(&connection)
                        .context("unknown notes disclosure connection")?;
                    ensure!(
                        connection.kind != crate::config::ProviderKind::Codex,
                        "managed Codex does not execute Xana's native recall tool; use explicit local recall inspection"
                    );
                    ensure!(
                        !model.is_empty()
                            && model.len() <= 256
                            && !model.chars().any(char::is_control),
                        "invalid disclosure model"
                    );
                    json!(owner.disclose_root(
                        root,
                        crate::session::compaction::semantic::route_digest(connection, &model),
                        !remove
                    )?)
                }
            },
        })
    });
    let result = tokio::select! {
        result=&mut worker=>result.context("recall worker stopped")??,
        _=tokio::signal::ctrl_c()=>{cancellation.cancel();worker.await.context("recall worker stopped during cancellation")??},
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
