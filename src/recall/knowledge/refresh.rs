//! Resumable selected-root indexing. Only bounded metadata crosses batches;
//! originals are opened and revalidated immediately before each index write.
use super::*;
use serde::de::{self, SeqAccess, Visitor};

const MAX_ENTRIES: usize = 10_000;
const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 2000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSlot {
    #[serde(deserialize_with = "read_path")]
    path: PathBuf,
    indexed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u16,
    root: Uuid,
    identity: String,
    scope: String,
    generation: u64,
    next: usize,
    #[serde(deserialize_with = "read_files")]
    files: Vec<FileSlot>,
}

fn cursor_name(id: Uuid) -> String {
    format!("recall/refresh/{id}")
}

impl RecallOwner {
    pub(crate) fn refresh_root(&self, id: Uuid, cancel: &CancellationToken) -> Result<Progress> {
        self.refresh_root_with_restart(id, cancel, false)
    }

    pub(crate) fn refresh_root_with_restart(
        &self,
        id: Uuid,
        cancel: &CancellationToken,
        restart: bool,
    ) -> Result<Progress> {
        let root = self.knowledge_root(id)?.context("unknown knowledge root")?;
        ensure!(
            root.scope == self.scope(self.conversation)?,
            "knowledge root belongs to another scope"
        );
        verify_root(&root)?;
        let generation = self.store.privacy_generation()?;
        let mut saved = self.store.document(&cursor_name(id), MAX_MANIFEST_BYTES)?;
        let mut progress = Progress::default();
        let mut manifest = if let Some(body) = saved.as_deref().filter(|_| !restart) {
            let state: Manifest = serde_json::from_slice(body)?;
            ensure!(
                state.version == 1
                    && state.root == id
                    && state.identity == root.identity
                    && state.scope == root.scope
                    && state.generation == generation
                    && state.next <= state.files.len(),
                "notes cursor is stale; repeat notes refresh with --restart after reviewing current selection"
            );
            state
        } else {
            let state = inventory(&root, generation, cancel, &mut progress)?;
            let body = serde_json::to_vec(&state)?;
            self.store.recall_refresh_checkpoint(
                id,
                saved.as_deref(),
                Some(&body),
                generation,
                None,
            )?;
            saved = Some(body);
            state
        };
        let mut bytes = 0usize;
        while manifest.next < manifest.files.len() && progress.inspected < MAX_FILES {
            if cancel.is_cancelled() {
                let body = serde_json::to_vec(&manifest)?;
                self.store.recall_refresh_checkpoint(
                    id,
                    saved.as_deref(),
                    Some(&body),
                    generation,
                    None,
                )?;
                anyhow::bail!(
                    "notes indexing cancelled; bounded cursor retained for the next refresh"
                );
            }
            let slot = &mut manifest.files[manifest.next];
            // Metadata is only a preflight budget check, never read authority.
            // read_source performs the full root/link/identity/byte checks.
            if let Ok(metadata) = fs::symlink_metadata(root.path.join(&slot.path))
                && metadata.len() <= MAX_SOURCE_BYTES as u64
                && !fits_batch(bytes, metadata.len())
            {
                break;
            }
            let text = match read_source(&root, &slot.path) {
                Ok(text) => text,
                Err(_) => {
                    progress.inspected += 1;
                    progress.skipped += 1;
                    manifest.next += 1;
                    continue;
                }
            };
            if !fits_batch(bytes, text.len() as u64) {
                break;
            }
            bytes += text.len();
            let source = IndexedSource {
                key: source_key(id, &slot.path),
                scope: root.scope.clone(),
                source: Source::File {
                    root: id,
                    relative_path: slot.path.clone(),
                },
                hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            };
            self.store.recall_index(&source, &text, generation)?;
            slot.indexed = true;
            manifest.next += 1;
            progress.inspected += 1;
            progress.indexed += 1;
        }
        verify_root(&root)?;
        ensure!(
            self.scope(self.conversation)? == root.scope,
            "Project or Profile changed during notes indexing"
        );
        progress.pending = manifest.next < manifest.files.len();
        if progress.pending {
            let body = serde_json::to_vec(&manifest)?;
            self.store.recall_refresh_checkpoint(
                id,
                saved.as_deref(),
                Some(&body),
                generation,
                None,
            )?;
        } else {
            let seen = manifest
                .files
                .iter()
                .filter(|file| file.indexed)
                .map(|file| source_key(id, &file.path))
                .collect::<Vec<_>>();
            // Cursor removal and stale-index cleanup share the CAS/privacy
            // transaction; a cancelled/stale/competing scan cannot prune work.
            self.store.recall_refresh_checkpoint(
                id,
                saved.as_deref(),
                None,
                generation,
                Some(&seen),
            )?;
        }
        Ok(progress)
    }
}

fn source_key(root: Uuid, relative: &Path) -> String {
    format!("root:{root}/{}", relative.to_string_lossy())
}

fn fits_batch(used: usize, next: u64) -> bool {
    used <= MAX_BATCH_BYTES && next <= (MAX_BATCH_BYTES - used) as u64
}

fn inventory(
    root: &KnowledgeRoot,
    generation: u64,
    cancel: &CancellationToken,
    progress: &mut Progress,
) -> Result<Manifest> {
    let mut files = Vec::new();
    let mut pending = vec![(root.path.clone(), 0usize)];
    let mut visited = 0usize;
    let mut path_bytes = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        ensure!(depth <= 32, "notes nesting exceeds 32 directories");
        ensure!(
            !is_link(&fs::symlink_metadata(&directory)?),
            "notes directory changed into a link"
        );
        for item in fs::read_dir(directory)? {
            ensure!(
                !cancel.is_cancelled(),
                "notes inventory cancelled; previous index remains inspectable"
            );
            visited += 1;
            ensure!(
                visited <= MAX_ENTRIES,
                "selected notes inventory exceeds 10000 entries; select narrower roots"
            );
            let item = item?;
            let metadata = fs::symlink_metadata(item.path())?;
            if is_link(&metadata) {
                progress.skipped += 1;
                continue;
            }
            let path = item.path();
            if metadata.is_dir() {
                ensure!(pending.len() < 256, "notes directory queue exceeds bound");
                pending.push((path, depth + 1));
            } else if metadata.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("md")
                            || extension.eq_ignore_ascii_case("txt")
                    })
            {
                let relative = path.strip_prefix(&root.path)?;
                let text = relative.to_str().context("notes paths must be UTF-8")?;
                ensure!(
                    text.len() <= MAX_PATH_BYTES,
                    "notes relative path exceeds 2000 bytes"
                );
                path_bytes += serde_json::to_vec(text)?.len() + 32;
                ensure!(
                    path_bytes <= MAX_MANIFEST_BYTES - 4096 && files.len() < MAX_ENTRIES,
                    "notes manifest exceeds its metadata bound; select narrower roots"
                );
                files.push(FileSlot {
                    path: relative.to_owned(),
                    indexed: false,
                });
            }
        }
    }
    // Stable immutable inventory, independent of filesystem enumeration order.
    // Changes between batches are revalidated per file; new names enter a fresh
    // scan after completion or an explicit --restart.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Manifest {
        version: 1,
        root: root.id,
        identity: root.identity.clone(),
        scope: root.scope.clone(),
        generation,
        next: 0,
        files,
    })
}

fn read_path<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<PathBuf, D::Error> {
    struct PathVisitor;
    impl Visitor<'_> for PathVisitor {
        type Value = PathBuf;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded relative notes path")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<PathBuf, E> {
            if value.is_empty()
                || value.len() > MAX_PATH_BYTES
                || !Path::new(value)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err(E::custom(
                    "notes manifest path escapes or exceeds its bound",
                ));
            }
            Ok(PathBuf::from(value))
        }
    }
    deserializer.deserialize_str(PathVisitor)
}

fn read_files<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<FileSlot>, D::Error> {
    struct FilesVisitor;
    impl<'de> Visitor<'de> for FilesVisitor {
        type Value = Vec<FileSlot>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most 10000 notes manifest entries")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut files = Vec::new();
            while files.len() < MAX_ENTRIES {
                let Some(file) = sequence.next_element()? else {
                    return Ok(files);
                };
                files.push(file);
            }
            if sequence.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("notes manifest file count exceeds bound"));
            }
            Ok(files)
        }
    }
    deserializer.deserialize_seq(FilesVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_byte_batch_refuses_next_source_without_relaxing_the_hundred_mib_cap() {
        assert!(fits_batch(
            MAX_BATCH_BYTES - MAX_SOURCE_BYTES,
            MAX_SOURCE_BYTES as u64
        ));
        assert!(!fits_batch(
            MAX_BATCH_BYTES - MAX_SOURCE_BYTES + 1,
            MAX_SOURCE_BYTES as u64
        ));
        assert!(!fits_batch(MAX_BATCH_BYTES, 1));
        assert!(!fits_batch(MAX_BATCH_BYTES + 1, 0));
    }

    #[test]
    fn notes_cursor_rejects_escape_paths_and_excess_inventory_before_source_access() {
        for path in [
            "../outside.txt".to_owned(),
            "/absolute.txt".into(),
            "x".repeat(MAX_PATH_BYTES + 1),
        ] {
            assert!(
                serde_json::from_value::<FileSlot>(
                    serde_json::json!({"path":path,"indexed":false})
                )
                .is_err()
            );
        }
        let manifest = serde_json::json!({
            "version":1, "root":Uuid::new_v4(), "identity":"fixture", "scope":"fixture", "generation":0,"next":0,
            "files":(0..=MAX_ENTRIES).map(|index| serde_json::json!({"path":format!("{index}.md"),"indexed":false})).collect::<Vec<_>>()
        });
        assert!(serde_json::from_value::<Manifest>(manifest).is_err());
    }
}
