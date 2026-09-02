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
        ObservedOutboundAudit, OutboundDisposition, OutboundGuard, OutboundItem,
        OutboundPolicyLayers, OutboundRequest, OutboundTransport, OutboundTransportFailure,
        RecipientIdentity, RecipientKind,
    },
    paths::XanaPaths,
    permission::PermissionScope,
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::{Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

const DEFAULT_RESPONSE_BYTES: usize = 1024 * 1024;
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

#[derive(Debug, Clone, Default)]
pub(crate) struct WebFetch {
    paths: Option<XanaPaths>,
    security: McpHttpSecurity,
}

impl WebFetch {
    #[cfg(test)]
    fn for_tests(paths: XanaPaths) -> Self {
        Self {
            paths: Some(paths),
            security: McpHttpSecurity {
                allow_loopback_http: true,
            },
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
                "web_fetch supports only UTF-8 plain text, Markdown, HTML, and XHTML",
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
    urls: Vec<Url>,
    security: McpHttpSecurity,
    max_response_bytes: usize,
    timeout: Duration,
    cancellation: CancellationToken,
    failure: Option<FetchError>,
}

impl OutboundTransport for FetchTransport {
    type Receipt = FetchReceipt;

    fn send<'a>(
        &'a mut self,
        _recipient: &'a RecipientIdentity,
        _items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        Box::pin(async move {
            match fetch_chain(
                &self.urls,
                self.security,
                self.max_response_bytes,
                self.timeout,
                &self.cancellation,
            )
            .await
            {
                Ok(receipt) => Ok(receipt),
                Err(error) => {
                    let failure = error.outbound_failure();
                    self.failure = Some(error);
                    Err(failure)
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
            description: "Fetch one reviewed public HTTPS text document as bounded, attributed, untrusted evidence. Redirects must be supplied as an exact reviewed chain; this tool has no cookies, credentials, proxy inheritance, JavaScript, or browser authority.".into(),
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["url"],
                "properties":{
                    "url":{"type":"string","description":"Public HTTPS URL to fetch"},
                    "redirects":{"type":"array","maxItems":MAX_REDIRECTS,"items":{"type":"string"},"description":"Exact ordered redirect destinations previously reported by web_fetch; omitted on the first attempt"},
                    "max_response_bytes":{"type":"integer","minimum":1,"maximum":MAX_RESPONSE_BYTES,"description":"Encoded response-byte ceiling; default 1048576"},
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
        let max_response_bytes = args.max_response_bytes.unwrap_or(DEFAULT_RESPONSE_BYTES);
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
        let review = OutboundRequest::new(
            OperationId::new(),
            recipient.clone(),
            "retrieve public text as untrusted evidence",
            items.clone(),
        )
        .map_err(|error| error.to_string())?
        .review();
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
            let mut transport = FetchTransport {
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
            let policy = web_fetch_policy();
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
            render_receipt(receipt, &plan.paths, context.operation_id).await
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

fn web_fetch_policy() -> OutboundPolicyLayers {
    let allowed = BTreeSet::from([OutboundDataClass::PromptText]);
    OutboundPolicyLayers {
        connection_allowed: allowed.clone(),
        user_ceiling: allowed.clone(),
        profile_allowed: allowed,
        conversation_allowed: None,
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

async fn fetch_chain(
    urls: &[Url],
    security: McpHttpSecurity,
    max_response_bytes: usize,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<FetchReceipt, FetchError> {
    let requested_url = urls.first().cloned().ok_or(FetchError::InvalidUrl)?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut followed = Vec::new();
    for (index, url) in urls.iter().enumerate() {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .ok_or(FetchError::TimedOut)?;
        let client = pinned_client(url, security, remaining)
            .await
            .map_err(|_| FetchError::DnsOrAddressPolicy)?;
        let request = client
            .get(url.clone())
            .header(
                header::ACCEPT,
                "text/plain, text/markdown, text/html, application/xhtml+xml",
            )
            .header(
                header::USER_AGENT,
                concat!("xana/", env!("CARGO_PKG_VERSION")),
            );
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(FetchError::Cancelled),
            response = request.send() => response.map_err(classify_reqwest)?,
        };
        validate_headers(&response)?;

        if response.status().is_redirection() {
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
            let Some(reviewed) = urls.get(index + 1) else {
                return Err(FetchError::RedirectRequiresReview(next.to_string()));
            };
            if reviewed != &next {
                return Err(FetchError::RedirectPlanMismatch);
            }
            followed.push(next);
            continue;
        }
        if index + 1 != urls.len() {
            return Err(FetchError::RedirectPlanMismatch);
        }
        if !response.status().is_success() {
            return Err(FetchError::Http(response.status().as_u16()));
        }
        let media_type = response_media_type(&response)?;
        let body = collect_body(response, max_response_bytes, cancellation).await?;
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
        "text/html" | "application/xhtml+xml" => Ok(ResponseKind::Html),
        _ => Err(FetchError::UnsupportedContentType),
    }
}

async fn collect_body(
    response: reqwest::Response,
    max_bytes: usize,
    cancellation: &CancellationToken,
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
) -> Result<String, String> {
    let response_kind =
        classify_media_type(&receipt.media_type).map_err(|error| error.to_string())?;
    let body_for_extraction = receipt.body.clone();
    let extracted = match response_kind {
        ResponseKind::PlainText | ResponseKind::Markdown => String::from_utf8(body_for_extraction)
            .map_err(|_| FetchError::MalformedText.to_string())?,
        ResponseKind::Html => tokio::time::timeout(
            HTML_EXTRACTION_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                html2text::from_read(body_for_extraction.as_slice(), HTML_RENDER_WIDTH)
            }),
        )
        .await
        .map_err(|_| FetchError::ExtractionFailed.to_string())?
        .map_err(|_| FetchError::ExtractionFailed.to_string())?
        .map_err(|_| FetchError::ExtractionFailed.to_string())?,
    };
    let extracted = sanitize_text(&extracted);
    let (extracted, extraction_truncated) = truncate_utf8(extracted, MAX_EXTRACTED_BYTES);
    let (text, inline_truncated) = truncate_utf8(extracted.clone(), MAX_INLINE_BYTES);
    let text_truncated = extraction_truncated || inline_truncated;
    let artifact = if text_truncated || receipt.body.len() > MAX_INLINE_BYTES {
        let store = ArtifactStore::new(paths.data_dir().join("artifacts"));
        let bytes = receipt.body.clone();
        let media_type = receipt.media_type.clone();
        Some(
            tokio::task::spawn_blocking(move || store.put(&bytes, &media_type, PrincipalId::new()))
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
    let title = preview_title(&extracted);
    let response_bytes = u64::try_from(receipt.body.len())
        .map_err(|_| "web_fetch response length is out of range".to_owned())?;
    let result = LinkPreviewCardV1 {
        requested_url: receipt.requested_url.to_string(),
        final_url: receipt.final_url.to_string(),
        site_name,
        title,
        fetched_unix_ms,
        media_type: receipt.media_type,
        response_bytes,
        content_digest: blake3::hash(&receipt.body).to_hex().to_string(),
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
