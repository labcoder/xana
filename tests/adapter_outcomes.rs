//! Consumer contract: only the supported Desktop facade is used to admit and
//! reconcile commands. A separate process is killed at real crash boundaries.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;
use xana::desktop::{DesktopClient, DesktopCommandKey, DesktopCommandState, DesktopLaunch};

const NAMESPACE: Uuid = Uuid::from_u128(0x183);
const INPUT: &str = "Give a short synthetic answer.";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate() {
        assert!(Instant::now() < deadline, "adapter fixture timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn setup(home: &Path, url: &str) {
    let key = home.parent().unwrap().join("recovery.key");
    for args in [
        vec!["storage", "recovery-key", "--output"],
        vec!["storage", "initialize", "--manual-unlock", "--recovery-key"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_xana"))
            .env("XANA_HOME", home)
            .env_remove("XANA_STORAGE_RECOVERY_KEY")
            .args(args)
            .arg(&key)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_xana"))
        .env("XANA_HOME", home)
        .env("NO_COLOR", "1")
        .env("XANA_STORAGE_RECOVERY_KEY", &key)
        .args([
            "init",
            "--non-interactive",
            "--kind",
            "ollama",
            "--provider-name",
            "fixture",
            "--base-url",
            url,
            "--model",
            "fixture-model",
            "--permission-mode",
            "deny",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Provider {
    url: String,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Provider {
    fn new(answer: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::clone(&calls);
        let stopping = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        read_request(&mut stream);
                        count.fetch_add(1, Ordering::SeqCst);
                        if answer {
                            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"durably committed synthetic answer\"}}]}\n\ndata: [DONE]\n\n";
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        } else {
                            while !stopping.load(Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(10));
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        });
        Self {
            url,
            calls,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn read_request(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let header = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0 && bytes.len() < 2 * 1024 * 1024);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let length = String::from_utf8_lossy(&bytes[..header])
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(length <= 2 * 1024 * 1024);
    while bytes.len() - header < length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn crash_boundaries_reconcile_without_replay_or_transcript_scraping() {
    for phase in ["before", "admitted", "committed", "delivered"] {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let provider = Provider::new(phase != "admitted");
        setup(&home, &provider.url);
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "adapter_fault_child", "--ignored", "--nocapture"])
            .env("XANA_ADAPTER_FIXTURE", directory.path())
            .env("XANA_ADAPTER_PHASE", phase)
            .env("XANA_HOME", &home)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("no_proxy", "127.0.0.1,localhost,::1")
            .env(
                "XANA_STORAGE_RECOVERY_KEY",
                directory.path().join("recovery.key"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut child = ChildGuard(child);
        let token = directory.path().join("command.json");
        wait(|| token.exists());
        let key: DesktopCommandKey =
            serde_json::from_slice(&std::fs::read(&token).unwrap()).unwrap();
        match phase {
            "admitted" => wait(|| provider.calls.load(Ordering::SeqCst) == 1),
            _ => wait(|| directory.path().join("ready").exists()),
        }
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(!key.command_id().is_nil());
        // Recovery custody is scoped to this subprocess, never a process-global
        // environment mutation in the parallel test runner or an OS keyring.
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "adapter_recovery_child",
                "--ignored",
                "--nocapture",
            ])
            .env("XANA_ADAPTER_FIXTURE", directory.path())
            .env("XANA_ADAPTER_PHASE", phase)
            .env("XANA_HOME", &home)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("no_proxy", "127.0.0.1,localhost,::1")
            .env(
                "XANA_STORAGE_RECOVERY_KEY",
                directory.path().join("recovery.key"),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            usize::from(phase != "before"),
            "reconciliation and duplicate admission must not call provider"
        );
    }
}

#[test]
#[ignore = "subprocess body; exercised by crash_boundaries_reconcile_without_replay_or_transcript_scraping"]
fn adapter_recovery_child() {
    let directory =
        PathBuf::from(std::env::var_os("XANA_ADAPTER_FIXTURE").expect("wrapper owns fixture"));
    let phase = std::env::var("XANA_ADAPTER_PHASE").unwrap();
    let key: DesktopCommandKey =
        serde_json::from_slice(&std::fs::read(directory.join("command.json")).unwrap()).unwrap();
    let launch = DesktopLaunch::new(
        directory.join("workspace"),
        Some(directory.join("home").into_os_string()),
    );
    let reader = launch
        .command_outcomes(NAMESPACE, key.conversation_id())
        .unwrap();
    let expected = match phase.as_str() {
        "before" => DesktopCommandState::NotFound,
        "admitted" => DesktopCommandState::Unknown,
        _ => DesktopCommandState::Completed,
    };
    let outcome = reader.lookup(&key);
    assert_eq!(outcome.state, expected, "{phase}: {outcome:?}");
    for _ in 0..3 {
        assert_eq!(reader.lookup(&key), outcome);
    }
    assert!(
        launch
            .command_outcomes(Uuid::new_v4(), key.conversation_id())
            .unwrap()
            .lookup(&key)
            .unavailable
            .is_some()
    );
    assert!(
        launch
            .command_outcomes(NAMESPACE, Uuid::new_v4())
            .unwrap()
            .lookup(&key)
            .unavailable
            .is_some()
    );
    if phase == "committed" || phase == "delivered" {
        let result = reader.read_result(&key).unwrap();
        assert!(result.content.iter().any(|part| {
            part.fallback_text
                .contains("durably committed synthetic answer")
        }));
        let mut client = DesktopClient::launch(launch).unwrap();
        assert!(
            client
                .submit_correlated(key.clone(), "different payload", Vec::new())
                .is_err()
        );
        let duplicate = client
            .submit_correlated(key.clone(), INPUT, Vec::new())
            .unwrap();
        // CommandRejected is a durable duplicate, not an alternate completion.
        wait(|| match client.try_next() {
            Ok(Some(xana::desktop::DesktopUpdate::CommandResult {
                command_id,
                accepted: false,
                ..
            })) => command_id == duplicate.command_id,
            Ok(Some(xana::desktop::DesktopUpdate::Observation(
                xana::desktop::DesktopObservation {
                    event: xana::desktop::DesktopEvent::Error(error),
                    ..
                },
            ))) => error.code == xana::desktop::DesktopErrorCode::CommandRejected,
            _ => false,
        });
        client.shutdown().unwrap();
    }
}

#[test]
#[ignore = "subprocess body; exercised by crash_boundaries_reconcile_without_replay_or_transcript_scraping"]
fn adapter_fault_child() {
    let directory =
        PathBuf::from(std::env::var_os("XANA_ADAPTER_FIXTURE").expect("wrapper owns fixture"));
    let phase = std::env::var("XANA_ADAPTER_PHASE").unwrap();
    let launch = DesktopLaunch::new(
        directory.join("workspace"),
        Some(directory.join("home").into_os_string()),
    );
    let mut client = DesktopClient::launch(launch).unwrap();
    let reader = client.command_outcomes(NAMESPACE);
    let key = reader.prepare(INPUT, &[]).unwrap();
    // The adapter owns correlation-token persistence; no private Xana record is read.
    let mut file = std::fs::File::create(directory.join("command.tmp")).unwrap();
    file.write_all(&serde_json::to_vec(&key).unwrap()).unwrap();
    file.sync_all().unwrap();
    std::fs::rename(
        directory.join("command.tmp"),
        directory.join("command.json"),
    )
    .unwrap();
    if phase != "before" {
        client
            .submit_correlated(key.clone(), INPUT, Vec::new())
            .unwrap();
    }
    if phase == "committed" || phase == "delivered" {
        wait(|| reader.lookup(&key).state == DesktopCommandState::Completed);
    }
    if phase == "delivered" {
        wait(|| {
            matches!(
                client.try_next(),
                Ok(Some(xana::desktop::DesktopUpdate::Observation(
                    xana::desktop::DesktopObservation {
                        event: xana::desktop::DesktopEvent::MessageFinal { .. },
                        ..
                    }
                )))
            )
        });
        assert!(reader.read_result(&key).is_ok());
    }
    if phase != "admitted" {
        std::fs::write(directory.join("ready"), b"ready").unwrap();
    }
    // The committed phase deliberately never drains Desktop events; delivered
    // confirms the same durable receipt after an adapter has consumed a result.
    loop {
        thread::park_timeout(Duration::from_secs(1));
    }
}
