//! Package-level smoke tests for the compiled Xana executable.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
};
use tempfile::tempdir;

fn xana(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xana"));
    command
        .env("XANA_HOME", home)
        .env("NO_COLOR", "1")
        .env_remove("XANA_STORAGE_RECOVERY_KEY");
    command
}

fn canonical_temp_root(directory: &tempfile::TempDir) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        directory
            .path()
            .canonicalize()
            .expect("canonical temporary directory")
    }
    #[cfg(not(unix))]
    {
        directory.path().to_path_buf()
    }
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "Xana failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fake_chat_server(final_text: &str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
    let address = listener.local_addr().expect("fake provider address");
    let body = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(final_text).expect("JSON text")
    );
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept provider request");
        read_http_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write provider response");
    });
    (format!("http://{address}/v1"), worker)
}

fn fake_catalog_server(requests: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake catalog");
    let address = listener.local_addr().expect("fake catalog address");
    let worker = thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().expect("accept catalog request");
            read_http_request(&mut stream);
            let body = r#"{"data":[{"id":"test-model"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write catalog response");
        }
    });
    (format!("http://{address}/v1"), worker)
}

fn blocking_chat_server() -> (
    String,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind blocking provider");
    let address = listener.local_addr().expect("blocking provider address");
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept blocking request");
        read_http_request(&mut stream);
        accepted_tx.send(()).expect("announce accepted request");
        release_rx.recv().expect("release blocking request");
        let body =
            "data: {\"choices\":[{\"delta\":{\"content\":\"finished\"}}]}\n\ndata: [DONE]\n\n";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write blocking response");
    });
    (
        format!("http://{address}/v1"),
        accepted_rx,
        release_tx,
        worker,
    )
}

fn read_http_request(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("request timeout");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).expect("read provider request");
        assert!(read > 0, "provider request ended before headers");
        request.extend_from_slice(&buffer[..read]);
        if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .unwrap_or_default();
    while request.len() - header_end < content_length {
        let read = stream.read(&mut buffer).expect("read provider body");
        assert!(read > 0, "provider request ended before body");
        request.extend_from_slice(&buffer[..read]);
    }
}

fn init_native(home: &Path, base_url: &str) {
    let output = xana(home)
        .args([
            "init",
            "--non-interactive",
            "--kind",
            "ollama",
            "--provider-name",
            "test",
            "--base-url",
            base_url,
            "--model",
            "test-model",
            "--permission-mode",
            "deny",
        ])
        .output()
        .expect("initialize fake provider");
    assert_success(&output);
}

#[test]
fn protected_home_setup_stream_restart_lock_and_manual_recovery() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("protected-home");
    let workspace = directory.path().join("ordinary-workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("notes.txt"), "ordinary editor source").unwrap();
    let key = directory.path().join("independent-recovery.key");
    let create = xana(&home)
        .args(["storage", "recovery-key", "--output"])
        .arg(&key)
        .output()
        .unwrap();
    assert_success(&create);
    let init = xana(&home)
        .args(["storage", "initialize", "--manual-unlock", "--recovery-key"])
        .arg(&key)
        .output()
        .unwrap();
    assert_success(&init);
    let command = || {
        let mut cmd = xana(&home);
        cmd.env("XANA_STORAGE_RECOVERY_KEY", &key)
            .current_dir(&workspace)
            .stdin(Stdio::null());
        cmd
    };
    let (base_url, server) = fake_chat_server("encrypted answer canary");
    let setup = command()
        .args([
            "init",
            "--non-interactive",
            "--kind",
            "ollama",
            "--provider-name",
            "test",
            "--base-url",
            &base_url,
            "--model",
            "test-model",
            "--permission-mode",
            "deny",
        ])
        .output()
        .unwrap();
    assert_success(&setup);
    let answer = command()
        .args([
            "--print",
            "encrypted question canary",
            "--output",
            "stream-json",
        ])
        .output()
        .unwrap();
    assert_success(&answer);
    server.join().unwrap();
    assert!(String::from_utf8_lossy(&answer.stdout).contains("encrypted answer canary"));
    assert!(
        !home.join("data/sessions").exists(),
        "normal execution must not create a plaintext journal"
    );
    assert!(
        !home.join("data/interoperable/projects.json").exists(),
        "sensitive catalog stays protected"
    );
    let verified = command().args(["storage", "verify"]).output().unwrap();
    assert_success(&verified);
    let locked = command().args(["storage", "lock"]).output().unwrap();
    assert_success(&locked);
    let denied = command().args(["storage", "verify"]).output().unwrap();
    assert!(
        !denied.status.success(),
        "manual key selection must not implicitly unlock"
    );
    let doctor = command()
        .args(["doctor", "--output", "json"])
        .output()
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).unwrap_or_else(|error| {
            panic!(
                "doctor JSON: {error}; status {}; stderr {}",
                doctor.status,
                String::from_utf8_lossy(&doctor.stderr)
            )
        });
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "storage.protected"
                && finding["summary"].as_str().unwrap().contains("locked"))
    );
    let unlocked = command()
        .args(["storage", "unlock", "--recovery-key"])
        .arg(&key)
        .output()
        .unwrap();
    assert_success(&unlocked);
    assert_success(&command().args(["storage", "verify"]).output().unwrap());
    assert_success(
        &command()
            .args(["config", "migrate", "--apply"])
            .output()
            .unwrap(),
    );
    let mut pending = vec![home.join("data")];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                for canary in [
                    b"encrypted question canary".as_slice(),
                    b"encrypted answer canary",
                ] {
                    assert!(
                        !bytes.windows(canary.len()).any(|part| part == canary),
                        "plaintext canary in {}",
                        path.display()
                    );
                }
            }
        }
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes.txt")).unwrap(),
        "ordinary editor source"
    );
    assert_success(
        &command()
            .args(["reset", "--scope", "sessions", "--yes"])
            .output()
            .unwrap(),
    );
    assert_success(&command().args(["storage", "verify"]).output().unwrap());
    assert!(home.join("data/protected/content.sqlite").is_file());
}

#[test]
fn storage_migration_backup_and_restore_preserve_a_real_cli_conversation() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("home");
    let key = directory.path().join("recovery.key");
    let (url, worker) = fake_chat_server("migration retained answer");
    init_native(&home, &url);
    assert_success(
        &xana(&home)
            .args(["-p", "migration retained question"])
            .output()
            .unwrap(),
    );
    worker.join().unwrap();
    let path = std::fs::read_dir(home.join("data/sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap();
    let id = path.file_stem().unwrap().to_str().unwrap();
    assert_success(
        &xana(&home)
            .args(["storage", "recovery-key", "--output"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    let inspect = xana(&home).args(["storage", "migrate"]).output().unwrap();
    assert_success(&inspect);
    let plan = serde_json::Deserializer::from_slice(&inspect.stdout)
        .into_iter::<serde_json::Value>()
        .next()
        .unwrap()
        .unwrap();
    let migrated = xana(&home)
        .args([
            "storage",
            "migrate",
            "--apply",
            "--manual-unlock",
            "--review",
            plan["review"].as_str().unwrap(),
            "--recovery-key",
        ])
        .arg(&key)
        .output()
        .unwrap();
    assert_success(&migrated);
    let command = || {
        let mut cmd = xana(&home);
        cmd.env("XANA_STORAGE_RECOVERY_KEY", &key);
        cmd
    };
    let before = command()
        .args(["conversation", "preview", id, "--json"])
        .output()
        .unwrap();
    assert_success(&before);
    assert!(String::from_utf8_lossy(&before.stdout).contains("migration retained answer"));
    let backup = command().args(["storage", "backup"]).output().unwrap();
    assert_success(&backup);
    let backup: serde_json::Value = serde_json::from_slice(&backup.stdout).unwrap();
    let snapshot = backup["snapshot"].as_str().unwrap();
    let inspect = command()
        .args([
            "storage",
            "restore",
            "--snapshot",
            snapshot,
            "--recovery-key",
        ])
        .arg(&key)
        .output()
        .unwrap();
    assert_success(&inspect);
    let plan = serde_json::Deserializer::from_slice(&inspect.stdout)
        .into_iter::<serde_json::Value>()
        .next()
        .unwrap()
        .unwrap();
    let restored = command()
        .args([
            "storage",
            "restore",
            "--snapshot",
            snapshot,
            "--apply",
            "--review",
            plan["review"].as_str().unwrap(),
            "--recovery-key",
        ])
        .arg(&key)
        .output()
        .unwrap();
    assert_success(&restored);
    let after = command()
        .args(["conversation", "preview", id, "--json"])
        .output()
        .unwrap();
    assert_success(&after);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&before.stdout).unwrap(),
        serde_json::from_slice::<serde_json::Value>(&after.stdout).unwrap()
    );
}

#[test]
fn help_runs_without_initializing_xana() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("unused-home");
    let output = xana(&home).arg("--help").output().expect("run Xana help");

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    assert!(!home.exists());
}

#[test]
fn config_path_honors_an_absolute_xana_home() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let output = xana(&home)
        .args(["config", "path"])
        .output()
        .expect("run config path");

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        home.join("config.toml").display().to_string()
    );
    assert!(!home.exists());
}

#[test]
fn connection_json_is_typed_secret_free_and_scope_explicit() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    init_native(&home, "http://127.0.0.1:1/v1");

    let output = xana(&home)
        .args(["connection", "--json", "list"])
        .output()
        .expect("list connection state");
    assert_success(&output);
    let snapshot: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("connection JSON");
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["connections"][0]["id"], "test");
    assert_eq!(
        snapshot["connections"][0]["selected_for_new_conversations"],
        true
    );
    assert_eq!(
        snapshot["connections"][0]["facets"]["reachability"],
        "not_tested"
    );
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(!rendered.to_ascii_lowercase().contains("api_key"));
    assert!(!rendered.contains("Bearer "));
}

#[test]
fn connection_add_test_and_repair_share_validated_catalog_semantics() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let (base_url, server) = fake_catalog_server(3);

    let added = xana(&home)
        .args([
            "connection",
            "--json",
            "add",
            "local",
            "--kind",
            "openai-compatible",
            "--base-url",
            &base_url,
            "--model",
            "test-model",
            "--yes",
        ])
        .output()
        .expect("add validated connection");
    assert_success(&added);
    let added_json: serde_json::Value = serde_json::from_slice(&added.stdout).unwrap();
    assert_eq!(added_json["semantic_code"], "connection.add.completed.v1");
    assert!(home.join("config.toml").is_file());

    std::fs::remove_dir_all(home.join("cache/models")).unwrap();
    let tested = xana(&home)
        .args(["connection", "--json", "test", "local"])
        .output()
        .expect("test connection");
    assert_success(&tested);
    let tested_json: serde_json::Value = serde_json::from_slice(&tested.stdout).unwrap();
    assert_eq!(tested_json["semantic_code"], "connection.test.completed.v1");
    assert_eq!(tested_json["usable"], true);
    assert!(!home.join("cache/models/local.json").exists());

    let repaired = xana(&home)
        .args(["connection", "--json", "repair", "local"])
        .output()
        .expect("repair connection");
    assert_success(&repaired);
    let repaired_json: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(
        repaired_json["semantic_code"],
        "connection.repair.completed.v1"
    );
    assert!(home.join("cache/models/local.json").is_file());
    server.join().unwrap();
}

#[test]
fn credential_deletion_requires_its_own_confirmation() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let output = xana(&home)
        .args(["connection", "delete-key", "missing"])
        .output()
        .expect("reject unconfirmed credential deletion");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("separate from connection removal"));
    assert!(stderr.contains("requires --yes"));
    assert!(!home.join("config.toml").exists());
}

#[test]
fn route_diagnostics_resolve_without_network_or_config_mutation() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused provider");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    init_native(
        &home,
        &format!("http://{}/v1", listener.local_addr().unwrap()),
    );
    let config_path = home.join("config.toml");
    let mut before = std::fs::read(&config_path).expect("config before route diagnostics");
    before.extend_from_slice(b"\n[routes.worker]\nprofile = \"default\"\n");
    std::fs::write(&config_path, &before).expect("configure a non-default route");

    let listed = xana(&home).args(["route", "list"]).output().unwrap();
    assert_success(&listed);
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        "* default\tnative\ttest/test-model\tprofile default\n  worker\tnative\ttest/test-model\tprofile default\n"
    );
    let checked = xana(&home)
        .args(["route", "check", "worker"])
        .output()
        .unwrap();
    assert_success(&checked);
    let checked = String::from_utf8(checked.stdout).unwrap();
    for expected in [
        "route: worker\n",
        "execution: native\n",
        "connection: test\n",
        "model: test-model\n",
    ] {
        assert!(
            checked.contains(expected),
            "missing {expected:?}: {checked}"
        );
    }
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert_eq!(std::fs::read(&config_path).unwrap(), before);
    assert!(!home.join("cache/models").exists());
}

#[test]
fn diagnostic_commands_list_and_export_metadata_without_starting_another_log() {
    let directory = tempdir().expect("temporary Xana home");
    let home = canonical_temp_root(&directory).join("xana-home");
    init_native(&home, "http://127.0.0.1:9/v1");

    let listed = xana(&home)
        .args(["logs", "list"])
        .output()
        .expect("list diagnostics");
    assert_success(&listed);
    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listing.contains("log\txana-"),
        "diagnostic listing did not contain a Xana log: {listing:?}"
    );

    let bundle = directory.path().join("support.json");
    let exported = xana(&home)
        .args(["logs", "export", "--output", bundle.to_str().unwrap()])
        .output()
        .expect("export diagnostics");
    assert_success(&exported);
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(bundle).unwrap()).unwrap();
    assert_eq!(value["version"], 1);
    assert!(
        value["logs"]
            .as_array()
            .is_some_and(|logs| !logs.is_empty())
    );
}

#[test]
fn redirected_bare_launch_stays_plain_and_explicit_tui_fails_cleanly() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    init_native(&home, "http://127.0.0.1:9/v1");

    let plain = xana(&home)
        .stdin(Stdio::null())
        .output()
        .expect("run redirected bare Xana");
    assert_success(&plain);
    for stream in [&plain.stdout, &plain.stderr] {
        assert!(
            !stream.contains(&0x1b),
            "redirected launch emitted controls"
        );
    }
    assert!(String::from_utf8_lossy(&plain.stdout).contains("provider connection: test"));

    let required = xana(&home)
        .arg("--tui")
        .stdin(Stdio::null())
        .output()
        .expect("require TUI without a terminal");
    assert!(!required.status.success());
    assert!(
        String::from_utf8_lossy(&required.stderr)
            .contains("--tui requires interactive stdin and stdout")
    );
    assert!(!required.stderr.contains(&0x1b));
}

#[test]
fn noninteractive_init_creates_once_and_config_check_loads_it() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let init_args = [
        "init",
        "--non-interactive",
        "--provider-name",
        "ollama",
        "--base-url",
        "http://localhost:11434/v1",
        "--model",
        "qwen3:1.7b",
        "--permission-mode",
        "ask",
    ];

    let first = xana(&home)
        .args(init_args)
        .output()
        .expect("initialize Xana");
    assert_success(&first);
    for stream in [&first.stdout, &first.stderr] {
        assert!(!stream.contains(&0x1b), "redirected setup emitted controls");
    }
    let config_path = home.join("config.toml");
    let original = std::fs::read(&config_path).expect("read created configuration");

    let second = xana(&home)
        .args(init_args)
        .output()
        .expect("repeat initialization");
    assert_success(&second);
    assert_eq!(
        std::fs::read(&config_path).expect("read unchanged configuration"),
        original
    );

    let check = xana(&home)
        .args(["config", "check"])
        .output()
        .expect("check configuration");
    assert_success(&check);
    assert!(String::from_utf8_lossy(&check.stdout).starts_with("configuration is valid:"));
}

#[test]
fn setup_if_needed_has_stable_noninteractive_ready_and_pending_results() {
    let directory = tempdir().expect("temporary Xana home");

    let missing_home = directory.path().join("missing");
    let missing = xana(&missing_home)
        .args(["setup", "--if-needed"])
        .stdin(Stdio::null())
        .output()
        .expect("check missing setup readiness");
    assert_eq!(missing.status.code(), Some(10));
    let missing_stdout = String::from_utf8_lossy(&missing.stdout);
    assert!(missing_stdout.contains("XANA_SETUP_RESULT"));
    assert!(missing_stdout.contains(r#""status":"pending""#));
    assert!(missing_stdout.contains(r#""reason":"missing""#));
    assert!(missing_stdout.contains("Next: xana setup"));
    assert!(!missing_home.exists());

    let healthy_home = directory.path().join("healthy");
    init_native(&healthy_home, "http://127.0.0.1:9/v1");
    let config_path = healthy_home.join("config.toml");
    let before = std::fs::read(&config_path).expect("read healthy config");
    let healthy = xana(&healthy_home)
        .args(["setup", "--if-needed"])
        .stdin(Stdio::null())
        .output()
        .expect("check healthy setup readiness");
    assert_success(&healthy);
    let healthy_stdout = String::from_utf8_lossy(&healthy.stdout);
    assert!(healthy_stdout.contains(r#""status":"ready""#));
    assert!(healthy_stdout.contains(r#""reason":"healthy""#));
    assert_eq!(std::fs::read(&config_path).expect("reread config"), before);
    assert!(!config_path.with_extension("toml.bak").exists());
}

#[test]
fn setup_if_needed_classifies_bad_state_without_disclosing_or_mutating_it() {
    let directory = tempdir().expect("temporary Xana home");
    for (name, contents, reason) in [
        (
            "invalid",
            "version = [\nsecret = 'setup-secret-sentinel'",
            "invalid",
        ),
        (
            "future",
            "version = 99\nfuture_secret = 'setup-secret-sentinel'",
            "incompatible",
        ),
    ] {
        let home = directory.path().join(name);
        std::fs::create_dir_all(&home).expect("create Xana home");
        let config_path = home.join("config.toml");
        std::fs::write(&config_path, contents).expect("write setup fixture");
        let before = std::fs::read(&config_path).expect("read setup fixture");

        let output = xana(&home)
            .args(["setup", "--if-needed"])
            .stdin(Stdio::null())
            .output()
            .expect("check bad setup readiness");
        assert_eq!(output.status.code(), Some(10));
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(combined.contains(&format!(r#""reason":"{reason}""#)));
        assert!(!combined.contains("setup-secret-sentinel"));
        assert_eq!(std::fs::read(&config_path).expect("reread fixture"), before);
        assert!(!config_path.with_extension("toml.bak").exists());
    }
}

#[test]
fn setup_if_needed_does_not_mislabel_indeterminate_filesystem_state() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("indeterminate");
    std::fs::create_dir_all(home.join("config.toml")).expect("create conflicting config path");

    let output = xana(&home)
        .args(["setup", "--if-needed"])
        .stdin(Stdio::null())
        .output()
        .expect("check indeterminate setup readiness");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("could not determine Xana setup readiness")
    );
    assert!(home.join("config.toml").is_dir());
}

#[test]
fn setup_if_needed_rejects_setup_choices_without_mutation() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("combined");

    let output = xana(&home)
        .args(["setup", "--if-needed", "--model", "llama3.2"])
        .stdin(Stdio::null())
        .output()
        .expect("reject combined setup options");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--if-needed cannot be combined with other setup options")
    );
    assert!(!home.exists());
}

#[test]
fn noninteractive_codex_init_creates_a_valid_managed_connection() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let output = xana(&home)
        .args([
            "init",
            "--non-interactive",
            "--kind",
            "codex",
            "--provider-name",
            "codex",
            "--codex-program",
            "codex-preview",
            "--model",
            "gpt-5.6-sol",
            "--permission-mode",
            "ask",
        ])
        .output()
        .expect("initialize managed Codex connection");

    assert_success(&output);
    let config = std::fs::read_to_string(home.join("config.toml"))
        .expect("read managed Codex configuration");
    assert!(config.contains("kind = \"codex\""));
    assert!(config.contains("codex_program = \"codex-preview\""));
    assert!(!config.contains("base_url"));

    let check = xana(&home)
        .args(["config", "check"])
        .output()
        .expect("check managed Codex configuration");
    assert_success(&check);
}

#[test]
fn reset_requires_confirmation_preserves_history_and_allows_reinitialization() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let init_args = [
        "init",
        "--non-interactive",
        "--kind",
        "ollama",
        "--provider-name",
        "ollama",
        "--model",
        "qwen3:1.7b",
        "--permission-mode",
        "ask",
    ];
    let initialized = xana(&home)
        .args(init_args)
        .output()
        .expect("initialize Xana");
    assert_success(&initialized);
    for path in [
        home.join("data/selection.toml"),
        home.join("data/managed-threads/route.json"),
        home.join("cache/models/ollama.json"),
        home.join("data/sessions/keep.jsonl"),
    ] {
        std::fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("create fixture parent");
        std::fs::write(path, b"fixture").expect("write fixture");
    }

    let refused = xana(&home).arg("reset").output().expect("refuse reset");
    assert!(!refused.status.success());
    assert!(home.join("config.toml").is_file());

    let reset = xana(&home)
        .args(["clean", "--yes"])
        .output()
        .expect("reset Xana");
    assert_success(&reset);
    assert!(!home.join("config.toml").exists());
    assert!(!home.join("data/selection.toml").exists());
    assert!(!home.join("data/managed-threads").exists());
    assert!(!home.join("cache/models").exists());
    assert!(home.join("data/sessions/keep.jsonl").is_file());

    let reinitialized = xana(&home)
        .args(init_args)
        .output()
        .expect("reinitialize Xana");
    assert_success(&reinitialized);
    assert!(home.join("config.toml").is_file());
}

#[test]
fn doctor_json_is_versioned_and_read_only() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    init_native(&home, "http://127.0.0.1:9/v1");
    let before = std::fs::read(home.join("config.toml")).expect("config before doctor");

    let output = xana(&home)
        .args(["doctor", "--output", "json"])
        .output()
        .expect("run doctor");

    assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("versioned doctor JSON");
    assert_eq!(report["version"], 1);
    assert!(
        report["findings"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert_eq!(
        std::fs::read(home.join("config.toml")).expect("config after doctor"),
        before
    );
    assert!(!home.join("cache/models").exists());
}

#[test]
fn legacy_home_without_interoperable_records_starts_and_doctor_reports_migration() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    init_native(&home, "http://127.0.0.1:9/v1");
    std::fs::remove_dir_all(home.join("data/interoperable"))
        .expect("remove post-M3 private records from legacy fixture");

    let doctor = xana(&home)
        .args(["doctor", "--output", "json"])
        .output()
        .expect("diagnose legacy home");
    assert_success(&doctor);
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("versioned doctor JSON");
    let findings = report["findings"].as_array().expect("doctor findings");
    assert!(
        findings
            .iter()
            .any(|finding| finding["code"] == "state.migration_required"
                && finding["action"] == "xana config migrate --apply")
    );
    assert!(!home.join("data/interoperable").exists());

    let mut child = xana(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start legacy Xana home");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"/quit\n")
        .expect("quit plain frontend");
    let output = child.wait_with_output().expect("wait for Xana");
    assert_success(&output);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("plugin state is unavailable"));
}

#[test]
fn reset_dry_run_and_session_scope_preserve_configuration() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    init_native(&home, "http://127.0.0.1:9/v1");
    let session = home.join("data/sessions/keep.jsonl");
    std::fs::create_dir_all(session.parent().expect("session parent")).unwrap();
    std::fs::write(&session, b"fixture").unwrap();

    let preview = xana(&home)
        .args(["reset", "--scope", "sessions", "--dry-run"])
        .output()
        .expect("preview sessions reset");
    assert_success(&preview);
    assert!(session.is_file());
    assert!(home.join("config.toml").is_file());

    let reset = xana(&home)
        .args(["reset", "--scope", "sessions", "--yes"])
        .output()
        .expect("reset sessions");
    assert_success(&reset);
    assert!(!session.exists());
    assert!(home.join("config.toml").is_file());
}

#[test]
fn one_shot_text_keeps_payload_on_stdout_and_diagnostics_on_stderr() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let (base_url, worker) = fake_chat_server("hello from Xana");
    init_native(&home, &base_url);

    let output = xana(&home)
        .args(["--plain", "-p", "say hello"])
        .output()
        .expect("run text one-shot");
    worker.join().expect("fake provider worker");

    assert_success(&output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello from Xana\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("loading Xana config"));
    assert!(!output.stdout.contains(&0x1b));
    let sessions = home.join("data/sessions");
    assert_eq!(
        std::fs::read_dir(sessions)
            .expect("durable sessions")
            .filter_map(Result::ok)
            .filter(|entry| entry
                .path()
                .extension()
                .is_some_and(|value| value == "jsonl"))
            .count(),
        1
    );
}

#[test]
fn conversation_preview_returns_bounded_history_without_starting_a_runtime() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let (base_url, worker) = fake_chat_server("retained answer");
    init_native(&home, &base_url);

    let turn = xana(&home)
        .args(["--plain", "-p", "retain this question"])
        .output()
        .expect("run retained turn");
    worker.join().expect("fake provider worker");
    assert_success(&turn);
    let session = std::fs::read_dir(home.join("data/sessions"))
        .expect("durable sessions")
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == "jsonl")
        })
        .expect("one session")
        .path()
        .file_stem()
        .expect("session id")
        .to_string_lossy()
        .into_owned();

    let preview = xana(&home)
        .args([
            "conversation",
            "preview",
            &session,
            "--limit",
            "2",
            "--json",
        ])
        .output()
        .expect("preview retained Conversation");

    assert_success(&preview);
    let value: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["messages"].as_array().map(Vec::len), Some(2));
    assert_eq!(value["total"], 2);
    assert_eq!(value["has_older"], false);
    assert!(!String::from_utf8_lossy(&preview.stderr).contains("provider"));
}

#[test]
fn one_shot_json_is_one_versioned_envelope() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let (base_url, worker) = fake_chat_server("structured answer");
    init_native(&home, &base_url);

    let output = xana(&home)
        .args(["--json", "-p", "answer"])
        .output()
        .expect("run JSON one-shot");
    worker.join().expect("fake provider worker");

    assert_success(&output);
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON envelope");
    assert_eq!(envelope["version"], 2);
    assert_eq!(envelope["status"], "success");
    assert_eq!(envelope["result"]["text"], "structured answer");
    assert_eq!(envelope["result"]["execution_owner"], "native");
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn one_shot_stream_json_is_ordered_jsonl_ending_in_authoritative_result() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let (base_url, worker) = fake_chat_server("streamed answer");
    init_native(&home, &base_url);

    let output = xana(&home)
        .args(["--output", "stream-json", "-p", "answer"])
        .output()
        .expect("run streaming JSON one-shot");
    worker.join().expect("fake provider worker");

    assert_success(&output);
    let frames = String::from_utf8(output.stdout)
        .expect("UTF-8 JSONL")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON frame"))
        .collect::<Vec<_>>();
    assert!(frames.len() >= 2, "observations precede the final result");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame["version"], 1);
        assert_eq!(frame["sequence"], u64::try_from(index + 1).unwrap());
        assert_eq!(frame["execution_owner"], "native");
        assert!(frame["conversation_id"].is_string());
    }
    let summary = frames
        .iter()
        .find(|frame| frame["type"] == "summary")
        .expect("one bounded semantic run summary");
    assert!(summary["payload"]["execution"].is_object());
    assert!(summary["payload"]["completion"].is_object());
    assert!(summary["payload"]["prompt_plan"].is_object());
    assert!(summary["payload"]["usage"].is_array());
    let final_frame = frames.last().expect("result frame");
    assert_eq!(final_frame["type"], "result");
    assert_eq!(final_frame["payload"]["status"], "success");
    assert_eq!(final_frame["payload"]["result"]["text"], "streamed answer");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("streamed answer"));
}

#[test]
fn one_shot_rejects_missing_and_ambiguous_input_before_provider_activation() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("unused-home");
    let missing = xana(&home)
        .args(["--json", "--print"])
        .stdin(Stdio::null())
        .output()
        .expect("missing prompt");
    assert_eq!(missing.status.code(), Some(2));
    let envelope: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("missing-input envelope");
    assert_eq!(envelope["error"]["category"], "invalid_input");

    let mut child = xana(&home)
        .args(["--json", "-p", "argument"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ambiguous prompt");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"stdin")
        .expect("write prompt");
    let ambiguous = child.wait_with_output().expect("ambiguous result");
    assert_eq!(ambiguous.status.code(), Some(2));
    let envelope: serde_json::Value =
        serde_json::from_slice(&ambiguous.stdout).expect("ambiguous-input envelope");
    assert_eq!(envelope["error"]["category"], "invalid_input");
    assert!(!home.exists(), "provider/config must not be activated");
}

#[test]
fn one_shot_configuration_failure_has_stable_exit_and_json_shape() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("unused-home");
    let output = xana(&home)
        .args(["--json", "-p", "hello"])
        .output()
        .expect("configuration failure");

    assert_eq!(output.status.code(), Some(3));
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("configuration envelope");
    assert_eq!(envelope["version"], 2);
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["category"], "configuration");
    assert!(!output.stdout.contains(&0x1b));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not initialized"));
}

#[test]
fn a_second_process_cannot_start_a_competing_workspace_root() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let (base_url, accepted, release, worker) = blocking_chat_server();
    init_native(&home, &base_url);

    let first = xana(&home)
        .args(["-p", "hold the root"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start first root");
    accepted
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("first root reached provider");

    let second = xana(&home)
        .args(["--json", "-p", "compete"])
        .output()
        .expect("run competing root");
    assert_eq!(second.status.code(), Some(6));
    let envelope: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("busy result envelope");
    assert_eq!(envelope["error"]["category"], "runtime");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("active Xana root")
    );

    release.send(()).expect("release first root");
    let first = first.wait_with_output().expect("first root result");
    worker.join().expect("blocking provider worker");
    assert_success(&first);
    assert_eq!(String::from_utf8_lossy(&first.stdout), "finished\n");
}
