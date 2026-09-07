use super::*;

#[tokio::test]
async fn explicit_close_releases_listener_and_joins_an_active_tunnel() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let policy = EgressPolicy::fixture(
        "https://fixture.invalid",
        Some(upstream.local_addr().unwrap()),
    );
    let mut proxy = Proxy::start(policy, CancellationToken::new())
        .await
        .unwrap();
    let mut client = request(
        proxy.address,
        b"CONNECT fixture.invalid:443 HTTP/1.1\r\n\r\n",
    )
    .await;
    let (mut peer, _) = upstream.accept().await.unwrap();
    let mut head = [0; 39];
    client.read_exact(&mut head).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), proxy.close())
        .await
        .unwrap()
        .unwrap();
    assert!(proxy.task.is_finished());
    let listener = TcpListener::bind(proxy.address).await.unwrap();
    let mut byte = [0];
    for stream in [&mut client, &mut peer] {
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .unwrap();
        assert!(
            matches!(read, Ok(0) | Err(_)),
            "closed tunnel must not retain data or sockets"
        );
    }
    drop(listener);
}

async fn request(address: SocketAddr, head: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(head).await.unwrap();
    stream
}

#[tokio::test]
async fn approved_host_uses_pinned_address_without_dns_and_forwards_once() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let policy = EgressPolicy::fixture(
        "https://nonexistent.invalid:8443",
        Some(upstream.local_addr().unwrap()),
    );
    let proxy = Proxy::start(policy, CancellationToken::new())
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let mut payload = [0; 4];
        stream.read_exact(&mut payload).await.unwrap();
        assert_eq!(&payload, b"once");
        stream.write_all(b"ack").await.unwrap();
    });
    let mut stream = request(
        proxy.address,
        b"CONNECT nonexistent.invalid:8443 HTTP/1.1\r\nHost: attacker.invalid\r\n\r\n",
    )
    .await;
    let mut head = [0; 39];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(&head, b"HTTP/1.1 200 Connection Established\r\n\r\n");
    stream.write_all(b"once").await.unwrap();
    let mut ack = [0; 3];
    stream.read_exact(&mut ack).await.unwrap();
    assert_eq!(&ack, b"ack");
    task.await.unwrap();
}

#[tokio::test]
async fn unreviewed_hosts_ports_and_plain_http_never_connect_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let policy = EgressPolicy::fixture(
        "https://approved.invalid",
        Some(upstream.local_addr().unwrap()),
    );
    let proxy = Proxy::start(policy, CancellationToken::new())
        .await
        .unwrap();
    for head in [
        "CONNECT approved.invalid:444 HTTP/1.1\r\n\r\n",
        "CONNECT other.invalid:443 HTTP/1.1\r\n\r\n",
        "CONNECT 127.0.0.1:443 HTTP/1.1\r\n\r\n",
        "GET https://approved.invalid/ HTTP/1.1\r\n\r\n",
    ] {
        let mut stream = request(proxy.address, head.as_bytes()).await;
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 403"));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), upstream.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn transfer_budget_is_shared_across_directions_and_connections() {
    let remaining = AtomicU64::new(5);
    let mut output = Vec::new();
    copy_bounded(&b"abc"[..], &mut output, &remaining)
        .await
        .unwrap();
    assert_eq!(output, b"abc");
    assert!(
        copy_bounded(&b"def"[..], &mut output, &remaining)
            .await
            .is_err()
    );
    assert_eq!(output, b"abc");
    assert_eq!(remaining.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn production_resolution_rejects_private_recipients() {
    for origin in [
        "https://127.0.0.1",
        "https://[::1]",
        "https://169.254.169.254",
        "https://192.168.1.2",
    ] {
        assert!(
            EgressPolicy::resolve(&[origin.into()], McpHttpSecurity::default())
                .await
                .is_err(),
            "{origin}"
        );
    }
}
