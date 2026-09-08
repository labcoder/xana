//! Reviewed, bounded retrieval of public HTTPS text as untrusted evidence.

use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, ToolExecutionContext,
};
use crate::{
    artifact::ArtifactStore,
    config::OutboundDataClass,
    frontend::semantic::{LinkPreviewCacheStatusV1, LinkPreviewCardV1},
    identity::{OperationId, PrincipalId},
    mcp::{McpHttpSecurity, pinned_client},
    outbound::{
        ObservedOutboundAudit, OutboundDisposition, OutboundGuard, OutboundItem, OutboundRequest,
        OutboundTransport, OutboundTransportFailure, RecipientIdentity, RecipientKind,
    },
    paths::XanaPaths,
    permission::PermissionScope,
    web::{WebConfig, WebFailure, WebRuntime, WebTurn},
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::{Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

const DEFAULT_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_HEADERS_BYTES: usize = 32 * 1024;
const MAX_EXTRACTED_BYTES: usize = 256 * 1024;
const MAX_INLINE_BYTES: usize = 24 * 1024;
const DEFAULT_TIMEOUT_SECONDS: u64 = 20;
const MAX_TIMEOUT_SECONDS: u64 = 60;
const MAX_REDIRECTS: usize = 3;
const MAX_URL_BYTES: usize = 1024;
const MAX_REVIEW_DESTINATION_BYTES: usize = 1900;
const HTML_RENDER_WIDTH: usize = 120;
const HTML_EXTRACTION_TIMEOUT: Duration = Duration::from_secs(2);

mod extraction;

#[derive(Clone)]
pub(crate) struct WebFetch {
    paths: Option<XanaPaths>,
    security: McpHttpSecurity,
    config: WebConfig,
    runtime: Arc<WebRuntime>,
}

impl Default for WebFetch {
    fn default() -> Self {
        let config = WebConfig::default();
        Self {
            paths: None,
            security: McpHttpSecurity::default(),
            runtime: Arc::new(WebRuntime::new(config.limits.clone())),
            config,
        }
    }
}

impl WebFetch {
    pub(super) fn configured(
        paths: XanaPaths,
        config: WebConfig,
        runtime: Arc<WebRuntime>,
    ) -> Self {
        Self {
            paths: Some(paths),
            security: McpHttpSecurity::default(),
            config,
            runtime,
        }
    }
    #[cfg(test)]
    pub(super) fn for_tests(paths: XanaPaths) -> Self {
        Self {
            paths: Some(paths),
            security: McpHttpSecurity {
                allow_loopback_http: true,
            },
            ..Self::default()
        }
    }

    fn paths(&self) -> Result<XanaPaths, String> {
        self.paths.clone().map_or_else(
            || XanaPaths::resolve(std::env::var_os("XANA_HOME")).map_err(|error| error.to_string()),
            Ok,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    url: String,
    #[serde(default)]
    redirects: Vec<String>,
    max_response_bytes: Option<usize>,
    timeout_seconds: Option<u64>,
}

struct Plan {
    args: Args,
    urls: Vec<Url>,
    recipient: RecipientIdentity,
    items: Vec<OutboundItem>,
    paths: XanaPaths,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseKind {
    Json,
    PlainText,
    Markdown,
    Html,
}

#[derive(Debug)]
struct FetchReceipt {
    requested_url: Url,
    final_url: Url,
    media_type: String,
    body: Vec<u8>,
    redirects: Vec<Url>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchError {
    Rendering(String),
    Web(WebFailure),
    InvalidUrl,
    CredentialsRejected,
    HttpsRequired,
    RedirectDowngrade,
    RedirectLimit,
    RedirectRequiresReview(String),
    RedirectPlanMismatch,
    DnsOrAddressPolicy,
    Unavailable,
    TimedOut,
    Cancelled,
    HeaderLimit,
    CompressedResponse,
    ResponseTooLarge,
    Http(u16),
    UnsupportedContentType,
    UnsupportedEncoding,
    MalformedText,
    ExtractionFailed,
}

impl FetchError {
    fn outbound_failure(&self) -> OutboundTransportFailure {
        match self {
            Self::Web(_) | Self::Rendering(_) => OutboundTransportFailure::Protocol,
            Self::Unavailable | Self::DnsOrAddressPolicy => OutboundTransportFailure::Unavailable,
            Self::TimedOut => OutboundTransportFailure::TimedOut,
            Self::Cancelled => OutboundTransportFailure::Cancelled,
            Self::Http(_) => OutboundTransportFailure::Rejected,
            Self::InvalidUrl
            | Self::CredentialsRejected
            | Self::HttpsRequired
            | Self::RedirectDowngrade
            | Self::RedirectLimit
            | Self::RedirectRequiresReview(_)
            | Self::RedirectPlanMismatch
            | Self::HeaderLimit
            | Self::CompressedResponse
            | Self::ResponseTooLarge
            | Self::UnsupportedContentType
            | Self::UnsupportedEncoding
            | Self::MalformedText
            | Self::ExtractionFailed => OutboundTransportFailure::Protocol,
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rendering(error) => formatter.write_str(error),
            Self::Web(error) => write!(formatter, "{error}"),
            Self::InvalidUrl => formatter.write_str("web_fetch URL is invalid"),
            Self::CredentialsRejected => {
                formatter.write_str("web_fetch rejects credentials embedded in URLs")
            }
            Self::HttpsRequired => formatter.write_str("web_fetch requires a public HTTPS URL"),
            Self::RedirectDowngrade => {
                formatter.write_str("web_fetch rejected an HTTPS-to-HTTP redirect")
            }
            Self::RedirectLimit => formatter.write_str("web_fetch redirect limit was exceeded"),
            Self::RedirectRequiresReview(url) => write!(
                formatter,
                "web_fetch stopped before an unreviewed redirect; retry with redirects: [{url:?}]"
            ),
            Self::RedirectPlanMismatch => formatter.write_str(
                "web_fetch response did not follow the exact reviewed redirect chain",
            ),
            Self::DnsOrAddressPolicy => formatter.write_str(
                "web_fetch destination could not be resolved to permitted public addresses",
            ),
            Self::Unavailable => formatter.write_str("web_fetch destination is unavailable"),
            Self::TimedOut => formatter.write_str("web_fetch timed out"),
            Self::Cancelled => formatter.write_str("web_fetch was cancelled"),
            Self::HeaderLimit => {
                formatter.write_str("web_fetch response headers exceed the 32768-byte limit")
            }
            Self::CompressedResponse => formatter.write_str(
                "web_fetch rejects compressed responses so decoded bytes cannot exceed the reviewed bound",
            ),
            Self::ResponseTooLarge => formatter.write_str("web_fetch response exceeds its byte limit"),
            Self::Http(status) => write!(formatter, "web_fetch destination returned HTTP {status}"),
            Self::UnsupportedContentType => formatter.write_str(
                "web_fetch supports only UTF-8 plain text, Markdown, HTML, XHTML, and JSON",
            ),
            Self::UnsupportedEncoding => {
                formatter.write_str("web_fetch supports only UTF-8 response text")
            }
            Self::MalformedText => formatter.write_str("web_fetch response is not valid UTF-8"),
            Self::ExtractionFailed => {
                formatter.write_str("web_fetch could not extract bounded text from the response")
            }
        }
    }
}

struct FetchTransport {
    turn: Arc<WebTurn>,
    public_web: bool,
    paths: XanaPaths,
    operation: OperationId,
    events: Option<crate::native_runtime::AgentEventSender>,
    urls: Vec<Url>,
    security: McpHttpSecurity,
    max_response_bytes: usize,
    timeout: Duration,
    cancellation: CancellationToken,
    failure: Option<FetchError>,
}

impl OutboundTransport for FetchTransport {
    type Receipt = String;

    fn send<'a>(
        &'a mut self,
        _recipient: &'a RecipientIdentity,
        _items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        Box::pin(async move {
            let turn = Arc::clone(&self.turn);
            let timeout = self.timeout;
            let urls = self.urls.iter().map(Url::as_str).collect::<Vec<_>>();
            let key = blake3::hash(
                &serde_json::to_vec(&("fetch", urls, self.max_response_bytes, self.public_web))
                    .map_err(|_| OutboundTransportFailure::Protocol)?,
            );
            let fetch = &mut *self;
            let work_turn = &turn;
            let work = move |slot| async move {
                crate::web::progress(
                    fetch.events.as_ref(),
                    fetch.operation,
                    crate::web::WebStage::Reading,
                    work_turn,
                );
                match fetch_chain(
                    &fetch.urls,
                    fetch.security,
                    fetch.max_response_bytes,
                    fetch.timeout,
                    &fetch.cancellation,
                    Some(FetchScope {
                        turn: work_turn,
                        public_web: fetch.public_web,
                        paths: &fetch.paths,
                        events: fetch.events.as_ref(),
                        operation: fetch.operation,
                    }),
                )
                .await
                {
                    Ok(receipt) => {
                        crate::web::progress(
                            fetch.events.as_ref(),
                            fetch.operation,
                            crate::web::WebStage::Extracting,
                            work_turn,
                        );
                        render_receipt(receipt, &fetch.paths, fetch.operation, slot)
                            .await
                            .map_err(|error| {
                                fetch.failure = Some(FetchError::Rendering(error));
                                WebFailure::InvalidResponse
                            })
                    }
                    Err(error) => {
                        let failure = match &error {
                            FetchError::Web(failure) => *failure,
                            FetchError::Http(404) => WebFailure::Missing,
                            FetchError::Http(401 | 403) => WebFailure::Challenge,
                            FetchError::Http(429) => WebFailure::RateLimited,
                            FetchError::TimedOut => WebFailure::TimedOut,
                            FetchError::Cancelled => WebFailure::Cancelled,
                            FetchError::ResponseTooLarge => WebFailure::TooLarge,
                            _ => WebFailure::InvalidResponse,
                        };
                        fetch.failure = Some(error);
                        Err(failure)
                    }
                }
            };
            let result = tokio::time::timeout(timeout, turn.cached(key, false, work))
                .await
                .unwrap_or(Err(WebFailure::TimedOut));
            crate::web::progress(
                self.events.as_ref(),
                self.operation,
                if result.is_ok() {
                    crate::web::WebStage::Complete
                } else {
                    crate::web::WebStage::Failed
                },
                &turn,
            );
            match result {
                Ok(receipt) => Ok(receipt),
                Err(failure) => {
                    let error = self.failure.get_or_insert(FetchError::Web(failure));
                    Err(error.outbound_failure())
                }
            }
        })
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Tool for WebFetch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_fetch".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Read a KNOWN public HTTPS URL as bounded, attributed, untrusted text or JSON. This is not search: use web_search to discover source URLs, never guess paths. Exact approval stops at unreviewed redirects; an explicit public-web turn grant follows bounded public redirects subject to saved denies. No cookies, credentials, proxy inheritance, JavaScript or browser authority. On 404 or oversize use another verified source instead of repeating the request.".into(),
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["url"],
                "properties":{
                    "url":{"type":"string","description":"Public HTTPS URL to fetch"},
                    "redirects":{"type":"array","maxItems":MAX_REDIRECTS,"items":{"type":"string"},"description":"Exact ordered redirect destinations previously reported by web_fetch; omitted on the first attempt"},
                    "max_response_bytes":{"type":"integer","minimum":1,"maximum":MAX_RESPONSE_BYTES,"description":"Encoded response-byte ceiling; configured default is 2097152"},
                    "timeout_seconds":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT_SECONDS,"description":"Whole-request timeout; default 20 seconds"}
                }
            }),
            effect_class: EffectClass::Network,
            replay_safety: ReplaySafety::Safe,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        _workspace_root: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        let mut args: Args = serde_json::from_value(arguments.clone())
            .map_err(|_| "web_fetch arguments are invalid".to_owned())?;
        let max_response_bytes = args
            .max_response_bytes
            .unwrap_or(self.config.limits.fetch_bytes);
        if max_response_bytes == 0 || max_response_bytes > MAX_RESPONSE_BYTES {
            return Err(format!(
                "max_response_bytes must be within 1..={MAX_RESPONSE_BYTES}"
            ));
        }
        let timeout_seconds = args.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        if timeout_seconds == 0 || timeout_seconds > MAX_TIMEOUT_SECONDS {
            return Err(format!(
                "timeout_seconds must be within 1..={MAX_TIMEOUT_SECONDS}"
            ));
        }
        if args.redirects.len() > MAX_REDIRECTS {
            return Err(format!(
                "web_fetch accepts at most {MAX_REDIRECTS} redirects"
            ));
        }

        let mut urls = Vec::with_capacity(args.redirects.len() + 1);
        urls.push(parse_url(&args.url, self.security).map_err(|error| error.to_string())?);
        for value in &args.redirects {
            urls.push(parse_url(value, self.security).map_err(|error| error.to_string())?);
        }
        if urls
            .windows(2)
            .any(|pair| pair[0].scheme() == "https" && pair[1].scheme() != "https")
        {
            return Err(FetchError::RedirectDowngrade.to_string());
        }
        let canonical = urls.iter().map(Url::as_str).collect::<Vec<_>>();
        if canonical.iter().collect::<BTreeSet<_>>().len() != canonical.len() {
            return Err("web_fetch redirect chain contains a cycle".to_owned());
        }
        let destination = canonical.join(" -> ");
        if destination.len() > MAX_REVIEW_DESTINATION_BYTES {
            return Err(format!(
                "web_fetch reviewed URL chain exceeds {MAX_REVIEW_DESTINATION_BYTES} bytes"
            ));
        }
        args.url = canonical[0].to_owned();
        args.redirects = canonical[1..]
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        args.max_response_bytes = Some(max_response_bytes);
        args.timeout_seconds = Some(timeout_seconds);

        let identity_material = serde_json::to_vec(&canonical)
            .map_err(|_| "web_fetch could not encode its recipient identity".to_owned())?;
        let recipient = RecipientIdentity::new(
            RecipientKind::WebFetch,
            "web_fetch",
            destination,
            &identity_material,
        )
        .map_err(|error| error.to_string())?;
        let request_bytes = serde_json::to_vec(&json!({"method":"GET","urls":canonical}))
            .map_err(|_| "web_fetch could not encode its reviewed request".to_owned())?;
        let item = OutboundItem::new(
            OutboundDataClass::PromptText,
            "explicit GET URL chain",
            Some(args.url.clone()),
            "model-proposed web_fetch arguments",
            request_bytes,
        )
        .map_err(|error| error.to_string())?;
        let items = vec![item];
        let mut review = OutboundRequest::new(
            OperationId::new(),
            recipient.clone(),
            "retrieve public text as untrusted evidence",
            items.clone(),
        )
        .map_err(|error| error.to_string())?
        .review();
        review.public_web = Some(Box::new(self.config.review()));
        let paths = self.paths()?;
        let final_arguments = serde_json::to_value(&args).map_err(|error| error.to_string())?;
        Ok(PlannedToolInvocation::new(
            final_arguments,
            PermissionScope::External {
                recipient_identity_digest: recipient.identity_digest.clone(),
                operation: "web_fetch".to_owned(),
            },
            Plan {
                args,
                urls,
                recipient,
                items,
                paths,
            },
        )
        .with_outbound_review(review))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<Plan>("web_fetch")?;
            let cancellation = CancelOnDrop(CancellationToken::new());
            let turn = self.runtime.turn(context.operation_id);
            if context
                .outbound_approval
                .as_ref()
                .is_some_and(|approval| approval.permits_public_web())
                || self.config.public_web == crate::web::PublicWebConsent::Allow
            {
                turn.allow_public_web();
            }
            let mut transport = FetchTransport {
                public_web: turn.permits_public_web(),
                turn,
                paths: plan.paths.clone(),
                operation: context.operation_id,
                events: context.events.clone(),
                urls: plan.urls.clone(),
                security: self.security,
                max_response_bytes: plan
                    .args
                    .max_response_bytes
                    .unwrap_or(DEFAULT_RESPONSE_BYTES),
                timeout: Duration::from_secs(
                    plan.args.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS),
                ),
                cancellation: cancellation.0.clone(),
                failure: None,
            };
            let request = OutboundRequest::new(
                context.operation_id,
                plan.recipient.clone(),
                "retrieve public text as untrusted evidence",
                plan.items.clone(),
            )
            .map_err(|error| error.to_string())?;
            let policy = self.runtime.policy();
            let observer = crate::diagnostics::outbound_audit(&plan.paths)
                .map_err(|error| error.to_string())?;
            let mut audit = ObservedOutboundAudit::new(observer.as_ref());
            let mut controller = context.outbound_approval;
            let receipt = match OutboundGuard::open(&plan.paths)
                .map_err(|error| error.to_string())?
                .dispatch(
                    request,
                    &policy,
                    controller.as_mut(),
                    &mut transport,
                    &mut audit,
                )
                .await
            {
                Ok(receipt) => receipt,
                Err(error) => {
                    return Err(transport
                        .failure
                        .take()
                        .map_or_else(|| error.to_string(), |failure| failure.to_string()));
                }
            };
            Ok(receipt)
        })
    }

    fn outbound_disposition(
        &self,
        planned: &PlannedToolInvocation,
    ) -> Result<Option<OutboundDisposition>, String> {
        let plan = planned.executable::<Plan>("web_fetch")?;
        OutboundGuard::open(&plan.paths)
            .map_err(|error| error.to_string())?
            .disposition(&plan.recipient, plan.items.iter().map(OutboundItem::class))
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

fn parse_url(value: &str, security: McpHttpSecurity) -> Result<Url, FetchError> {
    if value.is_empty() || value.len() > MAX_URL_BYTES {
        return Err(FetchError::InvalidUrl);
    }
    let url = Url::parse(value).map_err(|_| FetchError::InvalidUrl)?;
    if url.username() != "" || url.password().is_some() {
        return Err(FetchError::CredentialsRejected);
    }
    if url.host_str().is_none() || url.fragment().is_some() || url.cannot_be_a_base() {
        return Err(FetchError::InvalidUrl);
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if security.allow_loopback_http && textual_loopback(&url) => Ok(url),
        _ => Err(FetchError::HttpsRequired),
    }
}

fn textual_loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

#[derive(Clone, Copy)]
struct FetchScope<'a> {
    turn: &'a WebTurn,
    public_web: bool,
    paths: &'a XanaPaths,
    events: Option<&'a crate::native_runtime::AgentEventSender>,
    operation: OperationId,
}

async fn fetch_chain(
    urls: &[Url],
    security: McpHttpSecurity,
    max_response_bytes: usize,
    timeout: Duration,
    cancellation: &CancellationToken,
    turn: Option<FetchScope<'_>>,
) -> Result<FetchReceipt, FetchError> {
    let requested_url = urls.first().cloned().ok_or(FetchError::InvalidUrl)?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut followed = Vec::new();
    let mut chain = urls.to_vec();
    let mut index = 0;
    while index < chain.len() {
        let url = chain[index].clone();
        if let Some(scope) = turn {
            scope.turn.attempt().map_err(FetchError::Web)?;
        }
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .ok_or(FetchError::TimedOut)?;
        let client = tokio::time::timeout(remaining, pinned_client(&url, security, remaining))
            .await
            .map_err(|_| FetchError::TimedOut)?
            .map_err(|_| FetchError::DnsOrAddressPolicy)?;
        let request = client
            .get(url.clone())
            .header(
                header::ACCEPT,
                "text/plain, text/markdown, text/html, application/xhtml+xml, application/json",
            )
            .header(header::ACCEPT_ENCODING, "identity")
            .header(
                header::USER_AGENT,
                concat!("xana/", env!("CARGO_PKG_VERSION")),
            );
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(FetchError::Cancelled),
            response = request.send() => response.map_err(classify_reqwest)?,
        };
        if let Some(scope) = turn {
            scope
                .turn
                .ingress(
                    response
                        .headers()
                        .iter()
                        .map(|(name, value)| name.as_str().len() + value.len())
                        .sum(),
                )
                .map_err(FetchError::Web)?;
        }

        validate_headers(&response)?;
        if response.status().is_redirection() {
            if let Some(scope) = turn {
                crate::web::progress(
                    scope.events,
                    scope.operation,
                    crate::web::WebStage::Redirecting,
                    scope.turn,
                );
            }
            if index >= MAX_REDIRECTS {
                return Err(FetchError::RedirectLimit);
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::InvalidUrl)?;
            let next = url.join(location).map_err(|_| FetchError::InvalidUrl)?;
            let next = parse_url(next.as_str(), security)?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err(FetchError::RedirectDowngrade);
            }
            if chain[..=index].contains(&next) {
                return Err(FetchError::RedirectLimit);
            }
            if let Some(scope) = turn.filter(|scope| scope.public_web) {
                check_redirect_deny(&chain[..=index], &next, scope.paths)?;
                if chain.get(index + 1).is_none() {
                    chain.push(next.clone());
                }
            }
            let Some(reviewed) = chain.get(index + 1) else {
                return Err(FetchError::RedirectRequiresReview(next.to_string()));
            };
            if reviewed != &next {
                return Err(FetchError::RedirectPlanMismatch);
            }
            followed.push(next);
            index += 1;
            continue;
        }
        if index + 1 != chain.len() {
            return Err(FetchError::RedirectPlanMismatch);
        }
        if !response.status().is_success() {
            return Err(FetchError::Http(response.status().as_u16()));
        }
        let media_type = response_media_type(&response)?;
        let body = collect_body(
            response,
            max_response_bytes,
            cancellation,
            turn.map(|scope| scope.turn),
        )
        .await?;
        return Ok(FetchReceipt {
            requested_url,
            final_url: url.clone(),
            media_type,
            body,
            redirects: followed,
        });
    }
    Err(FetchError::RedirectPlanMismatch)
}

fn check_redirect_deny(chain: &[Url], next: &Url, paths: &XanaPaths) -> Result<(), FetchError> {
    let guard = OutboundGuard::open(paths).map_err(|_| FetchError::Web(WebFailure::Policy))?;
    let mut full = chain.iter().map(Url::as_str).collect::<Vec<_>>();
    full.push(next.as_str());
    for urls in [vec![next.as_str()], full] {
        let recipient = RecipientIdentity::new(
            RecipientKind::WebFetch,
            "web_fetch",
            urls.join(" -> "),
            &serde_json::to_vec(&urls).map_err(|_| FetchError::InvalidUrl)?,
        )
        .map_err(|_| FetchError::InvalidUrl)?;
        if guard
            .disposition(&recipient, [OutboundDataClass::PromptText])
            .map_err(|_| FetchError::Web(WebFailure::Policy))?
            == OutboundDisposition::SavedDeny
        {
            return Err(FetchError::Web(WebFailure::Policy));
        }
    }
    Ok(())
}

fn validate_headers(response: &reqwest::Response) -> Result<(), FetchError> {
    let bytes = response
        .headers()
        .iter()
        .try_fold(0_usize, |total, (name, value)| {
            total
                .checked_add(name.as_str().len())?
                .checked_add(value.as_bytes().len())
        });
    if bytes.is_none_or(|bytes| bytes > MAX_RESPONSE_HEADERS_BYTES) {
        return Err(FetchError::HeaderLimit);
    }
    if response
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| {
            value
                .to_str()
                .map_or(true, |value| !value.eq_ignore_ascii_case("identity"))
        })
    {
        return Err(FetchError::CompressedResponse);
    }
    Ok(())
}

fn response_media_type(response: &reqwest::Response) -> Result<String, FetchError> {
    let raw = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(FetchError::UnsupportedContentType)?;
    if raw.len() > 256 {
        return Err(FetchError::UnsupportedContentType);
    }
    let mut parts = raw.split(';');
    let media_type = parts
        .next()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .ok_or(FetchError::UnsupportedContentType)?;
    let kind = classify_media_type(&media_type)?;
    for parameter in parts {
        let Some((name, value)) = parameter.split_once('=') else {
            return Err(FetchError::UnsupportedContentType);
        };
        if name.trim().eq_ignore_ascii_case("charset")
            && !value.trim().trim_matches('"').eq_ignore_ascii_case("utf-8")
        {
            return Err(FetchError::UnsupportedEncoding);
        }
    }
    let _ = kind;
    Ok(media_type)
}

fn classify_media_type(media_type: &str) -> Result<ResponseKind, FetchError> {
    match media_type {
        "text/plain" => Ok(ResponseKind::PlainText),
        "text/markdown" => Ok(ResponseKind::Markdown),
        "application/json" | "text/json" => Ok(ResponseKind::Json),
        "text/html" | "application/xhtml+xml" => Ok(ResponseKind::Html),
        _ => Err(FetchError::UnsupportedContentType),
    }
}

async fn collect_body(
    response: reqwest::Response,
    max_bytes: usize,
    cancellation: &CancellationToken,
    turn: Option<&WebTurn>,
) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(FetchError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(FetchError::Cancelled),
            next = stream.next() => next,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(classify_reqwest)?;
        if let Some(turn) = turn {
            turn.ingress(chunk.len()).map_err(FetchError::Web)?;
        }
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(FetchError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn classify_reqwest(error: reqwest::Error) -> FetchError {
    if error.is_timeout() {
        FetchError::TimedOut
    } else {
        FetchError::Unavailable
    }
}

async fn render_receipt(
    receipt: FetchReceipt,
    paths: &XanaPaths,
    _operation_id: OperationId,
    slot: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<String, String> {
    let response_kind =
        classify_media_type(&receipt.media_type).map_err(|error| error.to_string())?;
    let response_bytes = receipt.body.len() as u64;
    let content_digest = blake3::hash(&receipt.body).to_hex().to_string();
    let parser_slot = Arc::clone(&slot);
    let body_for_extraction = receipt.body.clone();
    if response_kind == ResponseKind::Json {
        serde_json::from_slice::<serde::de::IgnoredAny>(&body_for_extraction)
            .map_err(|_| FetchError::MalformedText.to_string())?;
    }
    let extracted = match response_kind {
        ResponseKind::PlainText | ResponseKind::Markdown | ResponseKind::Json => {
            String::from_utf8(body_for_extraction)
                .map_err(|_| FetchError::MalformedText.to_string())?
        }
        ResponseKind::Html => tokio::time::timeout(
            HTML_EXTRACTION_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                // A dropped caller cannot release parser admission while this
                // non-cancellable blocking operation is still running.
                let _slot = parser_slot;
                extraction::html(&body_for_extraction, HTML_RENDER_WIDTH)
            }),
        )
        .await
        .map_err(|_| FetchError::ExtractionFailed.to_string())?
        .map_err(|_| FetchError::ExtractionFailed.to_string())?
        .map_err(|_| FetchError::ExtractionFailed.to_string())?,
    };
    let extracted = sanitize_text(&extracted);
    let (extracted, extraction_truncated) = truncate_utf8(extracted, MAX_EXTRACTED_BYTES);
    let title = preview_title(&extracted);
    let (text, inline_truncated) = truncate_utf8(extracted, MAX_INLINE_BYTES);
    let text_truncated = extraction_truncated || inline_truncated;
    let artifact = if text_truncated || receipt.body.len() > MAX_INLINE_BYTES {
        let store = ArtifactStore::new(paths.data_dir().join("artifacts"));
        let bytes = receipt.body;
        let media_type = receipt.media_type.clone();
        Some(
            tokio::task::spawn_blocking(move || {
                let _slot = slot;
                store.put(&bytes, &media_type, PrincipalId::new())
            })
            .await
            .map_err(|_| "web_fetch artifact publisher stopped unexpectedly".to_owned())?
            .map_err(|error| error.to_string())?
            .0,
        )
    } else {
        None
    };
    let fetched_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "web_fetch could not timestamp its result".to_owned())?
        .as_millis()
        .try_into()
        .map_err(|_| "web_fetch timestamp is out of range".to_owned())?;
    let site_name = receipt
        .final_url
        .host_str()
        .ok_or_else(|| FetchError::InvalidUrl.to_string())?
        .to_owned();
    let result = LinkPreviewCardV1 {
        requested_url: receipt.requested_url.to_string(),
        final_url: receipt.final_url.to_string(),
        site_name,
        title,
        fetched_unix_ms,
        media_type: receipt.media_type,
        response_bytes,
        content_digest,
        redirects: receipt
            .redirects
            .into_iter()
            .map(|url| url.to_string())
            .collect(),
        text,
        text_truncated,
        untrusted: true,
        cache_status: LinkPreviewCacheStatusV1::FreshNotCached,
        artifact,
    };
    result.validate().map_err(|error| error.to_string())?;
    serde_json::to_string(&result)
        .map_err(|_| "web_fetch could not encode its bounded result".to_owned())
}

fn preview_title(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_start_matches('#').trim_start())
        .filter(|line| !line.is_empty())
        .map(|line| truncate_utf8(line.to_owned(), 512).0)
}

fn sanitize_text(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    (value, true)
}

#[cfg(test)]
mod tests;
