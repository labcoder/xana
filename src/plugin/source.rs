//! Bounded source acquisition and content-addressed tree handling.

use super::{PackageSource, PluginError, SourceIdentity};
use std::{
    collections::{HashSet, VecDeque},
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

const MAX_PACKAGE_FILES: usize = 8_192;
const MAX_PACKAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PACKAGE_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PACKAGE_DEPTH: usize = 32;
const MAX_GIT_TREE_OUTPUT: usize = 2 * 1024 * 1024;
const STAGING_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(super) struct AcquiredSource {
    pub(super) root: PathBuf,
    pub(super) identity: SourceIdentity,
    pub(super) staging: Option<StagingTree>,
}

pub(super) struct StagingTree {
    directory: PathBuf,
    staging_root: PathBuf,
    keep: bool,
}

impl StagingTree {
    pub(super) fn commit_bundle(mut self, destination: &Path) -> Result<(), PluginError> {
        let source = self.directory.join("bundle");
        let parent = destination.parent().ok_or_else(|| {
            PluginError::Invalid("managed plugin destination has no parent".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(|source| PluginError::Io {
            path: parent.to_owned(),
            source,
        })?;
        if destination.exists() {
            self.keep = false;
            return Ok(());
        }
        if let Err(source_error) = fs::rename(&source, destination)
            && !destination.exists()
        {
            return Err(PluginError::Io {
                path: destination.to_owned(),
                source: source_error,
            });
        }
        self.keep = false;
        Ok(())
    }
}

impl Drop for StagingTree {
    fn drop(&mut self) {
        if self.keep || !self.directory.starts_with(&self.staging_root) {
            return;
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub(super) fn acquire(
    source: &PackageSource,
    package_store: &Path,
    copy_local: bool,
) -> Result<AcquiredSource, PluginError> {
    cleanup_stale_staging(package_store)?;
    match source {
        PackageSource::Directory(path) if copy_local => {
            let staging = create_staging(package_store)?;
            let root = staging.directory.join("bundle");
            copy_tree(path, &root)?;
            Ok(AcquiredSource {
                root,
                identity: SourceIdentity::directory(path)?,
                staging: Some(staging),
            })
        }
        PackageSource::Directory(path) => Ok(AcquiredSource {
            root: canonical_root(path)?,
            identity: SourceIdentity::directory(path)?,
            staging: None,
        }),
        PackageSource::Linked(path) => Ok(AcquiredSource {
            root: canonical_root(path)?,
            identity: SourceIdentity::linked(path)?,
            staging: None,
        }),
        PackageSource::Git { url, revision } => {
            let url = validate_git_source(url, revision)?;
            let staging = create_staging(package_store)?;
            let root = staging.directory.join("bundle");
            acquire_git(&url, revision, &staging.directory, &root)?;
            Ok(AcquiredSource {
                root,
                identity: SourceIdentity::git(&url, revision),
                staging: Some(staging),
            })
        }
    }
}

pub(super) fn tree_digest(root: &Path) -> Result<String, PluginError> {
    let root = canonical_root(root)?;
    let entries = bounded_entries(&root)?;
    let mut hasher = blake3::Hasher::new();
    for entry in entries {
        let relative = entry.strip_prefix(&root).map_err(|_| {
            PluginError::Invalid("plugin entry escaped its canonical root".to_owned())
        })?;
        let normalized = normalized_relative(relative)?;
        let metadata = fs::symlink_metadata(&entry).map_err(|source| PluginError::Io {
            path: entry.clone(),
            source,
        })?;
        if metadata.is_dir() {
            hasher.update(b"directory\0");
            hasher.update(normalized.as_bytes());
            hasher.update(b"\0");
            continue;
        }
        hasher.update(b"file\0");
        hasher.update(normalized.as_bytes());
        hasher.update(b"\0");
        let mut file = fs::File::open(&entry).map_err(|source| PluginError::Io {
            path: entry.clone(),
            source,
        })?;
        let mut buffer = [0_u8; 32 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|source| PluginError::Io {
                path: entry.clone(),
                source,
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        hasher.update(b"\0");
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn create_staging(package_store: &Path) -> Result<StagingTree, PluginError> {
    let staging_root = package_store.join(".staging");
    fs::create_dir_all(&staging_root).map_err(|source| PluginError::Io {
        path: staging_root.clone(),
        source,
    })?;
    let directory = staging_root.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&directory).map_err(|source| PluginError::Io {
        path: directory.clone(),
        source,
    })?;
    Ok(StagingTree {
        directory,
        staging_root,
        keep: false,
    })
}

fn cleanup_stale_staging(package_store: &Path) -> Result<(), PluginError> {
    let root = package_store.join(".staging");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(PluginError::Io { path: root, source }),
    };
    let now = SystemTime::now();
    for entry in entries {
        let entry = entry.map_err(|source| PluginError::Io {
            path: root.clone(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| PluginError::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STAGING_RETENTION);
        if stale && path.starts_with(&root) {
            fs::remove_dir_all(&path).map_err(|source| PluginError::Io { path, source })?;
        }
    }
    Ok(())
}

fn canonical_root(path: &Path) -> Result<PathBuf, PluginError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PluginError::Io {
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PluginError::Invalid(format!(
            "plugin root {} is not a regular directory",
            super::manifest::safe_path(path)
        )));
    }
    path.canonicalize().map_err(|source| PluginError::Io {
        path: path.to_owned(),
        source,
    })
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), PluginError> {
    let source = canonical_root(source)?;
    fs::create_dir(destination).map_err(|source_error| PluginError::Io {
        path: destination.to_owned(),
        source: source_error,
    })?;
    let entries = bounded_entries(&source)?;
    for entry in entries {
        let relative = entry.strip_prefix(&source).map_err(|_| {
            PluginError::Invalid("plugin entry escaped its canonical root".to_owned())
        })?;
        let target = destination.join(relative);
        let metadata = fs::symlink_metadata(&entry).map_err(|source_error| PluginError::Io {
            path: entry.clone(),
            source: source_error,
        })?;
        if metadata.is_dir() {
            fs::create_dir_all(&target).map_err(|source_error| PluginError::Io {
                path: target,
                source: source_error,
            })?;
        } else {
            let before = (metadata.len(), metadata.modified().ok());
            fs::copy(&entry, &target).map_err(|source_error| PluginError::Io {
                path: target.clone(),
                source: source_error,
            })?;
            let after = fs::symlink_metadata(&entry).map_err(|source_error| PluginError::Io {
                path: entry.clone(),
                source: source_error,
            })?;
            if before != (after.len(), after.modified().ok()) {
                return Err(PluginError::Changed(entry));
            }
        }
    }
    Ok(())
}

fn bounded_entries(root: &Path) -> Result<Vec<PathBuf>, PluginError> {
    let mut pending = VecDeque::from([(root.to_owned(), 0_usize)]);
    let mut output = Vec::new();
    let mut files = 0_usize;
    let mut total = 0_usize;
    let mut portable_paths = HashSet::new();
    while let Some((directory, depth)) = pending.pop_front() {
        if depth > MAX_PACKAGE_DEPTH {
            return Err(PluginError::Limit {
                what: "plugin tree depth",
                limit: MAX_PACKAGE_DEPTH,
            });
        }
        let mut children = fs::read_dir(&directory)
            .map_err(|source| PluginError::Io {
                path: directory.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| PluginError::Io {
                path: directory.clone(),
                source,
            })?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            if depth == 0 && child.file_name() == ".git" {
                continue;
            }
            let path = child.path();
            let normalized = normalized_relative(path.strip_prefix(root).map_err(|_| {
                PluginError::Invalid("plugin entry escaped its source root".to_owned())
            })?)?;
            if !portable_paths.insert(normalized.to_lowercase()) {
                return Err(PluginError::UnsafePath(path));
            }
            let metadata = fs::symlink_metadata(&path).map_err(|source| PluginError::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() {
                return Err(PluginError::UnsafePath(path));
            }
            if metadata.is_dir() {
                output.push(path.clone());
                pending.push_back((path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                return Err(PluginError::UnsafePath(path));
            }
            files += 1;
            total = total.saturating_add(metadata.len() as usize);
            if files > MAX_PACKAGE_FILES {
                return Err(PluginError::Limit {
                    what: "plugin files",
                    limit: MAX_PACKAGE_FILES,
                });
            }
            if metadata.len() > MAX_PACKAGE_FILE_BYTES as u64 {
                return Err(PluginError::Limit {
                    what: "plugin file bytes",
                    limit: MAX_PACKAGE_FILE_BYTES,
                });
            }
            if total > MAX_PACKAGE_BYTES {
                return Err(PluginError::Limit {
                    what: "plugin tree bytes",
                    limit: MAX_PACKAGE_BYTES,
                });
            }
            output.push(path);
        }
    }
    output.sort_by(|left, right| {
        normalized_relative(left.strip_prefix(root).unwrap_or(left))
            .unwrap_or_default()
            .cmp(
                &normalized_relative(right.strip_prefix(root).unwrap_or(right)).unwrap_or_default(),
            )
    });
    Ok(output)
}

fn normalized_relative(path: &Path) -> Result<String, PluginError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().count() > MAX_PACKAGE_DEPTH
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PluginError::UnsafePath(path.to_owned()));
    }
    let mut output = String::new();
    for (index, component) in path.components().enumerate() {
        let Component::Normal(part) = component else {
            unreachable!()
        };
        let part = part
            .to_str()
            .ok_or_else(|| PluginError::UnsafePath(path.to_owned()))?;
        if !portable_component(part) {
            return Err(PluginError::UnsafePath(path.to_owned()));
        }
        if index > 0 {
            output.push('/');
        }
        output.push_str(part);
    }
    Ok(output)
}

fn portable_component(part: &str) -> bool {
    if part.is_empty()
        || !part.is_ascii()
        || part.ends_with(['.', ' '])
        || part
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        return false;
    }
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) && !matches!(
        stem.strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT")),
        Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
    )
}

fn validate_git_source(url: &str, revision: &str) -> Result<String, PluginError> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PluginError::Invalid(
            "Git plugin revisions must be exact 40-character commit IDs".to_owned(),
        ));
    }
    if url.len() > 4_096 || url.chars().any(char::is_control) {
        return Err(PluginError::Invalid("Git plugin URL is invalid".to_owned()));
    }
    let https = url.starts_with("https://");
    let local_test = cfg!(test) && (url.starts_with("file://") || Path::new(url).is_absolute());
    if !https && !local_test {
        return Err(PluginError::Invalid(
            "Git plugin sources must use an explicit HTTPS URL".to_owned(),
        ));
    }
    if local_test {
        return Ok(url.to_owned());
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| PluginError::Invalid("Git plugin URL is invalid".to_owned()))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(PluginError::Invalid(
            "Git plugin URLs must be credential-free HTTPS origins without query or fragment data"
                .to_owned(),
        ));
    }
    Ok(parsed.to_string())
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    command.env_clear();
    for name in ["PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("LC_ALL", "C");
    command
}

fn acquire_git(
    url: &str,
    revision: &str,
    staging: &Path,
    destination: &Path,
) -> Result<(), PluginError> {
    let repository = staging.join("repository.git");
    run_git(
        git_command().arg("init").arg("--bare").arg(&repository),
        "initialize temporary repository",
    )?;
    let mut fetch = git_command();
    fetch
        .arg("-C")
        .arg(&repository)
        .args(["-c", "core.hooksPath="])
        .args(["-c", "submodule.recurse=false"])
        .args([
            "-c",
            if cfg!(test) {
                "protocol.file.allow=always"
            } else {
                "protocol.file.allow=never"
            },
        ])
        .args(["fetch", "--depth=1", "--no-tags", "--no-recurse-submodules"])
        .arg("--")
        .arg(url)
        .arg(revision);
    run_git(&mut fetch, "fetch exact plugin revision")?;
    let resolved = git_output(
        git_command()
            .arg("-C")
            .arg(&repository)
            .args(["rev-parse", "FETCH_HEAD^{commit}"]),
        256,
        "resolve fetched plugin revision",
    )?;
    if !resolved.trim().eq_ignore_ascii_case(revision) {
        return Err(PluginError::RevisionDrift {
            expected: revision.to_owned(),
            actual: resolved.trim().to_owned(),
        });
    }
    let tree = git_output(
        git_command().arg("-C").arg(&repository).args([
            "ls-tree",
            "-r",
            "--full-tree",
            "FETCH_HEAD",
        ]),
        MAX_GIT_TREE_OUTPUT,
        "inspect plugin tree",
    )?;
    if tree.lines().any(|line| line.starts_with("160000 commit ")) {
        return Err(PluginError::Invalid(
            "Git plugin source contains a submodule, which Xana does not acquire".to_owned(),
        ));
    }
    fs::create_dir(destination).map_err(|source| PluginError::Io {
        path: destination.to_owned(),
        source,
    })?;
    let mut command = git_command();
    command
        .arg("-C")
        .arg(&repository)
        .args(["archive", "--format=tar", "FETCH_HEAD"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_process_group(&mut command);
    let mut child = command.spawn().map_err(|source| PluginError::Process {
        action: "archive exact plugin revision",
        source,
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PluginError::Invalid("Git archive stdout was not available".to_owned()))?;
    let destination = destination.to_owned();
    let extraction = thread::spawn(move || extract_tar(stdout, &destination));
    let status = wait_for_child(
        &mut child,
        "archive exact plugin revision",
        GIT_COMMAND_TIMEOUT,
    );
    let extracted = extraction
        .join()
        .map_err(|_| PluginError::Invalid("Git archive extraction worker panicked".to_owned()))?;
    let status = status?;
    extracted?;
    if !status.success() {
        return Err(PluginError::ProcessFailed("archive exact plugin revision"));
    }
    Ok(())
}

fn extract_tar(reader: impl Read, destination: &Path) -> Result<(), PluginError> {
    let mut archive = tar::Archive::new(reader);
    let mut files = 0_usize;
    let mut total = 0_usize;
    let mut portable_paths = HashSet::new();
    let entries = archive
        .entries()
        .map_err(|source| PluginError::Archive(source.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|source| PluginError::Archive(source.to_string()))?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions()
            || kind.is_pax_local_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink()
        {
            continue;
        }
        let relative = entry
            .path()
            .map_err(|source| PluginError::Archive(source.to_string()))?;
        let normalized = normalized_relative(&relative)?;
        if !portable_paths.insert(normalized.to_lowercase()) {
            return Err(PluginError::UnsafePath(relative.into_owned()));
        }
        if kind.is_dir() {
            fs::create_dir_all(destination.join(&relative)).map_err(|source| PluginError::Io {
                path: destination.join(&relative),
                source,
            })?;
            continue;
        }
        if !kind.is_file() {
            return Err(PluginError::UnsafePath(relative.into_owned()));
        }
        let size = entry.size() as usize;
        files += 1;
        total = total.saturating_add(size);
        if files > MAX_PACKAGE_FILES || size > MAX_PACKAGE_FILE_BYTES || total > MAX_PACKAGE_BYTES {
            return Err(PluginError::Limit {
                what: "Git plugin archive",
                limit: MAX_PACKAGE_BYTES,
            });
        }
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| PluginError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|source| PluginError::Io {
                path: target.clone(),
                source,
            })?;
        io::copy(&mut entry, &mut file).map_err(|source| PluginError::Io {
            path: target,
            source,
        })?;
    }
    Ok(())
}

fn run_git(command: &mut Command, action: &'static str) -> Result<(), PluginError> {
    run_git_with_timeout(command, action, GIT_COMMAND_TIMEOUT)
}

fn run_git_with_timeout(
    command: &mut Command,
    action: &'static str,
    timeout: Duration,
) -> Result<(), PluginError> {
    configure_process_group(command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| PluginError::Process { action, source })?;
    let status = wait_for_child(&mut child, action, timeout)?;
    if status.success() {
        Ok(())
    } else {
        Err(PluginError::ProcessFailed(action))
    }
}

fn git_output(
    command: &mut Command,
    limit: usize,
    action: &'static str,
) -> Result<String, PluginError> {
    configure_process_group(command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| PluginError::Process { action, source })?;
    let mut stdout = child.stdout.take().ok_or_else(|| {
        PluginError::Invalid(format!(
            "Git output was unavailable while trying to {action}"
        ))
    })?;
    let output = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok::<_, io::Error>(bytes)
    });
    let status = wait_for_child(&mut child, action, GIT_COMMAND_TIMEOUT);
    let bytes = output
        .join()
        .map_err(|_| PluginError::Invalid("Git output reader panicked".to_owned()))?
        .map_err(|source| PluginError::Process { action, source })?;
    let status = status?;
    if bytes.len() > limit {
        return Err(PluginError::Limit {
            what: "Git metadata output",
            limit,
        });
    }
    if !status.success() {
        return Err(PluginError::ProcessFailed(action));
    }
    String::from_utf8(bytes).map_err(|_| {
        PluginError::Invalid(format!(
            "Git returned non-UTF-8 output while trying to {action}"
        ))
    })
}

fn wait_for_child(
    child: &mut std::process::Child,
    action: &'static str,
    timeout: Duration,
) -> Result<std::process::ExitStatus, PluginError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(source) => {
                kill_process_tree(child);
                let _ = child.wait();
                return Err(PluginError::Process { action, source });
            }
        }
        if Instant::now() >= deadline {
            kill_process_tree(child);
            let _ = child.wait();
            return Err(PluginError::ProcessTimeout(action));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_process_tree(child: &mut std::process::Child) {
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{}", child.id())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

#[cfg(windows)]
fn kill_process_tree(child: &mut std::process::Child) {
    let _ = Command::new("taskkill.exe")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

#[cfg(not(any(unix, windows)))]
fn kill_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_components_reject_cross_platform_aliases_and_reserved_names() {
        for rejected in [
            "CON",
            "nul.txt",
            "COM1",
            "COM¹.txt",
            "LPT9.log",
            "LPT²",
            "CONIN$",
            "CONOUT$.txt",
            "caf\u{e9}.md",
            "cafe\u{301}.md",
            "trailing.",
            "trailing ",
            "colon:name",
            "back\\slash",
        ] {
            assert!(!portable_component(rejected), "accepted {rejected:?}");
        }
        assert!(portable_component("portable-name.toml"));
    }

    #[test]
    fn git_sources_reject_credentials_and_ambiguous_url_data() {
        let revision = "a".repeat(40);
        for rejected in [
            "https://user@example.com/plugin.git",
            "https://user:secret@example.com/plugin.git",
            "https://example.com/plugin.git?token=secret",
            "https://example.com/plugin.git#revision",
        ] {
            assert!(validate_git_source(rejected, &revision).is_err());
        }
        assert_eq!(
            validate_git_source("https://example.com/plugin.git", &revision).unwrap(),
            "https://example.com/plugin.git"
        );
    }

    #[test]
    fn git_commands_disable_ambient_configuration_and_prompts() {
        let command = git_command();
        let environment = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment.get("GIT_CONFIG_NOSYSTEM"),
            Some(&Some("1".to_owned()))
        );
        assert_eq!(
            environment.get("GIT_TERMINAL_PROMPT"),
            Some(&Some("0".to_owned()))
        );
        assert_eq!(
            environment.get("GCM_INTERACTIVE"),
            Some(&Some("Never".to_owned()))
        );
    }

    #[test]
    fn archive_rejects_paths_that_alias_on_case_insensitive_filesystems() {
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            for name in ["Skills/guide.md", "skills/guide.md"] {
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_size(1);
                header.set_cksum();
                archive
                    .append_data(&mut header, name, b"x" as &[u8])
                    .unwrap();
            }
            archive.finish().unwrap();
        }
        let destination = tempfile::tempdir().unwrap();
        assert!(matches!(
            extract_tar(std::io::Cursor::new(bytes), destination.path()),
            Err(PluginError::UnsafePath(_))
        ));
    }

    #[test]
    fn child_deadline_kills_the_owned_process() {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd.exe");
            command.args(["/D", "/S", "/C", "ping -n 30 127.0.0.1 >NUL"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 30"]);
            command
        };
        let error = run_git_with_timeout(
            &mut command,
            "run deadline fixture",
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(matches!(error, PluginError::ProcessTimeout(_)));
    }
}
