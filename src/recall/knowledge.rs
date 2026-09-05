use super::*;
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path},
};
use tokio_util::sync::CancellationToken;

mod refresh;

const MAX_FILES: usize = 1000;
const MAX_BATCH_BYTES: usize = 100 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KnowledgeRoot {
    pub(crate) id: Uuid,
    pub(crate) path: PathBuf,
    pub(crate) identity: String,
    pub(crate) scope: String,
    pub(crate) disclosure_routes: Vec<String>,
}
fn name(id: Uuid) -> String {
    format!("recall/roots/{id}")
}

impl RecallOwner {
    pub(crate) fn select_root(&self, path: &Path) -> Result<KnowledgeRoot> {
        ensure!(
            !is_link(&fs::symlink_metadata(path)?),
            "select the real notes directory, not a link"
        );
        ensure!(path.is_dir(), "knowledge root must be a directory");
        let identity = crate::workspace_identity::WorkspaceIdentity::resolve(path)?;
        let root = KnowledgeRoot {
            id: Uuid::new_v4(),
            path: identity.canonical_path().to_owned(),
            identity: identity.collision_key().into(),
            scope: self.scope(self.conversation)?,
            disclosure_routes: Vec::new(),
        };
        ensure!(self.roots()?.len() < 32, "knowledge root limit reached");
        self.store
            .recall_policy(&name(root.id), Some(&serde_json::to_vec(&root)?))?;
        Ok(root)
    }
    pub(crate) fn roots(&self) -> Result<Vec<KnowledgeRoot>> {
        self.store
            .document_names("recall/roots/", 32)?
            .into_iter()
            .map(|key| {
                let bytes = self
                    .store
                    .document(&key, 16 * 1024)?
                    .context("knowledge root disappeared")?;
                Ok(serde_json::from_slice(&bytes)?)
            })
            .collect()
    }
    pub(crate) fn knowledge_root(&self, id: Uuid) -> Result<Option<KnowledgeRoot>> {
        self.store
            .document(&name(id), 16 * 1024)?
            .map(|body| serde_json::from_slice(&body).map_err(Into::into))
            .transpose()
    }
    pub(crate) fn revoke_root(&self, id: Uuid) -> Result<()> {
        self.store.recall_policy(&name(id), None)?;
        self.store.recall_forget_root(id)
    }
    pub(crate) fn disclose_root(
        &self,
        id: Uuid,
        route: String,
        allow: bool,
    ) -> Result<KnowledgeRoot> {
        ensure!(
            route.len() <= 1024 && !route.is_empty(),
            "invalid disclosure route"
        );
        let mut root = self.knowledge_root(id)?.context("unknown knowledge root")?;
        ensure!(
            root.scope == self.scope(self.conversation)?,
            "knowledge root belongs to another scope"
        );
        root.disclosure_routes.retain(|existing| existing != &route);
        if allow {
            ensure!(
                root.disclosure_routes.len() < 32,
                "knowledge disclosure route limit reached"
            );
            root.disclosure_routes.push(route);
        }
        self.store
            .recall_policy(&name(id), Some(&serde_json::to_vec(&root)?))?;
        Ok(root)
    }
    pub(crate) fn create_notes(&self) -> Result<PathBuf> {
        let path = self.paths.data_dir().join("notes");
        if path.exists() {
            ensure!(
                path.is_dir() && !is_link(&fs::symlink_metadata(&path)?),
                "notes location is not an ordinary directory"
            );
        } else {
            fs::create_dir(&path)?;
        }
        Ok(path)
    }

    /// Explicit plaintext export into a new directory; never overwrites an
    /// existing tree or silently exports stale indexed copies.
    pub(crate) fn export_notes(
        &self,
        id: Uuid,
        destination: &Path,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        let root = self.knowledge_root(id)?.context("unknown knowledge root")?;
        ensure!(
            root.scope == self.scope(self.conversation)?,
            "knowledge root belongs to another scope"
        );
        let parent = destination
            .parent()
            .context("export destination needs a parent")?;
        ensure!(
            parent.is_dir() && !is_link(&fs::symlink_metadata(parent)?),
            "export parent must be an ordinary directory"
        );
        fs::create_dir(destination).context("export requires a new destination directory")?;
        let destination = destination.canonicalize()?;
        let generation = self.store.privacy_generation()?;
        let mut count = 0;
        let mut bytes = 0;
        for source in self.store.recall_root_sources(id)? {
            ensure!(
                !cancel.is_cancelled(),
                "notes export cancelled; partial new export directory retained"
            );
            ensure!(
                self.store.privacy_generation()? == generation,
                "notes policy changed during export"
            );
            let Source::File {
                root: source_root,
                relative_path,
            } = source.source
            else {
                anyhow::bail!("invalid indexed notes source");
            };
            ensure!(source_root == id, "notes source belongs to another root");
            let text = read_source(&root, &relative_path)?;
            ensure!(
                blake3::hash(text.as_bytes()).to_hex().as_str() == source.hash,
                "notes source changed; refresh before export"
            );
            bytes += text.len();
            ensure!(bytes <= MAX_BATCH_BYTES, "notes export exceeds100MiB batch");
            let mut target = destination.clone();
            let components = relative_path.components().collect::<Vec<_>>();
            for component in &components[..components.len() - 1] {
                target.push(component.as_os_str());
                if !target.exists() {
                    fs::create_dir(&target)?;
                }
                ensure!(
                    target.is_dir() && !is_link(&fs::symlink_metadata(&target)?),
                    "export directory was replaced"
                );
            }
            target.push(
                components
                    .last()
                    .expect("validated relative file")
                    .as_os_str(),
            );
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            output.write_all(text.as_bytes())?;
            output.sync_all()?;
            count += 1;
        }
        Ok(count)
    }
}

fn verify_root(root: &KnowledgeRoot) -> Result<()> {
    let current = crate::workspace_identity::WorkspaceIdentity::resolve(&root.path)?;
    ensure!(
        current.canonical_path() == root.path && current.collision_key() == root.identity,
        "knowledge root identity changed"
    );
    Ok(())
}
pub(super) fn read_source(root: &KnowledgeRoot, relative: &Path) -> Result<String> {
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "notes paths must remain relative beneath the selected root"
    );
    verify_root(root)?;
    let mut path = root.path.clone();
    for component in relative.components() {
        path.push(component);
        ensure!(
            !is_link(&fs::symlink_metadata(&path)?),
            "notes links and reparse points are not indexed"
        );
    }
    let canonical = path.canonicalize()?;
    ensure!(
        canonical.starts_with(&root.path),
        "notes path escapes its selected root"
    );
    let file = fs::File::open(&canonical)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_SOURCE_BYTES as u64,
        "notes source exceeds its regular-file bound"
    );
    let identity = same_file::Handle::from_file(file.try_clone()?)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_SOURCE_BYTES,
        "notes source grew beyond its bound"
    );
    verify_root(root)?;
    ensure!(
        path.canonicalize()? == canonical && identity == same_file::Handle::from_path(&canonical)?,
        "notes source changed during read"
    );
    String::from_utf8(bytes).context("notes source must be UTF-8 text")
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
