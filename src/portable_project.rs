//! Explicit, non-secret portable project configuration under `.agents/xana/`.

use crate::{
    bounded_file,
    config::{
        ConnectionRegistry, OrchestrationLimits, OutboundDataClass, PermissionMode, ProfileConfig,
        XanaConfig,
    },
    paths::XanaPaths,
    private_state::{
        LocalBindingRecord, PrivateStateError, ProjectBindingsDocument, UpdateDocumentError,
        read_document, update_document,
    },
    project::{Project, ProjectError, ProjectStore},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

pub(crate) const PORTABLE_PROJECT_VERSION: u32 = 1;
pub(crate) const PORTABLE_PROJECT_RELATIVE: &str = ".agents/xana/project.toml";
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_DECLARATIONS: usize = 256;
const MAX_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableProjectManifest {
    pub(crate) version: u32,
    pub(crate) portable_id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) default_profile: Option<String>,
    #[serde(default)]
    pub(crate) requirements: PortableRequirements,
    #[serde(default)]
    pub(crate) profiles: BTreeMap<String, PortableProfile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableRequirements {
    #[serde(default)]
    pub(crate) connections: Vec<String>,
    #[serde(default)]
    pub(crate) service_connections: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableProfile {
    /// Exact user-global profile that supplies the authority ceiling.
    pub(crate) authority_profile: String,
    /// Logical project binding, never a credential or endpoint.
    pub(crate) connection: String,
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) identity: Option<String>,
    #[serde(default)]
    pub(crate) capabilities: Vec<String>,
    #[serde(default)]
    pub(crate) permission_mode: Option<PermissionMode>,
    #[serde(default)]
    pub(crate) max_tool_rounds: Option<usize>,
    #[serde(default)]
    pub(crate) orchestration: Option<OrchestrationLimits>,
    #[serde(default)]
    pub(crate) skills: Vec<String>,
    #[serde(default)]
    pub(crate) plugins: Vec<String>,
    #[serde(default)]
    pub(crate) mcp_servers: Vec<String>,
    #[serde(default)]
    pub(crate) external_agents: Vec<String>,
    #[serde(default)]
    pub(crate) service_routes: Vec<String>,
    #[serde(default)]
    pub(crate) egress: Vec<OutboundDataClass>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortableInspection {
    pub(crate) manifest: PortableProjectManifest,
    pub(crate) path: PathBuf,
    pub(crate) digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortableResolution {
    pub(crate) project: Project,
    pub(crate) manifest: PortableProjectManifest,
    pub(crate) bindings: BTreeMap<String, String>,
    pub(crate) missing: Vec<String>,
    pub(crate) stale: bool,
}

impl PortableResolution {
    pub(crate) fn is_ready(&self) -> bool {
        self.missing.is_empty() && !self.stale
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PortableProjectStore {
    bindings_file: PathBuf,
}

impl PortableProjectStore {
    pub(crate) fn open(paths: &XanaPaths) -> Self {
        Self {
            bindings_file: paths.project_bindings_file(),
        }
    }

    pub(crate) fn detect(workspace: &Path) -> Result<Option<PathBuf>, PortableProjectError> {
        let path = workspace.join(PORTABLE_PROJECT_RELATIVE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(PortableProjectError::EscapesWorkspace(path))
            }
            Ok(metadata) if metadata.is_file() => {
                validate_contained_manifest(workspace, &path)?;
                Ok(Some(path))
            }
            Ok(_) => Err(PortableProjectError::Invalid(format!(
                "{} is not a regular file",
                path.display()
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(PortableProjectError::Io { path, source }),
        }
    }

    pub(crate) fn inspect(workspace: &Path) -> Result<PortableInspection, PortableProjectError> {
        let path = Self::detect(workspace)?.ok_or_else(|| {
            PortableProjectError::Invalid(format!(
                "no portable Xana project exists at {}",
                workspace.join(PORTABLE_PROJECT_RELATIVE).display()
            ))
        })?;
        let before = fs::metadata(&path).map_err(|source| PortableProjectError::Io {
            path: path.clone(),
            source,
        })?;
        let bytes = bounded_file::read(&path, MAX_MANIFEST_BYTES).map_err(map_read_error)?;
        let after = fs::metadata(&path).map_err(|source| PortableProjectError::Io {
            path: path.clone(),
            source,
        })?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err(PortableProjectError::Changed(path));
        }
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            PortableProjectError::Invalid(format!("manifest is not UTF-8: {error}"))
        })?;
        let manifest: PortableProjectManifest =
            toml::from_str(text).map_err(PortableProjectError::Decode)?;
        validate_manifest(&manifest)?;
        Ok(PortableInspection {
            manifest,
            path,
            digest: blake3::hash(&bytes).to_hex().to_string(),
        })
    }

    pub(crate) fn share(project: &Project) -> Result<PortableInspection, PortableProjectError> {
        if Self::detect(&project.canonical_workspace)?.is_some() {
            return Self::inspect(&project.canonical_workspace);
        }
        let path = project.canonical_workspace.join(PORTABLE_PROJECT_RELATIVE);
        let parent = path.parent().ok_or_else(|| {
            PortableProjectError::Invalid("portable project path has no parent".into())
        })?;
        fs::create_dir_all(parent).map_err(|source| PortableProjectError::Io {
            path: parent.to_owned(),
            source,
        })?;
        let manifest = PortableProjectManifest {
            version: PORTABLE_PROJECT_VERSION,
            portable_id: uuid::Uuid::new_v4().to_string(),
            name: project.name.clone(),
            description: None,
            default_profile: None,
            requirements: PortableRequirements::default(),
            profiles: BTreeMap::new(),
        };
        let bytes = toml::to_string_pretty(&manifest)
            .map_err(|error| PortableProjectError::Invalid(error.to_string()))?
            .into_bytes();
        atomic_create(&path, &bytes)?;
        Self::inspect(&project.canonical_workspace)
    }

    pub(crate) fn register(
        &self,
        paths: &XanaPaths,
        workspace: &Path,
    ) -> Result<PortableResolution, PortableProjectError> {
        let inspection = Self::inspect_with_user_authority(workspace, paths.config_file())?;
        let projects = ProjectStore::open(paths).map_err(PortableProjectError::Project)?;
        let project = match projects
            .find_by_workspace(workspace)
            .map_err(PortableProjectError::Project)?
        {
            Some(project) => project,
            None => projects
                .create(&inspection.manifest.name, workspace)
                .map_err(PortableProjectError::Project)?,
        };
        let record = LocalBindingRecord {
            project_id: project.id,
            portable_root: project.canonical_workspace.clone(),
            manifest_digest: inspection.digest.clone(),
            bindings: BTreeMap::new(),
        };
        self.update(|document| {
            document.projects.entry(project.id).or_insert(record);
            Ok(())
        })?;
        self.resolve(paths, project.id)
    }

    pub(crate) fn refresh(
        &self,
        paths: &XanaPaths,
        project: crate::identity::ProjectId,
    ) -> Result<PortableResolution, PortableProjectError> {
        let projects = ProjectStore::open(paths).map_err(PortableProjectError::Project)?;
        let project_record = projects
            .get(project)
            .map_err(PortableProjectError::Project)?;
        let inspection = Self::inspect_with_user_authority(
            &project_record.canonical_workspace,
            paths.config_file(),
        )?;
        self.update(|document| {
            let binding = document.projects.get_mut(&project).ok_or_else(|| {
                PortableProjectError::Invalid(format!(
                    "project {project} is not registered from portable configuration"
                ))
            })?;
            binding.manifest_digest = inspection.digest;
            binding.portable_root = project_record.canonical_workspace.clone();
            Ok(())
        })?;
        self.resolve(paths, project)
    }

    pub(crate) fn bind(
        &self,
        project: crate::identity::ProjectId,
        logical: &str,
        local: &str,
    ) -> Result<(), PortableProjectError> {
        validate_stable_name("binding", logical)?;
        validate_stable_name("local reference", local)?;
        self.update(|document| {
            let binding = document.projects.get_mut(&project).ok_or_else(|| {
                PortableProjectError::Invalid(format!(
                    "project {project} is not registered from portable configuration"
                ))
            })?;
            binding
                .bindings
                .insert(logical.to_owned(), local.to_owned());
            Ok(())
        })
    }

    pub(crate) fn resolve(
        &self,
        paths: &XanaPaths,
        project: crate::identity::ProjectId,
    ) -> Result<PortableResolution, PortableProjectError> {
        let projects = ProjectStore::open(paths).map_err(PortableProjectError::Project)?;
        let project_record = projects
            .get(project)
            .map_err(PortableProjectError::Project)?;
        let inspection = Self::inspect_with_user_authority(
            &project_record.canonical_workspace,
            paths.config_file(),
        )?;
        let document: ProjectBindingsDocument =
            read_document(&self.bindings_file).map_err(PortableProjectError::State)?;
        let binding = document.projects.get(&project).ok_or_else(|| {
            PortableProjectError::Invalid(format!("project {project} has no local bindings"))
        })?;
        let registry = XanaConfig::load_registry_from(paths.config_file())
            .map_err(|error| PortableProjectError::Invalid(error.to_string()))?;
        let mut missing = BTreeSet::new();
        for logical in &inspection.manifest.requirements.connections {
            if binding
                .bindings
                .get(logical)
                .is_none_or(|local| !registry.connections.contains_key(local))
            {
                missing.insert(logical.clone());
            }
        }
        for logical in &inspection.manifest.requirements.service_connections {
            if binding
                .bindings
                .get(logical)
                .is_none_or(|local| !registry.service_connections.contains_key(local))
            {
                missing.insert(logical.clone());
            }
        }
        Ok(PortableResolution {
            project: project_record,
            manifest: inspection.manifest,
            bindings: binding.bindings.clone(),
            missing: missing.into_iter().collect(),
            stale: binding.manifest_digest != inspection.digest,
        })
    }

    pub(crate) fn stop_sharing(
        &self,
        paths: &XanaPaths,
        project: crate::identity::ProjectId,
    ) -> Result<bool, PortableProjectError> {
        let projects = ProjectStore::open(paths).map_err(PortableProjectError::Project)?;
        let project = projects
            .get(project)
            .map_err(PortableProjectError::Project)?;
        let Some(path) = Self::detect(&project.canonical_workspace)? else {
            return Ok(false);
        };
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(PortableProjectError::Io { path, source }),
        }
    }

    pub(crate) fn validate_authority(
        manifest: &PortableProjectManifest,
        registry: &ConnectionRegistry,
    ) -> Result<(), PortableProjectError> {
        for (name, profile) in &manifest.profiles {
            let ceiling = registry
                .profiles
                .get(&profile.authority_profile)
                .ok_or_else(|| PortableProjectError::Authority {
                    profile: name.clone(),
                    field: "authority_profile",
                    reason: format!("unknown user-global profile {}", profile.authority_profile),
                })?;
            validate_profile_subset(name, profile, ceiling, registry)?;
        }
        Ok(())
    }

    pub(crate) fn inspect_with_user_authority(
        workspace: &Path,
        config_path: &Path,
    ) -> Result<PortableInspection, PortableProjectError> {
        let inspection = Self::inspect(workspace)?;
        let registry = XanaConfig::load_registry_from(config_path)
            .map_err(|error| PortableProjectError::Invalid(error.to_string()))?;
        Self::validate_authority(&inspection.manifest, &registry)?;
        Ok(inspection)
    }

    fn update<T>(
        &self,
        update: impl FnOnce(&mut ProjectBindingsDocument) -> Result<T, PortableProjectError>,
    ) -> Result<T, PortableProjectError> {
        match update_document(&self.bindings_file, update) {
            Ok(output) => Ok(output),
            Err(UpdateDocumentError::State(error)) => Err(PortableProjectError::State(error)),
            Err(UpdateDocumentError::Update(error)) => Err(error),
        }
    }
}

fn validate_manifest(manifest: &PortableProjectManifest) -> Result<(), PortableProjectError> {
    if manifest.version != PORTABLE_PROJECT_VERSION {
        return Err(PortableProjectError::UnsupportedVersion(manifest.version));
    }
    uuid::Uuid::parse_str(&manifest.portable_id).map_err(|_| {
        PortableProjectError::Invalid(
            "portable_id must be a UUID and is not a local project id".into(),
        )
    })?;
    validate_text("project name", &manifest.name, 1, 128)?;
    if let Some(description) = manifest.description.as_deref() {
        validate_text("project description", description, 0, MAX_TEXT_BYTES)?;
    }
    if manifest.profiles.len() > MAX_DECLARATIONS {
        return Err(PortableProjectError::Invalid(format!(
            "portable profiles exceed the {MAX_DECLARATIONS}-entry limit"
        )));
    }
    validate_names("required connections", &manifest.requirements.connections)?;
    validate_names(
        "required service connections",
        &manifest.requirements.service_connections,
    )?;
    for (name, profile) in &manifest.profiles {
        validate_stable_name("portable profile", name)?;
        validate_stable_name("authority profile", &profile.authority_profile)?;
        validate_stable_name("logical connection", &profile.connection)?;
        validate_text("model", &profile.model, 1, 512)?;
        if let Some(identity) = profile.identity.as_deref() {
            validate_text("profile identity", identity, 0, MAX_TEXT_BYTES)?;
        }
        for (field, values) in [
            ("capabilities", &profile.capabilities),
            ("skills", &profile.skills),
            ("plugins", &profile.plugins),
            ("MCP servers", &profile.mcp_servers),
            ("external agents", &profile.external_agents),
            ("service routes", &profile.service_routes),
        ] {
            validate_names(field, values)?;
        }
        if !manifest
            .requirements
            .connections
            .contains(&profile.connection)
        {
            return Err(PortableProjectError::Invalid(format!(
                "portable profile {name} connection {} is not declared in requirements.connections",
                profile.connection
            )));
        }
    }
    if let Some(default) = manifest.default_profile.as_deref()
        && !manifest.profiles.contains_key(default)
    {
        return Err(PortableProjectError::Invalid(format!(
            "default_profile references unknown portable profile {default}"
        )));
    }
    let encoded = toml::to_string(manifest)
        .map_err(|error| PortableProjectError::Invalid(error.to_string()))?;
    reject_forbidden_portable_text(&encoded)
}

fn validate_profile_subset(
    name: &str,
    profile: &PortableProfile,
    ceiling: &ProfileConfig,
    registry: &ConnectionRegistry,
) -> Result<(), PortableProjectError> {
    if let Some(rounds) = profile.max_tool_rounds
        && rounds > ceiling.max_tool_rounds
    {
        return authority_error(name, "max_tool_rounds", "exceeds user-global ceiling");
    }
    if let Some(mode) = profile.permission_mode {
        let ceiling_mode = ceiling.permission_mode.unwrap_or(registry.permission_mode);
        if permission_rank(mode) > permission_rank(ceiling_mode) {
            return authority_error(name, "permission_mode", "broadens user-global authority");
        }
    }
    if let Some(allowed) = &ceiling.capabilities {
        ensure_subset(name, "capabilities", &profile.capabilities, allowed)?;
    }
    ensure_subset(name, "skills", &profile.skills, &ceiling.skills)?;
    ensure_subset(name, "plugins", &profile.plugins, &ceiling.plugins)?;
    ensure_subset(
        name,
        "mcp_servers",
        &profile.mcp_servers,
        &ceiling.mcp_servers,
    )?;
    ensure_subset(
        name,
        "external_agents",
        &profile.external_agents,
        &ceiling.external_agents,
    )?;
    ensure_subset(
        name,
        "service_routes",
        &profile.service_routes,
        &ceiling.service_routes,
    )?;
    if let Some(limits) = &profile.orchestration {
        let ceiling = &ceiling.orchestration;
        for (field, actual, maximum) in [
            (
                "max_fan_out",
                limits.max_fan_out as u64,
                ceiling.max_fan_out as u64,
            ),
            (
                "max_descendants",
                limits.max_descendants as u64,
                ceiling.max_descendants as u64,
            ),
            (
                "max_concurrency",
                limits.max_concurrency as u64,
                ceiling.max_concurrency as u64,
            ),
            (
                "deadline_seconds",
                limits.deadline_seconds,
                ceiling.deadline_seconds,
            ),
            (
                "max_context_tokens",
                limits.max_context_tokens as u64,
                ceiling.max_context_tokens as u64,
            ),
            (
                "max_report_bytes",
                limits.max_report_bytes as u64,
                ceiling.max_report_bytes as u64,
            ),
            (
                "max_artifact_bytes",
                limits.max_artifact_bytes as u64,
                ceiling.max_artifact_bytes as u64,
            ),
        ] {
            if actual > maximum {
                return authority_error(name, field, "exceeds user-global ceiling");
            }
        }
    }
    let allowed_egress = ceiling
        .egress_policy
        .as_ref()
        .and_then(|policy| registry.egress_policies.get(policy))
        .map(|policy| policy.allowed.as_slice())
        .unwrap_or(&[]);
    ensure_subset(name, "egress", &profile.egress, allowed_egress)
}

fn ensure_subset<T: Ord + Clone + fmt::Debug>(
    profile: &str,
    field: &'static str,
    requested: &[T],
    allowed: &[T],
) -> Result<(), PortableProjectError> {
    let allowed = allowed.iter().cloned().collect::<BTreeSet<_>>();
    if let Some(value) = requested.iter().find(|value| !allowed.contains(*value)) {
        return Err(PortableProjectError::Authority {
            profile: profile.to_owned(),
            field,
            reason: format!("value {value:?} is outside the user-global ceiling"),
        });
    }
    Ok(())
}

fn authority_error<T>(
    profile: &str,
    field: &'static str,
    reason: &str,
) -> Result<T, PortableProjectError> {
    Err(PortableProjectError::Authority {
        profile: profile.to_owned(),
        field,
        reason: reason.to_owned(),
    })
}

fn permission_rank(mode: PermissionMode) -> u8 {
    match mode {
        PermissionMode::Deny => 0,
        PermissionMode::Ask => 1,
        PermissionMode::Allow => 2,
    }
}

fn validate_names(field: &'static str, values: &[String]) -> Result<(), PortableProjectError> {
    if values.len() > MAX_DECLARATIONS {
        return Err(PortableProjectError::Invalid(format!(
            "{field} exceed the {MAX_DECLARATIONS}-entry limit"
        )));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        validate_stable_name(field, value)?;
        if !unique.insert(value) {
            return Err(PortableProjectError::Invalid(format!(
                "{field} contain duplicate {value}"
            )));
        }
    }
    Ok(())
}

fn validate_stable_name(field: &'static str, value: &str) -> Result<(), PortableProjectError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(PortableProjectError::Invalid(format!(
            "{field} must use 1..=128 ASCII letters, digits, '.', '-', or '_'"
        )));
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), PortableProjectError> {
    if value.len() < minimum || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(PortableProjectError::Invalid(format!(
            "{field} must contain {minimum}..={maximum} bytes without control characters"
        )));
    }
    Ok(())
}

fn reject_forbidden_portable_text(text: &str) -> Result<(), PortableProjectError> {
    let lowercase = text.to_ascii_lowercase();
    for forbidden in [
        "api_key",
        "apikey",
        "oauth",
        "access_token",
        "refresh_token",
        "credential",
        "session_id",
        "thread_id",
        "codex_home",
        "permission_rules",
        "base_url",
    ] {
        if lowercase.contains(forbidden) {
            return Err(PortableProjectError::Forbidden(forbidden));
        }
    }
    for secret_shape in [
        "sk-",
        "ghp_",
        "bearer ",
        "-----begin private key",
        "token=",
        "password=",
    ] {
        if lowercase.contains(secret_shape) {
            return Err(PortableProjectError::Forbidden("secret-shaped value"));
        }
    }
    if text.split_whitespace().any(|token| {
        token.starts_with('/')
            || token.starts_with("\\\\")
            || (token.len() >= 3
                && token.as_bytes()[1] == b':'
                && matches!(token.as_bytes()[2], b'\\' | b'/'))
    }) {
        return Err(PortableProjectError::Forbidden("absolute path"));
    }
    Ok(())
}

fn validate_contained_manifest(workspace: &Path, path: &Path) -> Result<(), PortableProjectError> {
    let canonical_workspace =
        workspace
            .canonicalize()
            .map_err(|source| PortableProjectError::Io {
                path: workspace.to_owned(),
                source,
            })?;
    let canonical = path
        .canonicalize()
        .map_err(|source| PortableProjectError::Io {
            path: path.to_owned(),
            source,
        })?;
    if !canonical.starts_with(&canonical_workspace) {
        return Err(PortableProjectError::EscapesWorkspace(path.to_owned()));
    }
    Ok(())
}

fn atomic_create(path: &Path, bytes: &[u8]) -> Result<(), PortableProjectError> {
    let parent = path.parent().ok_or_else(|| {
        PortableProjectError::Invalid(format!("{} has no parent", path.display()))
    })?;
    let temporary = parent.join(format!(".project.toml.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| PortableProjectError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(bytes)
        .map_err(|source| PortableProjectError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| PortableProjectError::Io {
        path: temporary.clone(),
        source,
    })?;
    drop(file);
    let linked = fs::hard_link(&temporary, path).map_err(|source| PortableProjectError::Io {
        path: path.to_owned(),
        source,
    });
    let _ = fs::remove_file(&temporary);
    linked
}

fn map_read_error(error: bounded_file::BoundedReadError) -> PortableProjectError {
    match error {
        bounded_file::BoundedReadError::TooLarge {
            path,
            actual,
            limit,
        } => PortableProjectError::Invalid(format!(
            "{} contains {actual} bytes, exceeding the {limit}-byte limit",
            path.display()
        )),
        bounded_file::BoundedReadError::Io { path, source } => {
            PortableProjectError::Io { path, source }
        }
    }
}

#[derive(Debug)]
pub(crate) enum PortableProjectError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Decode(toml::de::Error),
    UnsupportedVersion(u32),
    Changed(PathBuf),
    EscapesWorkspace(PathBuf),
    Forbidden(&'static str),
    Authority {
        profile: String,
        field: &'static str,
        reason: String,
    },
    State(PrivateStateError),
    Project(ProjectError),
    Invalid(String),
}

impl fmt::Display for PortableProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Decode(error) => write!(formatter, "invalid portable project TOML: {error}"),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported portable project version {version}; expected {PORTABLE_PROJECT_VERSION}"
            ),
            Self::Changed(path) => write!(
                formatter,
                "{} changed during inspection; retry the read-only review",
                path.display()
            ),
            Self::EscapesWorkspace(path) => write!(
                formatter,
                "portable project manifest escapes its workspace through {}",
                path.display()
            ),
            Self::Forbidden(field) => {
                write!(
                    formatter,
                    "portable project contains forbidden private field {field}"
                )
            }
            Self::Authority {
                profile,
                field,
                reason,
            } => {
                write!(
                    formatter,
                    "portable profile {profile} field {field}: {reason}"
                )
            }
            Self::State(error) => error.fmt(formatter),
            Self::Project(error) => error.fmt(formatter),
            Self::Invalid(reason) => formatter.write_str(reason),
        }
    }
}

impl Error for PortableProjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Decode(error) => Some(error),
            Self::State(error) => Some(error),
            Self::Project(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection},
        shell::ShellConfig,
    };
    use std::ffi::OsString;
    use tempfile::tempdir;

    fn paths() -> (tempfile::TempDir, XanaPaths) {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        let config = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".into(),
                base_url: "http://localhost:11434/v1".into(),
            },
            model: "qwen".into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        fs::write(paths.config_file(), config).unwrap();
        crate::private_state::ensure_interoperable_records(&paths).unwrap();
        (directory, paths)
    }

    fn shared_project(paths: &XanaPaths, workspace: &Path) -> (Project, PortableInspection) {
        fs::create_dir_all(workspace).unwrap();
        let project = ProjectStore::open(paths)
            .unwrap()
            .create("Shared", workspace)
            .unwrap();
        let inspection = PortableProjectStore::share(&project).unwrap();
        (project, inspection)
    }

    #[test]
    fn sharing_is_explicit_minimal_idempotent_and_comment_preserving() {
        let (directory, paths) = paths();
        let workspace = directory.path().join("workspace");
        let (project, first) = shared_project(&paths, &workspace);
        let text = fs::read_to_string(&first.path).unwrap();
        assert!(text.contains("version = 1"));
        for private in ["credential", "api_key", "session_id", "thread_id"] {
            assert!(!text.to_ascii_lowercase().contains(private));
        }
        fs::write(&first.path, format!("{text}\n# keep this comment\n")).unwrap();

        let second = PortableProjectStore::share(&project).unwrap();
        assert!(
            fs::read_to_string(second.path)
                .unwrap()
                .contains("# keep this comment")
        );
    }

    #[test]
    fn clean_home_registration_is_durable_but_missing_bindings_are_not_ready() {
        let (directory, paths) = paths();
        let workspace = directory.path().join("workspace");
        let (_, inspection) = shared_project(&paths, &workspace);
        let mut document: PortableProjectManifest =
            toml::from_str(&fs::read_to_string(&inspection.path).unwrap()).unwrap();
        document.requirements.connections.push("chat".into());
        fs::write(&inspection.path, toml::to_string_pretty(&document).unwrap()).unwrap();

        let portable = PortableProjectStore::open(&paths);
        let resolution = portable.register(&paths, &workspace).unwrap();
        assert_eq!(resolution.missing, ["chat"]);
        assert!(!resolution.is_ready());
        portable
            .bind(resolution.project.id, "chat", "local")
            .unwrap();
        let ready = portable.resolve(&paths, resolution.project.id).unwrap();
        assert!(ready.is_ready());
        assert_eq!(ready.bindings.len(), 1);
    }

    #[test]
    fn manifest_change_requires_explicit_refresh_and_preserves_private_binding() {
        let (directory, paths) = paths();
        let workspace = directory.path().join("workspace");
        let (_, _) = shared_project(&paths, &workspace);
        let portable = PortableProjectStore::open(&paths);
        let registered = portable.register(&paths, &workspace).unwrap();
        let path = workspace.join(PORTABLE_PROJECT_RELATIVE);
        let before = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{before}\n# reviewed change\n")).unwrap();

        assert!(
            portable
                .resolve(&paths, registered.project.id)
                .unwrap()
                .stale
        );
        assert!(
            !portable
                .refresh(&paths, registered.project.id)
                .unwrap()
                .stale
        );
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("# reviewed change")
        );
    }

    #[test]
    fn project_profiles_cannot_broaden_user_authority() {
        let registry = XanaConfig::parse_registry(
            r#"
version = 4
default_profile = "limited"
permission_mode = "ask"

[providers.local]
kind = "ollama"

[profiles.limited]
connection = "local"
model = "qwen"
permission_mode = "deny"
max_tool_rounds = 2
capabilities = ["fs.read"]
skills = ["review"]
plugins = []
mcp_servers = []
external_agents = []
service_routes = []
"#,
        )
        .unwrap();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "project".into(),
            PortableProfile {
                authority_profile: "limited".into(),
                connection: "chat".into(),
                model: "qwen".into(),
                identity: None,
                capabilities: vec!["fs.write".into()],
                permission_mode: Some(PermissionMode::Ask),
                max_tool_rounds: Some(3),
                orchestration: None,
                skills: vec!["review".into()],
                plugins: Vec::new(),
                mcp_servers: Vec::new(),
                external_agents: Vec::new(),
                service_routes: Vec::new(),
                egress: Vec::new(),
            },
        );
        let manifest = PortableProjectManifest {
            version: 1,
            portable_id: uuid::Uuid::new_v4().to_string(),
            name: "Project".into(),
            description: None,
            default_profile: Some("project".into()),
            requirements: PortableRequirements {
                connections: vec!["chat".into()],
                service_connections: Vec::new(),
            },
            profiles,
        };

        assert!(matches!(
            PortableProjectStore::validate_authority(&manifest, &registry),
            Err(PortableProjectError::Authority {
                field: "max_tool_rounds",
                ..
            })
        ));
    }

    #[test]
    fn malformed_future_oversized_and_forbidden_manifests_fail_closed() {
        let (directory, paths) = paths();
        let workspace = directory.path().join("workspace");
        let (_, inspection) = shared_project(&paths, &workspace);
        let binding_before = fs::read(paths.project_bindings_file()).unwrap();

        fs::write(
            &inspection.path,
            "version = 99\nportable_id = \"x\"\nname = \"x\"\n",
        )
        .unwrap();
        assert!(matches!(
            PortableProjectStore::inspect(&workspace),
            Err(PortableProjectError::UnsupportedVersion(99))
        ));
        fs::write(&inspection.path, vec![b'x'; MAX_MANIFEST_BYTES + 1]).unwrap();
        assert!(PortableProjectStore::inspect(&workspace).is_err());
        fs::write(
            &inspection.path,
            format!(
                "version = 1\nportable_id = \"{}\"\nname = \"x\"\napi_key = \"secret\"\n",
                uuid::Uuid::new_v4()
            ),
        )
        .unwrap();
        assert!(PortableProjectStore::inspect(&workspace).is_err());
        fs::write(
            &inspection.path,
            format!(
                "version = 1\nportable_id = \"{}\"\nname = \"x\"\ndescription = \"sk-secret\"\n",
                uuid::Uuid::new_v4()
            ),
        )
        .unwrap();
        assert!(matches!(
            PortableProjectStore::inspect(&workspace),
            Err(PortableProjectError::Forbidden("secret-shaped value"))
        ));
        assert_eq!(
            fs::read(paths.project_bindings_file()).unwrap(),
            binding_before
        );
    }

    #[test]
    fn stop_sharing_removes_only_manifest_and_keeps_private_project() {
        let (directory, paths) = paths();
        let workspace = directory.path().join("workspace");
        let (project, _) = shared_project(&paths, &workspace);
        let portable = PortableProjectStore::open(&paths);
        portable.register(&paths, &workspace).unwrap();

        assert!(portable.stop_sharing(&paths, project.id).unwrap());
        assert!(!workspace.join(PORTABLE_PROJECT_RELATIVE).exists());
        assert!(ProjectStore::open(&paths).unwrap().get(project.id).is_ok());
        assert!(
            read_document::<ProjectBindingsDocument>(&paths.project_bindings_file())
                .unwrap()
                .projects
                .contains_key(&project.id)
        );
    }

    #[test]
    fn cloned_manifest_gets_independent_local_project_and_bindings() {
        let (directory, paths) = paths();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let (_, inspection) = shared_project(&paths, &first);
        fs::create_dir_all(second.join(".agents/xana")).unwrap();
        let portable_bytes = fs::read(&inspection.path).unwrap();
        fs::write(second.join(PORTABLE_PROJECT_RELATIVE), &portable_bytes).unwrap();
        let portable = PortableProjectStore::open(&paths);

        let first_resolution = portable.register(&paths, &first).unwrap();
        let second_resolution = portable.register(&paths, &second).unwrap();
        assert_ne!(first_resolution.project.id, second_resolution.project.id);
        assert_eq!(
            first_resolution.manifest.portable_id,
            second_resolution.manifest.portable_id
        );
        assert_eq!(
            fs::read(second.join(PORTABLE_PROJECT_RELATIVE)).unwrap(),
            portable_bytes
        );
        let bindings: ProjectBindingsDocument =
            read_document(&paths.project_bindings_file()).unwrap();
        assert_eq!(bindings.projects.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_outside_workspace_is_rejected() {
        use std::os::unix::fs::symlink;

        let (directory, _) = paths();
        let workspace = directory.path().join("workspace");
        fs::create_dir_all(workspace.join(".agents/xana")).unwrap();
        let outside = directory.path().join("outside.toml");
        fs::write(&outside, "untrusted").unwrap();
        symlink(&outside, workspace.join(PORTABLE_PROJECT_RELATIVE)).unwrap();

        assert!(matches!(
            PortableProjectStore::detect(&workspace),
            Err(PortableProjectError::EscapesWorkspace(_))
        ));
    }
}
