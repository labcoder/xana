//! Private bounded CDP transport. Calls are sent once; response loss poisons the
//! task instead of reconnecting or replaying an input event.

use super::{BrowserError, proxy::EgressPolicy};
use futures::SinkExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{Mutex as AsyncMutex, oneshot},
    task::JoinHandle,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

mod reader;

type Writer = futures::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type Reply = oneshot::Sender<Result<Value, BrowserError>>;
const MAX_WIRE_BYTES: usize = 6 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_INFLIGHT: usize = 32;
const MAX_TARGETS: usize = 8;

#[derive(Default)]
struct State {
    next: u64,
    pending: BTreeMap<u64, Reply>,
    epochs: BTreeMap<String, u64>,
    sessions: BTreeMap<String, String>,
    ready: BTreeMap<(String, String), String>,
    configured: std::collections::BTreeSet<String>,
    failure: Option<BrowserError>,
    #[cfg(all(test, windows))]
    suppress_next_effect_reply: bool,
    #[cfg(all(test, windows))]
    suppressed_reply: Option<u64>,
}
struct Shared {
    writer: AsyncMutex<Option<Writer>>,
    state: Mutex<State>,
    cancelled: CancellationToken,
    policy: EgressPolicy,
}
#[derive(Clone)]
pub(super) struct Cdp {
    shared: Arc<Shared>,
}
pub(super) struct CdpOwner {
    pub(super) connection: Cdp,
    reader: JoinHandle<()>,
}
impl Drop for CdpOwner {
    fn drop(&mut self) {
        self.connection.shared.cancelled.cancel();
        self.reader.abort();
    }
}

impl CdpOwner {
    pub(super) async fn close(&mut self) -> Result<(), BrowserError> {
        self.connection.shared.cancelled.cancel();
        // The reader joins its policy tasks before returning. Abort-on-drop is
        // only a fallback, never evidence for a successful close receipt.
        let result = (&mut self.reader).await.map_err(|_| BrowserError::Process);
        self.connection.shared.writer.lock().await.take();
        result
    }

    pub(super) async fn connect(
        endpoint: &str,
        policy: EgressPolicy,
        cancelled: CancellationToken,
    ) -> Result<Self, BrowserError> {
        let endpoint = reqwest::Url::parse(endpoint).map_err(|_| BrowserError::Protocol)?;
        if endpoint.scheme() != "ws"
            || endpoint.host_str() != Some("127.0.0.1")
            || endpoint.port().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(BrowserError::Protocol);
        }
        let config = WebSocketConfig::default()
            .max_frame_size(Some(MAX_WIRE_BYTES))
            .max_message_size(Some(MAX_WIRE_BYTES))
            .read_buffer_size(8192);
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async_with_config(endpoint.as_str(), Some(config), false),
        )
        .await
        .map_err(|_| BrowserError::TimedOut)?
        .map_err(|_| BrowserError::Protocol)?;
        let (writer, reader) = futures::StreamExt::split(socket);
        let connection = Cdp {
            shared: Arc::new(Shared {
                writer: AsyncMutex::new(Some(writer)),
                state: Mutex::new(State::default()),
                cancelled,
                policy,
            }),
        };
        let task = tokio::spawn(reader::run(reader, connection.clone()));
        Ok(Self {
            connection,
            reader: task,
        })
    }
}

impl Cdp {
    #[cfg(all(test, windows))]
    pub(super) fn suppress_next_effect_reply_fixture(&self) {
        self.shared
            .state
            .lock()
            .expect("browser transport")
            .suppress_next_effect_reply = true;
    }
    pub(super) fn epoch(&self, session: &str) -> Result<u64, BrowserError> {
        let state = self.shared.state.lock().expect("browser transport");
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        state
            .epochs
            .get(session)
            .copied()
            .ok_or(BrowserError::Stale)
    }
    pub(super) async fn ready_session(&self, target: &str) -> Result<String, BrowserError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                {
                    let state = self.shared.state.lock().expect("browser transport");
                    if let Some(error) = state.failure {
                        return Err(error);
                    }
                    if let Some(session) = state.sessions.get(target)
                        && state.configured.contains(session)
                    {
                        return Ok(session.clone());
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| BrowserError::TimedOut)?
    }
    pub(super) async fn ready_document(
        &self,
        session: &str,
        frame: &str,
        loader: &str,
    ) -> Result<(), BrowserError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                {
                    let state = self.shared.state.lock().expect("browser transport");
                    if let Some(error) = state.failure {
                        return Err(error);
                    }
                    if state
                        .ready
                        .get(&(session.to_owned(), frame.to_owned()))
                        .is_some_and(|v| v == loader)
                    {
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| BrowserError::TimedOut)?
    }
    fn fail(&self, error: BrowserError) {
        let mut state = self.shared.state.lock().expect("browser transport");
        state.failure.get_or_insert(error);
        for (_, reply) in std::mem::take(&mut state.pending) {
            let _ = reply.send(Err(error));
        }
        self.shared.cancelled.cancel();
    }
    async fn configure(&self, session: &str) -> Result<(), BrowserError> {
        let patterns: Vec<_> = self
            .shared
            .policy
            .origins
            .iter()
            .map(|origin| json!({"urlPattern":format!("{origin}/*"),"block":false}))
            .chain(std::iter::once(
                json!({"urlPattern":"*://*:*/*","block":true}),
            ))
            .collect();
        for (method, params) in [
            (
                "Target.setAutoAttach",
                json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}),
            ),
            (
                "Network.enable",
                json!({"maxTotalBufferSize":131072,"maxResourceBufferSize":32768,"maxPostDataSize":0}),
            ),
            (
                "Network.setBlockedURLs",
                json!({"urlPatterns":patterns,"urls":["file:*"]}),
            ),
            ("Network.setBypassServiceWorker", json!({"bypass":true})),
            (
                "Fetch.enable",
                json!({"patterns":[{"urlPattern":"*","requestStage":"Request"}],"handleAuthRequests":true}),
            ),
            ("Page.enable", json!({})),
            ("DOM.enable", json!({})),
            (
                "Page.setInterceptFileChooserDialog",
                json!({"enabled":true}),
            ),
            ("Page.setLifecycleEventsEnabled", json!({"enabled":true})),
            ("Runtime.runIfWaitingForDebugger", json!({})),
        ] {
            self.call(method, params, Some(session)).await?;
        }
        self.shared
            .state
            .lock()
            .expect("browser transport")
            .configured
            .insert(session.to_owned());
        Ok(())
    }

    pub(super) async fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, BrowserError> {
        let (send, receive) = oneshot::channel();
        let id = {
            let mut state = self.shared.state.lock().expect("browser transport");
            if let Some(error) = state.failure {
                return Err(error);
            }
            if state.pending.len() >= MAX_INFLIGHT {
                return Err(BrowserError::Busy);
            }
            state.next = state.next.checked_add(1).ok_or(BrowserError::Limit)?;
            let id = state.next;
            #[cfg(all(test, windows))]
            if method == "Runtime.callFunctionOn"
                && params["userGesture"] == true
                && state.suppress_next_effect_reply
            {
                state.suppress_next_effect_reply = false;
                state.suppressed_reply = Some(id);
            }
            state.pending.insert(id, send);
            id
        };
        let mut envelope = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            envelope["sessionId"] = json!(session);
        }
        let bytes = serde_json::to_vec(&envelope).map_err(|_| BrowserError::InvalidInput)?;
        if bytes.len() > MAX_COMMAND_BYTES {
            self.shared
                .state
                .lock()
                .expect("browser transport")
                .pending
                .remove(&id);
            return Err(BrowserError::Limit);
        }
        let guard = CancelCall {
            connection: self.clone(),
            armed: true,
        };
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                biased;
                () = self.shared.cancelled.cancelled() => return Err(BrowserError::Cancelled),
                sent = async {
                    self.shared.writer.lock().await.as_mut()
                        .ok_or(BrowserError::Cancelled)?
                        .send(Message::Text(String::from_utf8(bytes).expect("JSON UTF8").into()))
                        .await.map_err(|_| BrowserError::Uncertain)
                } => sent?,
            }
            // A known reply wins over later connection closure. The reader's
            // shutdown drains unresolved replies; cancellation cannot erase an
            // acknowledgement that it already delivered.
            receive.await.map_err(|_| BrowserError::Uncertain)?
        })
        .await
        .unwrap_or(Err(BrowserError::Uncertain));
        if result.is_err() {
            self.fail(BrowserError::Uncertain);
        }
        let mut guard = guard;
        guard.armed = false;
        result
    }
}
struct CancelCall {
    connection: Cdp,
    armed: bool,
}
impl Drop for CancelCall {
    fn drop(&mut self) {
        if self.armed {
            self.connection.fail(BrowserError::Uncertain);
        }
    }
}

#[cfg(test)]
mod tests;
