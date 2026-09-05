//! Explicit storage control at the application edge; never ambient home migration.

use crate::{
    cli::StorageCommand,
    paths::XanaPaths,
    storage::{OsCustody, ProtectedStore, RecoveryIdentity, StorageStatus},
};
use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, ensure};
use std::{io::Write, path::Path};

pub(super) fn run(
    command: &StorageCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let data = paths.data_dir();
    match command {
        StorageCommand::Status => match ProtectedStore::status(data)? {
            StorageStatus::Legacy => writeln!(
                output,
                "Storage: legacy (not application-encrypted). No migration has been performed."
            )?,
            StorageStatus::Protected { id, locked } => writeln!(
                output,
                "Storage: protected {id}; {}",
                if locked {
                    "locked"
                } else {
                    "OS unlock available subject to custody"
                }
            )?,
        },
        StorageCommand::RecoveryKey {
            output: destination,
        } => {
            // Deliberate user-owned plaintext export, separate from the managed home.
            let parent = destination
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let parent = parent
                .canonicalize()
                .context("recovery destination directory is unavailable")?;
            let data_canonical = data.canonicalize().unwrap_or_else(|_| data.to_path_buf());
            ensure!(
                !parent.starts_with(&data_canonical),
                "keep the independent recovery key outside Xana's managed data directory"
            );
            let mut file = crate::storage::create_private_file(destination)
                .context("could not create recovery key; an existing file is never overwritten")?;
            let identity = RecoveryIdentity::generate();
            writeln!(file, "{}", identity.to_string().expose_secret())?;
            file.sync_all()?;
            writeln!(
                output,
                "Recovery key created at {}. Keep it private and back it up separately; anyone with this key and your encrypted home can recover the content.",
                destination.display()
            )?;
        }
        StorageCommand::Initialize {
            recovery_key,
            manual_unlock,
        } => {
            let identity = read_recovery_key(recovery_key)?;
            let custody: &dyn crate::storage::KeyCustody = if *manual_unlock {
                &crate::storage::RecoveryOnlyCustody
            } else {
                &OsCustody
            };
            let store = ProtectedStore::initialize(data, &identity, custody)?;
            writeln!(
                output,
                "Protected store {} initialized. {} Independent recovery verified; existing homes were not migrated.",
                store.id(),
                if *manual_unlock {
                    "Portable manual unlock selected; set XANA_STORAGE_RECOVERY_KEY to the independent key file for each launch."
                } else {
                    "OS custody verified."
                }
            )?;
        }
        StorageCommand::Verify => {
            ProtectedStore::configured(data)?
                .context("protected storage is not initialized")?
                .verify()?;
            writeln!(output, "Protected database integrity verified.")?;
        }
        StorageCommand::Lock => {
            if !matches!(
                ProtectedStore::status(data)?,
                StorageStatus::Protected { locked: true, .. }
            ) {
                ProtectedStore::configured(data)?
                    .context("protected storage is not initialized")?
                    .lock()?;
            }
            writeln!(output, "Protected storage locked.")?;
        }
        StorageCommand::Unlock {
            recovery_key,
            remember,
        } => {
            let store = if let Some(path) = recovery_key {
                ProtectedStore::recover(data, &read_recovery_key(path)?)?
            } else {
                ProtectedStore::unlock(data, &OsCustody)?
            };
            if *remember {
                store.bind_custody(&OsCustody)?;
            }
            writeln!(
                output,
                "Protected store {} unlocked and verified.{}",
                store.id(),
                if recovery_key.is_some() && !remember {
                    " Recovery key was not retained in OS custody; normal future opens still require custody or recovery."
                } else {
                    ""
                }
            )?;
        }
    }
    Ok(())
}

pub(super) fn read_recovery_key(path: &Path) -> Result<RecoveryIdentity> {
    crate::storage::read_recovery_identity(path)
}
