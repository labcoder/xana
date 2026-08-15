//! Bounded, metadata-only process diagnostics and crash reports.

use crate::{
    config::{DiagnosticLevel, DiagnosticTarget, DiagnosticsConfig, XanaConfig},
    paths::XanaPaths,
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    backtrace::Backtrace,
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub(crate) fn runtime_telemetry() -> Arc<dyn crate::telemetry::RuntimeTelemetry> {
    Arc::new(DiagnosticTelemetry)
}

pub(crate) fn outbound_audit(
    paths: &XanaPaths,
) -> Result<Arc<dyn crate::outbound::OutboundAuditObserver>> {
    Ok(Arc::new(DiagnosticOutboundAudit {
        journal: crate::outbound::OutboundAuditJournal::open(paths)?,
    }))
}

struct DiagnosticOutboundAudit {
    journal: crate::outbound::OutboundAuditJournal,
}

impl crate::outbound::OutboundAuditObserver for DiagnosticOutboundAudit {
    fn observe(
        &self,
        event: &crate::outbound::OutboundAuditEvent,
    ) -> std::result::Result<(), crate::private_state::PrivateStateError> {
        if let Err(error) = self.journal.append(event) {
            emit(
                DiagnosticFact::new(
                    DiagnosticLevel::Error,
                    DiagnosticTarget::Security,
                    EventKind::IntegrationFailed,
                    EventOutcome::Failed,
                )
                .subject("outbound_audit_journal")
                .correlation(event.operation_id.to_string()),
            );
            return Err(error);
        }
        use crate::outbound::OutboundAuditStage;
        let (level, outcome) = match event.stage {
            OutboundAuditStage::Requested | OutboundAuditStage::Sending => {
                (DiagnosticLevel::Debug, EventOutcome::Started)
            }
            OutboundAuditStage::Allowed { .. } | OutboundAuditStage::Succeeded => {
                (DiagnosticLevel::Info, EventOutcome::Completed)
            }
            OutboundAuditStage::Denied { .. } => (DiagnosticLevel::Warn, EventOutcome::Denied),
            OutboundAuditStage::Cancelled => (DiagnosticLevel::Info, EventOutcome::Cancelled),
            OutboundAuditStage::Failed => (DiagnosticLevel::Warn, EventOutcome::Failed),
        };
        let mut fact = DiagnosticFact::new(
            level,
            DiagnosticTarget::Security,
            EventKind::OutboundTransfer,
            outcome,
        )
        .subject(format!(
            "{:?}:{}:{}:{:?}",
            event.recipient_kind,
            event.recipient_connection,
            event.recipient_identity_digest,
            event.stage
        ))
        .correlation(event.operation_id.to_string());
        fact.size_bytes = Some(event.total_bytes as u64);
        emit(fact);
        Ok(())
    }
}

struct DiagnosticTelemetry;

impl crate::telemetry::RuntimeTelemetry for DiagnosticTelemetry {
    fn record(&self, event: crate::telemetry::RuntimeTelemetryEvent) {
        use crate::telemetry::RuntimeTelemetryKind;
        let (target, kind, outcome) = match event.kind {
            RuntimeTelemetryKind::ProviderFailed => (
                DiagnosticTarget::Provider,
                EventKind::ProviderFailed,
                EventOutcome::Failed,
            ),
            RuntimeTelemetryKind::ToolDenied => (
                DiagnosticTarget::Tool,
                EventKind::ToolDenied,
                EventOutcome::Denied,
            ),
            RuntimeTelemetryKind::ToolFailed => (
                DiagnosticTarget::Tool,
                EventKind::ToolFailed,
                EventOutcome::Failed,
            ),
            RuntimeTelemetryKind::StorageFailed => (
                DiagnosticTarget::Runtime,
                EventKind::StorageFailed,
                EventOutcome::Failed,
            ),
        };
        emit(
            DiagnosticFact::new(DiagnosticLevel::Warn, target, kind, outcome)
                .subject(event.subject)
                .correlation(event.operation_id.to_string()),
        );
    }
}

const RECORD_VERSION: u32 = 1;
const CRASH_VERSION: u32 = 1;
const HEALTH_VERSION: u32 = 1;
const SUPPORT_BUNDLE_VERSION: u32 = 1;
const MAX_BREADCRUMBS: usize = 64;
const MAX_DIRECTORY_ENTRIES: usize = 1_024;
const MAX_SHOW_LINES: usize = 1_000;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_SUPPORT_BYTES: usize = 8 * 1024 * 1024;
const SHUTDOWN_WAIT: Duration = Duration::from_millis(750);

static ACTIVE: OnceLock<RwLock<Option<Weak<ActiveDiagnostics>>>> = OnceLock::new();
static PANIC_HOOK: OnceLock<()> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventKind {
    ApplicationStarted,
    ApplicationStopped,
    ApplicationFailed,
    ProviderFailed,
    ManagedRuntimeExited,
    ToolDenied,
    ToolFailed,
    FrontendDisconnected,
    StorageFailed,
    IntegrationFailed,
    OutboundTransfer,
    TaskPanicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventOutcome {
    Started,
    Completed,
    Failed,
    Denied,
    Cancelled,
    Unavailable,
}

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticFact {
    pub(crate) level: DiagnosticLevel,
    pub(crate) target: DiagnosticTarget,
    pub(crate) kind: EventKind,
    pub(crate) outcome: EventOutcome,
    pub(crate) correlation_id: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) size_bytes: Option<u64>,
}

impl DiagnosticFact {
    pub(crate) fn new(
        level: DiagnosticLevel,
        target: DiagnosticTarget,
        kind: EventKind,
        outcome: EventOutcome,
    ) -> Self {
        Self {
            level,
            target,
            kind,
            outcome,
            correlation_id: None,
            subject: None,
            duration_ms: None,
            size_bytes: None,
        }
    }

    pub(crate) fn subject(mut self, value: impl AsRef<str>) -> Self {
        self.subject = Some(sanitize_identifier(value.as_ref()));
        self
    }

    pub(crate) fn correlation(mut self, value: impl AsRef<str>) -> Self {
        self.correlation_id = Some(sanitize_identifier(value.as_ref()));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagnosticRecord {
    version: u32,
    timestamp_ms: u64,
    sequence: u64,
    pid: u32,
    level: DiagnosticLevel,
    target: DiagnosticTarget,
    kind: EventKind,
    outcome: EventOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    dropped_before: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrashReport {
    version: u32,
    timestamp_ms: u64,
    xana_version: String,
    os: String,
    architecture: String,
    pid: u32,
    kind: EventKind,
    thread: String,
    location_hash: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    backtrace_hash: String,
    backtrace_lines: usize,
    breadcrumbs: Vec<DiagnosticRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticEntry {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) bytes: u64,
    pub(crate) modified_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticHealth {
    pub(crate) log_dir: PathBuf,
    pub(crate) crash_dir: PathBuf,
    pub(crate) enabled: bool,
    pub(crate) log_dir_exists: bool,
    pub(crate) crash_dir_exists: bool,
    pub(crate) directory_metadata_allows_writes: bool,
    pub(crate) permissions_private: bool,
    pub(crate) retention_compliant: bool,
    pub(crate) retained_files: usize,
    pub(crate) retained_bytes: u64,
    pub(crate) unsafe_path: bool,
    pub(crate) inspection_error: bool,
    pub(crate) stale_markers: usize,
    pub(crate) invalid_reports: usize,
    pub(crate) dropped_events: u64,
    pub(crate) writer_faults: u64,
}

struct ActiveDiagnostics {
    sender: SyncSender<WriterMessage>,
    dropped: AtomicU64,
    dropped_total: AtomicU64,
    writer_faults: Arc<AtomicU64>,
    sequence: AtomicU64,
    settings: DiagnosticsConfig,
    crash_dir: PathBuf,
    breadcrumbs: Mutex<VecDeque<DiagnosticRecord>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticHealthSummary {
    version: u32,
    timestamp_ms: u64,
    pid: u32,
    dropped_events: u64,
    writer_faults: u64,
}

enum WriterMessage {
    Record(Vec<u8>),
    Shutdown(mpsc::SyncSender<()>),
}

pub(crate) struct DiagnosticRuntime {
    active: Arc<ActiveDiagnostics>,
    writer: Option<thread::JoinHandle<()>>,
    marker_path: PathBuf,
    marker_file: Option<File>,
    stale_markers: usize,
}

impl DiagnosticRuntime {
    pub(crate) fn start(paths: &XanaPaths) -> Result<Option<Self>> {
        let settings = load_settings(paths);
        if !settings.enabled {
            return Ok(None);
        }
        let (log_dir, crash_dir) = resolve_directories(paths, &settings)?;
        ensure_private_directory(&log_dir)?;
        ensure_private_directory(&crash_dir)?;
        let stale_markers = count_stale_markers(&crash_dir)?;
        cleanup_logs(&log_dir, &settings)?;
        cleanup_crashes(&crash_dir, &settings)?;
        cleanup_health_summaries(&crash_dir, &settings)?;
        cleanup_stale_markers(&crash_dir, &settings)?;
        let timestamp = now_ms();
        let pid = std::process::id();
        let log_path = unique_file_path(&log_dir, &format!("xana-{timestamp}-{pid}"), "jsonl")?;
        let file = create_private_file(&log_path)?;
        file.try_lock_exclusive()
            .context("could not lock active Xana diagnostic log")?;
        let marker_path = unique_file_path(&crash_dir, &format!("run-{timestamp}-{pid}"), "json")?;
        let mut marker_file = create_private_file(&marker_path)?;
        marker_file
            .try_lock_exclusive()
            .context("could not lock Xana run marker")?;
        serde_json::to_writer(
            &mut marker_file,
            &serde_json::json!({
                "version": 1,
                "started_ms": timestamp,
                "pid": pid,
                "xana_version": env!("CARGO_PKG_VERSION"),
            }),
        )?;
        marker_file.flush()?;

        let (sender, receiver) = mpsc::sync_channel(settings.queue_capacity);
        let writer_settings = settings.clone();
        let writer_path = log_path.clone();
        let writer_faults = Arc::new(AtomicU64::new(0));
        let reported_writer_faults = Arc::clone(&writer_faults);
        let writer = thread::Builder::new()
            .name("xana-diagnostics".into())
            .spawn(move || {
                writer_loop(
                    receiver,
                    file,
                    writer_path,
                    writer_settings,
                    reported_writer_faults,
                )
            })
            .context("could not start Xana diagnostics writer")?;
        let active = Arc::new(ActiveDiagnostics {
            sender,
            dropped: AtomicU64::new(0),
            dropped_total: AtomicU64::new(0),
            writer_faults,
            sequence: AtomicU64::new(0),
            settings,
            crash_dir,
            breadcrumbs: Mutex::new(VecDeque::with_capacity(MAX_BREADCRUMBS)),
        });
        *active_slot().write().expect("diagnostics slot poisoned") = Some(Arc::downgrade(&active));
        install_panic_hook();
        Ok(Some(Self {
            active,
            writer: Some(writer),
            marker_path,
            marker_file: Some(marker_file),
            stale_markers,
        }))
    }

    pub(crate) fn stale_markers(&self) -> usize {
        self.stale_markers
    }
}

impl Drop for DiagnosticRuntime {
    fn drop(&mut self) {
        let (ack_sender, ack_receiver) = mpsc::sync_channel(1);
        let deadline = std::time::Instant::now() + SHUTDOWN_WAIT;
        let mut acknowledged = false;
        loop {
            match self
                .active
                .sender
                .try_send(WriterMessage::Shutdown(ack_sender.clone()))
            {
                Ok(()) => {
                    acknowledged = ack_receiver.recv_timeout(SHUTDOWN_WAIT).is_ok();
                    break;
                }
                Err(TrySendError::Full(_)) if std::time::Instant::now() < deadline => {
                    thread::yield_now();
                }
                Err(_) => break,
            }
        }
        if let Some(writer) = self.writer.take()
            && (acknowledged || writer.is_finished())
        {
            let _ = writer.join();
        }
        let dropped_events = self.active.dropped_total.load(Ordering::Relaxed);
        let writer_faults = self.active.writer_faults.load(Ordering::Relaxed);
        let health_persisted = (dropped_events == 0 && writer_faults == 0)
            || write_health_summary(&self.active.crash_dir, dropped_events, writer_faults).is_ok();
        *active_slot().write().expect("diagnostics slot poisoned") = None;
        let clean_shutdown = !thread::panicking();
        if let Some(file) = self.marker_file.take() {
            let _ = FileExt::unlock(&file);
            drop(file);
        }
        if clean_shutdown && health_persisted {
            let _ = fs::remove_file(&self.marker_path);
        }
    }
}

pub(crate) fn emit(fact: DiagnosticFact) {
    let Some(active) = current_active() else {
        return;
    };
    if !level_enabled(active.settings.level, fact.level)
        || !active.settings.targets.contains(&fact.target)
    {
        return;
    }
    let sequence = active.sequence.fetch_add(1, Ordering::Relaxed) + 1;
    let carried_drops = active.dropped.swap(0, Ordering::Relaxed);
    let record = DiagnosticRecord {
        version: RECORD_VERSION,
        timestamp_ms: now_ms(),
        sequence,
        pid: std::process::id(),
        level: fact.level,
        target: fact.target,
        kind: fact.kind,
        outcome: fact.outcome,
        correlation_id: fact.correlation_id.as_deref().map(sanitize_identifier),
        subject: fact.subject.as_deref().map(sanitize_identifier),
        duration_ms: fact.duration_ms,
        size_bytes: fact.size_bytes,
        dropped_before: carried_drops,
    };
    retain_breadcrumb(&active, record.clone());
    let Ok(mut encoded) = serde_json::to_vec(&record) else {
        active
            .dropped
            .fetch_add(carried_drops + 1, Ordering::Relaxed);
        active.dropped_total.fetch_add(1, Ordering::Relaxed);
        return;
    };
    encoded.push(b'\n');
    match active.sender.try_send(WriterMessage::Record(encoded)) {
        Ok(()) => {}
        Err(_) => {
            active
                .dropped
                .fetch_add(carried_drops + 1, Ordering::Relaxed);
            active.dropped_total.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) fn record_task_panic(subject: &str) {
    emit(
        DiagnosticFact::new(
            DiagnosticLevel::Error,
            DiagnosticTarget::Runtime,
            EventKind::TaskPanicked,
            EventOutcome::Failed,
        )
        .subject(subject),
    );
    if let Some(active) = current_active() {
        write_crash_report(&active, EventKind::TaskPanicked, None);
    }
}

pub(crate) fn load_settings(paths: &XanaPaths) -> DiagnosticsConfig {
    XanaConfig::load_registry_from(paths.config_file())
        .map(|registry| registry.diagnostics)
        .unwrap_or_default()
}

pub(crate) fn resolve_directories(
    paths: &XanaPaths,
    settings: &DiagnosticsConfig,
) -> Result<(PathBuf, PathBuf)> {
    let logs = match &settings.directory {
        Some(directory) if directory.is_absolute() => directory.clone(),
        Some(directory) => paths.data_dir().join(directory),
        None => paths.logs_dir(),
    };
    Ok((logs, paths.crashes_dir()))
}

pub(crate) fn inspect(paths: &XanaPaths) -> DiagnosticHealth {
    let settings = load_settings(paths);
    let (log_dir, crash_dir) = resolve_directories(paths, &settings)
        .unwrap_or_else(|_| (paths.logs_dir(), paths.crashes_dir()));
    let unsafe_path =
        path_has_symlink(&log_dir).unwrap_or(true) || path_has_symlink(&crash_dir).unwrap_or(true);
    let stale_marker_result = count_stale_markers(&crash_dir);
    let invalid_report_result = count_invalid_crash_reports(&crash_dir);
    let inventory_result = inspect_log_inventory(&log_dir, &settings);
    let persisted_health_result = read_health_summaries(&crash_dir, &settings);
    let directory_metadata_allows_writes =
        directory_metadata_allows_writes(&log_dir) && directory_metadata_allows_writes(&crash_dir);
    let permissions_private = directory_is_private(&log_dir) && directory_is_private(&crash_dir);
    let inspection_error = stale_marker_result.is_err()
        || invalid_report_result.is_err()
        || inventory_result.is_err()
        || persisted_health_result.is_err();
    let stale_markers = stale_marker_result.unwrap_or_default();
    let invalid_reports = invalid_report_result.unwrap_or_default();
    let persisted_health = persisted_health_result.unwrap_or_default();
    let dropped_events = persisted_health.0.saturating_add(
        current_active()
            .map(|active| active.dropped_total.load(Ordering::Relaxed))
            .unwrap_or_default(),
    );
    let writer_faults = persisted_health.1.saturating_add(
        current_active()
            .map(|active| active.writer_faults.load(Ordering::Relaxed))
            .unwrap_or_default(),
    );
    let inventory = inventory_result.unwrap_or_default();
    DiagnosticHealth {
        enabled: settings.enabled,
        log_dir_exists: log_dir.is_dir(),
        crash_dir_exists: crash_dir.is_dir(),
        directory_metadata_allows_writes,
        permissions_private,
        retention_compliant: inventory.compliant,
        retained_files: inventory.files,
        retained_bytes: inventory.bytes,
        log_dir,
        crash_dir,
        unsafe_path,
        inspection_error,
        stale_markers,
        invalid_reports,
        dropped_events,
        writer_faults,
    }
}

#[derive(Default)]
struct LogInventory {
    files: usize,
    bytes: u64,
    compliant: bool,
}

fn inspect_log_inventory(path: &Path, settings: &DiagnosticsConfig) -> io::Result<LogInventory> {
    if !path.exists() {
        return Ok(LogInventory {
            compliant: true,
            ..LogInventory::default()
        });
    }
    let now = SystemTime::now();
    let retention = Duration::from_secs(u64::from(settings.retention_days) * 24 * 60 * 60);
    let mut inventory = LogInventory {
        compliant: true,
        ..LogInventory::default()
    };
    for (index, entry) in fs::read_dir(path)?.enumerate() {
        if index >= MAX_DIRECTORY_ENTRIES {
            inventory.compliant = false;
            break;
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let recognized = file_type.is_file()
            && name
                .to_str()
                .is_some_and(|name| name.starts_with("xana-") && name.ends_with(".jsonl"));
        if !recognized {
            continue;
        }
        let metadata = entry.metadata()?;
        inventory.files += 1;
        inventory.bytes = inventory.bytes.saturating_add(metadata.len());
        if metadata.len() > settings.max_file_bytes
            || metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > retention)
        {
            inventory.compliant = false;
        }
    }
    inventory.compliant &= inventory.files <= usize::from(settings.max_files)
        && inventory.bytes <= settings.max_total_bytes;
    Ok(inventory)
}

fn directory_metadata_allows_writes(path: &Path) -> bool {
    let candidate = if path.exists() {
        Some(path)
    } else {
        path.ancestors().find(|candidate| candidate.exists())
    };
    candidate
        .and_then(|candidate| fs::metadata(candidate).ok())
        .is_some_and(|metadata| metadata.is_dir() && !metadata.permissions().readonly())
}

#[cfg(unix)]
fn directory_is_private(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !path.exists() {
        return true;
    }
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o077 == 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn directory_is_private(_path: &Path) -> bool {
    true
}

pub(crate) fn list(paths: &XanaPaths) -> Result<Vec<DiagnosticEntry>> {
    let settings = load_settings(paths);
    let (logs, crashes) = resolve_directories(paths, &settings)?;
    let mut entries = Vec::new();
    collect_entries(&logs, "log", "jsonl", &mut entries)?;
    collect_entries(&crashes, "crash", "json", &mut entries)?;
    entries.sort_by(|left, right| {
        right
            .modified_ms
            .cmp(&left.modified_ms)
            .then_with(|| left.name.cmp(&right.name))
    });
    entries.truncate(256);
    Ok(entries)
}

pub(crate) fn read_records(paths: &XanaPaths, name: &str, lines: usize) -> Result<Vec<String>> {
    let settings = load_settings(paths);
    let (logs, crashes) = resolve_directories(paths, &settings)?;
    let path = resolve_entry(&logs, &crashes, name)?;
    let kind = path.extension().and_then(|value| value.to_str());
    if kind == Some("jsonl") {
        read_log_records(&path, lines.min(MAX_SHOW_LINES))
    } else {
        read_crash_record(&path)
    }
}

pub(crate) fn follow_records(paths: &XanaPaths, name: &str, mut output: impl Write) -> Result<()> {
    let settings = load_settings(paths);
    let (logs, crashes) = resolve_directories(paths, &settings)?;
    let path = resolve_entry(&logs, &crashes, name)?;
    if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
        bail!("only a structured .jsonl log can be followed");
    }
    let mut file = File::open(&path)?;
    let mut offset = file.metadata()?.len();
    loop {
        thread::sleep(Duration::from_millis(200));
        let length = file.metadata()?.len();
        if length < offset {
            offset = 0;
        }
        if length == offset {
            continue;
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(&file);
        let mut line = Vec::new();
        let mut consumed = 0_u64;
        while let Some(meta) = read_bounded_line(&mut reader, &mut line)? {
            if !meta.terminated {
                break;
            }
            consumed = consumed.saturating_add(meta.consumed as u64);
            if meta.within_limit
                && let Ok(record) = serde_json::from_slice::<DiagnosticRecord>(&line)
            {
                serde_json::to_writer(&mut output, &record)?;
                writeln!(output)?;
            }
        }
        offset = offset.saturating_add(consumed);
        output.flush()?;
    }
}

pub(crate) fn export_support_bundle(paths: &XanaPaths, output: &Path) -> Result<()> {
    if !output.is_absolute() {
        bail!("support bundle output must be an absolute path");
    }
    if output.exists() {
        bail!(
            "refusing to overwrite existing support bundle {}",
            output.display()
        );
    }
    let entries = list(paths)?;
    let mut logs = Vec::<serde_json::Value>::new();
    let mut crashes = Vec::<serde_json::Value>::new();
    for entry in entries.iter().take(128) {
        for line in read_records(paths, &entry.name, MAX_SHOW_LINES)? {
            let value: serde_json::Value = serde_json::from_str(&line)?;
            if entry.kind == "log" {
                logs.push(value);
            } else {
                crashes.push(value);
            }
        }
    }
    let bundle = serde_json::json!({
        "version": SUPPORT_BUNDLE_VERSION,
        "created_ms": now_ms(),
        "xana_version": env!("CARGO_PKG_VERSION"),
        "platform": { "os": std::env::consts::OS, "architecture": std::env::consts::ARCH },
        "privacy": "metadata-only; no transcripts, prompts, tool arguments, credentials, or file contents",
        "logs": logs,
        "crashes": crashes,
    });
    let encoded = serde_json::to_vec_pretty(&bundle)?;
    if encoded.len() > MAX_SUPPORT_BYTES || contains_secret_shape(&encoded) {
        bail!("support bundle failed its bounded redaction scan");
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = create_private_file(output)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    Ok(())
}

fn writer_loop(
    receiver: Receiver<WriterMessage>,
    file: File,
    initial_path: PathBuf,
    settings: DiagnosticsConfig,
    writer_faults: Arc<AtomicU64>,
) {
    let mut sink = FileSink {
        file,
        path: initial_path,
        bytes: 0,
        segment: 0,
        settings,
    };
    while let Ok(message) = receiver.recv() {
        match message {
            WriterMessage::Record(bytes) => {
                if sink.write(&bytes).is_err() {
                    writer_faults.fetch_add(1, Ordering::Relaxed);
                }
            }
            WriterMessage::Shutdown(ack) => {
                let _ = sink.file.flush();
                let _ = sink.file.sync_data();
                let _ = ack.try_send(());
                break;
            }
        }
    }
}

fn write_health_summary(directory: &Path, dropped_events: u64, writer_faults: u64) -> Result<()> {
    let summary = DiagnosticHealthSummary {
        version: HEALTH_VERSION,
        timestamp_ms: now_ms(),
        pid: std::process::id(),
        dropped_events,
        writer_faults,
    };
    let path = unique_file_path(
        directory,
        &format!("health-{}-{}", now_ms(), std::process::id()),
        "json",
    )?;
    let mut file = create_private_file(&path)?;
    serde_json::to_writer(&mut file, &summary)?;
    file.flush()?;
    file.sync_data()?;
    Ok(())
}

struct FileSink {
    file: File,
    path: PathBuf,
    bytes: u64,
    segment: u32,
    settings: DiagnosticsConfig,
}

impl FileSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.bytes > 0
            && self.bytes.saturating_add(bytes.len() as u64) > self.settings.max_file_bytes
        {
            self.rotate()?;
        }
        self.file.write_all(bytes)?;
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.segment = self.segment.saturating_add(1);
        let parent = self
            .path
            .parent()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "diagnostic log has no parent")
            })?
            .to_path_buf();
        let stem = self
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("xana");
        let path = unique_file_path(&parent, &format!("{stem}-{}", self.segment), "jsonl")
            .map_err(io::Error::other)?;
        let file = create_private_file(&path).map_err(io::Error::other)?;
        file.try_lock_exclusive()?;
        self.file = file;
        self.path = path;
        self.bytes = 0;
        cleanup_logs(&parent, &self.settings).map_err(io::Error::other)?;
        Ok(())
    }
}

fn active_slot() -> &'static RwLock<Option<Weak<ActiveDiagnostics>>> {
    ACTIVE.get_or_init(|| RwLock::new(None))
}

fn current_active() -> Option<Arc<ActiveDiagnostics>> {
    active_slot()
        .read()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(Weak::upgrade))
}

fn retain_breadcrumb(active: &ActiveDiagnostics, record: DiagnosticRecord) {
    let Ok(mut breadcrumbs) = active.breadcrumbs.lock() else {
        return;
    };
    if breadcrumbs.len() == MAX_BREADCRUMBS {
        breadcrumbs.pop_front();
    }
    breadcrumbs.push_back(record);
}

fn install_panic_hook() {
    PANIC_HOOK.get_or_init(|| {
        let previous = std::panic::take_hook();
        #[cfg(not(test))]
        drop(previous);
        std::panic::set_hook(Box::new(move |information| {
            #[cfg(test)]
            previous(information);
            crate::tui::restore_terminal_best_effort();
            if let Some(active) = current_active() {
                write_crash_report(
                    &active,
                    EventKind::ApplicationFailed,
                    information.location(),
                );
            }
            eprintln!(
                "Xana encountered an internal panic; run `xana logs list` and `xana doctor` for redacted diagnostics"
            );
        }));
    });
}

fn write_crash_report(
    active: &ActiveDiagnostics,
    kind: EventKind,
    location: Option<&std::panic::Location<'_>>,
) {
    let backtrace = Backtrace::force_capture().to_string();
    let breadcrumbs = active
        .breadcrumbs
        .lock()
        .map(|values| values.iter().cloned().collect())
        .unwrap_or_default();
    let report = CrashReport {
        version: CRASH_VERSION,
        timestamp_ms: now_ms(),
        xana_version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        pid: std::process::id(),
        kind,
        thread: sanitize_identifier(thread::current().name().unwrap_or("unnamed")),
        location_hash: location.map(|value| hash_label(value.file())),
        line: location.map(std::panic::Location::line),
        column: location.map(std::panic::Location::column),
        backtrace_hash: hash_label(&backtrace),
        backtrace_lines: backtrace.lines().count().min(10_000),
        breadcrumbs,
    };
    let path = unique_file_path(
        &active.crash_dir,
        &format!("crash-{}-{}", now_ms(), std::process::id()),
        "json",
    );
    if let Ok(path) = path
        && let Ok(mut file) = create_private_file(&path)
        && file.try_lock_exclusive().is_ok()
        && serde_json::to_writer_pretty(&mut file, &report).is_ok()
    {
        let _ = file.flush();
        let _ = file.sync_data();
    }
}

fn level_enabled(configured: DiagnosticLevel, event: DiagnosticLevel) -> bool {
    level_rank(event) <= level_rank(configured)
}

fn level_rank(level: DiagnosticLevel) -> u8 {
    match level {
        DiagnosticLevel::Error => 0,
        DiagnosticLevel::Warn => 1,
        DiagnosticLevel::Info => 2,
        DiagnosticLevel::Debug => 3,
        DiagnosticLevel::Trace => 4,
    }
}

fn sanitize_identifier(value: &str) -> String {
    let value = value.trim();
    let safe = !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('/')
        && !value.contains("..")
        && !value.contains("://")
        && !looks_secret(value)
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        });
    if safe {
        value.to_owned()
    } else {
        hash_label(value)
    }
}

fn hash_label(value: &str) -> String {
    format!("hash:{}", &blake3::hash(value.as_bytes()).to_hex()[..16])
}

fn looks_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "authorization",
        "bearer",
        "api_key",
        "apikey",
        "password",
        "secret",
        "token",
        "sk-",
    ]
    .iter()
    .any(|shape| lower.contains(shape))
}

fn contains_secret_shape(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    ["authorization:", "bearer ", "api_key=", "password=", "sk-"]
        .iter()
        .any(|shape| text.contains(shape))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

mod storage;
use storage::*;

#[cfg(test)]
mod tests;
