//! Verified encrypted snapshots. Retention prunes only our complete generations,
//! and only after a usable replacement has been independently opened.

#[cfg(test)]
mod tests;

use super::{ProtectedStore, database::Database, keys::Secrets};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackupPolicy {
    pub enabled: bool,
    pub interval_hours: u32,
    pub retention_days: u32,
    pub max_snapshots: usize,
    pub max_bytes: u64,
    pub directory: Option<PathBuf>,
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: 24,
            retention_days: 7,
            max_snapshots: 7,
            max_bytes: 1024 * 1024 * 1024,
            directory: None,
        }
    }
}

impl BackupPolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=8760).contains(&self.interval_hours)
                && (1..=36500).contains(&self.retention_days)
                && (1..=365).contains(&self.max_snapshots)
                && (1..=1024 * 1024 * 1024 * 1024).contains(&self.max_bytes),
            "backup policy bounds are invalid"
        );
        ensure!(
            self.directory
                .as_ref()
                .is_none_or(|path| path.is_absolute()),
            "backup directory must be absolute"
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct BackupReport {
    pub snapshot: Option<PathBuf>,
    pub retained_snapshots: usize,
    pub retained_bytes: u64,
    pub oldest_unix_seconds: Option<u64>,
    pub notices: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    version: u32,
    id: Uuid,
    store: Uuid,
    created: u64,
    bytes: u64,
}

impl ProtectedStore {
    pub(crate) fn backup_policy(&self) -> Result<BackupPolicy> {
        self.document("maintenance/backup-policy", 4096)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
            .unwrap_or_else(|| Ok(BackupPolicy::default()))
    }

    pub(crate) fn set_backup_policy(&self, policy: &BackupPolicy) -> Result<()> {
        policy.validate()?;
        self.set_document(
            "maintenance/backup-policy",
            &serde_json::to_vec(policy)?,
            4096,
        )
    }

    pub(crate) fn backup(
        &self,
        policy: &BackupPolicy,
        now: u64,
        only_if_due: bool,
    ) -> Result<BackupReport> {
        policy.validate()?;
        let directory = policy.directory.clone().unwrap_or_else(|| {
            self.data_dir().with_file_name(format!(
                "{}.backups",
                self.data_dir()
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ))
        });
        fs::create_dir_all(&directory)?;
        let directory = directory.canonicalize()?;
        ensure!(
            !directory.starts_with(self.data_dir().canonicalize()?),
            "keep backup generations outside the active data directory"
        );
        let backup_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("backup.lock"))?;
        fs2::FileExt::try_lock_exclusive(&backup_lock)
            .map_err(|_| anyhow::anyhow!("another backup is in progress"))?;
        let mut existing = inventory(&directory, self.id())?;
        let report = |notice: &str| BackupReport {
            snapshot: None,
            retained_snapshots: existing.len(),
            retained_bytes: existing.iter().map(|(_, stamp)| stamp.bytes).sum(),
            oldest_unix_seconds: existing.first().map(|(_, stamp)| stamp.created),
            notices: vec![notice.into()],
        };
        if only_if_due
            && (!policy.enabled
                || existing.last().is_some_and(|(_, last)| {
                    now.saturating_sub(last.created) < u64::from(policy.interval_hours) * 3600
                }))
        {
            return Ok(report("automatic backup not due"));
        }
        let estimate = self
            .with_database(|db| {
                let pages: u64 = db.connection.query_row("PRAGMA page_count", [], |row| {
                    super::database::read_u64(row, 0)
                })?;
                let page_size: String =
                    db.connection
                        .query_row("PRAGMA cipher_page_size", [], |row| row.get(0))?;
                let page_size: u64 = page_size.parse()?;
                ensure!(
                    (512..=65536).contains(&page_size),
                    "unsupported backup page size"
                );
                Ok(pages.saturating_mul(page_size))
            })?
            .saturating_add(
                self.object_inventory()?
                    .iter()
                    .map(|(_, _, size)| size.saturating_add(size / 4096 + 65536))
                    .sum::<u64>(),
            )
            .saturating_add(16384);
        if estimate > policy.max_bytes || fs2::available_space(&directory)? < estimate {
            return Ok(report(
                "No fresh backup: size/space limit. The last usable copy was retained; increase the limit/location explicitly.",
            ));
        }
        let id = Uuid::new_v4();
        let pending = directory.join(format!("{id}.pending"));
        fs::create_dir(&pending)?;
        let copied = self.snapshot_into(&pending, policy.max_bytes)?;
        let mut stamp = Stamp {
            version: 1,
            id,
            store: self.id(),
            created: now,
            bytes: copied,
        };
        loop {
            let total = copied + serde_json::to_vec(&stamp)?.len() as u64;
            if stamp.bytes == total {
                break;
            }
            stamp.bytes = total;
        }
        ensure!(
            stamp.bytes <= policy.max_bytes,
            "backup plus metadata exceeds cap; prior recovery copy retained"
        );
        super::write_new_synced(&pending.join("snapshot.json"), &serde_json::to_vec(&stamp)?)?;
        let destination = directory.join(id.to_string());
        fs::rename(&pending, &destination)?;
        existing.push((destination.clone(), stamp));
        existing.sort_by_key(|(_, stamp)| (stamp.created, stamp.id));
        let mut total: u64 = existing.iter().map(|(_, stamp)| stamp.bytes).sum();
        let cutoff = now.saturating_sub(u64::from(policy.retention_days) * 86400);
        while existing.len() > 1
            && (existing.len() > policy.max_snapshots
                || total > policy.max_bytes
                || existing[0].1.created < cutoff)
        {
            let (path, stamp) = &existing[0];
            validate_snapshot_tree(path, stamp)?;
            let database = self.with_database(|db| {
                Database::open(
                    &path.join("protected"),
                    self.id(),
                    Secrets::decode(&db.secrets.encode()?)?,
                )
            })?;
            let old = ProtectedStore::from_database(path.join("protected"), self.id(), database);
            old.verify_content()?;
            let object_names: std::collections::HashSet<_> = old
                .object_inventory()?
                .into_iter()
                .map(|(_, id, _)| format!("{id}.age"))
                .collect();
            for entry in fs::read_dir(path.join("protected/objects"))? {
                let name = entry?.file_name();
                ensure!(
                    name.to_str()
                        .is_some_and(|name| object_names.contains(name)),
                    "unrecognized snapshot object; refusing cleanup"
                );
            }
            old.with_exclusive_database(|db| db.checkpoint())?;
            ensure!(
                path.parent() == Some(directory.as_path())
                    && path
                        .file_name()
                        .is_some_and(|name| name == stamp.id.to_string().as_str()),
                "unsafe backup cleanup target"
            );
            fs::remove_dir_all(path)?;
            total -= stamp.bytes;
            existing.remove(0);
        }
        Ok(BackupReport {
            snapshot: Some(destination),
            retained_snapshots: existing.len(),
            retained_bytes: total,
            oldest_unix_seconds: existing.first().map(|(_, stamp)| stamp.created),
            notices: vec![],
        })
    }

    /// Copies a transactionally consistent database and its immutable ciphertext
    /// objects. All content verification succeeds before the result is published.
    pub(super) fn snapshot_into(&self, destination: &Path, cap: u64) -> Result<u64> {
        self.snapshot_into_with(destination, cap, |_| Ok(()))
    }

    fn snapshot_into_with(
        &self,
        destination: &Path,
        cap: u64,
        fault: impl Fn(&str) -> Result<()>,
    ) -> Result<u64> {
        ensure!(
            fs::read_dir(destination)?.next().is_none(),
            "snapshot destination must be empty"
        );
        let root = destination.join("protected");
        fs::create_dir(&root)?;
        let database = self.with_database(|db| {
            let secrets = Secrets::decode(&db.secrets.encode()?)?;
            let mut target = Database::create(&root, self.id(), secrets)?;
            {
                // SQLCipher supports encrypted-to-encrypted backup with matching
                // cipher settings; both connections were keyed before this call.
                let backup = rusqlite::backup::Backup::new(&db.connection, &mut target.connection)?;
                let deadline = std::time::Instant::now() + Duration::from_secs(120);
                loop {
                    ensure!(
                        std::time::Instant::now() < deadline,
                        "backup deadline exceeded; source and prior backup retained"
                    );
                    let result = backup.step(256)?;
                    if result == rusqlite::backup::StepResult::Done {
                        break;
                    }
                    if matches!(
                        result,
                        rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked
                    ) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
            target.checkpoint()?;
            target.verify()?;
            Ok(target)
        })?;
        let copied = ProtectedStore::from_database(root.clone(), self.id(), database);
        fault("backup-database")?;
        let mut bytes = fs::metadata(root.join("content.sqlite"))?.len();
        ensure!(
            bytes <= cap,
            "backup exceeds byte cap; prior recovery copy retained"
        );
        for name in ["store.json", "recovery.age"] {
            let content =
                crate::bounded_file::read(&self.inner.root.join(name), super::MAX_BOOTSTRAP)?;
            super::write_new_synced(&root.join(name), &content)?;
            bytes += content.len() as u64;
        }
        if self.inner.root.join(super::recovery::STATUS_FILE).exists() {
            super::recovery::status(self.inner.root.parent().context("missing data directory")?)?;
            let content =
                crate::bounded_file::read(&self.inner.root.join(super::recovery::STATUS_FILE), 32)?;
            super::write_new_synced(&root.join(super::recovery::STATUS_FILE), &content)?;
            bytes += content.len() as u64;
        }
        ensure!(
            bytes <= cap,
            "backup bootstrap exceeds byte cap; prior recovery copy retained"
        );
        fault("backup-bootstrap")?;
        fs::create_dir(root.join("objects"))?;
        for (_, id, _) in copied.object_inventory()? {
            let id: Uuid = id.parse()?;
            let name = format!("{id}.age");
            let source = self.inner.root.join("objects").join(&name);
            ensure!(
                fs::symlink_metadata(&source)?.file_type().is_file(),
                "backup object must be a regular file"
            );
            let length = fs::metadata(&source)?.len();
            ensure!(
                bytes.checked_add(length).is_some_and(|n| n <= cap),
                "backup exceeds byte cap; prior recovery copy retained"
            );
            let mut output = fs::File::create_new(root.join("objects").join(name))?;
            ensure!(
                std::io::copy(&mut fs::File::open(source)?.take(length + 1), &mut output)?
                    == length,
                "backup object changed during copy"
            );
            output.flush()?;
            output.sync_all()?;
            bytes += length;
            fault("backup-object")?;
        }
        copied.verify_content()?;
        copied.with_database(|db| db.checkpoint())?;
        fault("backup-verified")?;
        drop(copied);
        Ok(bytes)
    }
}

fn inventory(directory: &Path, store: Uuid) -> Result<Vec<(PathBuf, Stamp)>> {
    let mut snapshots = Vec::new();
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        ensure!(index < 4096, "backup directory exceeds inventory bound");
        let path = entry?.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse::<Uuid>().ok())
        else {
            continue;
        };
        if !fs::symlink_metadata(&path)?.file_type().is_dir() {
            continue;
        }
        let stamp: Stamp = serde_json::from_slice(&crate::bounded_file::read(
            &path.join("snapshot.json"),
            4096,
        )?)?;
        ensure!(
            stamp.version == 1 && stamp.id == id,
            "backup stamp differs from generation"
        );
        if stamp.store == store {
            snapshots.push((path, stamp));
        }
    }
    snapshots.sort_by_key(|(_, stamp)| (stamp.created, stamp.id));
    Ok(snapshots)
}

fn validate_snapshot_tree(path: &Path, stamp: &Stamp) -> Result<()> {
    // Never recurse into an unrecognized or user-modified generation when pruning.
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        ensure!(
            matches!(
                entry.file_name().to_str(),
                Some("protected" | "snapshot.json")
            ),
            "backup contains unrecognized files; refusing cleanup"
        );
    }
    let bootstrap: super::Bootstrap = serde_json::from_slice(&crate::bounded_file::read(
        &path.join("protected/store.json"),
        4096,
    )?)?;
    ensure!(bootstrap.id == stamp.store, "backup home identity differs");
    let mut pending = vec![path.to_path_buf()];
    let mut visited = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            visited += 1;
            ensure!(visited <= 100_016, "backup cleanup exceeds inventory bound");
            let item = entry?.path();
            let kind = fs::symlink_metadata(&item)?.file_type();
            ensure!(!kind.is_symlink(), "backup cleanup does not follow links");
            let relative = item
                .strip_prefix(path)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid snapshot path"))?
                .replace('\\', "/");
            if kind.is_dir() {
                ensure!(
                    matches!(relative.as_str(), "protected" | "protected/objects"),
                    "unrecognized snapshot directory; refusing cleanup"
                );
                pending.push(item);
            } else {
                ensure!(kind.is_file(), "unrecognized backup entry");
                let object = relative
                    .strip_prefix("protected/objects/")
                    .and_then(|name| name.strip_suffix(".age"))
                    .is_some_and(|id| id.parse::<Uuid>().is_ok());
                ensure!(
                    object
                        || matches!(
                            relative.as_str(),
                            "snapshot.json"
                                | "protected/store.json"
                                | "protected/recovery.age"
                                | "protected/recovery-status"
                                | "protected/owner.lock"
                                | "protected/content.sqlite"
                                | "protected/content.sqlite-wal"
                                | "protected/content.sqlite-shm"
                        ),
                    "unrecognized snapshot file; refusing cleanup"
                );
            }
        }
    }
    Ok(())
}

pub(super) fn validate_snapshot(path: &Path) -> Result<u64> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "snapshot must be a real directory"
    );
    let stamp: Stamp = serde_json::from_slice(&crate::bounded_file::read(
        &path.join("snapshot.json"),
        4096,
    )?)?;
    ensure!(
        stamp.version == 1 && stamp.bytes <= 1024 * 1024 * 1024 * 1024,
        "unsupported snapshot format or size"
    );
    validate_snapshot_tree(path, &stamp)?;
    Ok(stamp.bytes)
}
