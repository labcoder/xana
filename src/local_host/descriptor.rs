use super::protocol::LOCAL_HOST_PROTOCOL_VERSION;
use crate::{
    bounded_file,
    workspace_identity::{WorkspaceIdentity, next_locked_generation},
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zeroize::Zeroize;

const DESCRIPTOR_VERSION: u16 = 2;
const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DescriptorHealth {
    Absent,
    Active {
        process_id: u32,
        endpoint: SocketAddr,
        generation: u64,
    },
    Stale {
        path: PathBuf,
        reason: String,
    },
    InvalidActive {
        path: PathBuf,
        reason: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeDescriptor {
    pub(crate) version: u16,
    pub(crate) protocol_version: u16,
    pub(crate) host_id: Uuid,
    pub(crate) generation: u64,
    pub(crate) process_id: u32,
    pub(crate) workspace: PathBuf,
    pub(crate) workspace_id: String,
    pub(crate) endpoint: SocketAddr,
    pub(crate) capability: String,
}

impl Drop for RuntimeDescriptor {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

pub(crate) struct DescriptorLease {
    path: PathBuf,
    host_id: Uuid,
    generation: u64,
    lock: Option<fs::File>,
}

pub(crate) enum DescriptorClaim {
    Owned(DescriptorLease),
    Attach(RuntimeDescriptor),
}

impl RuntimeDescriptor {
    pub(crate) fn new(
        host_id: Uuid,
        workspace: PathBuf,
        endpoint: SocketAddr,
        capability: String,
    ) -> Result<Self, String> {
        let identity = WorkspaceIdentity::resolve(&workspace)
            .map_err(|error| format!("could not identify local-host workspace: {error}"))?;
        Ok(Self {
            version: DESCRIPTOR_VERSION,
            protocol_version: LOCAL_HOST_PROTOCOL_VERSION,
            host_id,
            generation: 0,
            process_id: std::process::id(),
            workspace_id: identity.collision_key().to_owned(),
            workspace: identity.canonical_path().to_owned(),
            endpoint,
            capability,
        })
    }
}

impl DescriptorLease {
    pub(crate) fn create(
        runtime_root: &Path,
        descriptor: &mut RuntimeDescriptor,
    ) -> Result<Self, String> {
        match Self::claim(runtime_root, descriptor)? {
            DescriptorClaim::Owned(lease) => Ok(lease),
            DescriptorClaim::Attach(owner) => Err(format!(
                "a Xana foreground host is already active for this workspace (process {}, generation {})",
                owner.process_id, owner.generation
            )),
        }
    }

    pub(crate) fn claim(
        runtime_root: &Path,
        descriptor: &mut RuntimeDescriptor,
    ) -> Result<DescriptorClaim, String> {
        let directory = descriptor_directory(runtime_root);
        fs::create_dir_all(&directory).map_err(|error| {
            format!("could not create local-host descriptor directory: {error}")
        })?;
        protect_directory(&directory)?;
        let identity = WorkspaceIdentity::resolve(&descriptor.workspace)
            .map_err(|error| format!("could not identify local-host workspace: {error}"))?;
        if identity.collision_key() != descriptor.workspace_id {
            return Err("local-host descriptor workspace identity changed before claim".into());
        }
        let key = identity.collision_key();
        let lock_path = directory.join(format!("{key}.lock"));
        let path = directory.join(format!("{key}.json"));
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| format!("could not open local-host lock: {error}"))?;
        protect_open_file(&lock)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let owner = discover_path(&path)?;
                validate_descriptor(&owner, &identity)?;
                return Ok(DescriptorClaim::Attach(owner));
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(format!("could not lock local-host descriptor: {error}"));
            }
        }
        descriptor.generation = next_locked_generation(&lock).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                "local-host owner generation is invalid; run `xana doctor` before retrying"
                    .to_owned()
            } else {
                format!("could not advance local-host owner generation: {error}")
            }
        })?;
        write_descriptor(&path, descriptor)?;
        protect_file(&path)?;
        Ok(DescriptorClaim::Owned(Self {
            path,
            host_id: descriptor.host_id,
            generation: descriptor.generation,
            lock: Some(lock),
        }))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for DescriptorLease {
    fn drop(&mut self) {
        if let Ok(descriptor) = discover_path(&self.path)
            && descriptor.host_id == self.host_id
            && descriptor.generation == self.generation
        {
            let _ = fs::remove_file(&self.path);
        }
        if let Some(lock) = self.lock.take() {
            let _ = fs::File::unlock(&lock);
        }
    }
}

pub(crate) fn discover(runtime_root: &Path, workspace: &Path) -> Result<RuntimeDescriptor, String> {
    let identity = WorkspaceIdentity::resolve(workspace)
        .map_err(|error| format!("could not canonicalize attach workspace: {error}"))?;
    let key = identity.collision_key();
    let path = descriptor_directory(runtime_root).join(format!("{key}.json"));
    let descriptor = discover_path(&path)?;
    validate_descriptor(&descriptor, &identity)?;
    Ok(descriptor)
}

pub(crate) fn inspect_health(
    runtime_root: &Path,
    workspace: &Path,
) -> Result<DescriptorHealth, String> {
    let identity = WorkspaceIdentity::resolve(workspace)
        .map_err(|error| format!("could not canonicalize diagnostic workspace: {error}"))?;
    let key = identity.collision_key();
    let directory = descriptor_directory(runtime_root);
    let path = directory.join(format!("{key}.json"));
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DescriptorHealth::Absent);
        }
        Err(error) => return Err(format!("could not inspect local-host descriptor: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Ok(DescriptorHealth::InvalidActive {
            path,
            reason: "descriptor target is not a regular file".into(),
        });
    }

    let lock_path = directory.join(format!("{key}.lock"));
    let lock = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let reason = discover_path(&path)
                .and_then(|value| validate_descriptor(&value, &identity))
                .map(|_| "descriptor has no owner lock".to_owned())
                .unwrap_or_else(|reason| reason);
            return Ok(DescriptorHealth::Stale { path, reason });
        }
        Err(error) => {
            return Err(format!(
                "could not open existing local-host diagnostic lock: {error}"
            ));
        }
    };
    let unlocked = match lock.try_lock() {
        Ok(()) => true,
        Err(fs::TryLockError::WouldBlock) => false,
        Err(fs::TryLockError::Error(error)) => {
            return Err(format!(
                "could not inspect local-host diagnostic lock: {error}"
            ));
        }
    };
    let descriptor = discover_path(&path).and_then(|value| {
        validate_descriptor(&value, &identity)?;
        Ok(value)
    });
    if unlocked {
        let _ = fs::File::unlock(&lock);
        return Ok(DescriptorHealth::Stale {
            path,
            reason: descriptor
                .map(|_| "descriptor has no active lock owner".to_owned())
                .unwrap_or_else(|reason| reason),
        });
    }
    Ok(match descriptor {
        Ok(descriptor) => DescriptorHealth::Active {
            process_id: descriptor.process_id,
            endpoint: descriptor.endpoint,
            generation: descriptor.generation,
        },
        Err(reason) => DescriptorHealth::InvalidActive { path, reason },
    })
}

pub(crate) fn remove_stale(runtime_root: &Path, workspace: &Path) -> Result<bool, String> {
    let identity = WorkspaceIdentity::resolve(workspace)
        .map_err(|error| format!("could not canonicalize diagnostic workspace: {error}"))?;
    let key = identity.collision_key();
    let directory = descriptor_directory(runtime_root);
    let path = directory.join(format!("{key}.json"));
    let lock_path = directory.join(format!("{key}.lock"));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("could not open local-host repair lock: {error}"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            return Err("refusing to remove a descriptor whose owner lock is active".into());
        }
        Err(fs::TryLockError::Error(error)) => {
            return Err(format!("could not acquire local-host repair lock: {error}"));
        }
    }
    let removed = match fs::remove_file(&path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("could not remove stale descriptor: {error}")),
    };
    let _ = fs::File::unlock(&lock);
    Ok(removed)
}

fn discover_path(path: &Path) -> Result<RuntimeDescriptor, String> {
    let bytes = bounded_file::read(path, MAX_DESCRIPTOR_BYTES)
        .map_err(|error| format!("could not read local-host descriptor: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| "local-host descriptor is malformed; remove it or run `xana doctor`".into())
}

fn validate_descriptor(
    descriptor: &RuntimeDescriptor,
    workspace: &WorkspaceIdentity,
) -> Result<(), String> {
    if descriptor.version != DESCRIPTOR_VERSION
        || descriptor.protocol_version != LOCAL_HOST_PROTOCOL_VERSION
    {
        return Err("local-host descriptor uses an unsupported protocol version".into());
    }
    let matches_workspace = workspace.matches(&descriptor.workspace).unwrap_or(false);
    if descriptor.workspace_id != workspace.collision_key() || !matches_workspace {
        return Err("local-host descriptor belongs to another workspace".into());
    }
    if descriptor.generation == 0 {
        return Err("local-host descriptor contains an invalid owner generation".into());
    }
    if !descriptor.endpoint.ip().is_loopback() {
        return Err("local-host descriptor contains a non-loopback endpoint".into());
    }
    if descriptor.capability.len() < 64 || descriptor.capability.len() > 256 {
        return Err("local-host descriptor contains an invalid capability".into());
    }
    Ok(())
}

fn descriptor_directory(runtime_root: &Path) -> PathBuf {
    runtime_root.join("workspace-hosts")
}

fn write_descriptor(path: &Path, descriptor: &RuntimeDescriptor) -> Result<(), String> {
    use io::Write as _;
    let bytes = serde_json::to_vec(descriptor)
        .map_err(|error| format!("could not encode local-host descriptor: {error}"))?;
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err("local-host descriptor exceeds its byte bound".into());
    }
    let mut file = atomic_write_file::AtomicWriteFile::open(path)
        .map_err(|error| format!("could not create local-host descriptor: {error}"))?;
    protect_open_file(file.as_file())?;
    file.write_all(&bytes)
        .map_err(|error| format!("could not write local-host descriptor: {error}"))?;
    file.commit()
        .map_err(|error| format!("could not install local-host descriptor: {error}"))
}

#[cfg(test)]
pub(crate) fn write_stale_for_test(runtime_root: &Path, workspace: &Path) -> PathBuf {
    let identity = WorkspaceIdentity::resolve(workspace).expect("test workspace");
    let workspace = identity.canonical_path().to_owned();
    let key = identity.collision_key();
    let directory = descriptor_directory(runtime_root);
    fs::create_dir_all(&directory).expect("test descriptor directory");
    let path = directory.join(format!("{key}.json"));
    let mut descriptor = RuntimeDescriptor::new(
        Uuid::new_v4(),
        workspace,
        "127.0.0.1:12345".parse().expect("test endpoint"),
        "x".repeat(64),
    )
    .expect("test descriptor");
    descriptor.generation = 1;
    write_descriptor(&path, &descriptor).expect("test descriptor");
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!("{key}.lock")))
        .expect("test descriptor lock");
    path
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not protect local-host descriptor directory: {error}"))
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> Result<(), String> {
    // Platform-default Xana runtime directories are scoped to the current
    // Windows user. The capability remains mandatory even when an explicit
    // XANA_HOME inherits a broader ACL.
    Ok(())
}

#[cfg(unix)]
fn protect_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not protect local-host descriptor: {error}"))
}

#[cfg(not(unix))]
fn protect_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn protect_open_file(file: &fs::File) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not protect local-host state file: {error}"))
}

#[cfg(not(unix))]
fn protect_open_file(_file: &fs::File) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn descriptor(workspace: &Path, port: u16) -> RuntimeDescriptor {
        RuntimeDescriptor::new(
            Uuid::new_v4(),
            workspace.to_owned(),
            SocketAddr::from(([127, 0, 0, 1], port)),
            "x".repeat(64),
        )
        .unwrap()
    }

    #[test]
    fn simultaneous_claims_attach_to_the_lock_backed_generation() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("run");
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut first = descriptor(&workspace, 41001);
        let first_lease = match DescriptorLease::claim(&runtime, &mut first).unwrap() {
            DescriptorClaim::Owned(lease) => lease,
            DescriptorClaim::Attach(_) => panic!("first claimant must own"),
        };
        let mut second = descriptor(&workspace.join("."), 41002);

        let attached = match DescriptorLease::claim(&runtime, &mut second).unwrap() {
            DescriptorClaim::Attach(owner) => owner,
            DescriptorClaim::Owned(_) => panic!("second claimant split ownership"),
        };

        assert_eq!(attached.host_id, first.host_id);
        assert_eq!(attached.generation, first_lease.generation());
        assert_eq!(attached.endpoint, first.endpoint);
    }

    #[test]
    fn generations_increase_after_release_and_stale_descriptors_do_not_authorize_attach() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("run");
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut first = descriptor(&workspace, 41001);
        let first_generation = {
            let lease = DescriptorLease::create(&runtime, &mut first).unwrap();
            lease.generation()
        };
        assert!(matches!(
            inspect_health(&runtime, &workspace).unwrap(),
            DescriptorHealth::Absent
        ));

        write_stale_for_test(&runtime, &workspace);
        assert!(matches!(
            inspect_health(&runtime, &workspace).unwrap(),
            DescriptorHealth::Stale { .. }
        ));
        let mut second = descriptor(&workspace, 41002);
        let second_lease = DescriptorLease::create(&runtime, &mut second).unwrap();

        assert!(second_lease.generation() > first_generation);
    }
}
