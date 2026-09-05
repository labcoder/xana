//! Explicit per-user login registration. The OS entry carries only executable
//! and home identity; protected policy remains the authority at every launch.
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
use super::HostPolicy;
use crate::{paths::XanaPaths, storage::ProtectedStore, workspace_identity::WorkspaceIdentity};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

const RECORD: &str = "autonomy/startup-registration";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    name: String,
    arguments: Vec<String>,
}
impl Registration {
    fn new(paths: &XanaPaths, executable: &Path) -> Result<Self> {
        let identity = WorkspaceIdentity::resolve(paths.data_dir())?;
        let mut arguments = vec![
            executable
                .to_str()
                .context("startup executable is not valid UTF-8")?
                .into(),
            "autonomy".into(),
            "host".into(),
            "run".into(),
            "--from-startup".into(),
        ];
        if XanaPaths::resolve(None)? != *paths {
            let root = paths
                .config_file()
                .parent()
                .context("home parent missing")?;
            ensure!(
                XanaPaths::resolve(Some(root.as_os_str().to_owned()))? == *paths,
                "startup paths do not form one explicit Xana home"
            );
            arguments.extend([
                "--home".into(),
                root.to_str()
                    .context("startup home is not valid UTF-8")?
                    .into(),
            ]);
        }
        let registration = Self {
            name: format!(
                "io.github.labcoder.xana-autonomy-{}",
                identity.collision_key()
            ),
            arguments,
        };
        registration.validate()?;
        Ok(registration)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.name.starts_with("io.github.labcoder.xana-autonomy-")
                && self.name.len() <= 128
                && self
                    .name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-'),
            "invalid startup registration identity"
        );
        ensure!(
            matches!(self.arguments.len(), 5 | 7)
                && self.arguments[1..5] == ["autonomy", "host", "run", "--from-startup"],
            "invalid startup argument shape"
        );
        ensure!(
            Path::new(&self.arguments[0]).is_absolute(),
            "startup executable must be absolute"
        );
        if self.arguments.len() == 7 {
            ensure!(
                self.arguments[5] == "--home" && Path::new(&self.arguments[6]).is_absolute(),
                "startup home must be absolute"
            );
        }
        ensure!(
            self.arguments.iter().all(|arg| !arg.is_empty()
                && arg.len() <= 4096
                && !arg.chars().any(char::is_control)),
            "invalid startup argument"
        );
        Ok(())
    }
}

pub(crate) fn change(
    store: &ProtectedStore,
    paths: &XanaPaths,
    revision: u64,
    enable: bool,
) -> Result<HostPolicy> {
    change_with(
        store,
        paths,
        revision,
        enable,
        super::host::cli_executable,
        platform_change,
    )
}

fn change_with(
    store: &ProtectedStore,
    paths: &XanaPaths,
    revision: u64,
    enable: bool,
    executable: impl FnOnce() -> Result<std::path::PathBuf>,
    mut apply: impl FnMut(&Registration, bool) -> Result<()>,
) -> Result<HostPolicy> {
    ensure!(
        store.data_dir() == paths.data_dir(),
        "startup scope does not match protected custody"
    );
    // Serialize this home's multi-phase OS/policy edits across clients. The
    // foreground/background execution lanes are independent of registration.
    let lease_path = store.data_dir().join("protected/autonomy-startup.lease");
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lease_path)?;
    ensure!(
        std::fs::symlink_metadata(&lease_path)?
            .file_type()
            .is_file()
            && same_file::Handle::from_file(lease.try_clone()?)?
                == same_file::Handle::from_path(&lease_path)?,
        "startup lease identity changed"
    );
    fs2::FileExt::try_lock_exclusive(&lease)
        .context("another client is editing startup registration; refresh and retry")?;
    let previous = store.autonomy_policy()?;
    ensure!(
        previous.revision == revision,
        "host policy changed; refresh before editing"
    );
    ensure!(
        !enable || previous.detached_enabled,
        "enable detached work before OS startup"
    );
    let recorded = store
        .document(RECORD, 32768)?
        .map(|bytes| serde_json::from_slice::<Registration>(&bytes))
        .transpose()?;
    let registration = if !enable && let Some(recorded) = &recorded {
        recorded.clone()
    } else {
        Registration::new(paths, &executable()?)?
    };
    ensure!(
        registration.name
            == format!(
                "io.github.labcoder.xana-autonomy-{}",
                WorkspaceIdentity::resolve(paths.data_dir())?.collision_key()
            ),
        "recorded startup home identity changed"
    );
    if let Some(recorded) = &recorded {
        recorded.validate()?;
        ensure!(
            recorded.name == registration.name,
            "recorded startup home identity changed"
        );
    }
    // Revoke first. OS registration is not transactional with SQLCipher; any
    // interruption before the final policy edit leaves startup unauthorized.
    let disabled = super::host::policy_edit(store, revision, None, Some(false), false, false)?;
    if let Some(recorded) = &recorded {
        apply(recorded, false)?;
    }
    if !enable {
        if recorded.is_none() {
            apply(&registration, false)?;
        }
        store.remove_document(RECORD)?;
        return Ok(disabled);
    }
    apply(&registration, true)?;
    store.set_document(RECORD, &serde_json::to_vec(&registration)?, 32768)?;
    super::host::policy_edit(store, disabled.revision, None, Some(true), false, false).context(
        "registration exists but startup authorization changed concurrently; inspect host policy",
    )
}

fn platform_change(registration: &Registration, enable: bool) -> Result<()> {
    #[cfg(windows)]
    {
        windows::change(registration, enable)
    }
    #[cfg(unix)]
    {
        unix::change(registration, enable)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (registration, enable);
        anyhow::bail!("per-user startup is unsupported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_home_registration_contains_only_typed_launch_identity() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("data")).unwrap();
        let paths = XanaPaths::resolve(Some(home.path().as_os_str().to_owned())).unwrap();
        let registration = Registration::new(&paths, &home.path().join("xana.exe")).unwrap();
        assert_eq!(registration.arguments[5], "--home");
        assert_eq!(registration.arguments[6], home.path().to_str().unwrap());
        assert_eq!(registration.arguments.len(), 7);
        assert!(registration.name.len() <= 128);
        let mut invalid = registration;
        invalid.arguments.push("arbitrary-command".into());
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn registration_failure_leaves_startup_revoked_and_success_enables_last() {
        let home = tempfile::tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(home.path().as_os_str().to_owned())).unwrap();
        let store = ProtectedStore::initialize(
            paths.data_dir(),
            &crate::storage::RecoveryIdentity::generate(),
            &crate::storage::TestCustody::default(),
        )
        .unwrap();
        let enabled =
            super::super::host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
        let failed = change_with(
            &store,
            &paths,
            enabled.revision,
            true,
            || Ok(home.path().join("xana.exe")),
            |_, _| anyhow::bail!("synthetic OS failure"),
        );
        assert!(failed.is_err());
        assert!(!store.autonomy_policy().unwrap().startup_enabled);
        let revision = store.autonomy_policy().unwrap().revision;
        let mut installed = false;
        let ready = change_with(
            &store,
            &paths,
            revision,
            true,
            || Ok(home.path().join("xana.exe")),
            |_, enable| {
                assert!(!store.autonomy_policy()?.startup_enabled);
                installed = enable;
                Ok(())
            },
        )
        .unwrap();
        assert!(installed && ready.startup_enabled);
        let removed = change_with(
            &store,
            &paths,
            ready.revision,
            false,
            || anyhow::bail!("old executable no longer exists"),
            |_, enable| {
                assert!(!enable && !store.autonomy_policy()?.startup_enabled);
                Ok(())
            },
        )
        .unwrap();
        assert!(!removed.startup_enabled);
        assert!(store.document(RECORD, 32768).unwrap().is_none());
    }
}
