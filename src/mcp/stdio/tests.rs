use super::*;
use serde_json::json;
use tempfile::TempDir;

#[cfg(unix)]
fn echo_server_config(temp: &TempDir) -> McpProcessConfig {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"server/discover"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"supportedVersions":["2026-07-28"],"capabilities":{"tools":{"listChanged":true}},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"1"}}}}\n' "$id"
      ;;
    *'"method":"fixture/slow"'*) ;;
    *'"method":"fixture/env"'*)
      if [ "${XANA_MCP_EXPLICIT-}" = "allowed" ] && [ -z "${HOME-}" ]; then ok=true; else ok=false; fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","ok":%s}}\n' "$id" "$ok"
      ;;
    *'"id"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","ok":true}}\n' "$id"
      ;;
  esac
done
"#;
    let mut config = McpProcessConfig::new("sh", temp.path());
    config.arguments = vec![McpArgument::visible("-c"), McpArgument::visible(script)];
    config.request_timeout = Duration::from_secs(5);
    config.shutdown_grace = Duration::from_millis(200);
    config
}

#[cfg(windows)]
fn echo_server_config(temp: &TempDir) -> McpProcessConfig {
    let script = r#"
while (($line = [Console]::In.ReadLine()) -ne $null) {
  $message = $line | ConvertFrom-Json
  if ($null -eq $message.id) { continue }
  if ($message.method -eq 'server/discover') {
    $result = @{ jsonrpc='2.0'; id=$message.id; result=@{ supportedVersions=@('2026-07-28'); capabilities=@{ tools=@{ listChanged=$true } }; _meta=@{ 'io.modelcontextprotocol/serverInfo'=@{ name='fixture'; version='1' } } } }
  } elseif ($message.method -eq 'fixture/slow') {
    continue
  } elseif ($message.method -eq 'fixture/env') {
    $ok = ($env:XANA_MCP_EXPLICIT -eq 'allowed') -and ($null -eq $env:USERPROFILE)
    $result = @{ jsonrpc='2.0'; id=$message.id; result=@{ resultType='complete'; ok=$ok } }
  } else {
    $result = @{ jsonrpc='2.0'; id=$message.id; result=@{ resultType='complete'; ok=$true } }
  }
  [Console]::Out.WriteLine(($result | ConvertTo-Json -Compress -Depth 8))
  [Console]::Out.Flush()
}
"#;
    let mut config = McpProcessConfig::new("powershell.exe", temp.path());
    config.arguments = vec![
        McpArgument::visible("-NoLogo"),
        McpArgument::visible("-NoProfile"),
        McpArgument::visible("-NonInteractive"),
        McpArgument::visible("-Command"),
        McpArgument::visible(script),
    ];
    // Hosted Windows runners can cold-start several PowerShell fixtures in
    // parallel. Keep this test-only process budget above that startup cost.
    config.request_timeout = Duration::from_secs(15);
    config.shutdown_grace = Duration::from_millis(200);
    config
}

#[cfg(unix)]
fn sleeping_server_config(temp: &TempDir) -> McpProcessConfig {
    let mut config = McpProcessConfig::new("sh", temp.path());
    config.arguments = vec![McpArgument::visible("-c"), McpArgument::visible("sleep 30")];
    config.shutdown_grace = Duration::from_millis(50);
    config
}

#[cfg(windows)]
fn sleeping_server_config(temp: &TempDir) -> McpProcessConfig {
    let mut config = McpProcessConfig::new("powershell.exe", temp.path());
    config.arguments = vec![
        McpArgument::visible("-NoProfile"),
        McpArgument::visible("-NonInteractive"),
        McpArgument::visible("-Command"),
        McpArgument::visible("Start-Sleep -Seconds 30"),
    ];
    config.shutdown_grace = Duration::from_millis(50);
    config
}

#[cfg(unix)]
fn stderr_server_config(temp: &TempDir) -> McpProcessConfig {
    let mut config = echo_server_config(temp);
    config.arguments = vec![
        McpArgument::visible("-c"),
        McpArgument::visible(
            "head -c 100000 /dev/zero | tr '\\0' x >&2; while IFS= read -r line; do :; done",
        ),
    ];
    config
}

#[cfg(windows)]
fn stderr_server_config(temp: &TempDir) -> McpProcessConfig {
    let mut config = echo_server_config(temp);
    config.arguments = vec![
        McpArgument::visible("-NoProfile"),
        McpArgument::visible("-NonInteractive"),
        McpArgument::visible("-Command"),
        McpArgument::visible(
            "$s='x'*100000; [Console]::Error.Write($s); while ([Console]::In.ReadLine() -ne $null) {}",
        ),
    ];
    config
}

#[test]
fn process_configuration_is_exact_bounded_and_redacted() {
    let temp = TempDir::new().expect("temporary directory");
    let mut config = McpProcessConfig::new("server", temp.path());
    config.arguments = vec![
        McpArgument::visible("--mode"),
        McpArgument::sensitive("argument-secret"),
    ];
    config.environment.insert(
        "TOKEN".to_owned(),
        McpEnvironmentValue::new("environment-secret"),
    );
    config.validate().expect("config is valid");
    let debug = format!("{config:?}");
    assert!(debug.contains("TOKEN"));
    assert!(!debug.contains("argument-secret"));
    assert!(!debug.contains("environment-secret"));

    config.working_directory = PathBuf::from("relative");
    assert_eq!(
        config.validate(),
        Err(McpStdioError::InvalidConfig(
            "working directory must be absolute"
        ))
    );
}

#[tokio::test]
async fn fake_server_discovers_requests_and_stops_cleanly() {
    let temp = TempDir::new().expect("temporary directory");
    let client = McpStdioClient::spawn(echo_server_config(&temp))
        .await
        .expect("server starts");
    let mut activity = client.subscribe_activity();
    let cancellation = CancellationToken::new();
    let discovery = client
        .discover(&cancellation)
        .await
        .expect("server discovers");
    assert_eq!(discovery.server_name.as_deref(), Some("fixture"));
    assert_eq!(client.health().phase, McpProcessPhase::Ready);

    let response = client
        .request_raw("fixture/echo", json!({"value": true}), &cancellation)
        .await
        .expect("request completes");
    assert_eq!(
        serde_json::from_slice::<Value>(&response).expect("response is json")["result"]["ok"],
        true
    );
    let report = client.shutdown().await.expect("shutdown completes");
    assert!(!report.forced);
    assert!(report.success);
    assert_eq!(client.health().phase, McpProcessPhase::Stopped);
    let seen = std::iter::from_fn(|| activity.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        seen.iter()
            .any(|event| matches!(event, McpProcessActivity::Ready))
    );
    assert!(
        seen.iter()
            .any(|event| matches!(event, McpProcessActivity::RequestCompleted { .. }))
    );
}

#[tokio::test]
async fn child_environment_is_minimal_and_explicit() {
    let temp = TempDir::new().expect("temporary directory");
    let mut config = echo_server_config(&temp);
    config.environment.insert(
        "XANA_MCP_EXPLICIT".to_owned(),
        McpEnvironmentValue::new("allowed"),
    );
    let client = McpStdioClient::spawn(config).await.expect("server starts");
    let result = client
        .request_raw("fixture/env", json!({}), &CancellationToken::new())
        .await
        .expect("environment probe completes");
    let value: Value = serde_json::from_slice(&result).expect("result is JSON");
    assert_eq!(value["result"]["ok"], true);
    client.shutdown().await.expect("shutdown completes");
}

#[tokio::test]
async fn cancellation_and_outstanding_limits_are_bounded() {
    let temp = TempDir::new().expect("temporary directory");
    let mut config = echo_server_config(&temp);
    config.max_outstanding_requests = 1;
    let client = McpStdioClient::spawn(config).await.expect("server starts");
    let first_cancel = CancellationToken::new();
    let first_client = client.clone();
    let first_token = first_cancel.clone();
    let first = tokio::spawn(async move {
        first_client
            .request_raw("fixture/slow", json!({}), &first_token)
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = client
        .request_raw("fixture/echo", json!({}), &CancellationToken::new())
        .await;
    assert_eq!(second, Err(McpStdioError::OutstandingLimit));
    first_cancel.cancel();
    assert_eq!(
        first.await.expect("request task joins"),
        Err(McpStdioError::Cancelled)
    );
    client.shutdown().await.expect("shutdown completes");
}

#[tokio::test]
async fn stderr_is_drained_boundedly_without_becoming_protocol() {
    let temp = TempDir::new().expect("temporary directory");
    let client = McpStdioClient::spawn(stderr_server_config(&temp))
        .await
        .expect("server starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let health = client.health();
        if health.stderr_truncated || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    client.shutdown().await.expect("shutdown completes");
    let health = client.health();
    assert_eq!(health.stderr_bytes, MAX_STDERR_BYTES);
    assert!(health.stderr_truncated);
}

#[tokio::test]
async fn ignored_stdin_forces_bounded_process_cleanup() {
    let temp = TempDir::new().expect("temporary directory");
    let client = McpStdioClient::spawn(sleeping_server_config(&temp))
        .await
        .expect("server starts");
    let started = Instant::now();
    let report = client.shutdown().await.expect("shutdown completes");
    assert!(report.forced);
    assert!(report.success);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(client.health().phase, McpProcessPhase::Stopped);
}

#[tokio::test]
async fn malformed_stdout_fails_without_deadlock() {
    let temp = TempDir::new().expect("temporary directory");
    #[cfg(unix)]
    let mut config = {
        let mut config = McpProcessConfig::new("sh", temp.path());
        config.arguments = vec![
            McpArgument::visible("-c"),
            McpArgument::visible("printf 'not-json\\n'; sleep 1"),
        ];
        config
    };
    #[cfg(windows)]
    let mut config = {
        let mut config = McpProcessConfig::new("powershell.exe", temp.path());
        config.arguments = vec![
            McpArgument::visible("-NoProfile"),
            McpArgument::visible("-Command"),
            McpArgument::visible("[Console]::Out.WriteLine('not-json'); Start-Sleep 1"),
        ];
        config
    };
    config.request_timeout = Duration::from_millis(200);
    config.shutdown_grace = Duration::from_millis(50);
    let client = McpStdioClient::spawn(config).await.expect("server starts");
    let deadline = Instant::now() + Duration::from_secs(3);
    let health = loop {
        let health = client.health();
        if matches!(
            health.phase,
            McpProcessPhase::Crashed | McpProcessPhase::Stopped
        ) || Instant::now() >= deadline
        {
            break health;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(matches!(
        health.phase,
        McpProcessPhase::Crashed | McpProcessPhase::Stopped
    ));
    assert_eq!(health.last_failure, Some(McpFailureKind::Protocol));
}

#[tokio::test]
async fn partial_and_oversized_frames_are_rejected() {
    let (mut writer, reader) = tokio::io::duplex(MAX_FRAME_BYTES + 16);
    let task = tokio::spawn(async move {
        let mut reader = JsonLineReader::new(reader);
        reader.read_frame().await
    });
    writer
        .write_all(&vec![b'x'; MAX_FRAME_BYTES + 2])
        .await
        .expect("fixture writes");
    drop(writer);
    assert_eq!(
        task.await
            .expect("reader joins")
            .expect_err("frame fails")
            .kind(),
        io::ErrorKind::InvalidData
    );

    let (mut writer, reader) = tokio::io::duplex(64);
    let task = tokio::spawn(async move {
        let mut reader = JsonLineReader::new(reader);
        reader.read_frame().await
    });
    writer
        .write_all(b"{\"partial\":")
        .await
        .expect("fixture writes");
    drop(writer);
    assert_eq!(
        task.await
            .expect("reader joins")
            .expect_err("frame fails")
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn unknown_or_invalid_response_ids_fail_the_transport() {
    let (activity, _) = broadcast::channel(4);
    let mut pending = BTreeMap::new();
    let mut retired = VecDeque::new();
    assert_eq!(
        handle_frame(
            br#"{"jsonrpc":"2.0","id":99,"result":{}}"#.to_vec(),
            &mut pending,
            &mut retired,
            &activity,
        ),
        Err(McpFailureKind::Protocol)
    );
    assert_eq!(
        handle_frame(
            br#"{"jsonrpc":"1.0","id":99,"result":{}}"#.to_vec(),
            &mut pending,
            &mut retired,
            &activity,
        ),
        Err(McpFailureKind::Protocol)
    );
}

#[tokio::test(start_paused = true)]
async fn stalled_writes_hit_the_fixed_deadline() {
    let (mut writer, _reader) = tokio::io::duplex(1);
    let write = tokio::spawn(async move { write_frame(&mut writer, &[b'x'; 1024]).await });
    tokio::time::advance(WRITE_TIMEOUT + Duration::from_millis(1)).await;
    assert_eq!(
        write.await.expect("write task joins"),
        Err(McpStdioError::Timeout)
    );
}
