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
        StorageCommand::Archive {
            after,
            export,
            output: destination,
        } => {
            let store = ProtectedStore::configured(data)?.context("protected storage required")?;
            if let Some(id) = export {
                crate::storage::migration::archive::export(
                    &store,
                    id,
                    destination
                        .as_deref()
                        .context("explicit export destination required")?,
                )?;
                writeln!(
                    output,
                    "Archived file verified and exported. This is a deliberate plaintext copy."
                )?;
            } else {
                let entries = crate::storage::migration::archive::list(&store, after.as_deref())?;
                serde_json::to_writer_pretty(&mut *output, &entries)?;
                writeln!(
                    output,
                    "\nUp to 128 entries. Pass --after LAST_ID for the next page."
                )?;
            }
        }
        StorageCommand::Restore {
            snapshot,
            recovery_key,
            apply,
            review,
            resume,
        } => {
            let identity = read_recovery_key(recovery_key)?;
            if *resume {
                let retired = crate::storage::restore::resume(paths, &identity)?;
                writeln!(
                    output,
                    "Restore recovered. Prior encrypted generation retained at {}. Recall, learning and automation remain inactive until reviewed.",
                    retired.display()
                )?;
            } else {
                let snapshot = snapshot
                    .as_deref()
                    .context("select --snapshot or --resume")?;
                if *apply {
                    let retired = crate::storage::restore::apply(
                        paths,
                        snapshot,
                        &identity,
                        review
                            .as_deref()
                            .context("preview first and pass --review DIGEST")?,
                    )?;
                    writeln!(
                        output,
                        "Verified restore activated. Prior encrypted generation retained at {}. Recall, learning and automation remain inactive until reviewed; no effects replayed.",
                        retired.display()
                    )?;
                } else {
                    serde_json::to_writer_pretty(
                        &mut *output,
                        &crate::storage::restore::preview(paths, snapshot, &identity)?,
                    )?;
                    writeln!(
                        output,
                        "\nNo changes made. Ordinary source/configuration files are not restored or overwritten."
                    )?;
                }
            }
        }
        StorageCommand::Migrate {
            recovery_key,
            apply,
            review,
            resume,
            manual_unlock,
        } => {
            if *resume {
                let retained = if let Some(path) = recovery_key {
                    crate::storage::migration::resume(paths, &read_recovery_key(path)?)?
                } else {
                    crate::storage::migration::resume_managed(paths, &OsCustody)?
                };
                writeln!(
                    output,
                    "Protected migration recovered. Plaintext legacy generation retained at {}. No secure erasure or automatic effect replay.",
                    retained.display()
                )?;
            } else if *apply {
                let review = review
                    .as_deref()
                    .context("preview migration first and pass --review DIGEST")?;
                let retained = if let Some(path) = recovery_key {
                    ensure_independent_key(data, path)?;
                    let key = read_recovery_key(path)?;
                    let custody: &dyn crate::storage::KeyCustody = if *manual_unlock {
                        &crate::storage::RecoveryOnlyCustody
                    } else {
                        &OsCustody
                    };
                    crate::storage::migration::apply(paths, &key, custody, review)?
                } else {
                    ensure!(!manual_unlock, "manual unlock requires a recovery key file");
                    crate::storage::migration::apply_managed(paths, &OsCustody, review)?
                };
                writeln!(
                    output,
                    "Protected migration verified and activated. Plaintext legacy generation retained at {}. Remove it only after independently checking recovery; secure SSD erasure is not promised.",
                    retained.display()
                )?;
                write_recovery_status(data, output)?;
            } else {
                let plan = crate::storage::migration::preview(paths)?;
                serde_json::to_writer_pretty(&mut *output, &plan)?;
                writeln!(
                    output,
                    "\nNo changes made. Preserve the legacy copy; review unknown archived files before applying. Without --recovery-key, Xana manages keys automatically in OS custody. Save a recovery backup afterward: without it, losing OS custody can make your data unrecoverable. For guided review use `xana setup --section storage`."
                )?;
            }
        }
        StorageCommand::Backup { if_due } => {
            let store = ProtectedStore::configured(data)?.context("protected storage required")?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let report = store.backup(&store.backup_policy()?, now, *if_due)?;
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output)?;
            ensure!(
                *if_due || report.snapshot.is_some(),
                "no fresh backup was made; existing recovery coverage was retained"
            );
        }
        StorageCommand::BackupPolicy {
            directory,
            retention_days,
            max_snapshots,
            max_bytes,
            interval_hours,
            enabled,
        } => {
            let store = ProtectedStore::configured(data)?.context("protected storage required")?;
            let mut policy = store.backup_policy()?;
            let changed = directory.is_some()
                || retention_days.is_some()
                || max_snapshots.is_some()
                || max_bytes.is_some()
                || interval_hours.is_some()
                || enabled.is_some();
            if let Some(value) = directory {
                policy.directory = Some(value.clone());
            }
            if let Some(value) = retention_days {
                policy.retention_days = *value;
            }
            if let Some(value) = max_snapshots {
                policy.max_snapshots = *value;
            }
            if let Some(value) = max_bytes {
                policy.max_bytes = *value;
            }
            if let Some(value) = interval_hours {
                policy.interval_hours = *value;
            }
            if let Some(value) = enabled {
                policy.enabled = *value;
            }
            if changed {
                store.set_backup_policy(&policy)?;
            }
            serde_json::to_writer_pretty(&mut *output, &policy)?;
            writeln!(output)?;
        }
        StorageCommand::Status => match ProtectedStore::status(data)? {
            StorageStatus::Legacy => writeln!(
                output,
                "Storage: legacy (not application-encrypted). No migration has been performed."
            )?,
            StorageStatus::Protected { id, locked } => {
                writeln!(
                    output,
                    "Storage: protected {id}; {}",
                    if locked {
                        "locked"
                    } else {
                        "OS unlock available subject to custody"
                    }
                )?;
                write_recovery_status(data, output)?;
            }
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
        StorageCommand::RecoveryExport {
            output: destination,
        } => {
            ProtectedStore::configured(data)?
                .context("protected storage required")?
                .export_recovery(destination)?;
            writeln!(
                output,
                "Recovery backup saved at {}. Keep it private and copy it to a separate safe location; anyone with it and your encrypted data can recover the content.",
                destination.display()
            )?;
        }
        StorageCommand::Initialize {
            recovery_key,
            manual_unlock,
        } => {
            ensure!(
                !data.exists() || std::fs::read_dir(data)?.next().is_none(),
                "existing Xana data requires reviewed storage migration; no changes made"
            );
            let _config_lock = crate::config::ConfigTransactionLock::acquire(paths.config_file())?;
            crate::storage::migration::fence_config(paths)?;
            let store = if let Some(recovery_key) = recovery_key {
                ensure_independent_key(data, recovery_key)?;
                let identity = read_recovery_key(recovery_key)?;
                let custody: &dyn crate::storage::KeyCustody = if *manual_unlock {
                    &crate::storage::RecoveryOnlyCustody
                } else {
                    &OsCustody
                };
                ProtectedStore::initialize(data, &identity, custody)?
            } else {
                ensure!(!manual_unlock, "manual unlock requires a recovery key file");
                ProtectedStore::initialize_managed(data, &OsCustody)?
            };
            writeln!(
                output,
                "Protected store {} initialized. {} Recovery envelope verified; existing homes were not migrated.",
                store.id(),
                if *manual_unlock {
                    "Portable manual unlock selected; set XANA_STORAGE_RECOVERY_KEY to the independent key file for each launch."
                } else {
                    "OS custody verified."
                }
            )?;
            write_recovery_status(data, output)?;
        }
        StorageCommand::Verify => {
            ProtectedStore::configured(data)?
                .context("protected storage is not initialized")?
                .verify_content()?;
            writeln!(
                output,
                "Protected database, references and artifact integrity verified."
            )?;
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

fn write_recovery_status(data: &Path, output: &mut dyn Write) -> Result<()> {
    writeln!(
        output,
        "Key management: {}. Explicit manual-unlock homes still require their recovery file.",
        crate::storage::recovery::custody_label()
    )?;
    writeln!(
        output,
        "Recovery backup: {}",
        crate::storage::recovery::status(data)?.description()
    )?;
    writeln!(
        output,
        "Manage protection and backup: xana setup --section storage"
    )?;
    Ok(())
}

pub(super) fn read_recovery_key(path: &Path) -> Result<RecoveryIdentity> {
    crate::storage::read_recovery_identity(path)
}

fn ensure_independent_key(data: &Path, key: &Path) -> Result<()> {
    let key = key.canonicalize().context("recovery key is unavailable")?;
    ensure!(
        !key.ancestors()
            .any(|ancestor| same_file::is_same_file(ancestor, data).unwrap_or(false)),
        "keep the recovery key outside the managed data being protected; otherwise it is not independent recovery"
    );
    Ok(())
}
