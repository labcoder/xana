use super::protocol::{LOCAL_HOST_PROTOCOL_VERSION, workspace_identity};
use crate::bounded_file;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zeroize::Zeroize;

const DESCRIPTOR_VERSION: u16 = 1;
const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeDescriptor {
    pub(crate) version: u16,
    pub(crate) protocol_version: u16,
    pub(crate) host_id: Uuid,
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
    lock: Option<fs::File>,
}

impl RuntimeDescriptor {
    pub(crate) fn new(
        host_id: Uuid,
        workspace: PathBuf,
        endpoint: SocketAddr,
        capability: String,
    ) -> Self {
        Self {
            version: DESCRIPTOR_VERSION,
            protocol_version: LOCAL_HOST_PROTOCOL_VERSION,
            host_id,
            process_id: std::process::id(),
            workspace_id: workspace_identity(&workspace),
            workspace,
            endpoint,
            capability,
        }
    }
}

impl DescriptorLease {
    pub(crate) fn create(
        runtime_root: &Path,
        descriptor: &RuntimeDescriptor,
    ) -> Result<Self, String> {
        let directory = descriptor_directory(runtime_root);
        fs::create_dir_all(&directory).map_err(|error| {
            format!("could not create local-host descriptor directory: {error}")
        })?;
        protect_directory(&directory)?;
        let key = descriptor.workspace_id.as_str();
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
                return Err("a Xana foreground host is already active for this workspace".into());
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(format!("could not lock local-host descriptor: {error}"));
            }
        }
        write_descriptor(&path, descriptor)?;
        protect_file(&path)?;
        Ok(Self {
            path,
            host_id: descriptor.host_id,
            lock: Some(lock),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DescriptorLease {
    fn drop(&mut self) {
        if let Ok(descriptor) = discover_path(&self.path)
            && descriptor.host_id == self.host_id
        {
            let _ = fs::remove_file(&self.path);
        }
        if let Some(lock) = self.lock.take() {
            let _ = fs::File::unlock(&lock);
        }
    }
}

pub(crate) fn discover(runtime_root: &Path, workspace: &Path) -> Result<RuntimeDescriptor, String> {
    let canonical = workspace
        .canonicalize()
        .map_err(|error| format!("could not canonicalize attach workspace: {error}"))?;
    let key = workspace_identity(&canonical);
    let path = descriptor_directory(runtime_root).join(format!("{key}.json"));
    let descriptor = discover_path(&path)?;
    validate_descriptor(&descriptor, &canonical)?;
    Ok(descriptor)
}

fn discover_path(path: &Path) -> Result<RuntimeDescriptor, String> {
    let bytes = bounded_file::read(path, MAX_DESCRIPTOR_BYTES)
        .map_err(|error| format!("could not read local-host descriptor: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| "local-host descriptor is malformed; remove it or run `xana doctor`".into())
}

fn validate_descriptor(descriptor: &RuntimeDescriptor, workspace: &Path) -> Result<(), String> {
    if descriptor.version != DESCRIPTOR_VERSION
        || descriptor.protocol_version != LOCAL_HOST_PROTOCOL_VERSION
    {
        return Err("local-host descriptor uses an unsupported protocol version".into());
    }
    if descriptor.workspace != workspace || descriptor.workspace_id != workspace_identity(workspace)
    {
        return Err("local-host descriptor belongs to another workspace".into());
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
