//! Declarative Agent Plugin acquisition, validation, and lifecycle state.
//!
//! Plugin bundles never load code into Xana. Installation validates inert
//! manifests, skills, and MCP declarations, then records an exact immutable
//! tree (or an explicitly mutable development link) without enabling it.

mod manifest;
mod source;

use crate::{
    paths::XanaPaths,
    private_state::{
        InstalledPackageRecord, PackageRevisionRecord, PackageSourceKind, PackageSourceRecord,
        PackageStateDocument, PrivateStateError, UpdateDocumentError, ensure_interoperable_records,
        read_document, update_document,
    },
};
use serde::Serialize;
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const AGENT_PLUGIN_VERSION: &str = "1.0.0";
pub(crate) const AGENT_PLUGIN_STATUS: &str = "working_draft";
pub(crate) const AGENT_PLUGIN_MANIFEST_SCHEMA: &str =
    "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
pub(crate) const AGENT_PLUGIN_MCP_SCHEMA: &str =
    "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PackageSource {
    Directory(PathBuf),
    Git { url: String, revision: String },
    Linked(PathBuf),
}

impl PackageSource {
    pub(crate) fn mutable(&self) -> bool {
        matches!(self, Self::Linked(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SourceIdentity {
    pub(crate) kind: &'static str,
    pub(crate) location: String,
    pub(crate) revision: Option<String>,
}

impl SourceIdentity {
    fn directory(path: &Path) -> Result<Self, PluginError> {
        Ok(Self {
            kind: "directory",
            location: canonical_location(path)?,
            revision: None,
        })
    }

    fn linked(path: &Path) -> Result<Self, PluginError> {
        Ok(Self {
            kind: "linked",
            location: canonical_location(path)?,
            revision: None,
        })
    }

    fn git(url: &str, revision: &str) -> Self {
        Self {
            kind: "git",
            location: url.to_owned(),
            revision: Some(revision.to_ascii_lowercase()),
        }
    }

    fn record(&self) -> PackageSourceRecord {
        PackageSourceRecord {
            kind: match self.kind {
                "directory" => PackageSourceKind::Directory,
                "git" => PackageSourceKind::Git,
                "linked" => PackageSourceKind::Linked,
                _ => unreachable!("source identities have a closed kind"),
            },
            location: self.location.clone(),
            revision: self.revision.clone(),
        }
    }

    fn digest(&self) -> Result<String, PluginError> {
        let bytes = serde_json::to_vec(self).map_err(PluginError::Encode)?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PluginManifest {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum McpServerKind {
    Stdio,
    StreamableHttp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct McpServerReview {
    pub(crate) name: String,
    pub(crate) kind: McpServerKind,
    /// Process executable token or redacted HTTP origin/path. Arguments,
    /// environment values, headers, and credentials are never rendered here.
    pub(crate) destination: String,
    pub(crate) argument_count: usize,
    pub(crate) environment_names: Vec<String>,
    pub(crate) header_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PluginReview {
    pub(crate) manifest: PluginManifest,
    pub(crate) root: PathBuf,
    pub(crate) digest: String,
    pub(crate) mutable: bool,
    pub(crate) skills: Vec<String>,
    pub(crate) mcp_servers: Vec<McpServerReview>,
    pub(crate) diagnostics: Vec<String>,
}

impl PluginReview {
    fn capability_digest(&self) -> Result<String, PluginError> {
        #[derive(Serialize)]
        struct Capabilities<'a> {
            skills: &'a [String],
            mcp_servers: &'a [McpServerReview],
        }
        let bytes = serde_json::to_vec(&Capabilities {
            skills: &self.skills,
            mcp_servers: &self.mcp_servers,
        })
        .map_err(PluginError::Encode)?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InstalledPlugin {
    pub(crate) name: String,
    pub(crate) active_revision: String,
    pub(crate) source: PackageSourceRecord,
    pub(crate) mutable: bool,
    pub(crate) root: PathBuf,
    pub(crate) manifest_version: Option<String>,
    pub(crate) skill_names: Vec<String>,
    pub(crate) mcp_server_names: Vec<String>,
    pub(crate) enabled_scopes: Vec<String>,
    pub(crate) rollback_available: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PluginManager {
    paths: XanaPaths,
    state_file: PathBuf,
    store: PathBuf,
}

impl PluginManager {
    pub(crate) fn open(paths: &XanaPaths) -> Self {
        Self {
            paths: paths.clone(),
            state_file: paths.package_state_file(),
            store: paths.package_store_dir(),
        }
    }

    pub(crate) fn inspect_source(
        &self,
        source: &PackageSource,
    ) -> Result<PluginReview, PluginError> {
        let acquired = source::acquire(source, &self.store, false)?;
        let digest = source::tree_digest(&acquired.root)?;
        manifest::inspect(&acquired.root, source.mutable(), digest)
    }

    pub(crate) fn install(
        &self,
        source: &PackageSource,
        expected_digest: &str,
    ) -> Result<InstalledPlugin, PluginError> {
        ensure_interoperable_records(&self.paths).map_err(PluginError::State)?;
        let acquired = source::acquire(source, &self.store, !source.mutable())?;
        let digest = source::tree_digest(&acquired.root)?;
        let review = manifest::inspect(&acquired.root, source.mutable(), digest.clone())?;
        if digest != expected_digest {
            return Err(PluginError::ReviewChanged {
                expected: expected_digest.to_owned(),
                actual: digest,
            });
        }
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        let source_digest = acquired.identity.digest()?;
        let source_record = acquired.identity.record();
        let relative_root = PathBuf::from("versions")
            .join(&review.manifest.name)
            .join(&digest)
            .join("bundle");
        let final_root = if source.mutable() {
            acquired.root.clone()
        } else {
            let destination = self.store.join(&relative_root);
            let staging = acquired.staging.ok_or_else(|| {
                PluginError::Invalid("immutable install has no staging tree".to_owned())
            })?;
            staging.commit_bundle(&destination)?;
            let stored_digest = source::tree_digest(&destination)?;
            if stored_digest != digest {
                return Err(PluginError::Changed(destination));
            }
            destination
        };
        let capability_digest = review.capability_digest()?;
        let revision = PackageRevisionRecord {
            digest: digest.clone(),
            manifest_version: review.manifest.version.clone(),
            installed_unix_ms: unix_millis()?,
            mutable: source.mutable(),
            managed_root: (!source.mutable()).then_some(relative_root),
            linked_root: source.mutable().then_some(final_root.clone()),
            skill_names: review.skills.clone(),
            mcp_server_names: review
                .mcp_servers
                .iter()
                .map(|server| server.name.clone())
                .collect(),
            capability_digest,
        };
        let package_name = review.manifest.name.clone();
        update_document::<PackageStateDocument, _, PluginError>(&self.state_file, |document| {
            match document.plugins.get_mut(&package_name) {
                Some(existing) => {
                    if existing.source_digest != source_digest {
                        return Err(PluginError::ConflictingSource(package_name.clone()));
                    }
                    if existing.active_revision != digest {
                        return Err(PluginError::UpdateRequired(package_name.clone()));
                    }
                    existing
                        .revisions
                        .entry(digest.clone())
                        .or_insert(revision.clone());
                    if !existing.retained_revisions.contains(&digest) {
                        existing.retained_revisions.push(digest.clone());
                    }
                }
                None => {
                    document.plugins.insert(
                        package_name.clone(),
                        InstalledPackageRecord {
                            package: package_name.clone(),
                            source_digest,
                            active_revision: digest.clone(),
                            retained_revisions: vec![digest.clone()],
                            enabled_scopes: Vec::new(),
                            source: Some(source_record.clone()),
                            revisions: [(digest.clone(), revision.clone())].into_iter().collect(),
                        },
                    );
                }
            }
            Ok(())
        })
        .map_err(map_state_update)?;
        self.inspect_installed(&package_name)
    }

    pub(crate) fn list(&self) -> Result<Vec<InstalledPlugin>, PluginError> {
        let document = match read_document::<PackageStateDocument>(&self.state_file) {
            Ok(document) => document,
            Err(PrivateStateError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(PluginError::State(error)),
        };
        document
            .plugins
            .keys()
            .map(|name| installed_from_document(&self.store, &document, name))
            .collect()
    }

    pub(crate) fn inspect_installed(&self, name: &str) -> Result<InstalledPlugin, PluginError> {
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        installed_from_document(&self.store, &document, name)
    }
}

fn installed_from_document(
    store: &Path,
    document: &PackageStateDocument,
    name: &str,
) -> Result<InstalledPlugin, PluginError> {
    let package = document
        .plugins
        .get(name)
        .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
    let source = package.source.clone().ok_or_else(|| {
        PluginError::Invalid(format!(
            "plugin {name:?} has incomplete legacy source metadata"
        ))
    })?;
    let revision = package
        .revisions
        .get(&package.active_revision)
        .ok_or_else(|| {
            PluginError::Invalid(format!("plugin {name:?} has no active revision record"))
        })?;
    let root = revision
        .managed_root
        .as_ref()
        .map(|path| store.join(path))
        .or_else(|| revision.linked_root.clone())
        .ok_or_else(|| PluginError::Invalid(format!("plugin {name:?} has no package root")))?;
    Ok(InstalledPlugin {
        name: package.package.clone(),
        active_revision: package.active_revision.clone(),
        source,
        mutable: revision.mutable,
        root,
        manifest_version: revision.manifest_version.clone(),
        skill_names: revision.skill_names.clone(),
        mcp_server_names: revision.mcp_server_names.clone(),
        enabled_scopes: package.enabled_scopes.clone(),
        rollback_available: package.retained_revisions.len() > 1,
    })
}

fn map_state_update(error: UpdateDocumentError<PluginError>) -> PluginError {
    match error {
        UpdateDocumentError::State(error) => PluginError::State(error),
        UpdateDocumentError::Update(error) => error,
    }
}

fn unix_millis() -> Result<u64, PluginError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PluginError::Invalid("system clock precedes the Unix epoch".to_owned()))?
        .as_millis();
    u64::try_from(millis)
        .map_err(|_| PluginError::Invalid("system clock exceeds Xana's timestamp range".to_owned()))
}

fn canonical_location(path: &Path) -> Result<String, PluginError> {
    path.canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|source| PluginError::Io {
            path: path.to_owned(),
            source,
        })
}

struct PluginLifecycleLock {
    file: fs::File,
}

impl PluginLifecycleLock {
    fn acquire(store: &Path) -> Result<Self, PluginError> {
        fs::create_dir_all(store).map_err(|source| PluginError::Io {
            path: store.to_owned(),
            source,
        })?;
        let path = store.join("lifecycle.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| PluginError::Io {
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(fs::TryLockError::WouldBlock) => Err(PluginError::Busy(path)),
            Err(fs::TryLockError::Error(source)) => Err(PluginError::Io { path, source }),
        }
    }
}

impl Drop for PluginLifecycleLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug)]
pub(crate) enum PluginError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Json {
        path: PathBuf,
        reason: String,
    },
    Encode(serde_json::Error),
    State(PrivateStateError),
    Skill(crate::skill::SkillError),
    Process {
        action: &'static str,
        source: io::Error,
    },
    ProcessFailed(&'static str),
    Archive(String),
    Invalid(String),
    UnsupportedSchema(String),
    UnsafePath(PathBuf),
    Changed(PathBuf),
    RevisionDrift {
        expected: String,
        actual: String,
    },
    ReviewChanged {
        expected: String,
        actual: String,
    },
    Limit {
        what: &'static str,
        limit: usize,
    },
    ConflictingSource(String),
    UpdateRequired(String),
    Unknown(String),
    Busy(PathBuf),
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(
                formatter,
                "plugin I/O at {} failed: {source}",
                manifest::safe_path(path)
            ),
            Self::Json { path, reason } => write!(
                formatter,
                "{} is invalid JSON: {}",
                manifest::safe_path(path),
                manifest::safe_text(reason)
            ),
            Self::Encode(_) => formatter.write_str("could not encode plugin metadata"),
            Self::State(error) => write!(formatter, "plugin state is unavailable: {error}"),
            Self::Skill(error) => write!(formatter, "plugin skill validation failed: {error}"),
            Self::Process { action, source } => write!(formatter, "could not {action}: {source}"),
            Self::ProcessFailed(action) => {
                write!(formatter, "could not {action}; Git exited unsuccessfully")
            }
            Self::Archive(reason) => write!(
                formatter,
                "plugin archive is invalid: {}",
                manifest::safe_text(reason)
            ),
            Self::Invalid(reason) => formatter.write_str(reason),
            Self::UnsupportedSchema(schema) => write!(
                formatter,
                "unsupported Agent Plugin schema {schema:?}; Xana supports {AGENT_PLUGIN_MANIFEST_SCHEMA}"
            ),
            Self::UnsafePath(path) => write!(
                formatter,
                "plugin path {} is a link, special file, non-portable path, or traversal",
                manifest::safe_path(path)
            ),
            Self::Changed(path) => write!(
                formatter,
                "plugin source {} changed while Xana was inspecting it; retry",
                manifest::safe_path(path)
            ),
            Self::RevisionDrift { expected, actual } => write!(
                formatter,
                "Git revision drifted: expected {}, received {}",
                manifest::safe_text(expected),
                manifest::safe_text(actual)
            ),
            Self::ReviewChanged { expected, actual } => write!(
                formatter,
                "plugin source changed after review (reviewed {}, now {}); inspect it again",
                manifest::safe_text(expected),
                manifest::safe_text(actual)
            ),
            Self::Limit { what, limit } => {
                write!(formatter, "{what} exceed the bounded limit of {limit}")
            }
            Self::ConflictingSource(name) => write!(
                formatter,
                "plugin {name:?} is already installed from a different source; remove it or use a distinct package identity"
            ),
            Self::UpdateRequired(name) => write!(
                formatter,
                "plugin {name:?} changed; use the explicit plugin update workflow"
            ),
            Self::Unknown(name) => write!(formatter, "plugin {name:?} is not installed"),
            Self::Busy(path) => write!(
                formatter,
                "another Xana process is changing plugins ({})",
                manifest::safe_path(path)
            ),
        }
    }
}

impl Error for PluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Process { source, .. } => Some(source),
            Self::Encode(source) => Some(source),
            Self::State(source) => Some(source),
            Self::Skill(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
