//! A fresh task owns one explicit exact-recipient proxy with no direct fallback.
//! HTTPS remains end-to-end TLS: this governs recipients, not encrypted payloads.

use super::BrowserError;
use crate::mcp::{McpHttpSecurity, resolve_pinned_addresses};
use reqwest::Url;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

const MAX_ORIGINS: usize = 8;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
const MAX_TUNNEL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TASK_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) struct EgressPolicy {
    destinations: BTreeMap<String, Vec<SocketAddr>>,
    pub(super) origins: Vec<String>,
}

impl EgressPolicy {
    #[cfg(test)]
    pub(super) fn fixture(origin: &str, upstream: Option<SocketAddr>) -> Self {
        let url = Url::parse(origin).unwrap();
        Self {
            origins: vec![url.origin().ascii_serialization()],
            destinations: upstream
                .map(|address| BTreeMap::from([(authority(&url).unwrap(), vec![address])]))
                .unwrap_or_default(),
        }
    }
    pub(super) async fn resolve(
        origins: &[String],
        security: McpHttpSecurity,
    ) -> Result<Self, BrowserError> {
        let parsed = Self::parse(origins, security)?;
        let mut destinations = BTreeMap::new();
        for url in &parsed {
            let addresses = tokio::time::timeout(
                Duration::from_secs(3),
                resolve_pinned_addresses(url, security),
            )
            .await
            .map_err(|_| BrowserError::TimedOut)?
            .map_err(|_| BrowserError::UnsupportedEgress)?;
            if addresses.len() > 16 {
                return Err(BrowserError::Limit);
            }
            destinations.insert(authority(url)?, addresses);
        }
        Ok(Self {
            destinations,
            origins: parsed
                .iter()
                .map(|url| url.origin().ascii_serialization())
                .collect(),
        })
    }

    pub(super) fn parse(
        origins: &[String],
        security: McpHttpSecurity,
    ) -> Result<Vec<Url>, BrowserError> {
        if origins.is_empty() || origins.len() > MAX_ORIGINS {
            return Err(BrowserError::InvalidInput);
        }
        let mut parsed = Vec::new();
        for origin in origins {
            let url = Url::parse(origin).map_err(|_| BrowserError::InvalidInput)?;
            if origin.len() > 1024
                || url.host_str().is_none_or(|host| host.contains('*'))
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/"
                || !(url.scheme() == "https"
                    || (security.allow_loopback_http
                        && url.scheme() == "http"
                        && url.host_str() == Some("127.0.0.1")))
            {
                return Err(BrowserError::UnsupportedEgress);
            }
            if parsed.iter().any(|old: &Url| old.origin() == url.origin()) {
                return Err(BrowserError::InvalidInput);
            }
            parsed.push(url);
        }
        Ok(parsed)
    }

    pub(super) fn permits(&self, raw: &str) -> bool {
        let Ok(url) = Url::parse(raw) else {
            return false;
        };
        raw.len() <= 2048
            && url.username().is_empty()
            && url.password().is_none()
            && self.origins.contains(&url.origin().ascii_serialization())
    }
}

#[cfg(test)]
mod tests;

fn authority(url: &Url) -> Result<String, BrowserError> {
    let host = url.host_str().ok_or(BrowserError::InvalidInput)?;
    let port = url
        .port_or_known_default()
        .ok_or(BrowserError::InvalidInput)?;
    Ok(format!("{host}:{port}"))
}

pub(super) struct Proxy {
    pub(super) address: SocketAddr,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
    }
}
impl Proxy {
    pub(super) async fn start(
        policy: EgressPolicy,
        cancellation: CancellationToken,
    ) -> Result<Self, BrowserError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| BrowserError::Process)?;
        let address = listener.local_addr().map_err(|_| BrowserError::Process)?;
        let stop = cancellation.child_token();
        let task_stop = stop.clone();
        let task = tokio::spawn(async move {
            let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            let remaining = Arc::new(AtomicU64::new(MAX_TASK_BYTES));
            let policy = Arc::new(policy);
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    () = task_stop.cancelled() => break,
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        let Ok(slot) = slots.clone().try_acquire_owned() else { drop(stream); continue; };
                        let policy = policy.clone();
                        let remaining = remaining.clone();
                        let stop = task_stop.clone();
                        tasks.spawn(async move {
                            let _slot = slot;
                            tokio::select! {
                                () = stop.cancelled() => {},
                                _ = tokio::time::timeout(Duration::from_secs(300), serve(stream, &policy, &remaining)) => {}
                            }
                        });
                    }
                }
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        Ok(Self {
            address,
            cancellation: stop,
            task,
        })
    }
}

async fn serve(
    mut incoming: TcpStream,
    policy: &EgressPolicy,
    remaining: &AtomicU64,
) -> Result<(), BrowserError> {
    let mut head = Vec::with_capacity(1024);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() == MAX_HEADER_BYTES {
                return Err(BrowserError::Limit);
            }
            head.push(
                incoming
                    .read_u8()
                    .await
                    .map_err(|_| BrowserError::Protocol)?,
            );
        }
        Ok(())
    })
    .await
    .map_err(|_| BrowserError::TimedOut)??;
    let text = std::str::from_utf8(&head).map_err(|_| BrowserError::Protocol)?;
    let mut lines = text.split("\r\n");
    let request = lines.next().ok_or(BrowserError::Protocol)?;
    let fields: Vec<_> = request.split(' ').collect();
    let allowed = fields.len() == 3
        && fields[0] == "CONNECT"
        && fields[2] == "HTTP/1.1"
        && policy.destinations.contains_key(fields[1]);
    if !allowed {
        incoming
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await
            .map_err(|_| BrowserError::Process)?;
        return Err(BrowserError::UnsupportedEgress);
    }
    // No proxy credentials, forwarding configuration, DNS, URI or host supplied
    // by the browser can replace these addresses fixed at task admission.
    let addresses = &policy.destinations[fields[1]];
    let mut upstream = tokio::time::timeout(
        Duration::from_secs(3),
        TcpStream::connect(addresses.as_slice()),
    )
    .await
    .map_err(|_| BrowserError::TimedOut)?
    .map_err(|_| BrowserError::Process)?;
    incoming
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .map_err(|_| BrowserError::Process)?;
    let (read_in, mut write_in) = incoming.split();
    let (read_out, mut write_out) = upstream.split();
    tokio::try_join!(
        async {
            copy_bounded(read_in, &mut write_out, remaining).await?;
            write_out.shutdown().await
        },
        async {
            copy_bounded(read_out, &mut write_in, remaining).await?;
            write_in.shutdown().await
        }
    )
    .map_err(|_| BrowserError::Process)?;
    Ok(())
}

async fn copy_bounded(
    mut read: impl tokio::io::AsyncRead + Unpin,
    write: &mut (impl tokio::io::AsyncWrite + Unpin),
    remaining: &AtomicU64,
) -> std::io::Result<()> {
    let mut buffer = [0u8; 16 * 1024];
    let mut local = MAX_TUNNEL_BYTES;
    loop {
        let amount = read.read(&mut buffer).await?;
        if amount == 0 {
            return Ok(());
        }
        let bytes = amount as u64;
        if local < bytes
            || remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |available| {
                    available.checked_sub(bytes)
                })
                .is_err()
        {
            return Err(std::io::Error::other("browser transfer budget exhausted"));
        }
        local -= bytes;
        write.write_all(&buffer[..amount]).await?;
    }
}
