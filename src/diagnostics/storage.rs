//! Filesystem confinement, retention, and bounded diagnostic decoding.

use super::*;

pub(super) fn ensure_private_directory(path: &Path) -> Result<()> {
    reject_ambiguous_path(path)?;
    fs::create_dir_all(path)
        .with_context(|| format!("could not create diagnostic directory {}", path.display()))?;
    if path_has_symlink(path)? {
        bail!("diagnostic directory may not contain a symlink component");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(super) fn create_private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("could not create diagnostic file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn reject_ambiguous_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        bail!("diagnostic path must be absolute and normalized");
    }
    Ok(())
}

pub(super) fn path_has_symlink(path: &Path) -> io::Result<bool> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

pub(super) fn unique_file_path(directory: &Path, stem: &str, extension: &str) -> Result<PathBuf> {
    for suffix in 0..64 {
        let name = if suffix == 0 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{suffix}.{extension}")
        };
        let path = directory.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Ok(directory.join(format!("{stem}-{}.{}", Uuid::new_v4(), extension)))
}

pub(super) fn cleanup_logs(directory: &Path, settings: &DiagnosticsConfig) -> Result<()> {
    cleanup_recognized(directory, settings, "jsonl", "xana-")
}

pub(super) fn cleanup_crashes(directory: &Path, settings: &DiagnosticsConfig) -> Result<()> {
    cleanup_recognized(directory, settings, "json", "crash-")
}

pub(super) fn cleanup_stale_markers(directory: &Path, settings: &DiagnosticsConfig) -> Result<()> {
    let now = SystemTime::now();
    let retention = Duration::from_secs(u64::from(settings.retention_days) * 24 * 60 * 60);
    let mut stale = Vec::new();
    for path in collect_directory_paths(directory)? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("run-")
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                let modified = metadata.modified().ok();
                let expired = modified
                    .and_then(|value| now.duration_since(value).ok())
                    .is_some_and(|age| age > retention);
                let _ = FileExt::unlock(&file);
                if expired {
                    let _ = fs::remove_file(&path);
                } else {
                    stale.push((modified, path));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => {}
        }
    }
    stale.sort_by_key(|(modified, _)| *modified);
    let excess = stale.len().saturating_sub(256);
    for (_, path) in stale.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn cleanup_recognized(
    directory: &Path,
    settings: &DiagnosticsConfig,
    extension: &str,
    prefix: &str,
) -> Result<()> {
    let mut files = collect_regular_files(directory, extension)?;
    retain_recognized(&mut files, prefix);
    let now = SystemTime::now();
    let retention = Duration::from_secs(u64::from(settings.retention_days) * 24 * 60 * 60);
    for file in &files {
        if file
            .modified
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > retention)
        {
            let _ = fs::remove_file(&file.path);
        }
    }
    files = collect_regular_files(directory, extension)?;
    retain_recognized(&mut files, prefix);
    files.sort_by_key(|file| file.modified);
    let mut total = files.iter().map(|file| file.bytes).sum::<u64>();
    let mut count = files.len();
    for file in files {
        if count <= usize::from(settings.max_files) && total <= settings.max_total_bytes {
            break;
        }
        if fs::remove_file(&file.path).is_ok() {
            total = total.saturating_sub(file.bytes);
            count = count.saturating_sub(1);
        }
    }
    Ok(())
}

fn retain_recognized(files: &mut Vec<RegularFile>, prefix: &str) {
    files.retain(|file| {
        file.path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with(prefix))
    });
}

struct RegularFile {
    path: PathBuf,
    bytes: u64,
    modified: Option<SystemTime>,
}

fn collect_regular_files(directory: &Path, extension: &str) -> Result<Vec<RegularFile>> {
    let mut files = Vec::new();
    for path in collect_directory_paths(directory)? {
        if path.extension().and_then(|value| value.to_str()) != Some(extension) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        files.push(RegularFile {
            path,
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
        });
    }
    Ok(files)
}

fn collect_directory_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        if index >= MAX_DIRECTORY_ENTRIES {
            bail!(
                "diagnostic directory exceeds its {}-entry inspection bound",
                MAX_DIRECTORY_ENTRIES
            );
        }
        paths.push(entry?.path());
    }
    Ok(paths)
}

pub(super) fn count_stale_markers(directory: &Path) -> Result<usize> {
    if !directory.exists() {
        return Ok(0);
    }
    let mut stale = 0;
    for path in collect_directory_paths(directory)? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("run-")
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                stale += 1;
                let _ = FileExt::unlock(&file);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => stale += 1,
        }
    }
    Ok(stale)
}

pub(super) fn count_invalid_crash_reports(directory: &Path) -> Result<usize> {
    let mut invalid = 0;
    for file in collect_regular_files(directory, "json")? {
        let Some(name) = file.path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("crash-") {
            continue;
        }
        if file.bytes > 1024 * 1024
            || File::open(&file.path)
                .ok()
                .and_then(|reader| serde_json::from_reader::<_, CrashReport>(reader).ok())
                .is_none()
        {
            invalid += 1;
        }
    }
    Ok(invalid)
}

pub(super) fn collect_entries(
    directory: &Path,
    kind: &'static str,
    extension: &str,
    output: &mut Vec<DiagnosticEntry>,
) -> Result<()> {
    for file in collect_regular_files(directory, extension)? {
        let Some(name) = file.path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if (kind == "log" && !name.starts_with("xana-"))
            || (kind == "crash" && !name.starts_with("crash-"))
        {
            continue;
        }
        output.push(DiagnosticEntry {
            name: name.to_owned(),
            kind,
            bytes: file.bytes,
            modified_ms: file.modified.and_then(|value| {
                value
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|value| value.as_millis().try_into().ok())
            }),
        });
    }
    Ok(())
}

pub(super) fn resolve_entry(logs: &Path, crashes: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
        || name.contains(['/', '\\'])
    {
        bail!("diagnostic entry must be one exact listed filename");
    }
    let root = if name.ends_with(".jsonl") {
        logs
    } else {
        crashes
    };
    let path = root.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("diagnostic entry is not a regular file");
    }
    Ok(path)
}

pub(super) fn read_log_records(path: &Path, limit: usize) -> Result<Vec<String>> {
    let file = File::open(path)?;
    let mut retained = VecDeque::with_capacity(limit);
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    for _ in 0..100_000 {
        let Some(meta) = read_bounded_line(&mut reader, &mut line)? else {
            break;
        };
        if !meta.within_limit {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<DiagnosticRecord>(&line) else {
            continue;
        };
        let encoded = serde_json::to_string(&record)?;
        if retained.len() == limit {
            retained.pop_front();
        }
        retained.push_back(encoded);
    }
    Ok(retained.into())
}

pub(super) struct BoundedLine {
    pub(super) consumed: usize,
    pub(super) terminated: bool,
    pub(super) within_limit: bool,
}

pub(super) fn read_bounded_line(
    reader: &mut impl BufRead,
    output: &mut Vec<u8>,
) -> io::Result<Option<BoundedLine>> {
    output.clear();
    let mut consumed = 0;
    let mut within_limit = true;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if consumed == 0 {
                return Ok(None);
            }
            return Ok(Some(BoundedLine {
                consumed,
                terminated: false,
                within_limit,
            }));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        let payload = newline.map_or(take, |index| index);
        if within_limit {
            if output.len().saturating_add(payload) > MAX_LINE_BYTES {
                output.clear();
                within_limit = false;
            } else {
                output.extend_from_slice(&available[..payload]);
            }
        }
        reader.consume(take);
        consumed += take;
        if newline.is_some() {
            if output.last() == Some(&b'\r') {
                output.pop();
            }
            return Ok(Some(BoundedLine {
                consumed,
                terminated: true,
                within_limit,
            }));
        }
    }
}

pub(super) fn read_crash_record(path: &Path) -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(1024 * 1024)
        .read_to_end(&mut bytes)?;
    let report: CrashReport =
        serde_json::from_slice(&bytes).context("selected crash report is invalid or truncated")?;
    Ok(vec![serde_json::to_string_pretty(&report)?])
}
