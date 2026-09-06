//! A bounded metadata watcher: periodic rescans coalesce rename storms and
//! missed OS events without trusting file contents or following links.
use super::Observation;
use crate::{storage::ProtectedStore, workspace_identity::WorkspaceIdentity};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const MAX_ENTRIES: usize = 256;
const MAX_DEPTH: usize = 16;
const OWN_OUTPUTS: &str = "autonomy/own-file-outputs";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileTrigger {
    pub(crate) root: PathBuf,
    pub(crate) identity: String,
    pub(crate) baseline: BTreeMap<String, String>,
    pub(crate) candidate: Option<(String, i64)>,
    pub(crate) unknown_generation: u64,
    pub(crate) observation: Observation,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnOutputs {
    // Canonical path hashes and exact resulting metadata, not source contents.
    files: BTreeMap<String, (String, i64)>,
    unknown_generation: u64,
}

impl FileTrigger {
    pub(crate) fn create(
        store: &ProtectedStore,
        root: &Path,
        workspace: &Path,
        excluded: &[PathBuf],
    ) -> Result<Self> {
        let requested = if root.is_absolute() {
            root.to_owned()
        } else {
            workspace.join(root)
        };
        let identity = WorkspaceIdentity::resolve(&requested)?;
        let root = identity.canonical_path().to_owned();
        ensure!(
            root.starts_with(workspace) && root.is_dir(),
            "watch root must be a directory inside the task workspace"
        );
        for path in excluded {
            let path = path.canonicalize().or_else(|error| {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error);
                }
                Ok(path
                    .parent()
                    .context("excluded path has no parent")
                    .map_err(std::io::Error::other)?
                    .canonicalize()?
                    .join(
                        path.file_name()
                            .context("excluded path has no name")
                            .map_err(std::io::Error::other)?,
                    ))
            })?;
            ensure!(
                !root.starts_with(&path) && !path.starts_with(&root),
                "watch root overlaps Xana-managed state; select a narrower source directory"
            );
        }
        let mut watch = Self {
            root,
            identity: identity.collision_key().into(),
            baseline: BTreeMap::new(),
            candidate: None,
            unknown_generation: 0,
            observation: Observation::default(),
        };
        validate_root(&watch, workspace)?;
        watch.baseline = scan(&watch, workspace)?;
        watch.unknown_generation = outputs(store)?.unknown_generation;
        watch.observation.status =
            "Baseline captured; waiting for a stable selected-file change".into();
        Ok(watch)
    }
    pub(crate) fn validate(&self, workspace: &Path) -> Result<()> {
        ensure!(
            self.root.is_absolute()
                && self.root.starts_with(workspace)
                && self.identity.len() == 64,
            "invalid selected watcher root"
        );
        ensure!(
            self.baseline.len() <= MAX_ENTRIES
                && self
                    .baseline
                    .iter()
                    .all(|(k, v)| k.len() == 64 && v.len() == 64),
            "watcher snapshot exceeds bounds"
        );
        Ok(())
    }
}

pub(crate) fn validate_root(watch: &FileTrigger, workspace: &Path) -> Result<()> {
    let identity = WorkspaceIdentity::resolve(&watch.root)?;
    ensure!(
        identity.collision_key() == watch.identity
            && identity.canonical_path() == watch.root
            && watch.root.starts_with(workspace),
        "selected watcher root was replaced"
    );
    let mut current = watch.root.as_path();
    loop {
        ensure!(
            !is_link(&fs::symlink_metadata(current)?),
            "watcher roots cannot traverse links or reparse points"
        );
        if current == workspace {
            break;
        }
        current = current.parent().context("watch root escaped workspace")?;
    }
    Ok(())
}

pub(crate) fn observe(
    store: &ProtectedStore,
    mut watch: FileTrigger,
    workspace: &Path,
    now: i64,
) -> Result<FileTrigger> {
    let current = scan(&watch, workspace)?;
    let own = outputs(store)?;
    let generation = own.unknown_generation;
    // Suppress only the sampled post-result revision. This is not OS writer
    // attribution; a later edit with changed metadata remains eligible.
    for (path, (fingerprint, at)) in &own.files {
        if now.saturating_sub(*at) <= 86400 && current.get(path) == Some(fingerprint) {
            watch.baseline.insert(path.clone(), fingerprint.clone());
        }
    }
    watch.observation.last_checked = Some(now);
    if current == watch.baseline {
        watch.unknown_generation = generation;
        watch.candidate = None;
        watch.observation.status = "Unchanged; no model call or notification".into();
        return Ok(watch);
    }
    ensure!(
        generation == watch.unknown_generation,
        "unattributed Xana command effects require owner review before watching resumes"
    );
    let digest = blake3::hash(&serde_json::to_vec(&current)?)
        .to_hex()
        .to_string();
    if let Some((prior, since)) = &watch.candidate
        && *prior == digest
        && now.saturating_sub(*since) >= 2
    {
        watch.baseline = current;
        watch.candidate = None;
        watch.observation.pending = true;
        watch.observation.last_event = Some(now);
        watch.observation.status = "Stable selected-file change; fixed owner-authored action is ready (file contents are not instructions)".into();
    } else if watch
        .candidate
        .as_ref()
        .is_none_or(|(prior, _)| prior != &digest)
    {
        watch.candidate = Some((digest, now));
        watch.observation.status =
            "Coalescing selected-file changes until the snapshot is stable".into();
    }
    Ok(watch)
}

fn scan(watch: &FileTrigger, workspace: &Path) -> Result<BTreeMap<String, String>> {
    validate_root(watch, workspace)?;
    let mut pending = vec![(watch.root.clone(), 0usize)];
    let mut result = BTreeMap::new();
    let mut count = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        ensure!(
            depth <= MAX_DEPTH,
            "watcher directory depth exceeds its bounded rescan"
        );
        ensure!(
            !is_link(&fs::symlink_metadata(&directory)?)
                && directory.canonicalize()?.starts_with(&watch.root),
            "watcher directory replaced by a link"
        );
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            count += 1;
            ensure!(
                count <= MAX_ENTRIES,
                "watcher overflow; select a smaller root and review before retrying"
            );
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                !is_link(&metadata),
                "watcher refuses symlink/reparse replacement"
            );
            let canonical = path.canonicalize()?;
            ensure!(
                canonical.starts_with(&watch.root),
                "watcher path escaped selected root"
            );
            if metadata.is_dir() {
                pending.push((canonical, depth + 1));
            } else {
                ensure!(metadata.is_file(), "watcher refuses special files");
                result.insert(path_key(&canonical), fingerprint(&canonical)?);
            }
        }
    }
    validate_root(watch, workspace)?;
    Ok(result)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn fingerprint(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !is_link(&metadata),
        "not an ordinary output file"
    );
    let identity = WorkspaceIdentity::resolve(path)?;
    let stamp = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(
        blake3::hash(format!("{}:{}:{stamp}", identity.collision_key(), metadata.len()).as_bytes())
            .to_hex()
            .to_string(),
    )
}
fn path_key(path: &Path) -> String {
    blake3::hash(path.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string()
}
fn outputs(store: &ProtectedStore) -> Result<OwnOutputs> {
    store
        .document(OWN_OUTPUTS, 128 * 1024)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
        .map(|value| value.unwrap_or_default())
}

pub(crate) fn review_generation(tx: &rusqlite::Transaction<'_>) -> Result<u64> {
    use rusqlite::OptionalExtension;
    let bytes = tx
        .query_row(
            "SELECT body FROM documents WHERE name=?1",
            [OWN_OUTPUTS],
            |row| match row.get_ref(0)? {
                rusqlite::types::ValueRef::Blob(bytes) if bytes.len() <= 128 * 1024 => {
                    Ok(bytes.to_vec())
                }
                _ => Err(rusqlite::Error::InvalidQuery),
            },
        )
        .optional()?;
    let own: OwnOutputs = bytes
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?
        .unwrap_or_default();
    Ok(own.unknown_generation)
}

/// Called by the protected session writer, never by model-authored events.
pub(crate) fn record_invocation(
    store: &ProtectedStore,
    intent: &crate::operation::InvocationIntent,
    completed: bool,
) -> Result<()> {
    use crate::{
        operation::InvocationTarget,
        permission::{PermissionScope, PolicyDecision},
    };
    if intent.permission.effective != PolicyDecision::Allow {
        return Ok(());
    }
    let InvocationTarget::Tool { name, .. } = &intent.target else {
        return Ok(());
    };
    let known_write = completed && matches!(name.as_str(), "write_file" | "edit_file");
    let output = if known_write {
        match &intent.permission.request.scope {
            PermissionScope::WorkspacePath { canonical_path }
            | PermissionScope::ExternalPath { canonical_path } => fingerprint(canonical_path)
                .ok()
                .map(|fingerprint| (path_key(canonical_path), fingerprint)),
            _ => None,
        }
    } else {
        None
    };
    if output.is_none() && name != "run_command" && !known_write {
        return Ok(());
    }
    let now = super::super::now()?;
    store.autonomy_record_outputs(OWN_OUTPUTS, 128 * 1024, |bytes| {
        let mut own: OwnOutputs = bytes
            .map(serde_json::from_slice)
            .transpose()?
            .unwrap_or_default();
        own.files
            .retain(|_, (_, at)| now.saturating_sub(*at) <= 86400);
        if let Some((path, fingerprint)) = output {
            if own.files.len() >= MAX_ENTRIES && !own.files.contains_key(&path) {
                own.files.clear();
                own.unknown_generation = own
                    .unknown_generation
                    .checked_add(1)
                    .context("own-effect generation exhausted")?;
            }
            own.files.insert(path, (fingerprint, now));
        } else {
            // A missing/replaced post-write sample is uncertain, not a reason
            // to reject a successful foreground result. A shell command is
            // likewise not physically contained by its cwd. The
            // generation invalidates only a *changed* watcher sample; unchanged
            // roots absorb it without a task or notification.
            own.unknown_generation = own
                .unknown_generation
                .checked_add(1)
                .context("own-effect generation exhausted")?;
        }
        Ok(serde_json::to_vec(&own)?)
    })
}
