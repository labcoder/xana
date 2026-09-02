//! Bounded provider, model, request, and account usage observations.
//!
//! Fetching is an explicit control-plane action. This module never polls in the
//! background, never treats an unavailable value as zero, and never exposes a
//! provider response or credential through its frontend-safe report.

mod source;

use crate::{
    agent::AgentTurnUsage,
    bounded_file,
    config::{ConnectionConfig, ProviderKind},
    frontend::semantic::{
        AvailabilityV1, ContextOccupancyV1, FactAuthorityV1, FactSourceV1, FreshnessV1,
        SemanticError, UsageAccountingV1, UsageAmountsV1, UsageObservationV1, UsageScopeV1,
    },
    identity::{ConversationId, OperationId},
    managed::codex::ManagedTokenUsage,
    model_catalog::{ExecutionKind, ModelDescriptor, ModelManager, ModelPricing},
};
use atomic_write_file::AtomicWriteFile;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const CACHE_VERSION: u16 = 1;
const CACHE_MAX_BYTES: usize = 256 * 1024;
const MAX_OBSERVATIONS: usize = 128;
const DEFAULT_CACHE_AGE: Duration = Duration::from_secs(60);
const MIN_FORCE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const SOURCE_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageCacheStatusV1 {
    Live,
    FreshCache,
    RefreshLimitedCache,
    StaleCache,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelUsageFactsV1 {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) execution: ExecutionKind,
    pub(crate) input_modalities: Vec<String>,
    pub(crate) tools: Option<bool>,
    pub(crate) reasoning: Option<bool>,
    pub(crate) context_tokens: Option<usize>,
    pub(crate) max_output_tokens: Option<usize>,
    pub(crate) pricing: ModelPricing,
    pub(crate) source: FactSourceV1,
    pub(crate) freshness: FreshnessV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageReportV1 {
    pub(crate) version: u16,
    pub(crate) connection: String,
    pub(crate) provider: ProviderKind,
    pub(crate) model: Option<ModelUsageFactsV1>,
    pub(crate) cache_status: UsageCacheStatusV1,
    pub(crate) observations: Vec<UsageObservationV1>,
    pub(crate) notices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheDocument {
    version: u16,
    connection: String,
    provider: ProviderKind,
    observed_at_unix_millis: u64,
    observations: Vec<UsageObservationV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceError {
    pub(super) code: &'static str,
    pub(super) retryable: bool,
}

pub(super) trait AccountUsageSource: Send + Sync {
    fn fetch<'a>(
        &'a self,
        connection: &'a ConnectionConfig,
        observed_at_unix_millis: u64,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<UsageObservationV1>, SourceError>>;
}

pub(crate) struct UsageObservationService<S = source::LiveAccountUsageSource> {
    source: S,
    cache_root: PathBuf,
    cache_age: Duration,
    min_force_refresh_interval: Duration,
}

impl UsageObservationService<source::LiveAccountUsageSource> {
    pub(crate) fn new(cache_root: PathBuf) -> Self {
        Self::with_source(cache_root, source::LiveAccountUsageSource::new())
    }
}

impl<S: AccountUsageSource> UsageObservationService<S> {
    fn with_source(cache_root: PathBuf, source: S) -> Self {
        Self {
            source,
            cache_root,
            cache_age: DEFAULT_CACHE_AGE,
            min_force_refresh_interval: MIN_FORCE_REFRESH_INTERVAL,
        }
    }

    pub(crate) async fn query(
        &self,
        manager: &ModelManager,
        connection_id: &str,
        model: Option<&str>,
        force_refresh: bool,
        cancellation: &CancellationToken,
    ) -> Result<UsageReportV1, UsageObservationError> {
        self.query_at(
            manager,
            connection_id,
            model,
            force_refresh,
            now_unix_millis()?,
            cancellation,
        )
        .await
    }

    async fn query_at(
        &self,
        manager: &ModelManager,
        connection_id: &str,
        model: Option<&str>,
        force_refresh: bool,
        now_millis: u64,
        cancellation: &CancellationToken,
    ) -> Result<UsageReportV1, UsageObservationError> {
        if cancellation.is_cancelled() {
            return Err(UsageObservationError::Cancelled);
        }
        let connection = manager
            .connection(connection_id)
            .map_err(|error| UsageObservationError::Configuration(error.to_string()))?
            .clone();
        let model = model
            .map(|model| manager.descriptor(connection_id, model))
            .transpose()
            .map_err(|error| UsageObservationError::Configuration(error.to_string()))?
            .map(|descriptor| model_facts(&connection, descriptor, now_millis));
        let cached = self.read_cache(&connection);
        if let Some(cache) = cached.as_ref() {
            let age = now_millis.saturating_sub(cache.observed_at_unix_millis);
            if !force_refresh && age <= millis(self.cache_age) {
                return Ok(report_from_cache(
                    &connection,
                    model,
                    cache,
                    UsageCacheStatusV1::FreshCache,
                ));
            }
            if force_refresh && age < millis(self.min_force_refresh_interval) {
                return Ok(report_from_cache(
                    &connection,
                    model,
                    cache,
                    UsageCacheStatusV1::RefreshLimitedCache,
                ));
            }
        }

        let fetched = self
            .fetch_with_one_retry(&connection, now_millis, cancellation)
            .await;
        match fetched {
            Ok(observations) => {
                validate_observations(&observations)?;
                let document = CacheDocument {
                    version: CACHE_VERSION,
                    connection: connection.id.clone(),
                    provider: connection.kind,
                    observed_at_unix_millis: now_millis,
                    observations: observations.clone(),
                };
                let mut notices = Vec::new();
                if self.write_cache(&connection, &document).is_err() {
                    notices.push("usage.cache_write_unavailable".to_owned());
                }
                Ok(UsageReportV1 {
                    version: 1,
                    connection: connection.id,
                    provider: connection.kind,
                    model,
                    cache_status: UsageCacheStatusV1::Live,
                    observations,
                    notices,
                })
            }
            Err(error) => {
                if error.code == "usage.cancelled" || cancellation.is_cancelled() {
                    return Err(UsageObservationError::Cancelled);
                }
                if let Some(mut cache) = cached {
                    mark_stale(&mut cache.observations);
                    let mut report = report_from_cache(
                        &connection,
                        model,
                        &cache,
                        UsageCacheStatusV1::StaleCache,
                    );
                    report.notices.push(error.code.to_owned());
                    return Ok(report);
                }
                Ok(UsageReportV1 {
                    version: 1,
                    connection: connection.id.clone(),
                    provider: connection.kind,
                    model,
                    cache_status: UsageCacheStatusV1::Unavailable,
                    observations: vec![source::unavailable_observation(
                        &connection,
                        now_millis,
                        error.code,
                    )],
                    notices: vec![error.code.to_owned()],
                })
            }
        }
    }

    async fn fetch_with_one_retry(
        &self,
        connection: &ConnectionConfig,
        now_millis: u64,
        cancellation: &CancellationToken,
    ) -> Result<Vec<UsageObservationV1>, SourceError> {
        let first = self.fetch_once(connection, now_millis, cancellation).await;
        let Err(error) = first else {
            return first;
        };
        if !error.retryable {
            return Err(error);
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(SourceError { code: "usage.cancelled", retryable: false }),
            _ = tokio::time::sleep(RETRY_DELAY) => self.fetch_once(connection, now_millis, cancellation).await,
        }
    }

    async fn fetch_once(
        &self,
        connection: &ConnectionConfig,
        now_millis: u64,
        cancellation: &CancellationToken,
    ) -> Result<Vec<UsageObservationV1>, SourceError> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(SourceError { code: "usage.cancelled", retryable: false }),
            result = tokio::time::timeout(
                SOURCE_TIMEOUT,
                self.source.fetch(connection, now_millis, cancellation),
            ) => match result {
                Ok(result) => result,
                Err(_) => Err(SourceError { code: "usage.source_timeout", retryable: true }),
            },
        }
    }

    fn cache_path(&self, connection: &ConnectionConfig) -> PathBuf {
        self.cache_root
            .join("usage")
            .join(format!("{}.json", connection.id))
    }

    fn read_cache(&self, connection: &ConnectionConfig) -> Option<CacheDocument> {
        let input =
            bounded_file::read_to_string(&self.cache_path(connection), CACHE_MAX_BYTES).ok()?;
        let document = serde_json::from_str::<CacheDocument>(&input).ok()?;
        (document.version == CACHE_VERSION
            && document.connection == connection.id
            && document.provider == connection.kind
            && document.observations.len() <= MAX_OBSERVATIONS
            && validate_observations(&document.observations).is_ok())
        .then_some(document)
    }

    fn write_cache(
        &self,
        connection: &ConnectionConfig,
        document: &CacheDocument,
    ) -> Result<(), UsageObservationError> {
        let encoded = serde_json::to_vec_pretty(document)
            .map_err(|error| UsageObservationError::Cache(error.to_string()))?;
        if encoded.len() > CACHE_MAX_BYTES {
            return Err(UsageObservationError::Cache(
                "usage cache exceeds its byte limit".to_owned(),
            ));
        }
        let path = self.cache_path(connection);
        let parent = path
            .parent()
            .ok_or_else(|| UsageObservationError::Cache("usage cache has no parent".into()))?;
        fs::create_dir_all(parent).map_err(|error| cache_io(&path, error))?;
        let mut file = AtomicWriteFile::open(&path).map_err(|error| cache_io(&path, error))?;
        file.write_all(&encoded)
            .map_err(|error| cache_io(&path, error))?;
        file.commit().map_err(|error| cache_io(&path, error))
    }
}

fn report_from_cache(
    connection: &ConnectionConfig,
    model: Option<ModelUsageFactsV1>,
    cache: &CacheDocument,
    cache_status: UsageCacheStatusV1,
) -> UsageReportV1 {
    UsageReportV1 {
        version: 1,
        connection: connection.id.clone(),
        provider: connection.kind,
        model,
        cache_status,
        observations: cache.observations.clone(),
        notices: Vec::new(),
    }
}

fn model_facts(
    connection: &ConnectionConfig,
    descriptor: ModelDescriptor,
    observed_at_unix_millis: u64,
) -> ModelUsageFactsV1 {
    ModelUsageFactsV1 {
        connection: connection.id.clone(),
        model: descriptor.id,
        execution: if connection.kind == ProviderKind::Codex {
            ExecutionKind::Managed
        } else {
            ExecutionKind::Native
        },
        input_modalities: descriptor.input_modalities.into_iter().collect(),
        tools: descriptor.tools,
        reasoning: descriptor.reasoning,
        context_tokens: descriptor.context_tokens,
        max_output_tokens: descriptor.max_output_tokens,
        pricing: descriptor.pricing,
        source: match descriptor.source {
            crate::model_catalog::DescriptorSource::ManagedRuntime => FactSourceV1::ManagedRuntime,
            crate::model_catalog::DescriptorSource::Remote => FactSourceV1::Provider,
            crate::model_catalog::DescriptorSource::Configured => FactSourceV1::Cache,
        },
        freshness: FreshnessV1 {
            observed_at_unix_millis,
            max_age_millis: None,
        },
    }
}

#[allow(dead_code)] // The M4-05 command/event registry wires this producer into every surface.
pub(crate) fn native_usage_observation(
    run_id: OperationId,
    period: &str,
    usage: &AgentTurnUsage,
    context_capacity_tokens: Option<u64>,
    observed_at_unix_millis: u64,
) -> UsageObservationV1 {
    let mut observation = UsageObservationV1 {
        id: Uuid::nil(),
        scope: UsageScopeV1::Run { run_id },
        period: period.to_owned(),
        accounting: UsageAccountingV1::Delta,
        amounts: UsageAmountsV1 {
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_input_tokens: usage.cache_write_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            tool_tokens: usage.tool_tokens,
            request_count: Some(usage.requests),
            prompt_bytes: usage.prompt_bytes,
            tool_schema_bytes: usage.tool_schema_bytes,
            cost_microunits: usage.cost_microunits,
        },
        context: usage.input_tokens.map(|input_tokens| ContextOccupancyV1 {
            input_tokens,
            capacity_tokens: context_capacity_tokens,
            compacted_tokens: None,
            derived_summary_tokens: None,
            retrieval_tokens: None,
        }),
        rate_limit: None,
        quota: None,
        credits: None,
        request_affinity_digest: affinity_digest(&usage.request_affinities),
        availability: AvailabilityV1::Available,
        source: FactSourceV1::Provider,
        authority: FactAuthorityV1::ProviderReported,
        freshness: FreshnessV1 {
            observed_at_unix_millis,
            max_age_millis: None,
        },
    };
    assign_stable_id(&mut observation);
    observation
}

#[allow(dead_code)] // The M4-05 command/event registry wires this producer into every surface.
pub(crate) fn managed_usage_observation(
    conversation_id: ConversationId,
    period: &str,
    usage: ManagedTokenUsage,
    sequence: u64,
    observed_at_unix_millis: u64,
) -> UsageObservationV1 {
    let mut observation = UsageObservationV1 {
        id: Uuid::nil(),
        scope: UsageScopeV1::Conversation { conversation_id },
        period: period.to_owned(),
        accounting: UsageAccountingV1::CumulativeSnapshot { sequence },
        amounts: UsageAmountsV1 {
            input_tokens: Some(usage.input_tokens),
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_input_tokens: None,
            output_tokens: Some(usage.output_tokens),
            reasoning_tokens: usage.reasoning_tokens,
            tool_tokens: None,
            request_count: None,
            prompt_bytes: None,
            tool_schema_bytes: None,
            cost_microunits: None,
        },
        context: usage
            .context_input_tokens
            .map(|input_tokens| ContextOccupancyV1 {
                input_tokens,
                capacity_tokens: usage
                    .context_window_tokens
                    .filter(|capacity| input_tokens <= *capacity),
                compacted_tokens: None,
                derived_summary_tokens: None,
                retrieval_tokens: None,
            }),
        rate_limit: None,
        quota: None,
        credits: None,
        request_affinity_digest: None,
        availability: AvailabilityV1::Available,
        source: FactSourceV1::ManagedRuntime,
        authority: FactAuthorityV1::ProviderReported,
        freshness: FreshnessV1 {
            observed_at_unix_millis,
            max_age_millis: None,
        },
    };
    assign_stable_id(&mut observation);
    observation
}

pub(super) fn assign_stable_id(observation: &mut UsageObservationV1) {
    let observed_at = observation.freshness.observed_at_unix_millis;
    observation.id = Uuid::nil();
    observation.freshness.observed_at_unix_millis = 0;
    let encoded = serde_json::to_vec(observation).unwrap_or_default();
    observation.id = Uuid::new_v5(&Uuid::NAMESPACE_URL, &encoded);
    observation.freshness.observed_at_unix_millis = observed_at;
}

fn affinity_digest(values: &[[u8; 16]]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    for value in values.iter().take(64) {
        hasher.update(value);
    }
    Some(format!("blake3-{}", &hasher.finalize().to_hex()[..32]))
}

fn validate_observations(observations: &[UsageObservationV1]) -> Result<(), UsageObservationError> {
    if observations.len() > MAX_OBSERVATIONS {
        return Err(UsageObservationError::InvalidSource(
            "usage source returned too many observations".to_owned(),
        ));
    }
    for observation in observations {
        observation
            .validate()
            .map_err(UsageObservationError::Semantic)?;
    }
    Ok(())
}

fn mark_stale(observations: &mut [UsageObservationV1]) {
    for observation in observations {
        if observation.availability == AvailabilityV1::Available {
            observation.availability = AvailabilityV1::Stale;
        }
    }
}

fn now_unix_millis() -> Result<u64, UsageObservationError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UsageObservationError::Clock)?
        .as_millis();
    u64::try_from(millis).map_err(|_| UsageObservationError::Clock)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn cache_io(path: &Path, source: io::Error) -> UsageObservationError {
    UsageObservationError::Cache(format!("{}: {source}", path.display()))
}

#[derive(Debug)]
pub(crate) enum UsageObservationError {
    Cancelled,
    Clock,
    Configuration(String),
    Cache(String),
    InvalidSource(String),
    Semantic(SemanticError),
}

impl fmt::Display for UsageObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("usage refresh was cancelled"),
            Self::Clock => formatter.write_str("system clock cannot represent usage freshness"),
            Self::Configuration(reason) => write!(formatter, "invalid usage query: {reason}"),
            Self::Cache(reason) => write!(formatter, "could not update usage cache: {reason}"),
            Self::InvalidSource(reason) => write!(formatter, "invalid usage source: {reason}"),
            Self::Semantic(source) => write!(formatter, "invalid usage observation: {source}"),
        }
    }
}

impl Error for UsageObservationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Semantic(source) => Some(source),
            Self::Cancelled
            | Self::Clock
            | Self::Configuration(_)
            | Self::Cache(_)
            | Self::InvalidSource(_) => None,
        }
    }
}

#[cfg(test)]
mod tests;
