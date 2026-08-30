use super::*;
use crate::config::{InitialConfig, InitialConnection, PermissionMode};
use crate::shell::ShellConfig;
use tempfile::tempdir;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let directory = tempdir().unwrap();
    #[cfg(unix)]
    let canonical = directory.path().canonicalize().unwrap();
    #[cfg(not(unix))]
    let canonical = directory.path().to_path_buf();
    (directory, canonical)
}

fn fixture() -> (tempfile::TempDir, XanaPaths) {
    let (directory, canonical) = canonical_tempdir();
    let paths = XanaPaths::resolve(Some(canonical.join("home").into_os_string())).unwrap();
    fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    fs::write(
        paths.config_file(),
        XanaConfig::render_initial(InitialConfig {
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
        .unwrap(),
    )
    .unwrap();
    (directory, paths)
}

#[test]
fn records_are_metadata_only_and_secret_shaped_subjects_are_hashed() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    emit(
        DiagnosticFact::new(
            DiagnosticLevel::Info,
            DiagnosticTarget::Provider,
            EventKind::ProviderFailed,
            EventOutcome::Failed,
        )
        .subject("sk-super-secret-token"),
    );
    drop(runtime);
    let log = list(&paths)
        .unwrap()
        .into_iter()
        .find(|entry| entry.kind == "log")
        .unwrap();
    let text = read_records(&paths, &log.name, 20).unwrap().join("\n");
    assert!(text.contains("provider_failed"));
    assert!(text.contains("hash:"));
    assert!(!text.contains("super-secret"));
    assert!(!text.contains("sk-"));
}

#[test]
fn saturation_never_blocks_and_reports_loss_on_a_later_record() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    for _ in 0..20_000 {
        emit(DiagnosticFact::new(
            DiagnosticLevel::Info,
            DiagnosticTarget::Runtime,
            EventKind::ApplicationStarted,
            EventOutcome::Started,
        ));
    }
    assert!(runtime.active.sequence.load(Ordering::Relaxed) == 20_000);
    drop(runtime);
}

#[test]
fn standalone_inspection_reports_prior_writer_health_failures() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    runtime.active.dropped_total.store(3, Ordering::Relaxed);
    runtime.active.writer_faults.store(2, Ordering::Relaxed);
    drop(runtime);

    let health = inspect(&paths);

    assert_eq!(health.dropped_events, 3);
    assert_eq!(health.writer_faults, 2);
    assert!(
        !list(&paths)
            .unwrap()
            .iter()
            .any(|entry| entry.name.starts_with("health-"))
    );
}

#[test]
fn stale_locked_markers_and_crash_reports_are_distinct() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    assert!(runtime.marker_path.exists());
    record_task_panic("fixture-task");
    let crashes = list(&paths).unwrap();
    assert!(crashes.iter().any(|entry| entry.kind == "crash"));
    let marker = runtime.marker_path.clone();
    drop(runtime);
    assert!(!marker.exists());
}

#[test]
fn caught_process_panic_writes_a_sanitized_report_and_clean_drop_removes_marker() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    let marker = runtime.marker_path.clone();
    let result = std::panic::catch_unwind(|| panic!("sk-private panic body"));
    assert!(result.is_err());
    let crash = list(&paths)
        .unwrap()
        .into_iter()
        .find(|entry| entry.kind == "crash")
        .unwrap();
    let report = read_records(&paths, &crash.name, 1).unwrap().join("\n");
    assert!(report.contains("application_failed"));
    assert!(!report.contains("private panic body"));
    drop(runtime);
    assert!(!marker.exists());
}

#[test]
fn support_bundle_reparses_known_records_and_rejects_overwrite() {
    let _guard = test_guard();
    let (directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    emit(DiagnosticFact::new(
        DiagnosticLevel::Warn,
        DiagnosticTarget::Security,
        EventKind::ToolDenied,
        EventOutcome::Denied,
    ));
    drop(runtime);
    let bundle = directory.path().join("support.json");
    export_support_bundle(&paths, &bundle).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert_eq!(value["version"], 1);
    assert!(
        value["privacy"]
            .as_str()
            .unwrap()
            .contains("no transcripts")
    );
    assert!(export_support_bundle(&paths, &bundle).is_err());
}

#[test]
fn cleanup_deletes_only_recognized_bounded_files() {
    let _guard = test_guard();
    let (_directory, canonical) = canonical_tempdir();
    let logs = canonical.join("logs");
    ensure_private_directory(&logs).unwrap();
    fs::write(logs.join("notes.jsonl"), b"owner data").unwrap();
    fs::write(logs.join("xana-1.jsonl"), b"{}\n").unwrap();
    fs::write(logs.join("xana-2.jsonl"), b"{}\n").unwrap();
    let settings = DiagnosticsConfig {
        max_files: 1,
        max_file_bytes: 64 * 1024,
        max_total_bytes: 64 * 1024,
        ..DiagnosticsConfig::default()
    };

    cleanup_logs(&logs, &settings).unwrap();

    assert!(logs.join("notes.jsonl").exists());
    assert_eq!(
        fs::read_dir(&logs)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("xana-"))
            .count(),
        1
    );
}

#[test]
fn inspection_reports_retention_and_disk_bound_violations() {
    let (_directory, canonical) = canonical_tempdir();
    let logs = canonical.join("logs");
    ensure_private_directory(&logs).unwrap();
    fs::write(logs.join("xana-oversized.jsonl"), vec![b'x'; 65_537]).unwrap();
    let settings = DiagnosticsConfig {
        max_files: 1,
        max_file_bytes: 65_536,
        max_total_bytes: 65_536,
        ..DiagnosticsConfig::default()
    };

    let inventory = inspect_log_inventory(&logs, &settings).unwrap();
    assert_eq!(inventory.files, 1);
    assert_eq!(inventory.bytes, 65_537);
    assert!(!inventory.compliant);
}

#[test]
fn cleanup_never_deletes_another_live_process_log() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();
    let active = list(&paths)
        .unwrap()
        .into_iter()
        .find(|entry| entry.kind == "log")
        .unwrap()
        .name;
    std::thread::sleep(Duration::from_millis(5));
    let disposable = paths.logs_dir().join("xana-disposable.jsonl");
    fs::write(&disposable, b"{}\n").unwrap();
    let settings = DiagnosticsConfig {
        max_files: 1,
        max_file_bytes: 64 * 1024,
        max_total_bytes: 64 * 1024,
        ..DiagnosticsConfig::default()
    };

    cleanup_logs(&paths.logs_dir(), &settings).unwrap();

    assert!(paths.logs_dir().join(active).exists());
    assert!(!disposable.exists());
    drop(runtime);
}

#[test]
fn unlocked_prior_marker_is_reported_without_deleting_it() {
    let _guard = test_guard();
    let (_directory, paths) = fixture();
    ensure_private_directory(&paths.crashes_dir()).unwrap();
    let stale = paths.crashes_dir().join("run-1-1.json");
    fs::write(&stale, b"{\"version\":1}").unwrap();

    let runtime = DiagnosticRuntime::start(&paths).unwrap().unwrap();

    assert_eq!(runtime.stale_markers(), 1);
    assert!(stale.exists());
    drop(runtime);
}

#[test]
fn oversized_log_lines_are_discarded_without_unbounded_allocation() {
    let _guard = test_guard();
    let (_directory, canonical) = canonical_tempdir();
    let log = canonical.join("xana-fixture.jsonl");
    let valid = br#"{"version":1,"timestamp_ms":1,"sequence":1,"pid":1,"level":"info","target":"runtime","kind":"application_started","outcome":"started","dropped_before":0}"#;
    let mut file = File::create(&log).unwrap();
    file.write_all(&vec![b'x'; MAX_LINE_BYTES * 8]).unwrap();
    file.write_all(b"\n").unwrap();
    file.write_all(valid).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);

    let records = read_log_records(&log, 10).unwrap();

    assert_eq!(records.len(), 1);
    assert!(records[0].contains("application_started"));
}

#[test]
fn oversized_diagnostic_directories_fail_closed_instead_of_scanning_forever() {
    let _guard = test_guard();
    let (_directory, canonical) = canonical_tempdir();
    for index in 0..=MAX_DIRECTORY_ENTRIES {
        fs::write(canonical.join(format!("owner-{index}.txt")), b"x").unwrap();
    }

    let error = cleanup_logs(&canonical, &DiagnosticsConfig::default()).unwrap_err();

    assert!(error.to_string().contains("inspection bound"));
}

#[test]
fn unrelated_entries_cannot_hide_an_incomplete_retention_scan() {
    let _guard = test_guard();
    let (_directory, canonical) = canonical_tempdir();
    for index in 0..MAX_DIRECTORY_ENTRIES {
        fs::write(canonical.join(format!("owner-{index}.txt")), b"x").unwrap();
    }
    fs::write(canonical.join("xana-hidden.jsonl"), b"{}\n").unwrap();

    let inventory = inspect_log_inventory(&canonical, &DiagnosticsConfig::default()).unwrap();

    assert!(!inventory.compliant);
}

#[cfg(unix)]
#[test]
fn canonical_temporary_root_is_accepted_while_symlinked_root_is_rejected() {
    use std::os::unix::fs::symlink;

    let (_directory, canonical) = canonical_tempdir();
    let link_parent = tempdir().unwrap();
    let linked_root = link_parent.path().join("diagnostic-root");
    symlink(&canonical, &linked_root).unwrap();

    let error = ensure_private_directory(&linked_root.join("logs")).unwrap_err();
    assert!(error.to_string().contains("symlink component"));
    ensure_private_directory(&canonical.join("logs")).unwrap();
}
