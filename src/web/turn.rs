use super::WebLimits;
use crate::identity::OperationId;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebFailure {
    NotConfigured,
    Authentication,
    RateLimited,
    Missing,
    Challenge,
    InvalidResponse,
    TooLarge,
    Policy,
    Budget,
    Cancelled,
    TimedOut,
    Unavailable,
    Interrupted,
}

impl std::fmt::Display for WebFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let advice = match self {
            Self::NotConfigured => {
                "search is not configured; use xana connect web, not guessed URLs"
            }
            Self::Authentication => {
                "check the selected search credential; no provider fallback was attempted"
            }
            Self::RateLimited => "the recipient rate-limited this request; stop or try again later",
            Self::Missing => {
                "the page was not found; use search for a verified source instead of guessing paths"
            }
            Self::Challenge => {
                "the page requires interaction or refused access; report the limitation or request a browser"
            }
            Self::InvalidResponse => "the response does not match the supported text contract",
            Self::TooLarge => "the response exceeds the configured bound; choose a smaller source",
            Self::Policy => {
                "the request is outside public-web authority; no private destination or credential forwarding is allowed"
            }
            Self::Budget => {
                "this turn's web allowance is exhausted; answer from collected evidence and state what remains unknown"
            }
            Self::Cancelled => "the request was cancelled; no automatic replay",
            Self::TimedOut => {
                "the request deadline expired; its remote outcome may be unknown; no automatic replay"
            }
            Self::Unavailable => "the recipient or transport is unavailable; no automatic replay",
            Self::Interrupted => {
                "an identical request was interrupted after admission; its outcome is unknown and it will not be replayed in this turn"
            }
        };
        write!(f, "web {self:?}: {advice}")
    }
}

/// Installed with a Conversation's tool registry. Native runtime admission
/// serializes its owner turns; in-flight work retains its original Arc even
/// after a later turn starts. Nothing is persisted or shared across clients.
pub(crate) struct WebRuntime {
    limits: WebLimits,
    concurrency: Arc<Semaphore>,
    profile_egress: std::collections::BTreeSet<crate::config::OutboundDataClass>,
    current: Mutex<Option<(OperationId, Arc<WebTurn>)>>,
}

impl WebRuntime {
    pub(crate) fn new(limits: WebLimits) -> Self {
        Self {
            concurrency: Arc::new(Semaphore::new(usize::from(limits.concurrency))),
            limits,
            profile_egress: std::collections::BTreeSet::from([
                crate::config::OutboundDataClass::PromptText,
            ]),
            current: Mutex::new(None),
        }
    }
    pub(crate) fn with_profile(mut self, egress: &[crate::config::OutboundDataClass]) -> Self {
        self.profile_egress = egress.iter().copied().collect();
        self
    }
    pub(crate) fn policy(&self) -> crate::outbound::OutboundPolicyLayers {
        let allowed =
            std::collections::BTreeSet::from([crate::config::OutboundDataClass::PromptText]);
        crate::outbound::OutboundPolicyLayers {
            connection_allowed: allowed.clone(),
            user_ceiling: allowed,
            profile_allowed: self.profile_egress.clone(),
            conversation_allowed: None,
        }
    }
    pub(crate) fn turn(&self, operation: OperationId) -> Arc<WebTurn> {
        let mut current = self.current.lock().expect("web turn owner");
        if let Some((id, turn)) = current.as_ref()
            && *id == operation
        {
            return Arc::clone(turn);
        }
        let turn = Arc::new(WebTurn::with_admission(
            self.limits.clone(),
            Arc::clone(&self.concurrency),
        ));
        *current = Some((operation, Arc::clone(&turn)));
        turn
    }
}

#[derive(Default)]
struct Counters {
    searches: u16,
    attempts: u16,
    ingress: usize,
}
#[derive(Default)]
struct Cached {
    started: bool,
    result: Option<Result<String, WebFailure>>,
}

pub(crate) struct WebTurn {
    public_web: AtomicBool,
    limits: WebLimits,
    started: Instant,
    counters: Mutex<Counters>,
    concurrency: Arc<Semaphore>,
    cache: Mutex<BTreeMap<[u8; 32], Arc<AsyncMutex<Cached>>>>,
}

impl WebTurn {
    #[cfg(test)]
    fn new(limits: WebLimits) -> Self {
        let concurrency = Arc::new(Semaphore::new(usize::from(limits.concurrency)));
        Self::with_admission(limits, concurrency)
    }
    fn with_admission(limits: WebLimits, concurrency: Arc<Semaphore>) -> Self {
        Self {
            public_web: AtomicBool::new(false),
            concurrency,
            limits,
            started: Instant::now(),
            counters: Mutex::new(Counters::default()),
            cache: Mutex::new(BTreeMap::new()),
        }
    }
    pub(crate) fn elapsed_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    pub(crate) fn allow_public_web(&self) {
        self.public_web.store(true, Ordering::Release);
    }
    pub(crate) fn permits_public_web(&self) -> bool {
        self.public_web.load(Ordering::Acquire)
    }
    pub(crate) fn attempt(&self) -> Result<(), WebFailure> {
        let mut counters = self.counters.lock().expect("web counters");
        if counters.attempts >= self.limits.attempts
            || counters.ingress >= self.limits.ingress_bytes
        {
            return Err(WebFailure::Budget);
        }
        counters.attempts += 1;
        Ok(())
    }
    pub(crate) fn ingress(&self, bytes: usize) -> Result<(), WebFailure> {
        let mut counters = self.counters.lock().expect("web counters");
        counters.ingress = counters.ingress.saturating_add(bytes);
        if counters.ingress > self.limits.ingress_bytes {
            return Err(WebFailure::Budget);
        }
        Ok(())
    }
    /// Call only after fresh authority checks. Cache terminal failures as well
    /// as successes; cancellation cannot cause OnceCell-style reinitialization.
    pub(crate) async fn cached<F: Future<Output = Result<String, WebFailure>>>(
        &self,
        key: blake3::Hash,
        search: bool,
        work: impl FnOnce(Arc<OwnedSemaphorePermit>) -> F,
    ) -> Result<String, WebFailure> {
        let key = *key.as_bytes();
        let entry = {
            let mut cache = self.cache.lock().expect("web cache");
            if !cache.contains_key(&key) && cache.len() >= usize::from(self.limits.attempts) {
                return Err(WebFailure::Budget);
            }
            Arc::clone(cache.entry(key).or_default())
        };
        let mut cached = entry.lock().await;
        if let Some(result) = &cached.result {
            return result.clone();
        }
        if cached.started {
            return Err(WebFailure::Interrupted);
        }
        let slot = Arc::new(
            Arc::clone(&self.concurrency)
                .acquire_owned()
                .await
                .map_err(|_| WebFailure::Cancelled)?,
        );
        cached.started = true;
        if search {
            let mut counters = self.counters.lock().expect("web counters");
            if counters.searches >= self.limits.searches {
                cached.result = Some(Err(WebFailure::Budget));
                return Err(WebFailure::Budget);
            }
            counters.searches += 1;
        }
        let result = work(slot).await;
        cached.result = Some(result.clone());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn identical_authorized_work_and_failures_do_not_spend_again() {
        let turn = WebTurn::new(WebLimits::default());
        let key = blake3::hash(b"search");
        assert_eq!(
            turn.cached(key, true, |_| async { Err(WebFailure::RateLimited) })
                .await,
            Err(WebFailure::RateLimited)
        );
        assert_eq!(
            turn.cached(key, true, |_| async { panic!("duplicate dispatch") })
                .await,
            Err(WebFailure::RateLimited)
        );
        assert_eq!(turn.counters.lock().unwrap().searches, 1);
    }
    #[tokio::test]
    async fn interrupted_work_is_not_replayed_and_new_turn_is_independent() {
        let runtime = WebRuntime::new(WebLimits::default());
        let operation = OperationId::new();
        let turn = runtime.turn(operation);
        let key = blake3::hash(b"request");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(5),
                turn.cached(key, true, |_| std::future::pending())
            )
            .await
            .is_err()
        );
        assert_eq!(
            runtime
                .turn(operation)
                .cached(key, true, |_| async { panic!("unsafe replay") })
                .await,
            Err(WebFailure::Interrupted)
        );
        assert_eq!(
            runtime
                .turn(OperationId::new())
                .cached(key, true, |_| async { Ok("fresh".into()) })
                .await,
            Ok("fresh".into())
        );
    }
    #[test]
    fn attempts_and_ingress_are_aggregate_not_success_counters() {
        let turn = WebTurn::new(WebLimits::default());
        for _ in 0..8 {
            turn.attempt().unwrap();
        }
        assert_eq!(turn.attempt(), Err(WebFailure::Budget));
        turn.ingress(16 * 1024 * 1024).unwrap();
        assert_eq!(turn.ingress(1), Err(WebFailure::Budget));
        let ingress = WebTurn::new(WebLimits::default());
        assert_eq!(
            ingress.ingress(16 * 1024 * 1024 + 1),
            Err(WebFailure::Budget)
        );
        assert_eq!(
            ingress.attempt(),
            Err(WebFailure::Budget),
            "exhaustion stops new sends before transport"
        );
    }

    #[tokio::test]
    async fn cancelled_blocking_work_retains_admission_until_it_really_exits() {
        let runtime = WebRuntime::new(WebLimits {
            concurrency: 1,
            ..Default::default()
        });
        let turn = runtime.turn(OperationId::new());
        let (release, held) = std::sync::mpsc::channel();
        let (entered, started) = tokio::sync::oneshot::channel();
        let mut first = Box::pin(
            turn.cached(blake3::hash(b"slow"), false, |slot| async move {
                tokio::task::spawn_blocking(move || {
                    let _slot = slot;
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                })
                .await
                .unwrap();
                Ok("done".into())
            }),
        );
        tokio::select! { _ = &mut first => panic!("should wait"), _ = started => {} }
        drop(first);
        let next_turn = runtime.turn(OperationId::new());
        let second = next_turn.cached(blake3::hash(b"next"), false, |_| async {
            Ok("escaped".into())
        });
        let blocked = tokio::time::timeout(std::time::Duration::from_millis(10), second)
            .await
            .is_err();
        release.send(()).unwrap();
        assert!(
            blocked,
            "cancelled blocking work must retain its admission slot"
        );
        assert_eq!(
            turn.cached(blake3::hash(b"later"), false, |_| async { Ok("ok".into()) })
                .await
                .unwrap(),
            "ok"
        );
    }
}
