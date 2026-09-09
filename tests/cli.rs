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

#[test]
fn memory_cli_preserves_expiry_checks_revisions_and_exports_without_provider_configuration() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("home");
    let key = directory.path().join("recovery.key");
    assert_success(
        &xana(&home)
            .args(["storage", "recovery-key", "--output"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    assert_success(
        &xana(&home)
            .args(["storage", "initialize", "--manual-unlock", "--recovery-key"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    let command = || {
        let mut cmd = xana(&home);
        cmd.env("XANA_STORAGE_RECOVERY_KEY", &key)
            .stdin(Stdio::null());
        cmd
    };
    let remember = command()
        .args([
            "memory",
            "remember",
            "--scope",
            "user",
            "--text",
            "Synthetic preference",
            "--expires-at",
            "4102444800",
        ])
        .output()
        .unwrap();
    assert_success(&remember);
    let value: serde_json::Value = serde_json::from_slice(&remember.stdout).unwrap();
    let id = value["id"].as_str().unwrap();
    let correction = command()
        .args([
            "memory",
            "correct",
            id,
            "--revision",
            "1",
            "--text",
            "Corrected preference",
        ])
        .output()
        .unwrap();
    assert_success(&correction);
    let value: serde_json::Value = serde_json::from_slice(&correction.stdout).unwrap();
    assert_eq!(value["valid_until_unix_seconds"], 4102444800_u64);
    assert!(
        !command()
            .args([
                "memory",
                "correct",
                id,
                "--revision",
                "1",
                "--text",
                "Stale edit"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    let cleared = command()
        .args([
            "memory",
            "correct",
            id,
            "--revision",
            "2",
            "--text",
            "Permanent preference",
            "--clear-expiry",
        ])
        .output()
        .unwrap();
    assert_success(&cleared);
    let value: serde_json::Value = serde_json::from_slice(&cleared.stdout).unwrap();
    assert!(value["valid_until_unix_seconds"].is_null());
    let scope = format!("conversation:{}", uuid::Uuid::new_v4());
    assert!(
        !command()
            .args(["memory", "scope", id, "--revision", "3", "--to", &scope])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_success(
        &command()
            .args([
                "memory",
                "scope",
                id,
                "--revision",
                "3",
                "--to",
                &scope,
                "--confirm",
            ])
            .output()
            .unwrap(),
    );
    assert_success(
        &command()
            .args([
                "memory",
                "controls",
                "--scope",
                &scope,
                "--no-memory",
                "on",
                "--learn",
                "off",
            ])
            .output()
            .unwrap(),
    );
    let export = directory.path().join("readable.json");
    assert_success(
        &command()
            .args(["memory", "export", "--scope", &scope, "--output"])
            .arg(&export)
            .output()
            .unwrap(),
    );
    let exported: serde_json::Value =
        serde_json::from_slice(&std::fs::read(export).unwrap()).unwrap();
    assert_eq!(exported["records"][0]["statement"], "Permanent preference");
    let forgotten = command()
        .args(["memory", "forget", id, "--revision", "4"])
        .output()
        .unwrap();
    assert_success(&forgotten);
    let record: serde_json::Value = serde_json::from_slice(&forgotten.stdout).unwrap();
    assert_eq!(record["state"], "forgotten");
    assert!(
        !command()
            .args(["memory", "restore", id, "--revision", "5"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let restored = command()
        .args(["memory", "restore", id, "--revision", "5", "--confirm"])
        .output()
        .unwrap();
    assert_success(&restored);
    let record: serde_json::Value = serde_json::from_slice(&restored.stdout).unwrap();
    assert_eq!(record["state"], "active");
    let status = command()
        .args(["memory", "learning-status"])
        .output()
        .unwrap();
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert!(status["route"].is_null());
    assert_eq!(status["pending"], 0);
    assert!(
        !command()
            .args(["memory", "process"])
            .output()
            .unwrap()
            .status
            .success(),
        "missing helper fails visibly without requiring a provider for local controls"
    );
    // A protected home does not silently unlock without custody/recovery authority.
    assert!(
        !xana(&home)
            .args(["memory", "list"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn candidate_cli_review_survives_restart_and_never_installs_a_skill() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("home");
    let key = directory.path().join("recovery.key");
    assert_success(
        &xana(&home)
            .args(["storage", "recovery-key", "--output"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    assert_success(
        &xana(&home)
            .args(["storage", "initialize", "--manual-unlock", "--recovery-key"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    let run = |arguments: &[&str]| {
        xana(&home)
            .env("XANA_STORAGE_RECOVERY_KEY", &key)
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .args(arguments)
            .output()
            .unwrap()
    };
    let staged = run(&[
        "memory",
        "candidate",
        "stage-skill",
        "--scope",
        "user",
        "--name",
        "synthetic",
        "--markdown",
        "# Procedure\nNever execute this canary",
    ]);
    assert_success(&staged);
    let record: serde_json::Value = serde_json::from_slice(&staged.stdout).unwrap();
    let id = record["id"].as_str().unwrap();
    let revision = record["revision"].as_u64().unwrap().to_string();
    let page = run(&["memory", "candidate", "list", "--scope", "user"]);
    assert_success(&page);
    assert!(!String::from_utf8_lossy(&page.stdout).contains("Never execute"));
    let inspected = run(&["memory", "candidate", "diff", id]);
    assert_success(&inspected);
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("Never execute"));
    let approved = run(&[
        "memory",
        "candidate",
        "approve",
        id,
        "--revision",
        &revision,
    ]);
    assert_success(&approved);
    let approved: serde_json::Value = serde_json::from_slice(&approved.stdout).unwrap();
    assert_eq!(approved["state"], "reviewed_only");
    assert!(
        !run(&[
            "memory",
            "candidate",
            "archive",
            id,
            "--revision",
            &revision
        ])
        .status
        .success()
    );
    let next_revision = approved["revision"].as_u64().unwrap().to_string();
    let archived = run(&[
        "memory",
        "candidate",
        "archive",
        id,
        "--revision",
        &next_revision,
    ]);
    assert_success(&archived);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&archived.stdout).unwrap()["state"],
        "archived"
    );
    assert!(!home.join(".agents").exists());
    assert!(!directory.path().join(".agents").exists());
    assert!(
        !home.join("config.toml").exists(),
        "no provider configuration required"
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

fn read_http_request(stream: &mut TcpStream) -> serde_json::Value {
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
    if content_length == 0 {
        return serde_json::Value::Null;
    }
    serde_json::from_slice(&request[header_end..header_end + content_length])
        .expect("provider JSON request")
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
fn web_enablement_during_a_turn_applies_to_next_turn_in_the_same_conversation() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("home");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    init_native(
        &home,
        &format!("http://{}/v1", listener.local_addr().unwrap()),
    );
    let fixture_home = home.clone();
    let worker = thread::spawn(move || {
        let mut captured = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        for index in 0..3 {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "missing provider request {index}"
                        );
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            captured.push(read_http_request(&mut stream));
            let delta = if index == 0 {
                // Edit real configuration while the first request is in flight.
                // Pages-only setup performs no network or credential operation.
                assert_success(
                    &xana(&fixture_home)
                        .args(["connect", "web", "--web-provider", "pages-only", "--yes"])
                        .output()
                        .unwrap(),
                );
                serde_json::json!({"tool_calls":[{"index":0,"id":"fixture-call","type":"function","function":{
                    "name":"xana_docs","arguments":"{\"op\":\"list\"}"
                }}]})
            } else {
                serde_json::json!({"content": format!("fixture answer {index}")})
            };
            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                serde_json::json!({"choices":[{"delta":delta}]})
            );
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
        captured
    });
    let mut process = xana(&home)
        .current_dir(directory.path())
        .args(["--plain"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(b"first fixture question\nsecond fixture question\n/quit\n")
        .unwrap();
    let output = process.wait_with_output().unwrap();
    assert_success(&output);
    let requests = worker.join().unwrap();
    let has_fetch = |request: &serde_json::Value| {
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "web_fetch")
    };
    assert!(!has_fetch(&requests[0]));
    assert!(
        !has_fetch(&requests[1]),
        "active tool loop changed configuration"
    );
    assert!(
        has_fetch(&requests[2]),
        "new turn did not acquire enabled web capability"
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("first fixture question")
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("fixture answer 1")
    );
    let journals = std::fs::read_dir(home.join("data/sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    assert_eq!(
        journals.len(),
        1,
        "settings must not create another Conversation"
    );
    let records = std::fs::read_to_string(&journals[0])
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let revisions = records
        .iter()
        .filter(|record| record["kind"] == "execution_configured")
        .collect::<Vec<_>>();
    assert_eq!(revisions.len(), 2);
    let bound = records
        .iter()
        .filter(|record| record["kind"] == "operation_configuration_bound")
        .map(|record| record["data"]["configuration_digest"].clone())
        .collect::<Vec<_>>();
    assert_eq!(bound.len(), 2);
    assert_ne!(bound[0], bound[1]);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("settings updated for this conversation")
    );
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
    assert_success(
        &command()
            .args(["profile", "edit", "default", "--max-tool-rounds", "7"])
            .output()
            .unwrap(),
    );
    let doctor = command()
        .args(["doctor", "--output", "json"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("conversation.profile.update_ready"));
    let reopened = command().output().unwrap();
    assert_success(&reopened);
    assert!(String::from_utf8_lossy(&reopened.stdout).contains("resumed: yes"));
    let usage = command().args(["usage", "ledger"]).output().unwrap();
    assert_success(&usage);
    let ledger: serde_json::Value = serde_json::from_slice(&usage.stdout).unwrap();
    assert_eq!(ledger["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        ledger["records"][0]["admission"]["facts"]["owner"],
        "native"
    );
    assert!(ledger["records"][0]["receipt"]["reported_cost_microunits"].is_null());
    let budget = command()
        .args([
            "budget",
            "--daily-requests",
            "1",
            "--foreground-request-reserve",
            "0",
        ])
        .output()
        .unwrap();
    assert_success(&budget);
    let denied_request = command()
        .args(["--print", "no second dispatch"])
        .output()
        .unwrap();
    assert!(!denied_request.status.success());
    assert!(String::from_utf8_lossy(&denied_request.stderr).contains("allowance exhausted"));
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
fn bare_restart_updates_execution_settings_without_replacing_conversation() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("home");
    init_native(&home, "http://127.0.0.1:9/v1");
    let run = |args: &[&str]| {
        xana(&home)
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .args(args)
            .output()
            .unwrap()
    };
    assert_success(&run(&[]));
    let projects = home.join("data/interoperable/projects.json");
    let before = std::fs::read(&projects).unwrap();
    let config_path = home.join("config.toml");
    let mut config: toml_edit::DocumentMut = std::fs::read_to_string(&config_path)
        .unwrap()
        .parse()
        .unwrap();
    config["profiles"]["default"]["max_tool_rounds"] = toml_edit::value(7);
    config["profiles"]["default"]["permission_mode"] = toml_edit::value("allow");
    std::fs::write(&config_path, config.to_string()).unwrap();

    let doctor = run(&["doctor", "--output", "json"]);
    assert_success(&doctor);
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("conversation.profile.update_ready"));
    assert_eq!(std::fs::read(&projects).unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        config.to_string()
    );

    // A bare launch selects the retained session and records a new execution revision.
    let resumed = run(&[]);
    assert_success(&resumed);
    assert!(String::from_utf8_lossy(&resumed.stdout).contains("resumed: yes"));
    assert_success(&run(&["--continue"]));
    assert_eq!(std::fs::read(&projects).unwrap(), before);
    assert_success(&run(&["conversation", "new"]));
    let old: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(projects).unwrap()).unwrap();
    let snapshots = after["conversation_profiles"].as_object().unwrap();
    assert_eq!(snapshots.len(), 2);
    let (old_id, old_snapshot) = old["conversation_profiles"]
        .as_object()
        .unwrap()
        .iter()
        .next()
        .unwrap();
    assert_eq!(&snapshots[old_id], old_snapshot);
    let new_snapshot = snapshots.iter().find(|(id, _)| *id != old_id).unwrap().1;
    assert_eq!(new_snapshot["resolved"]["max_tool_rounds"]["value"], 7);
    assert_eq!(
        new_snapshot["resolved"]["permission_mode"]["value"],
        "allow"
    );
    assert_success(&run(&["--resume", old_id]));
    // Native /model preserves this Conversation; only the execution is recomposed.
    let mut child = xana(&home)
        .current_dir(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/model test/test-model\n")
        .unwrap();
    let switched = child.wait_with_output().unwrap();
    assert_success(&switched);
    assert!(
        String::from_utf8_lossy(&switched.stdout).contains("selected test/test-model"),
        "{}",
        String::from_utf8_lossy(&switched.stdout)
    );
    let after: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("data/interoperable/projects.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        after["conversation_profiles"].as_object().unwrap().len(),
        2,
        "{}",
        String::from_utf8_lossy(&switched.stdout)
    );
}

#[test]
fn invalid_frozen_profile_is_diagnosed_without_replacing_history() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("home");
    init_native(&home, "http://127.0.0.1:9/v1");
    let run = |args: &[&str]| {
        xana(&home)
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .args(args)
            .output()
            .unwrap()
    };
    assert_success(&run(&[]));
    let projects = home.join("data/interoperable/projects.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&projects).unwrap()).unwrap();
    let (id, snapshot) = document["conversation_profiles"]
        .as_object_mut()
        .unwrap()
        .iter_mut()
        .next()
        .unwrap();
    let id = id.clone();
    snapshot["resolved"]["max_tool_rounds"]["value"] = "invalid".into();
    let damaged = serde_json::to_vec(&document).unwrap();
    std::fs::write(&projects, &damaged).unwrap();
    let journal_path = home.join("data/sessions").join(format!("{id}.jsonl"));
    let journal = std::fs::read(&journal_path).unwrap();
    let doctor = run(&["doctor", "--output", "json"]);
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("conversation.profile.invalid"));
    let resumed = run(&[]);
    assert!(!resumed.status.success());
    assert!(String::from_utf8_lossy(&resumed.stderr).contains("xana conversation new"));
    assert_eq!(std::fs::read(&projects).unwrap(), damaged);
    assert_eq!(std::fs::read(journal_path).unwrap(), journal);
    // Recovery never depends on successfully opening the damaged Conversation.
    assert_success(&run(&["conversation", "new"]));
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
    // Catalog behavior must not depend on a developer's keychain or a CI login
    // session. Keep first-connection setup protected, with fixture-only custody.
    let key = directory.path().join("recovery.key");
    assert_success(
        &xana(&home)
            .args(["storage", "recovery-key", "--output"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    assert_success(
        &xana(&home)
            .args(["storage", "initialize", "--manual-unlock", "--recovery-key"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    assert!(!home.join("config.toml").exists());
    let command = || {
        let mut cmd = xana(&home);
        cmd.env("XANA_STORAGE_RECOVERY_KEY", &key)
            .stdin(Stdio::null());
        cmd
    };
    let (base_url, server) = fake_catalog_server(3);

    let added = command()
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
    let tested = command()
        .args(["connection", "--json", "test", "local"])
        .output()
        .expect("test connection");
    assert_success(&tested);
    let tested_json: serde_json::Value = serde_json::from_slice(&tested.stdout).unwrap();
    assert_eq!(tested_json["semantic_code"], "connection.test.completed.v1");
    assert_eq!(tested_json["usable"], true);
    assert!(!home.join("cache/models/local.json").exists());

    let repaired = command()
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

    let status = command().args(["storage", "status"]).output().unwrap();
    assert_success(&status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("Storage: protected"));
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
    // Setup must not write logs into an empty home before protection is planned.
    // An ordinary application launch owns diagnostics; EOF makes no model call.
    let launch = xana(&home).stdin(Stdio::null()).output().unwrap();
    assert_success(&launch);

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
    assert_eq!(
        summary["payload"]["completion"]["evidence"]["outcome"],
        "delivery_verified"
    );
    assert!(summary["payload"]["prompt_plan"].is_object());
    assert!(summary["payload"]["usage"].is_array());
    let final_frame = frames.last().expect("result frame");
    assert_eq!(final_frame["type"], "result");
    assert_eq!(final_frame["payload"]["status"], "success");
    assert_eq!(final_frame["payload"]["result"]["text"], "streamed answer");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("streamed answer"));
}

#[test]
fn one_shot_declared_check_cannot_be_satisfied_by_provider_prose() {
    let directory = tempdir().expect("temporary Xana home");
    let home = directory.path().join("xana-home");
    let (base_url, worker) = fake_chat_server("All tests passed. The task is complete.");
    init_native(&home, &base_url);

    let output = xana(&home)
        .args([
            "--output",
            "stream-json",
            "-p",
            "Check the task",
            "--accept-command",
            "cargo test --offline",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("finite native one-shot");
    worker.join().expect("exactly one fake provider call");
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frames: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let receipt = &frames
        .iter()
        .find(|frame| frame["type"] == "summary")
        .expect("typed finite receipt")["payload"]["completion"];
    assert_eq!(receipt["evidence"]["outcome"], "needs_attention");
    assert_eq!(receipt["evidence"]["claim"], "completed");
    assert_eq!(
        frames.last().unwrap()["payload"]["error"]["category"],
        "incomplete"
    );
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
#[test]
fn invalid_recovery_setup_never_creates_logs_or_misclassifies_a_fresh_home() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("fresh");
    let output = xana(&home)
        .args([
            "setup",
            "--blank",
            "--non-interactive",
            "--yes",
            "--recovery-output",
            "relative.key",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("absolute destination"));
    assert!(
        !home.exists(),
        "setup must not create logs before reviewing fresh protection"
    );
}
