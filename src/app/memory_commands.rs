//! One owner command adapter over the governed protected-memory API.
use crate::{
    cli::{MemoryArgs, MemoryCommand},
    memory::{MemoryContext, MemoryControlEdit, MemoryEdit, MemoryOwner},
    paths::XanaPaths,
    storage::ProtectedStore,
};
use anyhow::{Context, Result};
use std::io::Write;

pub(super) fn compose(
    paths: &XanaPaths,
    artifacts: &crate::artifact::ArtifactStore,
    conversation: &str,
    profile: uuid::Uuid,
) -> Result<Option<MemoryOwner>> {
    let Some(store) = artifacts.protected_home() else {
        return Ok(None);
    };
    let project = crate::project::ProjectStore::open(paths)?
        .membership(conversation)?
        .map(|id| id.to_string().parse())
        .transpose()?;
    let mut owner = MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(conversation.parse()?),
            profile: Some(profile),
            project,
        },
    );
    owner.learner = learning_worker(paths, &owner.store)?;
    Ok(Some(owner))
}

pub(super) fn learning_worker(
    paths: &XanaPaths,
    store: &ProtectedStore,
) -> Result<Option<std::sync::Arc<crate::memory::learning::LearningWorker>>> {
    let Some(route) = store.learning_status()?.route else {
        return Ok(None);
    };
    let registry = crate::config::XanaConfig::load_registry_from(paths.config_file())?;
    let Some(connection) = registry.connections.get(&route.connection) else {
        return Ok(None);
    };
    if connection.kind == crate::config::ProviderKind::Codex
        || route.digest
            != crate::session::compaction::semantic::route_digest(connection, &route.model)
    {
        return Ok(None);
    }
    let Ok((provider, _)) = crate::orchestration::compose_native_provider(
        connection,
        &route.model,
        crate::artifact::ArtifactStore::protected(store.clone()),
        true,
    ) else {
        store.set_document("memory/learning-receipt",br#"{"state":"pending_route","notice":"The configured learning helper is unavailable; offline memory controls and chat remain usable."}"#,4096)?;
        return Ok(None);
    };
    Ok(Some(std::sync::Arc::new(
        crate::memory::learning::LearningWorker {
            store: store.clone(),
            route,
            provider: provider.into(),
            validate_route: live_learning_route(paths.config_file().to_path_buf()),
        },
    )))
}

fn live_learning_route(
    path: std::path::PathBuf,
) -> std::sync::Arc<crate::memory::learning::LearningRouteValidator> {
    std::sync::Arc::new(move |route| {
        let registry = crate::config::XanaConfig::load_registry_from(&path)?;
        let connection = registry
            .connections
            .get(&route.connection)
            .context("learning helper connection is no longer configured")?;
        anyhow::ensure!(
            connection.kind != crate::config::ProviderKind::Codex
                && route.digest
                    == crate::session::compaction::semantic::route_digest(connection, &route.model),
            "learning helper endpoint, credentials or configuration changed; authorize its route again"
        );
        Ok(())
    })
}

pub(super) async fn run<W: Write>(
    args: MemoryArgs,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let store=ProtectedStore::configured(paths.data_dir())?.context("Personal memory requires protected storage; inspect xana storage status. No plaintext memory was created")?;
    let owner = MemoryOwner::new(store, MemoryContext::default());
    let value = match args.command {
        MemoryCommand::ReviewRestore { review } => {
            owner.store.review_restored_memory(review.as_deref())?
        }
        MemoryCommand::LearningStatus => serde_json::to_value(owner.store.learning_status()?)?,
        MemoryCommand::LearningRoute {
            connection,
            model,
            confirm,
            disable,
        } => {
            anyhow::ensure!(
                confirm,
                "--confirm authorizes learning input disclosure to this exact native helper route; it does not enable managed helpers"
            );
            if disable {
                owner.store.remove_document("memory/learning-route")?;
                serde_json::json!({"state":"disabled","notice":"Queued sources remain private and pending; scope controls independently disable learning."})
            } else {
                let registry = crate::config::XanaConfig::load_registry_from(paths.config_file())?;
                let config = registry
                    .connections
                    .get(&connection)
                    .context("unknown learning helper connection")?;
                anyhow::ensure!(
                    config.kind != crate::config::ProviderKind::Codex,
                    "managed subscription-only learning is unsupported; select a native helper"
                );
                anyhow::ensure!(
                    !model.trim().is_empty()
                        && model.len() <= 256
                        && !model.chars().any(char::is_control),
                    "invalid helper model"
                );
                let route = crate::memory::learning::LearningRoute {
                    connection,
                    model: model.clone(),
                    digest: crate::session::compaction::semantic::route_digest(config, &model),
                };
                owner.store.set_document(
                    "memory/learning-route",
                    &serde_json::to_vec(&route)?,
                    4096,
                )?;
                serde_json::json!({"route":route,"disclosure":crate::memory::learning::DISCLOSURE,"next":"New runtime launches process batches while idle; memory process processes one batch now."})
            }
        }
        MemoryCommand::Process => {
            owner.store.retire_stale_learning()?;
            let worker=learning_worker(paths,&owner.store)?.context("learning route absent or changed; inspect memory learning-status and explicitly authorize an available native route")?;
            let cancellation = tokio_util::sync::CancellationToken::new();
            let processing = worker.process(true, &cancellation);
            tokio::pin!(processing);
            let count = tokio::select! {
                result=&mut processing=>result?,
                _=tokio::signal::ctrl_c()=> {cancellation.cancel();processing.await?},
            };
            serde_json::json!({"created":count,"status":owner.store.learning_status()?})
        }
        MemoryCommand::List { scope, after } => {
            serde_json::to_value(owner.page(scope.as_ref(), after)?)?
        }
        MemoryCommand::Show { id } => serde_json::to_value(owner.record(id)?)?,
        MemoryCommand::Forget { id, revision } => {
            serde_json::to_value(owner.revise(id, revision, MemoryEdit::Forget)?)?
        }
        MemoryCommand::Restore {
            id,
            revision,
            confirm,
        } => serde_json::to_value(owner.revise(id, revision, MemoryEdit::Restore { confirm })?)?,
        MemoryCommand::DeleteSource {
            conversation,
            review,
        } => match review {
            None => serde_json::to_value(owner.deletion_preview(conversation)?)?,
            Some(review) => serde_json::to_value(owner.delete_source(conversation, &review)?)?,
        },
        MemoryCommand::Remember {
            scope,
            text,
            expires_at,
        } => serde_json::to_value(owner.remember(scope, text, expires_at)?)?,
        MemoryCommand::Correct {
            id,
            revision,
            text,
            expires_at,
            clear_expiry,
        } => {
            let until = if clear_expiry {
                None
            } else {
                expires_at.or(owner.record(id)?.valid_until_unix_seconds)
            };
            serde_json::to_value(owner.revise(
                id,
                revision,
                MemoryEdit::Correct {
                    statement: text,
                    valid_until_unix_seconds: until,
                },
            )?)?
        }
        MemoryCommand::Scope {
            id,
            revision,
            to,
            confirm,
        } => serde_json::to_value(owner.revise(
            id,
            revision,
            MemoryEdit::Scope {
                target: to,
                confirm,
            },
        )?)?,
        MemoryCommand::Disable { id, revision } => {
            serde_json::to_value(owner.revise(id, revision, MemoryEdit::Disable)?)?
        }
        MemoryCommand::Controls {
            scope,
            revision,
            use_memory,
            learn,
            no_memory,
        } => serde_json::to_value(owner.controls(
            scope,
            MemoryControlEdit {
                expected_revision: revision,
                use_enabled: use_memory.map(Into::into),
                learning_enabled: learn.map(Into::into),
                no_memory: no_memory.map(Into::into),
            },
        )?)?,
        MemoryCommand::Export {
            scope,
            output: destination,
        } => {
            serde_json::json!({"exported_records":owner.export(scope.as_ref(),&destination)?,"output":destination,"notice":"Readable private export outside managed encryption; store or remove it deliberately."})
        }
        MemoryCommand::Say {
            conversation,
            profile,
            project,
            request,
        } => {
            let owner = MemoryOwner::new(
                owner.store,
                MemoryContext {
                    conversation,
                    profile,
                    project,
                },
            );
            writeln!(
                output,
                "{}",
                owner
                    .respond(&request)
                    .context("Unknown local memory control; use memory --help")??
            )?;
            return Ok(());
        }
    };
    serde_json::to_writer_pretty(&mut *output, &value)?;
    writeln!(output)?;
    Ok(())
}

#[cfg(test)]
mod learning_route_tests {
    use super::*;

    #[test]
    fn live_guard_rechecks_registry_endpoint_and_connection_existence() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let input = "version=1\ndefault_profile='default'\npermission_mode='ask'\n[providers.local]\nkind='openai_compat'\nbase_url='http://localhost:11434/v1'\n[profiles.default]\nprovider='local'\nmodel='synthetic'\n";
        std::fs::write(&path, input).unwrap();
        let registry = crate::config::XanaConfig::load_registry_from(&path).unwrap();
        let route = crate::memory::learning::LearningRoute {
            connection: "local".into(),
            model: "synthetic".into(),
            digest: crate::session::compaction::semantic::route_digest(
                &registry.connections["local"],
                "synthetic",
            ),
        };
        let validate = live_learning_route(path.clone());
        validate(&route).unwrap();
        std::fs::write(&path, input.replace("11434", "11435")).unwrap();
        assert!(validate(&route).is_err());
        std::fs::write(&path, input.replace("local", "other")).unwrap();
        assert!(validate(&route).is_err());
        std::fs::write(&path, input).unwrap();
        validate(&route).unwrap();
    }
}
