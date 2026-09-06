//! Exact named GitHub Actions read adapter. No account discovery, model polling,
//! credential fallback, redirects, logs, reruns, or repository mutation.
use super::Observation;
use crate::{
    config::CredentialReference,
    credential::{CredentialResolver, SecretString},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const RESPONSE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GithubTrigger {
    pub(crate) repository: String,
    pub(crate) run: u64,
    pub(crate) credential: CredentialReference,
    pub(crate) last: Option<RunStatus>,
    pub(crate) etag: Option<String>,
    pub(crate) observation: Observation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunStatus {
    pub(crate) attempt: u64,
    pub(crate) head_sha: String,
    pub(crate) status: String,
    pub(crate) conclusion: Option<String>,
    pub(crate) updated_at: i64,
}

impl GithubTrigger {
    pub(crate) fn create(value: &str, credential: CredentialReference) -> Result<Self> {
        let (repository, run) = value
            .rsplit_once('/')
            .context("GitHub run must be OWNER/REPO/RUN_ID")?;
        let watch = Self {
            repository: repository.into(),
            run: run.parse()?,
            credential,
            last: None,
            etag: None,
            observation: Observation::default(),
        };
        watch.validate()?;
        Ok(watch)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let parts = self.repository.split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() == 2
                && parts.iter().all(|part| !part.is_empty()
                    && part.len() <= 100
                    && *part != "."
                    && *part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
            "invalid exact GitHub repository"
        );
        ensure!(
            self.run > 0
                && self
                    .etag
                    .as_ref()
                    .is_none_or(|v| v.len() <= 256 && !v.chars().any(char::is_control)),
            "invalid GitHub run or cache validator"
        );
        let reference = match &self.credential {
            CredentialReference::Environment { variable } => variable,
            CredentialReference::Stored { id } => id,
        };
        ensure!(
            !reference.is_empty()
                && reference.len() <= 128
                && reference
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)),
            "invalid explicit credential source"
        );
        if let Some(last) = &self.last {
            last.validate()?;
        }
        Ok(())
    }
    fn url(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/actions/runs/{}",
            self.repository, self.run
        )
    }
}
impl RunStatus {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.attempt > 0
                && self.updated_at > 0
                && self.head_sha.len() == 40
                && self.head_sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid GitHub execution identity"
        );
        ensure!(
            matches!(
                self.status.as_str(),
                "queued" | "requested" | "waiting" | "pending" | "in_progress" | "completed"
            ),
            "unsupported GitHub status"
        );
        ensure!(
            self.conclusion.as_ref().is_none_or(|value| matches!(
                value.as_str(),
                "success"
                    | "failure"
                    | "neutral"
                    | "cancelled"
                    | "skipped"
                    | "timed_out"
                    | "action_required"
                    | "stale"
                    | "startup_failure"
            )),
            "unsupported GitHub conclusion"
        );
        ensure!(
            (self.status == "completed") == self.conclusion.is_some(),
            "inconsistent GitHub terminal status"
        );
        Ok(())
    }
}

pub(crate) async fn observe(
    watch: &mut GithubTrigger,
    now: i64,
    cancelled: &CancellationToken,
) -> Result<u32> {
    watch.validate()?;
    let reference = watch.credential.clone();
    let secret =
        tokio::task::spawn_blocking(move || CredentialResolver::default().resolve(&reference))
            .await??;
    let url = watch.url();
    poll(watch, &url, secret, now, cancelled).await
}

async fn poll(
    watch: &mut GithubTrigger,
    url: &str,
    secret: SecretString,
    now: i64,
    cancelled: &CancellationToken,
) -> Result<u32> {
    let client = crate::http_client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    let mut request = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .header("User-Agent", "Xana-named-ci-observer")
        .bearer_auth(secret.expose());
    if let Some(etag) = &watch.etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let result = tokio::select! { _=cancelled.cancelled()=>anyhow::bail!("CI observation cancelled"), value=request.send()=>value };
    watch.observation.last_checked = Some(now);
    let response = match result {
        Ok(response) => response,
        Err(_) => return retry(watch, "GitHub transport unavailable", 0),
    };
    let code = response.status();
    if code == reqwest::StatusCode::NOT_MODIFIED {
        ensure!(
            watch.last.is_some(),
            "GitHub returned a validator response without a baseline"
        );
        watch.observation.status = "Unchanged; no model call or notification".into();
        watch.observation.failures = 0;
        return Ok(60);
    }
    if code == reqwest::StatusCode::TOO_MANY_REQUESTS || code == reqwest::StatusCode::FORBIDDEN {
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
        };
        let retry_after = header("retry-after");
        let reset = header("x-ratelimit-reset");
        ensure!(
            code == reqwest::StatusCode::TOO_MANY_REQUESTS
                || retry_after.is_some()
                || header("x-ratelimit-remaining") == Some(0),
            "GitHub credential lacks named-run read permission"
        );
        let delay = retry_after
            .unwrap_or(60)
            .max(reset.map_or(0, |at| at.saturating_sub(now as u64)));
        ensure!(
            delay <= 86400,
            "GitHub retry delay exceeds bounded task horizon"
        );
        return retry(
            watch,
            "GitHub rate limited; waiting for server retry window",
            delay as u32,
        );
    }
    if code.is_server_error() {
        return retry(watch, "GitHub temporarily unavailable", 0);
    }
    ensure!(
        code == reqwest::StatusCode::OK,
        "GitHub run unavailable or credential rejected; no alternative account was tried"
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|bytes| bytes <= RESPONSE_BYTES as u64),
        "GitHub response exceeds bound"
    );
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 256)
        .map(str::to_owned);
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = tokio::select! { _=cancelled.cancelled()=>anyhow::bail!("CI observation cancelled"), value=response.chunk()=>value? }
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= RESPONSE_BYTES,
            "GitHub response exceeds bound"
        );
        bytes.extend_from_slice(&chunk);
    }
    accept(watch, &bytes, etag, now)?;
    Ok(60)
}

fn retry(watch: &mut GithubTrigger, detail: &str, minimum: u32) -> Result<u32> {
    watch.observation.failures = watch.observation.failures.saturating_add(1).min(16);
    ensure!(
        watch.observation.failures <= 6,
        "GitHub retry budget exhausted; owner review required"
    );
    watch.observation.status = detail.into();
    Ok(minimum.max((30u32.saturating_mul(1 << watch.observation.failures)).min(3600)))
}

fn accept(watch: &mut GithubTrigger, bytes: &[u8], etag: Option<String>, now: i64) -> Result<()> {
    #[derive(Deserialize)]
    struct Repository {
        full_name: String,
    }
    #[derive(Deserialize)]
    struct WireRun {
        id: u64,
        repository: Repository,
        run_attempt: u64,
        head_sha: String,
        status: String,
        conclusion: Option<String>,
        updated_at: String,
    }
    let wire: WireRun = serde_json::from_slice(bytes)?;
    ensure!(
        wire.id == watch.run
            && wire
                .repository
                .full_name
                .eq_ignore_ascii_case(&watch.repository),
        "GitHub response resource identity changed"
    );
    let status = RunStatus {
        attempt: wire.run_attempt,
        head_sha: wire.head_sha,
        status: wire.status,
        conclusion: wire.conclusion,
        updated_at: wire.updated_at.parse::<jiff::Timestamp>()?.as_second(),
    };
    status.validate()?;
    if let Some(last) = &watch.last {
        ensure!(
            last.attempt == status.attempt && last.head_sha == status.head_sha,
            "named GitHub run was rerun or changed revision; review a new intent"
        );
        if status.updated_at < last.updated_at
            || (last.status == "completed" && status.status != "completed")
            || (last.status == "in_progress"
                && status.status != "in_progress"
                && status.status != "completed")
        {
            watch.observation.status =
                "Ignored reordered GitHub status; last accepted observation retained".into();
            return Ok(());
        }
    }
    let changed = watch
        .last
        .as_ref()
        .is_none_or(|last| last.status != status.status || last.conclusion != status.conclusion);
    watch.observation.failures = 0;
    watch.observation.last_checked = Some(now);
    if changed {
        watch.observation.pending = true;
        watch.observation.last_event = Some(now);
        watch.observation.status = format!(
            "GitHub {}/{}: {}{}",
            watch.repository,
            watch.run,
            status.status,
            status
                .conclusion
                .as_ref()
                .map_or(String::new(), |v| format!(" ({v})"))
        );
    } else {
        watch.observation.status = "Unchanged; no model call or notification".into();
    }
    watch.last = Some(status);
    watch.etag = etag;
    Ok(())
}

#[cfg(test)]
mod tests;
