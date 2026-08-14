//! Declarative Agent Plugin acquisition, validation, and lifecycle state.
//!
//! Plugin bundles never load code into Xana. Installation validates inert
//! manifests, skills, and MCP declarations, then records an exact immutable
//! tree (or an explicitly mutable development link) without enabling it.

mod manifest;
mod source;

use crate::{
    identity::ProjectId,
    paths::XanaPaths,
    private_state::{
        InstalledPackageRecord, PackageRevisionRecord, PackageSourceKind, PackageSourceRecord,
        PackageStateDocument, PrivateStateError, UpdateDocumentError, ensure_interoperable_records,
        read_document, update_document,
    },
    skill::SkillSource,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
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

    fn from_record(record: &PackageSourceRecord, revision: Option<String>) -> Self {
        match record.kind {
            PackageSourceKind::Directory => Self::Directory(PathBuf::from(&record.location)),
            PackageSourceKind::Linked => Self::Linked(PathBuf::from(&record.location)),
            PackageSourceKind::Git => Self::Git {
                url: record.location.clone(),
                revision: revision
                    .or_else(|| record.revision.clone())
                    .unwrap_or_default(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PluginScope {
    User,
    Project(ProjectId),
    Profile {
        project: Option<ProjectId>,
        profile: String,
    },
}

impl PluginScope {
    pub(crate) fn key(&self) -> Result<String, PluginError> {
        match self {
            Self::User => Ok("user".to_owned()),
            Self::Project(project) => Ok(format!("project:{project}")),
            Self::Profile { project, profile } => {
                validate_scope_name(profile)?;
                Ok(match project {
                    Some(project) => format!("profile:project:{project}:{profile}"),
                    None => format!("profile:global:{profile}"),
                })
            }
        }
    }

    pub(crate) fn candidates(&self) -> Result<Vec<String>, PluginError> {
        let mut candidates = vec!["user".to_owned()];
        match self {
            Self::User => {}
            Self::Project(project) => candidates.push(format!("project:{project}")),
            Self::Profile { project, profile } => {
                if let Some(project) = project {
                    candidates.push(format!("project:{project}"));
                }
                candidates.push(self.key()?);
                let _ = profile;
            }
        }
        Ok(candidates)
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
    /// Digest of the complete bounded MCP declaration, including values that
    /// must affect reapproval but must never be printed or persisted verbatim.
    #[serde(skip)]
    pub(crate) mcp_configuration_digest: Option<String>,
}

impl PluginReview {
    fn capability_digest(&self) -> Result<String, PluginError> {
        #[derive(Serialize)]
        struct Capabilities<'a> {
            skills: &'a [String],
            mcp_servers: &'a [McpServerReview],
            mcp_configuration_digest: &'a Option<String>,
        }
        let bytes = serde_json::to_vec(&Capabilities {
            skills: &self.skills,
            mcp_servers: &self.mcp_servers,
            mcp_configuration_digest: &self.mcp_configuration_digest,
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
    pub(crate) health: String,
    pub(crate) pending_update: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PluginUpdateReview {
    pub(crate) name: String,
    pub(crate) current_revision: String,
    pub(crate) candidate_revision: String,
    pub(crate) changed: bool,
    pub(crate) added_skills: Vec<String>,
    pub(crate) removed_skills: Vec<String>,
    pub(crate) added_mcp_servers: Vec<String>,
    pub(crate) removed_mcp_servers: Vec<String>,
    pub(crate) requires_reapproval: bool,
    pub(crate) mutable: bool,
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
            approved_scopes: Vec::new(),
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
                            pending_revision: None,
                            previous_revision: None,
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
            .iter()
            .map(|(name, package)| {
                let mut installed = installed_from_document(&self.store, &document, name)?;
                installed.health = match self.validate_revision(package, &package.active_revision) {
                    Ok(()) if package.enabled_scopes.is_empty() => "disabled".to_owned(),
                    Ok(()) => "ready".to_owned(),
                    Err(error) => format!("degraded: {error}"),
                };
                Ok(installed)
            })
            .collect()
    }

    pub(crate) fn inspect_installed(&self, name: &str) -> Result<InstalledPlugin, PluginError> {
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        let mut installed = installed_from_document(&self.store, &document, name)?;
        installed.health = self.revision_health(name, &installed.active_revision)?;
        Ok(installed)
    }

    pub(crate) fn enable(
        &self,
        name: &str,
        scope: &PluginScope,
    ) -> Result<InstalledPlugin, PluginError> {
        let scope = scope.key()?;
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        let package = document
            .plugins
            .get(name)
            .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
        self.validate_revision(package, &package.active_revision)?;
        update_document::<PackageStateDocument, _, PluginError>(&self.state_file, |document| {
            let package = document
                .plugins
                .get_mut(name)
                .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
            insert_sorted(&mut package.enabled_scopes, &scope);
            let revision = package
                .revisions
                .get_mut(&package.active_revision)
                .ok_or_else(|| {
                    PluginError::Invalid(format!("plugin {name:?} has no active revision"))
                })?;
            insert_sorted(&mut revision.approved_scopes, &scope);
            Ok(())
        })
        .map_err(map_state_update)?;
        self.inspect_installed(name)
    }

    pub(crate) fn disable(
        &self,
        name: &str,
        scope: &PluginScope,
    ) -> Result<InstalledPlugin, PluginError> {
        let scope = scope.key()?;
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        update_document::<PackageStateDocument, _, PluginError>(&self.state_file, |document| {
            let package = document
                .plugins
                .get_mut(name)
                .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
            package.enabled_scopes.retain(|item| item != &scope);
            if let Some(revision) = package.revisions.get_mut(&package.active_revision) {
                revision.approved_scopes.retain(|item| item != &scope);
            }
            Ok(())
        })
        .map_err(map_state_update)?;
        self.inspect_installed(name)
    }

    pub(crate) fn check_update(
        &self,
        name: &str,
        revision: Option<String>,
    ) -> Result<PluginUpdateReview, PluginError> {
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        let package = document
            .plugins
            .get(name)
            .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
        let source_record = package.source.as_ref().ok_or_else(|| {
            PluginError::Invalid(format!("plugin {name:?} has incomplete source metadata"))
        })?;
        if revision.is_some() && source_record.kind != PackageSourceKind::Git {
            return Err(PluginError::Invalid(
                "--revision is valid only for a Git-installed plugin".to_owned(),
            ));
        }
        let source = PackageSource::from_record(source_record, revision);
        let candidate = self.inspect_source(&source)?;
        if candidate.manifest.name != name {
            return Err(PluginError::Invalid(format!(
                "update source declares plugin {:?}, expected {name:?}",
                manifest::safe_text(&candidate.manifest.name)
            )));
        }
        let current = package
            .revisions
            .get(&package.active_revision)
            .ok_or_else(|| {
                PluginError::Invalid(format!("plugin {name:?} has no active revision"))
            })?;
        let candidate_capability = candidate.capability_digest()?;
        let review = update_review(name, package, current, &candidate, &candidate_capability);
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        update_document::<PackageStateDocument, _, PluginError>(&self.state_file, |document| {
            let package = document
                .plugins
                .get_mut(name)
                .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
            if package.active_revision != review.current_revision {
                return Err(PluginError::Changed(self.state_file.clone()));
            }
            package.pending_revision = review.changed.then(|| review.candidate_revision.clone());
            Ok(())
        })
        .map_err(map_state_update)?;
        Ok(review)
    }

    pub(crate) fn update(
        &self,
        name: &str,
        revision: Option<String>,
        expected_digest: &str,
    ) -> Result<InstalledPlugin, PluginError> {
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        let package = document
            .plugins
            .get(name)
            .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
        let old_active = package.active_revision.clone();
        let source_record = package.source.as_ref().ok_or_else(|| {
            PluginError::Invalid(format!("plugin {name:?} has incomplete source metadata"))
        })?;
        let source = PackageSource::from_record(source_record, revision);
        let acquired = source::acquire(&source, &self.store, !source.mutable())?;
        let digest = source::tree_digest(&acquired.root)?;
        let review = manifest::inspect(&acquired.root, source.mutable(), digest.clone())?;
        if review.manifest.name != name {
            return Err(PluginError::Invalid(format!(
                "update source declares plugin {:?}, expected {name:?}",
                manifest::safe_text(&review.manifest.name)
            )));
        }
        if digest != expected_digest {
            return Err(PluginError::ReviewChanged {
                expected: expected_digest.to_owned(),
                actual: digest,
            });
        }
        let capability_digest = review.capability_digest()?;
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        let current_package = document
            .plugins
            .get(name)
            .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
        if current_package.active_revision != old_active
            || current_package.pending_revision.as_deref() != Some(expected_digest)
        {
            return Err(PluginError::Invalid(
                "plugin update review is stale; run update-check again".to_owned(),
            ));
        }
        let current_capability = current_package
            .revisions
            .get(&old_active)
            .map(|item| item.capability_digest.as_str())
            .ok_or_else(|| {
                PluginError::Invalid(format!("plugin {name:?} has no active revision"))
            })?;
        let inherited_scopes = if current_capability == capability_digest {
            current_package.enabled_scopes.clone()
        } else {
            Vec::new()
        };
        let source_identity = acquired.identity;
        let relative_root = PathBuf::from("versions")
            .join(name)
            .join(expected_digest)
            .join("bundle");
        let final_root = if source.mutable() {
            acquired.root
        } else {
            let destination = self.store.join(&relative_root);
            acquired
                .staging
                .ok_or_else(|| {
                    PluginError::Invalid("immutable update has no staging tree".to_owned())
                })?
                .commit_bundle(&destination)?;
            if source::tree_digest(&destination)? != expected_digest {
                return Err(PluginError::Changed(destination));
            }
            destination
        };
        let new_revision = PackageRevisionRecord {
            digest: expected_digest.to_owned(),
            manifest_version: review.manifest.version,
            installed_unix_ms: unix_millis()?,
            mutable: source.mutable(),
            managed_root: (!source.mutable()).then_some(relative_root),
            linked_root: source.mutable().then_some(final_root),
            skill_names: review.skills,
            mcp_server_names: review
                .mcp_servers
                .into_iter()
                .map(|item| item.name)
                .collect(),
            capability_digest,
            approved_scopes: inherited_scopes.clone(),
        };
        let new_source_digest = source_identity.digest()?;
        let new_source = source_identity.record();
        update_document::<PackageStateDocument, _, PluginError>(&self.state_file, |document| {
            let package = document
                .plugins
                .get_mut(name)
                .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
            if package.active_revision != old_active
                || package.pending_revision.as_deref() != Some(expected_digest)
            {
                return Err(PluginError::Invalid(
                    "plugin update review changed before commit".to_owned(),
                ));
            }
            if source.mutable() {
                package.revisions.clear();
                package.retained_revisions.clear();
                package.previous_revision = None;
            } else {
                package.previous_revision = Some(old_active.clone());
            }
            package
                .revisions
                .insert(expected_digest.to_owned(), new_revision.clone());
            insert_sorted(&mut package.retained_revisions, expected_digest);
            package.active_revision = expected_digest.to_owned();
            package.source_digest = new_source_digest.clone();
            package.source = Some(new_source.clone());
            package.enabled_scopes = inherited_scopes.clone();
            package.pending_revision = None;
            Ok(())
        })
        .map_err(map_state_update)?;
        self.inspect_installed(name)
    }

    pub(crate) fn rollback(&self, name: &str) -> Result<InstalledPlugin, PluginError> {
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        let package = document
            .plugins
            .get(name)
            .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
        let target = package.previous_revision.clone().ok_or_else(|| {
            PluginError::Invalid(format!("plugin {name:?} has no rollback revision"))
        })?;
        let target_record = package.revisions.get(&target).ok_or_else(|| {
            PluginError::Invalid(format!("plugin {name:?} rollback revision is missing"))
        })?;
        if target_record.mutable {
            return Err(PluginError::Invalid(
                "linked development plugins cannot claim an immutable rollback".to_owned(),
            ));
        }
        self.validate_revision(package, &target)?;
        let current = package.active_revision.clone();
        let approved = target_record.approved_scopes.clone();
        update_document::<PackageStateDocument, _, PluginError>(&self.state_file, |document| {
            let package = document
                .plugins
                .get_mut(name)
                .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
            if package.active_revision != current
                || package.previous_revision.as_deref() != Some(target.as_str())
            {
                return Err(PluginError::Invalid(
                    "plugin rollback state changed before commit".to_owned(),
                ));
            }
            package.active_revision = target.clone();
            package.previous_revision = Some(current.clone());
            package.enabled_scopes = approved.clone();
            package.pending_revision = None;
            Ok(())
        })
        .map_err(map_state_update)?;
        self.inspect_installed(name)
    }

    pub(crate) fn remove(&self, name: &str) -> Result<bool, PluginError> {
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        let mut managed_roots = Vec::new();
        let removed =
            update_document::<PackageStateDocument, _, PluginError>(&self.state_file, |document| {
                let Some(package) = document.plugins.get(name) else {
                    return Ok(false);
                };
                if !package.enabled_scopes.is_empty() {
                    return Err(PluginError::Enabled {
                        name: name.to_owned(),
                        scopes: package.enabled_scopes.clone(),
                    });
                }
                managed_roots.extend(
                    package
                        .revisions
                        .values()
                        .filter_map(|revision| revision.managed_root.clone()),
                );
                document.plugins.remove(name);
                Ok(true)
            })
            .map_err(map_state_update)?;
        for root in managed_roots {
            remove_managed_revision(&self.store, &root)?;
        }
        Ok(removed)
    }

    pub(crate) fn garbage_collect(&self) -> Result<usize, PluginError> {
        let _lock = PluginLifecycleLock::acquire(&self.store)?;
        let document = match read_document::<PackageStateDocument>(&self.state_file) {
            Ok(document) => document,
            Err(PrivateStateError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(0);
            }
            Err(error) => return Err(PluginError::State(error)),
        };
        let referenced = document
            .plugins
            .values()
            .flat_map(|package| package.revisions.values())
            .filter_map(|revision| revision.managed_root.as_ref())
            .map(|root| self.store.join(root))
            .collect::<BTreeSet<_>>();
        collect_orphan_versions(&self.store, &referenced)
    }

    pub(crate) fn resolve_profile_plugins(
        &self,
        names: &[String],
        scope: &PluginScope,
    ) -> Result<(BTreeMap<String, String>, Vec<String>), PluginError> {
        let document = match read_document::<PackageStateDocument>(&self.state_file) {
            Ok(document) => document,
            Err(PrivateStateError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok((
                    BTreeMap::new(),
                    names
                        .iter()
                        .map(|name| format!("install plugin {name:?}"))
                        .collect(),
                ));
            }
            Err(error) => return Err(PluginError::State(error)),
        };
        let candidates = scope.candidates()?;
        let mut resolved = BTreeMap::new();
        let mut readiness = Vec::new();
        for name in names {
            let Some(package) = document.plugins.get(name) else {
                readiness.push(format!("install plugin {name:?}"));
                continue;
            };
            if !package
                .enabled_scopes
                .iter()
                .any(|scope| candidates.contains(scope))
            {
                readiness.push(format!("enable plugin {name:?} for {}", scope.key()?));
                continue;
            }
            match self.validate_revision(package, &package.active_revision) {
                Ok(()) => {
                    resolved.insert(name.clone(), package.active_revision.clone());
                }
                Err(error) => readiness.push(format!("plugin {name:?} is unavailable: {error}")),
            }
        }
        Ok((resolved, readiness))
    }

    pub(crate) fn skill_sources_for_revisions(
        &self,
        revisions: &BTreeMap<String, String>,
    ) -> Result<Vec<SkillSource>, PluginError> {
        if revisions.is_empty() {
            return Ok(Vec::new());
        }
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        revisions
            .iter()
            .map(|(name, revision)| {
                let package = document
                    .plugins
                    .get(name)
                    .ok_or_else(|| PluginError::Unknown(name.clone()))?;
                self.validate_revision(package, revision)?;
                let record = package.revisions.get(revision).ok_or_else(|| {
                    PluginError::Invalid(format!("plugin {name:?} revision {revision} is missing"))
                })?;
                SkillSource::plugin(
                    name,
                    revision_root(&self.store, record)?.join("skills"),
                    record.mutable,
                )
                .map_err(PluginError::Skill)
            })
            .collect()
    }

    fn revision_health(&self, name: &str, revision: &str) -> Result<String, PluginError> {
        let document =
            read_document::<PackageStateDocument>(&self.state_file).map_err(PluginError::State)?;
        let package = document
            .plugins
            .get(name)
            .ok_or_else(|| PluginError::Unknown(name.to_owned()))?;
        Ok(match self.validate_revision(package, revision) {
            Ok(()) if package.enabled_scopes.is_empty() => "disabled".to_owned(),
            Ok(()) => "ready".to_owned(),
            Err(error) => format!("degraded: {error}"),
        })
    }

    fn validate_revision(
        &self,
        package: &InstalledPackageRecord,
        revision: &str,
    ) -> Result<(), PluginError> {
        let record = package.revisions.get(revision).ok_or_else(|| {
            PluginError::Invalid(format!(
                "plugin {:?} revision {revision} is missing",
                package.package
            ))
        })?;
        let root = revision_root(&self.store, record)?;
        let digest = source::tree_digest(&root)?;
        if digest != revision || digest != record.digest {
            return Err(PluginError::Drifted(package.package.clone()));
        }
        let review = manifest::inspect(&root, record.mutable, digest)?;
        if review.manifest.name != package.package
            || review.capability_digest()? != record.capability_digest
        {
            return Err(PluginError::Drifted(package.package.clone()));
        }
        Ok(())
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
        rollback_available: package.previous_revision.is_some(),
        health: "unchecked".to_owned(),
        pending_update: package.pending_revision.clone(),
    })
}

fn update_review(
    name: &str,
    package: &InstalledPackageRecord,
    current: &PackageRevisionRecord,
    candidate: &PluginReview,
    candidate_capability: &str,
) -> PluginUpdateReview {
    let current_skills = current.skill_names.iter().cloned().collect::<BTreeSet<_>>();
    let candidate_skills = candidate.skills.iter().cloned().collect::<BTreeSet<_>>();
    let current_mcp = current
        .mcp_server_names
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let candidate_mcp = candidate
        .mcp_servers
        .iter()
        .map(|item| item.name.clone())
        .collect::<BTreeSet<_>>();
    PluginUpdateReview {
        name: name.to_owned(),
        current_revision: package.active_revision.clone(),
        candidate_revision: candidate.digest.clone(),
        changed: candidate.digest != package.active_revision,
        added_skills: candidate_skills
            .difference(&current_skills)
            .cloned()
            .collect(),
        removed_skills: current_skills
            .difference(&candidate_skills)
            .cloned()
            .collect(),
        added_mcp_servers: candidate_mcp.difference(&current_mcp).cloned().collect(),
        removed_mcp_servers: current_mcp.difference(&candidate_mcp).cloned().collect(),
        requires_reapproval: candidate_capability != current.capability_digest,
        mutable: candidate.mutable,
    }
}

fn revision_root(store: &Path, revision: &PackageRevisionRecord) -> Result<PathBuf, PluginError> {
    revision
        .managed_root
        .as_ref()
        .map(|path| store.join(path))
        .or_else(|| revision.linked_root.clone())
        .ok_or_else(|| PluginError::Invalid("plugin revision has no package root".to_owned()))
}

fn insert_sorted(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|item| item == value) {
        values.push(value.to_owned());
        values.sort();
    }
}

fn validate_scope_name(value: &str) -> Result<(), PluginError> {
    if value.is_empty()
        || value.len() > 128
        || value.contains(':')
        || value.chars().any(char::is_control)
    {
        Err(PluginError::Invalid(format!(
            "plugin scope name {:?} is invalid",
            manifest::safe_text(value)
        )))
    } else {
        Ok(())
    }
}

fn remove_managed_revision(store: &Path, relative_root: &Path) -> Result<(), PluginError> {
    let versions = store.join("versions");
    let bundle = store.join(relative_root);
    if !bundle.starts_with(&versions)
        || bundle.file_name().and_then(|name| name.to_str()) != Some("bundle")
    {
        return Err(PluginError::UnsafePath(bundle));
    }
    let Some(revision_root) = bundle.parent() else {
        return Err(PluginError::UnsafePath(bundle));
    };
    match fs::symlink_metadata(revision_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PluginError::Io {
            path: revision_root.to_owned(),
            source,
        }),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(revision_root).map_err(|source| PluginError::Io {
                path: revision_root.to_owned(),
                source,
            })
        }
        Ok(_) => Err(PluginError::UnsafePath(revision_root.to_owned())),
    }
}

fn collect_orphan_versions(
    store: &Path,
    referenced: &BTreeSet<PathBuf>,
) -> Result<usize, PluginError> {
    let versions = store.join("versions");
    let plugins = match fs::read_dir(&versions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(PluginError::Io {
                path: versions,
                source,
            });
        }
    };
    let mut removed = 0_usize;
    for plugin in plugins {
        let plugin = plugin.map_err(|source| PluginError::Io {
            path: versions.clone(),
            source,
        })?;
        let plugin_path = plugin.path();
        let metadata = fs::symlink_metadata(&plugin_path).map_err(|source| PluginError::Io {
            path: plugin_path.clone(),
            source,
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        for revision in fs::read_dir(&plugin_path).map_err(|source| PluginError::Io {
            path: plugin_path.clone(),
            source,
        })? {
            let revision = revision.map_err(|source| PluginError::Io {
                path: plugin_path.clone(),
                source,
            })?;
            let revision_path = revision.path();
            let name = revision.file_name();
            let name = name.to_str().unwrap_or_default();
            let metadata =
                fs::symlink_metadata(&revision_path).map_err(|source| PluginError::Io {
                    path: revision_path.clone(),
                    source,
                })?;
            if name.len() != 64
                || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !metadata.is_dir()
                || metadata.file_type().is_symlink()
            {
                continue;
            }
            if referenced.contains(&revision_path.join("bundle")) {
                continue;
            }
            fs::remove_dir_all(&revision_path).map_err(|source| PluginError::Io {
                path: revision_path,
                source,
            })?;
            removed += 1;
        }
    }
    Ok(removed)
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
    Enabled {
        name: String,
        scopes: Vec<String>,
    },
    Drifted(String),
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
            Self::Enabled { name, scopes } => write!(
                formatter,
                "plugin {name:?} is still enabled for {}; disable it before removal",
                scopes.join(", ")
            ),
            Self::Drifted(name) => write!(
                formatter,
                "plugin {name:?} no longer matches its reviewed package; review and update it before use"
            ),
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
