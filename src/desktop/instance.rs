//! Authenticated, per-home Desktop instance coordination.
//!
//! The descriptor contains only a loopback endpoint, a high-entropy capability,
//! and the canonical Xana runtime directory. Forwarded values are a closed,
//! bounded intent enum rather than commands, prompts, credentials, or paths.

use super::{DesktopError, DesktopErrorCode, DesktopLaunch};
use crate::{bounded_file, paths::XanaPaths};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write as _},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use uuid::Uuid;
use zeroize::Zeroize as _;

const DESCRIPTOR_VERSION: u16 = 1;
const FORWARD_PROTOCOL_VERSION: u16 = 1;
const MAX_DESCRIPTOR_BYTES: usize = 4 * 1024;
const MAX_FORWARD_BYTES: usize = 4 * 1024;
const FORWARD_CAPACITY: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const DESCRIPTOR_RETRIES: usize = 20;
const DESCRIPTOR_RETRY_DELAY: Duration = Duration::from_millis(25);
const ACCEPT_POLL_DELAY: Duration = Duration::from_millis(10);

/// A closed set of safe destinations accepted from a later Desktop launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopNavigationTarget {
    Conversation,
    Activity,
    Diagnostics,
    Settings,
    Espejo,
}

impl DesktopNavigationTarget {
    /// Parses the documented process argument without accepting paths or URLs.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "conversation" => Some(Self::Conversation),
            "activity" => Some(Self::Activity),
            "diagnostics" => Some(Self::Diagnostics),
            "settings" => Some(Self::Settings),
            "espejo" => Some(Self::Espejo),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Activity => "activity",
            Self::Diagnostics => "diagnostics",
            Self::Settings => "settings",
            Self::Espejo => "espejo",
        }
    }
}

/// A bounded request accepted from another process using the same Xana home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
pub enum DesktopLaunchIntent {
    Focus,
    Navigate(DesktopNavigationTarget),
}

/// Trusted Xana-owned paths exposed to the Desktop's platform adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopNativePaths {
    pub config_file: PathBuf,
    pub logs_directory: PathBuf,
}

/// Result of claiming one canonical Xana home.
pub enum DesktopInstanceClaim {
    Primary(DesktopInstanceLease),
    Forwarded,
}

/// Keeps the per-home lock, descriptor, and bounded loopback receiver alive.
pub struct DesktopInstanceLease {
    descriptor_path: PathBuf,
    capability: String,
    endpoint: SocketAddr,
    lock: Option<fs::File>,
    receiver: mpsc::Receiver<DesktopLaunchIntent>,
    stopping: Arc<AtomicBool>,
    listener: Option<thread::JoinHandle<()>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceDescriptor {
    version: u16,
    protocol_version: u16,
    process_id: u32,
    instance_root: PathBuf,
    endpoint: SocketAddr,
    capability: String,
}

impl Drop for InstanceDescriptor {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardRequest {
    version: u16,
    capability: String,
    intent: DesktopLaunchIntent,
}

impl Drop for ForwardRequest {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardResponse {
    version: u16,
    accepted: bool,
}

impl DesktopLaunch {
    /// Claims the one Desktop instance for this launch's canonical Xana home.
    ///
    /// A later launch forwards only `intent` and returns [`DesktopInstanceClaim::Forwarded`].
    pub fn claim_instance(
        &self,
        intent: DesktopLaunchIntent,
    ) -> Result<DesktopInstanceClaim, DesktopError> {
        let paths = XanaPaths::resolve(self.xana_home.clone()).map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::ConfigurationUnavailable,
                format!("could not resolve Xana paths: {error}"),
            )
        })?;
        claim(&paths, intent)
    }

    /// Resolves only Xana-owned paths suitable for explicit platform actions.
    pub fn native_paths(&self) -> Result<DesktopNativePaths, DesktopError> {
        let paths = XanaPaths::resolve(self.xana_home.clone()).map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::ConfigurationUnavailable,
                format!("could not resolve Xana paths: {error}"),
            )
        })?;
        Ok(DesktopNativePaths {
            config_file: paths.config_file().to_owned(),
            logs_directory: paths.logs_dir(),
        })
    }
}

impl DesktopInstanceLease {
    /// Receives one already-authenticated launch intent without blocking GPUI.
    pub fn try_next(&self) -> Option<DesktopLaunchIntent> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for DesktopInstanceLease {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Ok(stream) = TcpStream::connect_timeout(&self.endpoint, IO_TIMEOUT) {
            let _ = stream.shutdown(Shutdown::Write);
        }
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        if descriptor_capability(&self.descriptor_path)
            .is_some_and(|capability| capability == self.capability)
        {
            let _ = fs::remove_file(&self.descriptor_path);
        }
        if let Some(lock) = self.lock.take() {
            let _ = lock.unlock();
        }
        self.capability.zeroize();
    }
}

fn claim(
    paths: &XanaPaths,
    intent: DesktopLaunchIntent,
) -> Result<DesktopInstanceClaim, DesktopError> {
    let directory = paths.runtime_dir().join("desktop-instance");
    fs::create_dir_all(&directory)
        .and_then(|()| protect_directory(&directory))
        .map_err(|error| instance_io("could not prepare the Desktop instance directory", error))?;
    let instance_root = directory.canonicalize().map_err(|error| {
        instance_io(
            "could not canonicalize the Desktop instance directory",
            error,
        )
    })?;
    let lock_path = instance_root.join("owner.lock");
    let descriptor_path = instance_root.join("owner.json");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .and_then(|file| {
            protect_open_file(&file)?;
            Ok(file)
        })
        .map_err(|error| instance_io("could not open the Desktop instance lock", error))?;

    match lock.try_lock() {
        Ok(()) => own(instance_root, descriptor_path, lock),
        Err(fs::TryLockError::WouldBlock) => {
            forward_when_ready(&descriptor_path, &instance_root, intent)?;
            Ok(DesktopInstanceClaim::Forwarded)
        }
        Err(fs::TryLockError::Error(error)) => Err(instance_io(
            "could not acquire the Desktop instance lock",
            error,
        )),
    }
}

fn own(
    instance_root: PathBuf,
    descriptor_path: PathBuf,
    lock: fs::File,
) -> Result<DesktopInstanceClaim, DesktopError> {
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0))
        .map_err(|error| instance_io("could not bind Desktop loopback forwarding", error))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| instance_io("could not configure Desktop loopback forwarding", error))?;
    let endpoint = listener
        .local_addr()
        .map_err(|error| instance_io("could not inspect Desktop loopback forwarding", error))?;
    let capability = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let descriptor = InstanceDescriptor {
        version: DESCRIPTOR_VERSION,
        protocol_version: FORWARD_PROTOCOL_VERSION,
        process_id: std::process::id(),
        instance_root,
        endpoint,
        capability: capability.clone(),
    };
    write_descriptor(&descriptor_path, &descriptor)?;

    let (sender, receiver) = mpsc::sync_channel(FORWARD_CAPACITY);
    let stopping = Arc::new(AtomicBool::new(false));
    let listener_stopping = stopping.clone();
    let listener_capability = capability.clone();
    let listener_thread = thread::Builder::new()
        .name("xana-desktop-instance".to_owned())
        .spawn(move || {
            serve(listener, &listener_capability, sender, &listener_stopping);
            let mut listener_capability = listener_capability;
            listener_capability.zeroize();
        })
        .map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::InstanceUnavailable,
                format!("could not start Desktop instance forwarding: {error}"),
            )
        })?;

    Ok(DesktopInstanceClaim::Primary(DesktopInstanceLease {
        descriptor_path,
        capability,
        endpoint,
        lock: Some(lock),
        receiver,
        stopping,
        listener: Some(listener_thread),
    }))
}

fn serve(
    listener: TcpListener,
    capability: &str,
    sender: mpsc::SyncSender<DesktopLaunchIntent>,
    stopping: &AtomicBool,
) {
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => handle_connection(stream, capability, &sender),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_DELAY);
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    capability: &str,
    sender: &mpsc::SyncSender<DesktopLaunchIntent>,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let request = read_bounded(&mut stream).and_then(|bytes| {
        serde_json::from_slice::<ForwardRequest>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    });
    let accepted = request.is_ok_and(|request| {
        request.version == FORWARD_PROTOCOL_VERSION
            && request.capability == capability
            && sender.try_send(request.intent).is_ok()
    });
    let response = ForwardResponse {
        version: FORWARD_PROTOCOL_VERSION,
        accepted,
    };
    if let Ok(bytes) = serde_json::to_vec(&response) {
        let _ = stream.write_all(&bytes);
        let _ = stream.flush();
    }
}

fn forward_when_ready(
    descriptor_path: &Path,
    instance_root: &Path,
    intent: DesktopLaunchIntent,
) -> Result<(), DesktopError> {
    let mut last_error = None;
    for _ in 0..DESCRIPTOR_RETRIES {
        match read_descriptor(descriptor_path, instance_root) {
            Ok(descriptor) => return forward(descriptor, intent),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(DESCRIPTOR_RETRY_DELAY);
    }
    Err(last_error.unwrap_or_else(|| {
        DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            "the existing Desktop instance did not publish a usable endpoint",
        )
    }))
}

fn forward(
    descriptor: InstanceDescriptor,
    intent: DesktopLaunchIntent,
) -> Result<(), DesktopError> {
    let mut stream = TcpStream::connect_timeout(&descriptor.endpoint, IO_TIMEOUT)
        .map_err(|error| instance_io("could not reach the existing Desktop instance", error))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|error| instance_io("could not configure Desktop forwarding", error))?;
    let request = ForwardRequest {
        version: FORWARD_PROTOCOL_VERSION,
        capability: descriptor.capability.clone(),
        intent,
    };
    let bytes = serde_json::to_vec(&request).map_err(|error| {
        DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            format!("could not encode Desktop launch intent: {error}"),
        )
    })?;
    if bytes.len() > MAX_FORWARD_BYTES {
        return Err(DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            "Desktop launch intent exceeds its byte bound",
        ));
    }
    stream
        .write_all(&bytes)
        .and_then(|()| stream.shutdown(Shutdown::Write))
        .map_err(|error| instance_io("could not forward the Desktop launch intent", error))?;
    let response = read_bounded(&mut stream)
        .and_then(|bytes| {
            serde_json::from_slice::<ForwardResponse>(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })
        .map_err(|error| instance_io("existing Desktop returned an invalid response", error))?;
    if response.version != FORWARD_PROTOCOL_VERSION || !response.accepted {
        return Err(DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            "existing Desktop rejected the bounded launch intent; retry once it is responsive",
        ));
    }
    Ok(())
}

fn read_descriptor(path: &Path, expected_root: &Path) -> Result<InstanceDescriptor, DesktopError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| instance_io("could not inspect the Desktop instance descriptor", error))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            "Desktop instance descriptor is not a regular file",
        ));
    }
    let bytes = bounded_file::read(path, MAX_DESCRIPTOR_BYTES)
        .map_err(|error| instance_io("could not read the Desktop instance descriptor", error))?;
    let descriptor: InstanceDescriptor = serde_json::from_slice(&bytes).map_err(|_| {
        DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            "Desktop instance descriptor is malformed",
        )
    })?;
    if descriptor.version != DESCRIPTOR_VERSION
        || descriptor.protocol_version != FORWARD_PROTOCOL_VERSION
        || descriptor.instance_root != expected_root
        || !descriptor.endpoint.ip().is_loopback()
        || descriptor.capability.len() != 64
        || !descriptor
            .capability
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            "Desktop instance descriptor failed validation",
        ));
    }
    Ok(descriptor)
}

fn descriptor_capability(path: &Path) -> Option<String> {
    let expected_root = path.parent()?.canonicalize().ok()?;
    read_descriptor(path, &expected_root)
        .ok()
        .map(|descriptor| descriptor.capability.clone())
}

fn write_descriptor(path: &Path, descriptor: &InstanceDescriptor) -> Result<(), DesktopError> {
    let bytes = serde_json::to_vec(descriptor).map_err(|error| {
        DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            format!("could not encode Desktop instance descriptor: {error}"),
        )
    })?;
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(DesktopError::new(
            DesktopErrorCode::InstanceUnavailable,
            "Desktop instance descriptor exceeds its byte bound",
        ));
    }
    let mut file = atomic_write_file::AtomicWriteFile::open(path)
        .map_err(|error| instance_io("could not create the Desktop instance descriptor", error))?;
    protect_open_file(file.as_file())
        .map_err(|error| instance_io("could not protect the Desktop instance descriptor", error))?;
    file.write_all(&bytes)
        .and_then(|()| file.commit())
        .map_err(|error| instance_io("could not install the Desktop instance descriptor", error))
}

fn read_bounded(reader: &mut impl io::Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                if bytes.len().saturating_add(read) > MAX_FORWARD_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Desktop forwarding payload exceeds its byte bound",
                    ));
                }
                bytes.extend_from_slice(&chunk[..read]);
            }
            // Windows can report a reset after the peer has written its complete
            // bounded response and closed. Preserve the received frame; its JSON
            // decoder still rejects a partial or hostile payload.
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset && !bytes.is_empty() => {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}

fn instance_io(context: &str, error: impl std::fmt::Display) -> DesktopError {
    DesktopError::new(
        DesktopErrorCode::InstanceUnavailable,
        format!("{context}: {error}"),
    )
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_open_file(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn protect_open_file(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::io::Cursor;
    use std::sync::Barrier;

    fn launch(root: &Path) -> DesktopLaunch {
        DesktopLaunch::new(root, Some(OsString::from(root)))
    }

    #[test]
    fn later_same_home_launch_forwards_one_closed_intent() {
        let root = tempfile::tempdir().unwrap();
        let launch = launch(root.path());
        let DesktopInstanceClaim::Primary(primary) =
            launch.claim_instance(DesktopLaunchIntent::Focus).unwrap()
        else {
            panic!("first launch must own the instance")
        };

        assert!(matches!(
            launch
                .claim_instance(DesktopLaunchIntent::Navigate(
                    DesktopNavigationTarget::Diagnostics
                ))
                .unwrap(),
            DesktopInstanceClaim::Forwarded
        ));
        let mut received = None;
        for _ in 0..50 {
            received = primary.try_next();
            if received.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            received,
            Some(DesktopLaunchIntent::Navigate(
                DesktopNavigationTarget::Diagnostics
            ))
        );
    }

    #[test]
    fn distinct_homes_have_distinct_primary_instances() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        assert!(matches!(
            launch(first.path())
                .claim_instance(DesktopLaunchIntent::Focus)
                .unwrap(),
            DesktopInstanceClaim::Primary(_)
        ));
        assert!(matches!(
            launch(second.path())
                .claim_instance(DesktopLaunchIntent::Focus)
                .unwrap(),
            DesktopInstanceClaim::Primary(_)
        ));
    }

    #[test]
    fn stale_descriptor_is_replaced_when_no_owner_holds_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let instance_directory = root.path().join("run").join("desktop-instance");
        fs::create_dir_all(&instance_directory).unwrap();
        fs::write(instance_directory.join("owner.json"), b"stale descriptor").unwrap();

        let claim = launch(root.path())
            .claim_instance(DesktopLaunchIntent::Focus)
            .unwrap();
        assert!(matches!(claim, DesktopInstanceClaim::Primary(_)));
    }

    #[test]
    fn canonical_home_aliases_share_one_instance() {
        let root = tempfile::tempdir().unwrap();
        let direct = launch(root.path());
        let alias = launch(&root.path().join("."));
        let DesktopInstanceClaim::Primary(primary) =
            direct.claim_instance(DesktopLaunchIntent::Focus).unwrap()
        else {
            panic!("first launch must own the instance")
        };

        assert!(matches!(
            alias.claim_instance(DesktopLaunchIntent::Focus).unwrap(),
            DesktopInstanceClaim::Forwarded
        ));
        drop(primary);
    }

    #[test]
    fn concurrent_launch_race_elects_one_primary() {
        const LAUNCHES: usize = 6;
        let root = tempfile::tempdir().unwrap();
        let home = root.path().to_owned();
        let start = Arc::new(Barrier::new(LAUNCHES + 1));
        let finish = Arc::new(Barrier::new(LAUNCHES + 1));
        let (sender, receiver) = mpsc::channel();
        let threads = (0..LAUNCHES)
            .map(|_| {
                let home = home.clone();
                let start = start.clone();
                let finish = finish.clone();
                let sender = sender.clone();
                thread::spawn(move || {
                    start.wait();
                    let claim = launch(&home)
                        .claim_instance(DesktopLaunchIntent::Focus)
                        .unwrap();
                    sender
                        .send(matches!(claim, DesktopInstanceClaim::Primary(_)))
                        .unwrap();
                    finish.wait();
                    drop(claim);
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        let primary_count = (0..LAUNCHES)
            .map(|_| receiver.recv_timeout(IO_TIMEOUT).unwrap())
            .filter(|primary| *primary)
            .count();
        finish.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(primary_count, 1);
    }

    #[test]
    fn hostile_payload_is_rejected_without_entering_the_intent_queue() {
        let root = tempfile::tempdir().unwrap();
        let launch = launch(root.path());
        let DesktopInstanceClaim::Primary(primary) =
            launch.claim_instance(DesktopLaunchIntent::Focus).unwrap()
        else {
            panic!("first launch must own the instance")
        };
        let mut stream = TcpStream::connect_timeout(&primary.endpoint, IO_TIMEOUT).unwrap();
        stream
            .write_all(
                br#"{"version":1,"capability":"wrong","intent":{"kind":"focus"},"command":"rm"}"#,
            )
            .unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let response: ForwardResponse = serde_json::from_slice(&read_bounded(&mut stream).unwrap())
            .expect("bounded rejection response");
        assert!(!response.accepted);
        assert!(primary.try_next().is_none());
    }

    #[test]
    fn bounded_reader_preserves_a_complete_frame_before_windows_reset() {
        struct ResetAfterFrame {
            frame: Cursor<Vec<u8>>,
        }

        impl io::Read for ResetAfterFrame {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let read = self.frame.read(buffer)?;
                if read == 0 {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "fixture peer reset",
                    ))
                } else {
                    Ok(read)
                }
            }
        }

        let expected = br#"{"version":1,"accepted":false}"#.to_vec();
        let mut reader = ResetAfterFrame {
            frame: Cursor::new(expected.clone()),
        };

        assert_eq!(read_bounded(&mut reader).unwrap(), expected);
    }

    #[test]
    fn navigation_parser_accepts_only_named_destinations() {
        assert_eq!(
            DesktopNavigationTarget::parse("activity"),
            Some(DesktopNavigationTarget::Activity)
        );
        assert_eq!(DesktopNavigationTarget::parse("../secrets"), None);
        assert_eq!(DesktopNavigationTarget::parse("https://example.com"), None);
    }
}
