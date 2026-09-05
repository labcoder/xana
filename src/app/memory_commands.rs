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
    Ok(Some(MemoryOwner::new(
        store.clone(),
        MemoryContext {
            conversation: Some(conversation.parse()?),
            profile: Some(profile),
            project,
        },
    )))
}

pub(super) fn run<W: Write>(args: MemoryArgs, paths: &XanaPaths, output: &mut W) -> Result<()> {
    let store=ProtectedStore::configured(paths.data_dir())?.context("Personal memory requires protected storage; inspect xana storage status. No plaintext memory was created")?;
    let owner = MemoryOwner::new(store, MemoryContext::default());
    let value = match args.command {
        MemoryCommand::List { scope, after } => {
            serde_json::to_value(owner.page(scope.as_ref(), after)?)?
        }
        MemoryCommand::Show { id } => serde_json::to_value(owner.record(id)?)?,
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
