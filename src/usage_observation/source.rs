use super::{AccountUsageSource, SourceError, assign_stable_id};
use crate::{
    config::{ConnectionConfig, ProviderKind},
    credential::{CredentialResolver, SecretString},
    frontend::semantic::{
        AvailabilityV1, CreditBalanceV1, FactAuthorityV1, FactSourceV1, FreshnessV1,
        LimitObservationV1, UsageAccountingV1, UsageAmountsV1, UsageObservationV1, UsageScopeV1,
    },
    managed::codex::{AccountStatus, CodexAppServer, CodexLaunchConfig},
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_ACCOUNT_RESPONSE_BYTES: usize = 256 * 1024;
const OBSERVATION_MAX_AGE_MILLIS: u64 = 60_000;

pub(crate) struct LiveAccountUsageSource {
    client: Client,
    credentials: CredentialResolver,
}

impl LiveAccountUsageSource {
    pub(super) fn new() -> Self {
        Self {
            client: crate::http_client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(12))
                .build()
                .expect("static account-usage client configuration is valid"),
            credentials: CredentialResolver::default(),
        }
    }

    async fn fetch_openrouter(
        &self,
        connection: &ConnectionConfig,
        observed_at: u64,
        cancellation: &CancellationToken,
    ) -> Result<Vec<UsageObservationV1>, SourceError> {
        let Some(reference) = &connection.credential else {
            return Ok(vec![permission_observation(
                connection,
                observed_at,
                "usage.inference_credential_required",
            )]);
        };
        let Ok(secret) = self.credentials.resolve(reference) else {
            return Ok(vec![permission_observation(
                connection,
                observed_at,
                "usage.inference_credential_unavailable",
            )]);
        };
        self.fetch_openrouter_authorized(connection, observed_at, cancellation, &secret)
            .await
    }

    async fn fetch_openrouter_authorized(
        &self,
        connection: &ConnectionConfig,
        observed_at: u64,
        cancellation: &CancellationToken,
        secret: &SecretString,
    ) -> Result<Vec<UsageObservationV1>, SourceError> {
        let base = connection
            .base_url
            .as_deref()
            .ok_or(SourceError::permanent("usage.invalid_endpoint"))?
            .trim_end_matches('/');
        let key = match self
            .get_json(&format!("{base}/key"), secret, cancellation)
            .await
        {
            Ok(value) => value,
            Err(HttpObservationError::Permission) => {
                return Ok(vec![permission_observation(
                    connection,
                    observed_at,
                    "usage.credential_rejected",
                )]);
            }
            Err(error) => return Err(error.into_source()),
        };
        let data = key
            .get("data")
            .and_then(Value::as_object)
            .ok_or(SourceError::permanent("usage.invalid_response"))?;
        let period = data
            .get("limit_reset")
            .and_then(Value::as_str)
            .map(|value| safe_fragment(value, "key-window"))
            .unwrap_or_else(|| "key-lifetime".to_owned());
        let usage = money_microunits(data.get("usage"));
        let limit = money_microunits(data.get("limit"));
        let remaining = money_microunits(data.get("limit_remaining"));
        let mut observations = vec![account_observation(
            connection,
            observed_at,
            &period,
            UsageAmountsV1 {
                cost_microunits: usage,
                ..UsageAmountsV1::default()
            },
            limit.or(remaining).map(|_| LimitObservationV1 {
                used_percent_basis_points: percentage_basis_points(usage, limit),
                remaining,
                limit,
                window_millis: None,
                reset_at_unix_millis: None,
            }),
            None,
            AvailabilityV1::Available,
        )];

        if data
            .get("is_management_key")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let credits = self
                .get_json(&format!("{base}/credits"), secret, cancellation)
                .await;
            observations.push(match credits {
                Ok(value) => openrouter_credit_observation(connection, observed_at, &value),
                Err(HttpObservationError::Permission) => permission_observation(
                    connection,
                    observed_at,
                    "usage.account_management_credential_rejected",
                ),
                Err(error) => unavailable_observation(connection, observed_at, error.code()),
            });
        } else {
            observations.push(permission_observation(
                connection,
                observed_at,
                "usage.account_management_credential_required",
            ));
        }
        Ok(observations)
    }

    async fn get_json(
        &self,
        endpoint: &str,
        secret: &SecretString,
        cancellation: &CancellationToken,
    ) -> Result<Value, HttpObservationError> {
        let request = self
            .client
            .get(endpoint)
            .bearer_auth(secret.expose())
            .send();
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(HttpObservationError::Cancelled),
            result = request => result.map_err(|_| HttpObservationError::Transport)?,
        };
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(HttpObservationError::Permission);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(HttpObservationError::Retryable);
        }
        if !status.is_success() {
            return Err(HttpObservationError::Rejected);
        }
        if response
            .content_length()
            .is_some_and(|bytes| bytes > MAX_ACCOUNT_RESPONSE_BYTES as u64)
        {
            return Err(HttpObservationError::TooLarge);
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(HttpObservationError::Cancelled),
            chunk = stream.next() => chunk,
        } {
            let chunk = chunk.map_err(|_| HttpObservationError::Transport)?;
            if body.len().saturating_add(chunk.len()) > MAX_ACCOUNT_RESPONSE_BYTES {
                return Err(HttpObservationError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| HttpObservationError::Invalid)
    }
}

impl AccountUsageSource for LiveAccountUsageSource {
    fn fetch<'a>(
        &'a self,
        connection: &'a ConnectionConfig,
        observed_at: u64,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<UsageObservationV1>, SourceError>> {
        Box::pin(async move {
            match connection.kind {
                ProviderKind::Codex => fetch_codex(connection, observed_at, cancellation).await,
                ProviderKind::OpenRouter => {
                    self.fetch_openrouter(connection, observed_at, cancellation)
                        .await
                }
                ProviderKind::OpenAi | ProviderKind::Anthropic => Ok(vec![permission_observation(
                    connection,
                    observed_at,
                    "usage.account_management_credential_required",
                )]),
                ProviderKind::Ollama | ProviderKind::OpenAiCompat => {
                    Ok(vec![unsupported_observation(connection, observed_at)])
                }
            }
        })
    }
}

async fn fetch_codex(
    connection: &ConnectionConfig,
    observed_at: u64,
    cancellation: &CancellationToken,
) -> Result<Vec<UsageObservationV1>, SourceError> {
    if cancellation.is_cancelled() {
        return Err(SourceError::permanent("usage.cancelled"));
    }
    let launch = CodexLaunchConfig {
        program: connection
            .codex_program
            .clone()
            .unwrap_or_else(|| "codex".into()),
        home: connection.codex_home.clone(),
    };
    let mut server = CodexAppServer::spawn(&launch)
        .await
        .map_err(|_| SourceError::retryable("usage.managed_runtime_unavailable"))?;
    let result = async {
        let status = server
            .account_status()
            .await
            .map_err(|_| SourceError::retryable("usage.managed_account_unavailable"))?;
        match status {
            AccountStatus::ChatGpt { .. } => {}
            AccountStatus::LoggedOut => {
                return Ok(vec![permission_observation(
                    connection,
                    observed_at,
                    "usage.managed_login_required",
                )]);
            }
            AccountStatus::ApiKey | AccountStatus::Other { .. } => {
                return Ok(vec![unsupported_observation_with_code(
                    connection,
                    observed_at,
                    "usage.codex_subscription_limits_unsupported",
                    FactSourceV1::ManagedRuntime,
                )]);
            }
        }
        let limits = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(SourceError::permanent("usage.cancelled")),
            result = server.rate_limits() => result.map_err(|_| SourceError::retryable("usage.codex_rate_limits_unavailable"))?,
        };
        Ok(parse_codex_limits(connection, observed_at, &limits))
    }
    .await;
    let _ = server.shutdown().await;
    result
}

fn parse_codex_limits(
    connection: &ConnectionConfig,
    observed_at: u64,
    value: &Value,
) -> Vec<UsageObservationV1> {
    let mut observations = Vec::new();
    if let Some(by_id) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (limit_id, limit) in by_id.iter().take(32) {
            append_codex_windows(&mut observations, connection, observed_at, limit_id, limit);
        }
    } else if let Some(limits) = value.get("rateLimits") {
        let limit_id = limits
            .get("limitId")
            .and_then(Value::as_str)
            .unwrap_or("default");
        append_codex_windows(&mut observations, connection, observed_at, limit_id, limits);
    }
    if observations.is_empty() {
        observations.push(unavailable_observation_with_source(
            connection,
            observed_at,
            "usage.codex_rate_limits_unavailable",
            FactSourceV1::ManagedRuntime,
        ));
    }
    observations
}

fn append_codex_windows(
    target: &mut Vec<UsageObservationV1>,
    connection: &ConnectionConfig,
    observed_at: u64,
    limit_id: &str,
    value: &Value,
) {
    for window_name in ["primary", "secondary"] {
        let Some(window) = value.get(window_name).filter(|value| !value.is_null()) else {
            continue;
        };
        let used = window
            .get("usedPercent")
            .and_then(Value::as_f64)
            .and_then(percent_to_basis_points);
        let window_millis = window
            .get("windowDurationMins")
            .and_then(Value::as_u64)
            .and_then(|minutes| minutes.checked_mul(60_000));
        let reset_at = window
            .get("resetsAt")
            .and_then(Value::as_u64)
            .and_then(|seconds| seconds.checked_mul(1_000));
        if used.is_none() && window_millis.is_none() && reset_at.is_none() {
            continue;
        }
        let period = reset_at
            .map(|reset| format!("reset-{reset}"))
            .unwrap_or_else(|| format!("active-{window_name}"));
        let bucket = safe_fragment(
            &format!("{limit_id}-{window_name}"),
            &format!("codex-{window_name}"),
        );
        let mut observation = UsageObservationV1 {
            id: Uuid::nil(),
            scope: UsageScopeV1::RateLimitBucket {
                connection: connection.id.clone(),
                bucket,
            },
            period,
            accounting: UsageAccountingV1::CumulativeSnapshot {
                sequence: observed_at,
            },
            amounts: UsageAmountsV1::default(),
            context: None,
            rate_limit: Some(LimitObservationV1 {
                used_percent_basis_points: used,
                remaining: used.map(|used| u64::from(10_000_u16.saturating_sub(used))),
                limit: Some(10_000),
                window_millis,
                reset_at_unix_millis: reset_at,
            }),
            quota: None,
            credits: None,
            request_affinity_digest: None,
            availability: AvailabilityV1::Available,
            source: FactSourceV1::ManagedRuntime,
            authority: FactAuthorityV1::ProviderReported,
            freshness: freshness(observed_at),
        };
        assign_stable_id(&mut observation);
        target.push(observation);
    }
}

fn openrouter_credit_observation(
    connection: &ConnectionConfig,
    observed_at: u64,
    value: &Value,
) -> UsageObservationV1 {
    let data = value.get("data").unwrap_or(&Value::Null);
    let purchased = money_microunits(data.get("total_credits"));
    let used = money_microunits(data.get("total_usage"));
    let remaining = purchased
        .zip(used)
        .and_then(|(total, used)| total.checked_sub(used));
    account_observation(
        connection,
        observed_at,
        "credits-lifetime",
        UsageAmountsV1::default(),
        None,
        Some(CreditBalanceV1 {
            currency: "usd".to_owned(),
            purchased_microunits: purchased,
            used_microunits: used,
            remaining_microunits: remaining,
        }),
        if purchased.is_some() || used.is_some() {
            AvailabilityV1::Available
        } else {
            AvailabilityV1::Unavailable {
                code: "usage.invalid_response".to_owned(),
            }
        },
    )
}

fn account_observation(
    connection: &ConnectionConfig,
    observed_at: u64,
    period: &str,
    amounts: UsageAmountsV1,
    quota: Option<LimitObservationV1>,
    credits: Option<CreditBalanceV1>,
    availability: AvailabilityV1,
) -> UsageObservationV1 {
    let mut observation = UsageObservationV1 {
        id: Uuid::nil(),
        scope: UsageScopeV1::Account {
            connection: connection.id.clone(),
            account: "configured".to_owned(),
        },
        period: period.to_owned(),
        accounting: UsageAccountingV1::CumulativeSnapshot {
            sequence: observed_at,
        },
        amounts,
        context: None,
        rate_limit: None,
        quota,
        credits,
        request_affinity_digest: None,
        availability,
        source: FactSourceV1::Provider,
        authority: FactAuthorityV1::ProviderReported,
        freshness: freshness(observed_at),
    };
    assign_stable_id(&mut observation);
    observation
}

fn permission_observation(
    connection: &ConnectionConfig,
    observed_at: u64,
    code: &str,
) -> UsageObservationV1 {
    unavailable_with(
        connection,
        observed_at,
        AvailabilityV1::PermissionRequired {
            code: code.to_owned(),
        },
        source_for(connection.kind),
    )
}

fn unsupported_observation(connection: &ConnectionConfig, observed_at: u64) -> UsageObservationV1 {
    unsupported_observation_with_code(
        connection,
        observed_at,
        "usage.account_observation_unsupported",
        source_for(connection.kind),
    )
}

fn unsupported_observation_with_code(
    connection: &ConnectionConfig,
    observed_at: u64,
    code: &str,
    source: FactSourceV1,
) -> UsageObservationV1 {
    let mut observation =
        unavailable_with(connection, observed_at, AvailabilityV1::Unsupported, source);
    observation.period = code.to_owned();
    assign_stable_id(&mut observation);
    observation
}

pub(super) fn unavailable_observation(
    connection: &ConnectionConfig,
    observed_at: u64,
    code: &str,
) -> UsageObservationV1 {
    unavailable_observation_with_source(connection, observed_at, code, source_for(connection.kind))
}

fn unavailable_observation_with_source(
    connection: &ConnectionConfig,
    observed_at: u64,
    code: &str,
    source: FactSourceV1,
) -> UsageObservationV1 {
    unavailable_with(
        connection,
        observed_at,
        AvailabilityV1::Unavailable {
            code: code.to_owned(),
        },
        source,
    )
}

fn unavailable_with(
    connection: &ConnectionConfig,
    observed_at: u64,
    availability: AvailabilityV1,
    source: FactSourceV1,
) -> UsageObservationV1 {
    let mut observation = UsageObservationV1 {
        id: Uuid::nil(),
        scope: UsageScopeV1::Connection {
            connection: connection.id.clone(),
        },
        period: "current".to_owned(),
        accounting: UsageAccountingV1::CumulativeSnapshot {
            sequence: observed_at,
        },
        amounts: UsageAmountsV1::default(),
        context: None,
        rate_limit: None,
        quota: None,
        credits: None,
        request_affinity_digest: None,
        availability,
        source,
        authority: FactAuthorityV1::ProviderReported,
        freshness: freshness(observed_at),
    };
    assign_stable_id(&mut observation);
    observation
}

fn source_for(kind: ProviderKind) -> FactSourceV1 {
    if kind == ProviderKind::Codex {
        FactSourceV1::ManagedRuntime
    } else {
        FactSourceV1::Provider
    }
}

fn freshness(observed_at: u64) -> FreshnessV1 {
    FreshnessV1 {
        observed_at_unix_millis: observed_at,
        max_age_millis: Some(OBSERVATION_MAX_AGE_MILLIS),
    }
}

fn money_microunits(value: Option<&Value>) -> Option<u64> {
    let value = value.and_then(Value::as_f64)?;
    let scaled = value * 1_000_000.0;
    (scaled.is_finite() && scaled >= 0.0 && scaled <= u64::MAX as f64)
        .then(|| scaled.round() as u64)
}

fn percentage_basis_points(used: Option<u64>, limit: Option<u64>) -> Option<u16> {
    let (used, limit) = (u128::from(used?), u128::from(limit?));
    if limit == 0 {
        return None;
    }
    u16::try_from(
        used.saturating_mul(10_000)
            .min(limit.saturating_mul(10_000))
            / limit,
    )
    .ok()
}

fn percent_to_basis_points(percent: f64) -> Option<u16> {
    let scaled = percent * 100.0;
    (scaled.is_finite() && (0.0..=10_000.0).contains(&scaled)).then(|| scaled.round() as u16)
}

fn safe_fragment(value: &str, fallback: &str) -> String {
    if !value.is_empty()
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b'/')
        })
    {
        value.to_owned()
    } else {
        let digest = blake3::hash(value.as_bytes());
        format!("{fallback}-{}", &digest.to_hex()[..16])
    }
}

impl SourceError {
    const fn permanent(code: &'static str) -> Self {
        Self {
            code,
            retryable: false,
        }
    }

    const fn retryable(code: &'static str) -> Self {
        Self {
            code,
            retryable: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum HttpObservationError {
    Cancelled,
    Permission,
    Transport,
    Retryable,
    Rejected,
    TooLarge,
    Invalid,
}

impl HttpObservationError {
    const fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "usage.cancelled",
            Self::Permission => "usage.credential_rejected",
            Self::Transport => "usage.provider_unreachable",
            Self::Retryable => "usage.provider_retryable",
            Self::Rejected => "usage.provider_rejected",
            Self::TooLarge => "usage.response_too_large",
            Self::Invalid => "usage.invalid_response",
        }
    }

    const fn into_source(self) -> SourceError {
        SourceError {
            code: self.code(),
            retryable: matches!(self, Self::Transport | Self::Retryable),
        }
    }
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
