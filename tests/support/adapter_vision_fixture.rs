use base64::Engine as _;
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use xana::desktop::*;

/// The isolated child contains synthetic content only. Preserve test assertions
/// after the runtime installs its production content-free crash hook.
pub fn launch(launch: DesktopLaunch) -> Result<DesktopClient, DesktopError> {
    let test_hook = std::panic::take_hook();
    let result = DesktopClient::launch(launch);
    std::panic::set_hook(test_hook);
    result
}

pub fn initialize(home: &Path, brain: &str, specialist: &str, native: bool) {
    let key = std::env::var_os("XANA_STORAGE_RECOVERY_KEY").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_xana"))
        .env("XANA_HOME", home)
        .env_remove("XANA_STORAGE_RECOVERY_KEY")
        .args(["storage", "initialize", "--manual-unlock", "--recovery-key"])
        .arg(key)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let modalities = if native {
        "[\"text\", \"image\"]"
    } else {
        "[\"text\"]"
    };
    let config = format!(
        r#"version = 4
default_profile = "default"
permission_mode = "ask"
[providers.fixture]
kind = "ollama"
base_url = "{brain}"
[providers.fixture.models."fixture-model"]
input_modalities = {modalities}
tools = false
context_tokens = 65536
[profiles.default]
connection = "fixture"
model = "fixture-model"
service_routes = ["describe"]
egress_policy = "vision"
[service_connections.vision]
adapter = "openai.vision"
base_url = "{specialist}"
credential = {{ source = "environment", variable = "XANA_VISION_FIXTURE_KEY" }}
[service_routes.describe]
operation = "vision.analyze"
connection = "vision"
model = "fixture-specialist"
default = true
egress_policy = "vision"
[egress_policies.vision]
allowed = ["prompt_text", "selected_artifacts"]
"#
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
}

pub struct Capture {
    pub client: DesktopClient,
    seen: Vec<DesktopUpdate>,
}
impl Capture {
    pub fn new(client: DesktopClient) -> Self {
        Self {
            client,
            seen: Vec::new(),
        }
    }
    fn until(&mut self, predicate: impl Fn(&DesktopUpdate) -> bool) -> DesktopUpdate {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(found) = self.seen.iter().find(|update| predicate(update)) {
                return found.clone();
            }
            assert!(
                Instant::now() < deadline,
                "vision fixture timed out; observed {:?}",
                self.seen
            );
            if let Some(update) = self.client.try_next().unwrap() {
                assert!(self.seen.len() < 4096);
                self.seen.push(update);
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    pub fn stage(&mut self, bytes: Vec<u8>) -> DesktopAttachment {
        let command = self.client.stage_image_bytes(bytes, "image/png").unwrap();
        let DesktopUpdate::AttachmentStaged { attachment, .. } = self.until(|update| matches!(update, DesktopUpdate::AttachmentStaged { command_id, .. } if *command_id == command.command_id)) else { unreachable!() };
        attachment
    }
    pub fn plan(
        &mut self,
        prompt: &str,
        attachments: Vec<DesktopAttachment>,
        route: Option<String>,
    ) -> DesktopVisionPlan {
        let command = self
            .client
            .plan_vision_turn(prompt, attachments, route)
            .unwrap();
        let DesktopUpdate::Vision(DesktopVisionUpdate::Planned { plan, .. }) = self.until(|update| matches!(update, DesktopUpdate::Vision(DesktopVisionUpdate::Planned { command_id, .. }) if *command_id == command.command_id)) else { unreachable!() };
        plan
    }
    pub fn receipt(&mut self, id: u64) -> VisionReceipt {
        let DesktopUpdate::Vision(DesktopVisionUpdate::Receipt { receipt, .. }) = self.until(|update| matches!(update, DesktopUpdate::Vision(DesktopVisionUpdate::Receipt { command_id, .. }) if *command_id == id)) else { unreachable!() };
        receipt
    }
    pub fn rejected(&mut self, id: u64, expected: DesktopVisionError) {
        let DesktopUpdate::Vision(DesktopVisionUpdate::Rejected { reason, .. }) = self.until(|update| matches!(update, DesktopUpdate::Vision(DesktopVisionUpdate::Rejected { command_id, .. }) if *command_id == id)) else { unreachable!() };
        assert_eq!(reason, expected);
    }
    pub fn completed(&mut self, operation: DesktopOperationId) {
        self.until(|update| matches!(update, DesktopUpdate::Observation(DesktopObservation { event: DesktopEvent::OperationState { operation_id, state: DesktopOperationState::Completed }, .. }) if *operation_id == operation));
    }
}

pub fn png(color: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba(color));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}
pub fn decode_image(url: &str) -> Vec<u8> {
    decode(url.split_once(',').unwrap().1)
}
fn decode(value: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .unwrap()
}
pub fn wait(duration: Duration, mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + duration;
    while !predicate() {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Clone, Copy)]
pub enum Reply {
    Text,
    Usage,
    Hold,
    Failure,
}
pub struct Provider {
    pub url: String,
    captured: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Provider {
    pub fn new(reply: Reply) -> Self {
        Self::serve(reply, None)
    }

    pub fn https(reply: Reply) -> Self {
        let certificate = rustls::pki_types::CertificateDer::from(decode(
            include_str!("../fixtures/adapter-vision-tls/test-server.der.b64").trim(),
        ));
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(decode(
            include_str!("../fixtures/adapter-vision-tls/test-server-key.der.b64").trim(),
        ));
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key.into())
        .unwrap();
        Self::serve(reply, Some(Arc::new(config)))
    }

    pub fn launch(&self, launch: DesktopLaunch) -> DesktopLaunch {
        let url = reqwest::Url::parse(&self.url).unwrap();
        launch
            .with_service_certificate(
                &url.origin().ascii_serialization(),
                decode(include_str!("../fixtures/adapter-vision-tls/test-ca.der.b64").trim()),
            )
            .unwrap()
    }

    fn serve(reply: Reply, tls: Option<Arc<rustls::ServerConfig>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let scheme = if tls.is_some() { "https" } else { "http" };
        let url = format!("{scheme}://{}/v1", listener.local_addr().unwrap());
        let captured = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::clone(&captured);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Windows can inherit the listener's nonblocking mode;
                        // this worker uses bounded blocking I/O, including TLS.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut stream: Box<dyn FixtureIo> = if let Some(config) = &tls {
                            Box::new(rustls::StreamOwned::new(
                                rustls::ServerConnection::new(Arc::clone(config)).unwrap(),
                                stream,
                            ))
                        } else {
                            Box::new(stream)
                        };
                        let Some(request) = read_request(&mut stream) else {
                            continue;
                        };
                        requests.lock().unwrap().push(request);
                        if matches!(reply, Reply::Hold) {
                            while !stopping.load(Ordering::Acquire) {
                                thread::sleep(Duration::from_millis(5));
                            }
                        } else if matches!(reply, Reply::Failure) {
                            let body = "private-provider-canary";
                            let _ = write!(
                                stream,
                                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        } else {
                            let usage = if matches!(reply, Reply::Usage) {
                                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n"
                            } else {
                                ""
                            };
                            let body = format!(
                                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"synthetic selected-image description\"}}}}]}}\n\n{usage}data: [DONE]\n\n"
                            );
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
        });
        Self {
            url,
            captured,
            stop,
            worker: Some(worker),
        }
    }
    pub fn calls(&self) -> usize {
        self.captured.lock().unwrap().len()
    }
    pub fn requests(&self) -> Vec<serde_json::Value> {
        self.captured.lock().unwrap().clone()
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.expect("vision fixture worker panicked");
        }
    }
}
trait FixtureIo: Read + Write {}
impl<T: Read + Write> FixtureIo for T {}

fn read_request(stream: &mut impl Read) -> Option<serde_json::Value> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let header = loop {
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            Err(error) => {
                eprintln!("synthetic provider request read: {error}");
                return None;
            }
        };
        if count == 0 {
            return None;
        }
        assert!(bytes.len() < 28 * 1024 * 1024);
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
    assert!(length < 28 * 1024 * 1024);
    while bytes.len() - header < length {
        let count = stream.read(&mut buffer).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[header..header + length]).ok()
}
