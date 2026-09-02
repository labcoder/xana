use super::*;
use crate::{
    config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
    frontend::semantic::{FactAuthorityV1, FactSourceV1, UsageLedgerV1},
    shell::ShellConfig,
};
use futures::future::BoxFuture;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

type FakeResult = Result<Vec<UsageObservationV1>, SourceError>;

#[derive(Clone)]
struct FakeSource {
    calls: Arc<AtomicUsize>,
    results: Arc<Mutex<VecDeque<FakeResult>>>,
}

impl FakeSource {
    fn new(results: Vec<FakeResult>) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            results: Arc::new(Mutex::new(results.into())),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AccountUsageSource for FakeSource {
    fn fetch<'a>(
        &'a self,
        _: &'a ConnectionConfig,
        _: u64,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<UsageObservationV1>, SourceError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = self
            .results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(source_error("usage.unexpected_poll", false)));
        Box::pin(async move { result })
    }
}

const fn source_error(code: &'static str, retryable: bool) -> SourceError {
    SourceError { code, retryable }
}

fn manager(root: &Path) -> ModelManager {
    let rendered = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Ollama {
            name: "local".into(),
            base_url: "http://localhost:11434/v1".into(),
        },
        model: "qwen".into(),
        max_tool_rounds: 8,
        shell: ShellConfig::default(),
        permission_mode: PermissionMode::Ask,
        reasoning_effort: None,
    })
    .unwrap();
    ModelManager::new(
        XanaConfig::parse_registry(&rendered).unwrap(),
        root.join("models"),
        root.join("selection.toml"),
    )
}

fn account_fact(value: u64, observed_at: u64) -> UsageObservationV1 {
    let mut observation = UsageObservationV1 {
        id: Uuid::nil(),
        scope: UsageScopeV1::Account {
            connection: "local".into(),
            account: "configured".into(),
        },
        period: "current".into(),
        accounting: UsageAccountingV1::CumulativeSnapshot { sequence: 1 },
        amounts: UsageAmountsV1 {
            input_tokens: Some(value),
            ..UsageAmountsV1::default()
        },
        context: None,
        rate_limit: None,
        quota: None,
        credits: None,
        request_affinity_digest: None,
        availability: AvailabilityV1::Available,
        source: FactSourceV1::Provider,
        authority: FactAuthorityV1::ProviderReported,
        freshness: FreshnessV1 {
            observed_at_unix_millis: observed_at,
            max_age_millis: Some(60_000),
        },
    };
    assign_stable_id(&mut observation);
    observation
}

#[test]
fn native_observation_keeps_partial_provider_facts_distinct() {
    let usage = AgentTurnUsage {
        input_tokens: Some(10),
        cached_input_tokens: Some(4),
        output_tokens: Some(3),
        requests: 1,
        request_affinities: vec![[7; 16]],
        ..AgentTurnUsage::default()
    };
    let observation =
        native_usage_observation(OperationId::new(), "turn-1", &usage, Some(1_000), 100);

    assert_eq!(observation.amounts.input_tokens, Some(10));
    assert_eq!(observation.amounts.cached_input_tokens, Some(4));
    assert_eq!(observation.amounts.cache_write_input_tokens, None);
    assert!(observation.request_affinity_digest.is_some());
    assert_eq!(
        observation
            .context
            .as_ref()
            .and_then(|value| value.capacity_tokens),
        Some(1_000)
    );
    observation.validate().unwrap();
}

#[test]
fn native_and_managed_observation_ids_ignore_freshness_but_preserve_accounting_identity() {
    let run = OperationId::new();
    let usage = AgentTurnUsage {
        input_tokens: Some(10),
        output_tokens: Some(2),
        requests: 1,
        ..AgentTurnUsage::default()
    };
    let first = native_usage_observation(run, "turn", &usage, Some(100), 1);
    let replay = native_usage_observation(run, "turn", &usage, Some(100), 999);
    assert_eq!(first.id, replay.id);

    let conversation = ConversationId::for_native(crate::identity::SessionId::new());
    let managed = ManagedTokenUsage {
        input_tokens: 7,
        output_tokens: 3,
        total_tokens: 10,
        cached_input_tokens: Some(2),
        reasoning_tokens: Some(1),
        context_input_tokens: Some(7),
        context_window_tokens: Some(100),
    };
    let snapshot = managed_usage_observation(conversation, "thread", managed, 4, 1);
    let replay = managed_usage_observation(conversation, "thread", managed, 4, 2);
    let newer = managed_usage_observation(conversation, "thread", managed, 5, 2);
    assert_eq!(snapshot.id, replay.id);
    assert_ne!(snapshot.id, newer.id);
    assert_eq!(
        snapshot.context.as_ref().map(|value| value.input_tokens),
        Some(7)
    );
}

#[test]
fn delta_and_cumulative_replays_do_not_double_count_and_reset_periods_are_isolated() {
    let run = OperationId::new();
    let scope = UsageScopeV1::Run { run_id: run };
    let usage = AgentTurnUsage {
        input_tokens: Some(3),
        cached_input_tokens: Some(1),
        output_tokens: Some(2),
        requests: 1,
        ..AgentTurnUsage::default()
    };
    let delta = native_usage_observation(run, "window-a", &usage, None, 1);
    let mut ledger = UsageLedgerV1::default();
    assert!(ledger.observe(delta.clone()).unwrap());
    assert!(!ledger.observe(delta).unwrap());

    let mut cumulative = account_fact(10, 1);
    cumulative.scope = scope.clone();
    cumulative.period = "window-a".into();
    cumulative.accounting = UsageAccountingV1::CumulativeSnapshot { sequence: 2 };
    assign_stable_id(&mut cumulative);
    ledger.observe(cumulative).unwrap();
    let mut older = account_fact(99, 2);
    older.scope = scope.clone();
    older.period = "window-a".into();
    older.accounting = UsageAccountingV1::CumulativeSnapshot { sequence: 1 };
    assign_stable_id(&mut older);
    assert!(!ledger.observe(older).unwrap());

    let aggregate = ledger.aggregate(&scope, "window-a").unwrap();
    assert_eq!(aggregate.amounts.input_tokens, Some(13));
    assert_eq!(
        ledger
            .aggregate(&scope, "window-b")
            .unwrap()
            .amounts
            .input_tokens,
        None
    );
}

#[tokio::test]
async fn fresh_cache_and_forced_refresh_throttle_do_not_poll_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let source = FakeSource::new(vec![Ok(vec![account_fact(7, 1_000)])]);
    let service =
        UsageObservationService::with_source(directory.path().join("cache"), source.clone());
    let cancellation = CancellationToken::new();

    let live = service
        .query_at(&manager, "local", Some("qwen"), false, 1_000, &cancellation)
        .await
        .unwrap();
    assert_eq!(live.cache_status, UsageCacheStatusV1::Live);
    let cached = service
        .query_at(&manager, "local", Some("qwen"), false, 1_500, &cancellation)
        .await
        .unwrap();
    assert_eq!(cached.cache_status, UsageCacheStatusV1::FreshCache);
    let throttled = service
        .query_at(&manager, "local", Some("qwen"), true, 1_500, &cancellation)
        .await
        .unwrap();
    assert_eq!(
        throttled.cache_status,
        UsageCacheStatusV1::RefreshLimitedCache
    );
    assert_eq!(source.calls(), 1);
    tokio::task::yield_now().await;
    assert_eq!(source.calls(), 1, "rendering must not start an idle poller");
}

#[tokio::test]
async fn failed_expired_refresh_returns_explicitly_stale_cached_facts() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let source = FakeSource::new(vec![
        Ok(vec![account_fact(7, 1_000)]),
        Err(source_error("usage.provider_unavailable", false)),
    ]);
    let service =
        UsageObservationService::with_source(directory.path().join("cache"), source.clone());
    let cancellation = CancellationToken::new();
    service
        .query_at(&manager, "local", None, false, 1_000, &cancellation)
        .await
        .unwrap();
    let stale = service
        .query_at(&manager, "local", None, false, 62_000, &cancellation)
        .await
        .unwrap();

    assert_eq!(stale.cache_status, UsageCacheStatusV1::StaleCache);
    assert_eq!(stale.observations[0].availability, AvailabilityV1::Stale);
    assert_eq!(stale.notices, ["usage.provider_unavailable"]);
    assert_eq!(source.calls(), 2);
}

#[tokio::test]
async fn retry_is_bounded_to_one_attempt_and_cancellation_is_control_flow() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let retrying = FakeSource::new(vec![
        Err(source_error("usage.provider_retryable", true)),
        Ok(vec![account_fact(2, 1)]),
    ]);
    let service =
        UsageObservationService::with_source(directory.path().join("cache-a"), retrying.clone());
    let report = service
        .query_at(&manager, "local", None, false, 1, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.cache_status, UsageCacheStatusV1::Live);
    assert_eq!(retrying.calls(), 2);

    let cancelled = FakeSource::new(vec![Err(source_error("usage.cancelled", false))]);
    let service = UsageObservationService::with_source(directory.path().join("cache-b"), cancelled);
    assert!(matches!(
        service
            .query_at(&manager, "local", None, false, 1, &CancellationToken::new(),)
            .await,
        Err(UsageObservationError::Cancelled)
    ));
}

#[tokio::test]
async fn corrupt_or_oversized_cache_is_ignored_and_replaced_by_bounded_live_data() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let cache_root = directory.path().join("cache");
    std::fs::create_dir_all(cache_root.join("usage")).unwrap();
    std::fs::write(cache_root.join("usage/local.json"), b"not-json").unwrap();
    let source = FakeSource::new(vec![Ok(vec![account_fact(4, 1)])]);
    let service = UsageObservationService::with_source(cache_root.clone(), source.clone());
    let report = service
        .query_at(&manager, "local", None, false, 1, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.cache_status, UsageCacheStatusV1::Live);
    assert_eq!(source.calls(), 1);
    assert!(
        std::fs::metadata(cache_root.join("usage/local.json"))
            .unwrap()
            .len()
            < CACHE_MAX_BYTES as u64
    );
}
