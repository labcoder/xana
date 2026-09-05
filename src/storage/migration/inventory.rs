//! A bounded, content-hashed inventory. Nothing outside the selected data root is read.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path};

const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    History,
    Artifact,
    Document,
    Ordinary,
    Archive,
    Lock,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub name: String,
    pub length: u64,
    pub modified_unix_millis: u64,
    pub hash: String,
    pub kind: Kind,
}

pub(super) fn scan(root: &Path) -> Result<Vec<Entry>> {
    ensure!(
        fs::symlink_metadata(root)?.file_type().is_dir(),
        "migration source must be a real directory"
    );
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut entries = Vec::new();
    let mut visited = 0;
    while let Some((directory, depth)) = pending.pop() {
        ensure!(depth <= MAX_DEPTH, "migration tree exceeds depth bound");
        for item in fs::read_dir(directory)? {
            visited += 1;
            ensure!(
                visited <= MAX_ENTRIES,
                "migration inventory exceeds entry bound"
            );
            let path = item?.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "migration does not follow links: {}",
                path.display()
            );
            if metadata.is_dir() {
                pending.push((path, depth + 1));
                continue;
            }
            ensure!(
                metadata.is_file(),
                "migration requires regular files: {}",
                path.display()
            );
            let name = path
                .strip_prefix(root)?
                .to_str()
                .context("migration path is not UTF-8")?
                .replace('\\', "/");
            ensure!(
                name.len() <= 1024 && !name.chars().any(char::is_control),
                "migration filename exceeds supported shape"
            );
            let kind = classify(&name)?;
            let hash = if kind == Kind::Lock {
                String::new()
            } else {
                hash_file(&path, metadata.len())?
            };
            entries.push(Entry {
                name,
                length: metadata.len(),
                modified_unix_millis: u64::try_from(
                    metadata
                        .modified()?
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis(),
                )?,
                hash,
                kind,
            });
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn classify(name: &str) -> Result<Kind> {
    ensure!(
        !name.starts_with("protected/"),
        "this is already a protected home"
    );
    ensure!(
        name != "interoperable/private-state-migration.json",
        "finish xana config migrate before storage migration"
    );
    if name.ends_with(".lock") || name.ends_with(".writer") {
        return Ok(Kind::Lock);
    }
    if name.starts_with("sessions/") && name.ends_with(".jsonl") {
        return Ok(Kind::History);
    }
    if let Some(hash) = name.strip_prefix("artifacts/")
        && crate::artifact::ContentHash::parse(hash.into()).is_ok()
    {
        return Ok(Kind::Artifact);
    }
    if (name.starts_with("interoperable/")
        && name.matches('/').count() == 1
        && name.ends_with(".json"))
        || (name.starts_with("managed-threads/") && name.ends_with(".json"))
        || (name.starts_with("workspace-hosts/") && name.ends_with(".json"))
        || name.starts_with("frontend/composer-history/")
    {
        return Ok(Kind::Document);
    }
    // Authored package code and inert settings remain usable as ordinary files.
    // Logs/crash reports use the existing metadata-only diagnostic contract.
    if name.starts_with("interoperable/plugins/")
        || name.starts_with("logs/")
        || name.starts_with("crashes/")
        || name.starts_with("setup/")
        || name == "selection.toml"
        || name == "frontend/presentation.toml"
        || name.starts_with("frontend/workbench/")
        || name.starts_with("workspace-hosts/")
    {
        return Ok(Kind::Ordinary);
    }
    // Unknown and abandoned derived files are preserved in an encrypted archive,
    // not silently reinstalled into a live plaintext path.
    Ok(Kind::Archive)
}

pub(super) fn hash_file(path: &Path, expected: u64) -> Result<String> {
    ensure!(
        expected <= crate::resource::MAX_RESOURCE_SOURCE_BYTES as u64,
        "migration file exceeds supported bound: {}",
        path.display()
    );
    let mut file = fs::File::open(path)?;
    let identity = same_file::Handle::from_file(file.try_clone()?)?;
    let before = file.metadata()?;
    let mut hash = blake3::Hasher::new();
    let mut bytes = 0u64;
    let mut buffer = zeroize::Zeroizing::new([0u8; 64 * 1024]);
    loop {
        let count = file.read(&mut *buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        ensure!(bytes <= expected, "migration source grew during inspection");
        hash.update(&buffer[..count]);
    }
    ensure!(
        bytes == expected
            && fs::metadata(path)?.modified()? == before.modified()?
            && same_file::Handle::from_path(path)? == identity,
        "migration source changed during inspection"
    );
    Ok(hash.finalize().to_hex().to_string())
}

pub(super) fn lock_writers(root: &Path, entries: &[Entry]) -> Result<Vec<fs::File>> {
    entries
        .iter()
        .filter(|entry| entry.kind == Kind::Lock)
        .map(|entry| {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join(&entry.name))?;
            fs2::FileExt::try_lock_exclusive(&file)
                .context("close all Xana processes before migrating this home")?;
            Ok(file)
        })
        .collect()
}
