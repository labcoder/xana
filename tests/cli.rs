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
    let mut command = Command::new(env!("CARGO_BIN_EXE_xana-dev"));
    command.env("XANA_HOME", home).env("NO_COLOR", "1");
    command
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
    assert!(config.contains("codex_program = \"codex\""));
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
fn doctor_json_is_versioned_redacted_and_read_only() {
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
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
    assert_eq!(
        std::fs::read(home.join("config.toml")).expect("config after doctor"),
        before
    );
    assert!(!home.join("cache/models").exists());
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
    assert_eq!(envelope["version"], 1);
    assert_eq!(envelope["status"], "success");
    assert_eq!(envelope["result"]["text"], "structured answer");
    assert_eq!(envelope["result"]["execution_owner"], "native");
    assert!(!output.stdout.contains(&0x1b));
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
    assert_eq!(envelope["version"], 1);
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
