//! Managed recovery onboarding and deliberate private exports, never plaintext fallback.
//!
//! The identity is encrypted in the existing database so OS custody unlocks it.
//! This deliberately cannot recover machine loss until an independent export exists.

use super::{ProtectedStore, RecoveryIdentity, Secrets};
use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, ensure};
use std::{fs, io::Write, path::Path};
use zeroize::Zeroizing;

pub(super) const STATUS_FILE: &str = "recovery-status";
const IDENTITY_DOCUMENT: &str = "storage/managed-recovery-identity";

pub(crate) fn custody_label() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager (automatic)"
    } else if cfg!(target_os = "macos") {
        "macOS Keychain (automatic)"
    } else {
        "Linux Secret Service (automatic when available)"
    }
}

pub(crate) fn summary(data: &Path) -> String {
    match ProtectedStore::status(data) {
        Ok(super::StorageStatus::Legacy) => {
            "not enabled; personal memory unavailable; use xana setup --section storage".into()
        }
        Ok(super::StorageStatus::Protected { locked, .. }) => {
            let backup = status(data).map_or(
                "unavailable; inspect recovery status",
                RecoveryStatus::description,
            );
            format!(
                "{}; recovery backup: {backup}",
                if locked { "locked" } else { "enabled" }
            )
        }
        Err(_) => {
            "transition pending or invalid; inspect xana storage status before continuing".into()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryStatus {
    UserManaged,
    Pending,
    Exported,
}

impl RecoveryStatus {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::UserManaged => "user-supplied recovery key (keep your separate copy safe)",
            Self::Pending => "not exported; losing the OS key can make your data unrecoverable",
            Self::Exported => {
                "export recorded; keep the file backed up separately (off-device storage is not verified)"
            }
        }
    }
}

/// Reads only non-secret metadata, including while explicitly locked.
pub(crate) fn status(data: &Path) -> Result<RecoveryStatus> {
    let path = data.join("protected").join(STATUS_FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecoveryStatus::UserManaged);
        }
        Err(error) => return Err(error).context("could not inspect recovery status"),
        Ok(metadata) => ensure!(
            metadata.file_type().is_file(),
            "invalid recovery status file"
        ),
    }
    match crate::bounded_file::read(&path, 32)?.as_slice() {
        b"pending\n" => Ok(RecoveryStatus::Pending),
        b"exported\n" => Ok(RecoveryStatus::Exported),
        _ => anyhow::bail!("invalid recovery status; no backup assurance is available"),
    }
}

pub(super) fn retain_identity(
    connection: &rusqlite::Connection,
    identity: &RecoveryIdentity,
) -> Result<()> {
    let secret = identity.to_string();
    connection.execute(
        "INSERT INTO documents(name,revision,body) VALUES(?1,1,?2)",
        rusqlite::params![IDENTITY_DOCUMENT, secret.expose_secret().as_bytes()],
    )?;
    Ok(())
}

/// Planning is read-only. Recheck at commit, then rely on create-new for races.
pub(crate) fn validate_destination(data: &Path, destination: &Path) -> Result<()> {
    ensure!(
        destination.is_absolute(),
        "recovery export requires an absolute destination"
    );
    let parent = destination
        .parent()
        .context("recovery destination needs a parent")?
        .canonicalize()
        .context("recovery destination directory is unavailable")?;
    let name = destination
        .file_name()
        .context("recovery destination needs a filename")?;
    let resolved = parent.join(name);
    if data.exists() {
        ensure!(
            !resolved.starts_with(data.canonicalize()?),
            "save the recovery backup outside Xana's managed data directory"
        );
    } else if let (Some(data_parent), Some(data_name)) = (data.parent(), data.file_name())
        && let Ok(data_parent) = data_parent.canonicalize()
    {
        ensure!(
            resolved != data_parent.join(data_name),
            "recovery backup cannot replace Xana's data directory"
        );
    }
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("could not inspect recovery destination"),
        Ok(_) => anyhow::bail!("recovery destination already exists; it will never be overwritten"),
    }
}

impl ProtectedStore {
    pub(super) fn managed_recovery_identity(&self) -> Result<RecoveryIdentity> {
        let bytes = Zeroizing::new(self.document(IDENTITY_DOCUMENT, 4096)?.context(
            "this home uses a user-supplied recovery key; use your original separate key file",
        )?);
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid managed recovery identity"))?;
        let identity: RecoveryIdentity = text
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid managed recovery identity"))?;
        let envelope =
            crate::bounded_file::read(&self.inner.root.join("recovery.age"), super::MAX_BOOTSTRAP)?;
        let recovered = Secrets::recover(self.id(), &envelope, &identity)?;
        self.with_database(|db| {
            ensure!(
                *recovered.database == *db.secrets.database,
                "managed recovery verification differs"
            );
            Ok(())
        })?;
        Ok(identity)
    }

    /// Creates a new private file. Never rotates the identity: earlier encrypted
    /// snapshots must remain recoverable with a backup exported later.
    pub(crate) fn export_recovery(&self, destination: &Path) -> Result<()> {
        let data = self
            .inner
            .root
            .parent()
            .context("data directory unavailable")?;
        validate_destination(data, destination)?;
        let identity = self.managed_recovery_identity()?;
        // Serialize the deliberate export with handle revocation; a lock that
        // wins this race must prevent plaintext creation.
        self.with_database(|_| {
            let mut file = super::create_private_file(destination)?;
            writeln!(file, "{}", identity.to_string().expose_secret())?;
            file.sync_all()?;
            drop(file);
            let verified = super::read_recovery_identity(destination)?;
            ensure!(
                verified.to_public() == identity.to_public(),
                "recovery export verification differs"
            );
            // No key, destination path, or user content enters this metadata receipt.
            super::migration::write_atomic(&self.inner.root.join(STATUS_FILE), b"exported\n")
                .context("recovery file saved, but its status receipt could not be updated")
        })
    }
}

#[cfg(test)]
mod tests;
