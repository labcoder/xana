//! Pure policy for Xana-owned filesystem locations.
//!
//! Resolution maps an optional explicit override or platform directories into
//! paths without creating or canonicalizing anything.

use directories::ProjectDirs;
use std::{
    error::Error,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};

const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct XanaPaths {
    config_file: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    runtime_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum XanaPathError {
    PlatformDirectoriesUnavailable,
    RelativeHomeOverride { path: PathBuf },
}

impl fmt::Display for XanaPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlatformDirectoriesUnavailable => {
                write!(f, "could not determine Xana's platform directories")
            }
            Self::RelativeHomeOverride { path } => write!(
                f,
                "XANA_HOME must be an absolute path when set, got {}",
                path.display()
            ),
        }
    }
}

impl Error for XanaPathError {}

impl XanaPaths {
    pub(crate) fn resolve(xana_home: Option<OsString>) -> Result<Self, XanaPathError> {
        match xana_home {
            Some(root) if !root.is_empty() => Self::from_override(PathBuf::from(root)),
            Some(_) | None => Self::from_platform_defaults(),
        }
    }

    fn from_override(root: PathBuf) -> Result<Self, XanaPathError> {
        if !root.is_absolute() {
            return Err(XanaPathError::RelativeHomeOverride { path: root });
        }

        Ok(Self {
            config_file: root.join(CONFIG_FILE_NAME),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            runtime_dir: root.join("run"),
        })
    }

    fn from_platform_defaults() -> Result<Self, XanaPathError> {
        let dirs = ProjectDirs::from("io.github", "labcoder", "xana")
            .ok_or(XanaPathError::PlatformDirectoriesUnavailable)?;
        Ok(Self::from_project_dirs(&dirs))
    }

    fn from_project_dirs(dirs: &ProjectDirs) -> Self {
        Self {
            config_file: dirs.config_dir().join(CONFIG_FILE_NAME),
            data_dir: dirs.data_dir().to_owned(),
            cache_dir: dirs.cache_dir().to_owned(),
            runtime_dir: choose_runtime_dir(dirs.runtime_dir(), dirs.cache_dir()),
        }
    }

    pub(crate) fn config_file(&self) -> &Path {
        &self.config_file
    }

    #[allow(dead_code)]
    pub(crate) fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    #[allow(dead_code)]
    pub(crate) fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    #[allow(dead_code)]
    pub(crate) fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub(crate) fn presentation_file(&self) -> PathBuf {
        self.data_dir.join("frontend").join("presentation.toml")
    }

    pub(crate) fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub(crate) fn crashes_dir(&self) -> PathBuf {
        self.data_dir.join("crashes")
    }

    pub(crate) fn projects_file(&self) -> PathBuf {
        self.data_dir.join("interoperable").join("projects.json")
    }

    pub(crate) fn project_bindings_file(&self) -> PathBuf {
        self.data_dir
            .join("interoperable")
            .join("project-bindings.json")
    }

    pub(crate) fn package_state_file(&self) -> PathBuf {
        self.data_dir.join("interoperable").join("packages.json")
    }

    pub(crate) fn package_store_dir(&self) -> PathBuf {
        self.data_dir.join("interoperable").join("plugins")
    }

    pub(crate) fn endpoint_trust_file(&self) -> PathBuf {
        self.data_dir
            .join("interoperable")
            .join("endpoint-trust.json")
    }

    pub(crate) fn external_agent_state_file(&self) -> PathBuf {
        self.data_dir
            .join("interoperable")
            .join("external-agents.json")
    }

    pub(crate) fn outbound_decisions_file(&self) -> PathBuf {
        self.data_dir
            .join("interoperable")
            .join("outbound-decisions.json")
    }

    pub(crate) fn outbound_audit_file(&self) -> PathBuf {
        self.data_dir
            .join("interoperable")
            .join("outbound-audit.json")
    }
}

fn choose_runtime_dir(runtime_dir: Option<&Path>, cache_dir: &Path) -> PathBuf {
    runtime_dir
        .map(Path::to_owned)
        .unwrap_or_else(|| cache_dir.join("run"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn absolute_override_maps_every_category() {
        let parent = tempdir().expect("temporary parent");
        let root = parent.path().join("xana-home");

        let paths =
            XanaPaths::resolve(Some(root.clone().into_os_string())).expect("absolute override");

        assert_eq!(paths.config_file(), root.join("config.toml"));
        assert_eq!(paths.data_dir(), root.join("data"));
        assert_eq!(paths.cache_dir(), root.join("cache"));
        assert_eq!(paths.runtime_dir(), root.join("run"));
        assert_eq!(
            paths.presentation_file(),
            root.join("data/frontend/presentation.toml")
        );
        assert_eq!(paths.logs_dir(), root.join("data/logs"));
        assert_eq!(paths.crashes_dir(), root.join("data/crashes"));
        assert_eq!(
            paths.projects_file(),
            root.join("data/interoperable/projects.json")
        );
        assert_eq!(
            paths.project_bindings_file(),
            root.join("data/interoperable/project-bindings.json")
        );
        assert_eq!(
            paths.package_state_file(),
            root.join("data/interoperable/packages.json")
        );
        assert_eq!(
            paths.endpoint_trust_file(),
            root.join("data/interoperable/endpoint-trust.json")
        );
        assert_eq!(
            paths.external_agent_state_file(),
            root.join("data/interoperable/external-agents.json")
        );
        assert_eq!(
            paths.outbound_decisions_file(),
            root.join("data/interoperable/outbound-decisions.json")
        );
        assert_eq!(
            paths.outbound_audit_file(),
            root.join("data/interoperable/outbound-audit.json")
        );
    }

    #[test]
    fn empty_override_uses_platform_defaults() {
        let dirs =
            ProjectDirs::from("io.github", "labcoder", "xana").expect("platform directories");

        let paths = XanaPaths::resolve(Some(OsString::new())).expect("empty fallback");

        assert_eq!(paths.config_file(), dirs.config_dir().join("config.toml"));
        assert_eq!(paths.data_dir(), dirs.data_dir());
        assert_eq!(paths.cache_dir(), dirs.cache_dir());
        assert_eq!(
            paths.runtime_dir(),
            choose_runtime_dir(dirs.runtime_dir(), dirs.cache_dir())
        );
    }

    #[test]
    fn relative_override_is_rejected() {
        let relative = PathBuf::from("portable-xana");

        let result = XanaPaths::resolve(Some(relative.clone().into_os_string()));

        assert_eq!(
            result,
            Err(XanaPathError::RelativeHomeOverride { path: relative })
        );
    }

    #[test]
    fn resolving_an_override_does_not_create_directories() {
        let parent = tempdir().expect("temporary parent");
        let root = parent.path().join("not-created");
        assert!(!root.exists());

        let paths =
            XanaPaths::resolve(Some(root.clone().into_os_string())).expect("absolute override");

        assert_eq!(paths.config_file(), root.join("config.toml"));
        assert!(!root.exists());
    }

    #[test]
    fn platform_defaults_use_project_directory_categories() {
        let dirs =
            ProjectDirs::from("io.github", "labcoder", "xana").expect("platform directories");
        let paths = XanaPaths::resolve(None).expect("default paths");

        assert_eq!(paths.config_file(), dirs.config_dir().join("config.toml"));
        assert_eq!(paths.data_dir(), dirs.data_dir());
        assert_eq!(paths.cache_dir(), dirs.cache_dir());
        assert_eq!(
            paths.runtime_dir(),
            choose_runtime_dir(dirs.runtime_dir(), dirs.cache_dir())
        );
    }

    #[test]
    fn runtime_directory_is_used_when_available() {
        let actual = choose_runtime_dir(Some(Path::new("runtime-dir")), Path::new("cache-dir"));

        assert_eq!(actual, Path::new("runtime-dir"));
    }

    #[test]
    fn runtime_fallback_is_cache_run() {
        let actual = choose_runtime_dir(None, Path::new("cache-dir"));

        assert_eq!(actual, Path::new("cache-dir/run"));
    }
}
