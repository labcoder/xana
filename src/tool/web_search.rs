//! Search returns source evidence, never a provider-generated final answer.
use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, ToolExecutionContext,
};
use crate::{
    config::OutboundDataClass,
    credential::CredentialResolver,
    identity::OperationId,
    mcp::{
        McpHttpBudget, McpHttpClient, McpHttpEndpoint, McpHttpError, McpHttpSecurity,
        McpHttpToolHeaders, pinned_client,
    },
    outbound::{
        ObservedOutboundAudit, OutboundDisposition, OutboundGuard, OutboundItem, OutboundRequest,
        OutboundTransport, OutboundTransportFailure, RecipientIdentity, RecipientKind,
    },
    paths::XanaPaths,
    permission::PermissionScope,
    web::{SearchProvider, WebConfig, WebFailure, WebRuntime, WebStage, WebTurn, progress},
};
use futures::{StreamExt, future::BoxFuture};
use reqwest::{Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

const MAX_QUERY_BYTES: usize = 2048;
const MAX_OUTPUT_BYTES: usize = 24 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(25);
const PURPOSE: &str = "search public sources as untrusted evidence, not hosted Answers";

pub(super) struct WebSearch {
    pub(super) paths: XanaPaths,
    pub(super) config: WebConfig,
    pub(super) runtime: Arc<WebRuntime>,
    #[cfg(test)]
    pub(super) fixture: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Args {
    query: String,
    #[serde(default = "default_count")]
    count: usize,
}
fn default_count() -> usize {
    5
}
struct Plan {
    args: Args,
    recipient: RecipientIdentity,
    items: Vec<OutboundItem>,
}

impl Tool for WebSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "web_search".into(), contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Find current public sources when you do not already know the correct URL. Use a concise query for the CURRENT user question, then use returned source URLs with web_fetch when more evidence is needed. Returns source evidence, not guaranteed truth or an Answers-service synthesis. Never guess URL paths or reuse an unrelated previous query. Empty results and errors are not evidence. No cookies, local files or browser interaction.".into(),
            parameters: json!({"type":"object","additionalProperties":false,"required":["query"],"properties":{
                "query":{"type":"string","minLength":1,"maxLength":MAX_QUERY_BYTES},
                "count":{"type":"integer","minimum":1,"maximum":10,"default":5}}}),
            effect_class: EffectClass::Network, replay_safety: ReplaySafety::Never }
    }
    fn plan(&self, arguments: &Value, _: &Path) -> Result<PlannedToolInvocation, String> {
        let mut args: Args = serde_json::from_value(arguments.clone())
            .map_err(|_| "web_search requires query and optional count".to_owned())?;
        args.query = args.query.trim().to_owned();
        if args.query.is_empty()
            || args.query.len() > MAX_QUERY_BYTES
            || args.query.chars().any(char::is_control)
            || !(1..=10).contains(&args.count)
        {
            return Err("web_search query must be 1–2048 UTF-8 bytes without control characters; count must be 1–10".into());
        }
        let (name, connection) = self
            .config
            .selected()
            .ok_or_else(|| WebFailure::NotConfigured.to_string())?;
        let identity = serde_json::to_vec(&(name, connection))
            .map_err(|_| "could not encode search recipient")?;
        let recipient = RecipientIdentity::new(
            RecipientKind::WebSearch,
            name,
            connection.provider.endpoint(),
            &identity,
        )
        .map_err(|e| e.to_string())?;
        let items = vec![
            OutboundItem::new(
                OutboundDataClass::PromptText,
                "public search query (review preview capped at 1024 bytes)",
                Some(bounded(&args.query, 1024)),
                "model-proposed current-turn search",
                serde_json::to_vec(&args).map_err(|_| "invalid search")?,
            )
            .map_err(|e| e.to_string())?,
        ];
        let mut review = OutboundRequest::new(
            OperationId::new(),
            recipient.clone(),
            PURPOSE,
            items.clone(),
        )
        .map_err(|e| e.to_string())?
        .review();
        review.public_web = Some(Box::new(self.config.review()));
        Ok(PlannedToolInvocation::new(
            serde_json::to_value(&args).map_err(|e| e.to_string())?,
            PermissionScope::External {
                recipient_identity_digest: recipient.identity_digest.clone(),
                operation: "web_search".into(),
            },
            Plan {
                args,
                recipient,
                items,
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
            let plan = planned.executable::<Plan>("web_search")?;
            let turn = self.runtime.turn(context.operation_id);
            if context
                .outbound_approval
                .as_ref()
                .is_some_and(|approval| approval.permits_public_web())
            {
                turn.allow_public_web();
            }
            let mut transport = Transport {
                tool: self,
                args: &plan.args,
                turn,
                failure: None,
                events: context.events.clone(),
                operation: context.operation_id,
            };
            let request = OutboundRequest::new(
                context.operation_id,
                plan.recipient.clone(),
                PURPOSE,
                plan.items.clone(),
            )
            .map_err(|e| e.to_string())?;
            let observer =
                crate::diagnostics::outbound_audit(&self.paths).map_err(|e| e.to_string())?;
            let mut audit = ObservedOutboundAudit::new(observer.as_ref());
            let mut approval = context.outbound_approval;
            let result = OutboundGuard::open(&self.paths)
                .map_err(|e| e.to_string())?
                .dispatch(
                    request,
                    &self.runtime.policy(),
                    approval.as_mut(),
                    &mut transport,
                    &mut audit,
                )
                .await;
            result.map_err(|error| {
                transport
                    .failure
                    .map_or_else(|| error.to_string(), |failure| failure.to_string())
            })
        })
    }
    fn outbound_disposition(
        &self,
        planned: &PlannedToolInvocation,
    ) -> Result<Option<OutboundDisposition>, String> {
        let plan = planned.executable::<Plan>("web_search")?;
        OutboundGuard::open(&self.paths)
            .map_err(|e| e.to_string())?
            .disposition(&plan.recipient, plan.items.iter().map(OutboundItem::class))
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

struct Transport<'a> {
    tool: &'a WebSearch,
    args: &'a Args,
    turn: Arc<WebTurn>,
    failure: Option<WebFailure>,
    events: Option<crate::native_runtime::AgentEventSender>,
    operation: OperationId,
}
impl OutboundTransport for Transport<'_> {
    type Receipt = String;
    fn send<'a>(
        &'a mut self,
        _: &'a RecipientIdentity,
        _: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<String, OutboundTransportFailure>> {
        Box::pin(async move {
            let key = blake3::hash(
                &serde_json::to_vec(&("search", self.args))
                    .map_err(|_| OutboundTransportFailure::Protocol)?,
            );
            let result = tokio::time::timeout(
                DEADLINE,
                self.turn.cached(key, true, |slot| async {
                    let _slot = slot;
                    progress(
                        self.events.as_ref(),
                        self.operation,
                        WebStage::Searching,
                        &self.turn,
                    );
                    self.tool.search(self.args, &self.turn).await
                }),
            )
            .await
            .unwrap_or(Err(WebFailure::TimedOut));
            progress(
                self.events.as_ref(),
                self.operation,
                if result.is_ok() {
                    WebStage::Complete
                } else {
                    WebStage::Failed
                },
                &self.turn,
            );
            result.map_err(|failure| {
                self.failure = Some(failure);
                OutboundTransportFailure::Protocol
            })
        })
    }
}

impl McpHttpBudget for WebTurn {
    fn request(&self) -> Result<(), McpHttpError> {
        self.attempt().map_err(|_| McpHttpError::AggregateLimit)
    }
    fn received(&self, bytes: usize) -> Result<(), McpHttpError> {
        self.ingress(bytes)
            .map_err(|_| McpHttpError::AggregateLimit)
    }
}

impl WebSearch {
    async fn search(&self, args: &Args, turn: &WebTurn) -> Result<String, WebFailure> {
        let (name, connection) = self.config.selected().ok_or(WebFailure::NotConfigured)?;
        // Resolve secrets only inside the authorized send. Do not retain them in
        // the catalog, logs, config or cached results.
        let credential = connection.credential.clone();
        let secret = tokio::task::spawn_blocking(move || {
            credential
                .as_ref()
                .map(|reference| CredentialResolver::default().resolve(reference))
                .transpose()
        })
        .await
        .map_err(|_| WebFailure::Unavailable)?
        .map_err(|_| WebFailure::Authentication)?;
        #[cfg(test)]
        let secret = if self.fixture.is_some() {
            Some(
                crate::credential::SecretString::new("fixture-only-key".into())
                    .expect("valid fixture secret"),
            )
        } else {
            secret
        };
        let endpoint = connection.provider.endpoint();
        #[cfg(test)]
        let endpoint = self.fixture.as_deref().unwrap_or(endpoint);
        let security = McpHttpSecurity {
            allow_loopback_http: cfg!(test) && endpoint.starts_with("http://127.0.0.1:"),
        };
        let value = if connection.provider == SearchProvider::ExaMcp {
            let endpoint =
                McpHttpEndpoint::parse(endpoint, security).map_err(|_| WebFailure::Policy)?;
            let client = McpHttpClient::connect_exa_search(endpoint)
                .await
                .map_err(mcp_failure)?;
            let cancellation = CancellationToken::new();
            let headers = McpHttpToolHeaders::default();
            let initialize = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":crate::mcp::EXA_SEARCH_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"xana","version":env!("CARGO_PKG_VERSION")}}})).unwrap();
            let initialized = client
                .request_with_budget(&initialize, None, &headers, &cancellation, Some(turn))
                .await
                .map_err(mcp_failure)?;
            let initialized: Value = serde_json::from_slice(&initialized.final_response)
                .map_err(|_| WebFailure::InvalidResponse)?;
            if initialized.get("error").is_some()
                || initialized["result"]["capabilities"]["tools"].is_null()
                || initialized["result"]["protocolVersion"]
                    != crate::mcp::EXA_SEARCH_PROTOCOL_VERSION
            {
                return Err(WebFailure::InvalidResponse);
            }
            client
                .initialized_with_budget(&cancellation, turn)
                .await
                .map_err(mcp_failure)?;
            let request = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"web_search_exa","arguments":{"query":args.query,"numResults":args.count,"type":"auto","contextMaxCharacters":16000}}})).unwrap();
            let response = client
                .request_with_budget(&request, None, &headers, &cancellation, Some(turn))
                .await
                .map_err(mcp_failure)?;
            serde_json::from_slice(&response.final_response)
                .map_err(|_| WebFailure::InvalidResponse)?
        } else {
            turn.attempt()?;
            let mut url = Url::parse(endpoint).map_err(|_| WebFailure::Policy)?;
            if connection.provider == SearchProvider::Brave {
                url.query_pairs_mut()
                    .append_pair("q", &args.query)
                    .append_pair("count", &args.count.to_string());
            }
            let client = pinned_client(&url, security, DEADLINE)
                .await
                .map_err(|_| WebFailure::Policy)?;
            let key = secret.as_ref().ok_or(WebFailure::Authentication)?;
            let request = match connection.provider {
                SearchProvider::Exa => client.post(url).header("x-api-key", key.expose())
                    .json(&json!({"query":args.query,"type":"auto","numResults":args.count,"contents":{"text":{"maxCharacters":3000}}})),
                SearchProvider::Brave => client.get(url).header("X-Subscription-Token", key.expose()),
                SearchProvider::ExaMcp => unreachable!(),
            };
            let response = request
                .header(header::ACCEPT, "application/json")
                .header(header::ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(request_failure)?;
            let header_bytes = response
                .headers()
                .iter()
                .map(|(name, value)| name.as_str().len() + value.len())
                .sum::<usize>();
            turn.ingress(header_bytes)?;
            match response.status().as_u16() {
                200 => {}
                401..=403 => return Err(WebFailure::Authentication),
                429 => return Err(WebFailure::RateLimited),
                404 => return Err(WebFailure::Missing),
                _ => return Err(WebFailure::Unavailable),
            }
            if header_bytes > 32 * 1024
                || response
                    .content_length()
                    .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
            {
                return Err(WebFailure::TooLarge);
            }
            if response
                .headers()
                .get(header::CONTENT_ENCODING)
                .is_some_and(|value| value != "identity")
                || !response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.split(';').next() == Some("application/json"))
            {
                return Err(WebFailure::InvalidResponse);
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(request_failure)?;
                turn.ingress(chunk.len())?;
                if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err(WebFailure::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&body).map_err(|_| WebFailure::InvalidResponse)?
        };
        normalize(
            connection.provider,
            name,
            args.count,
            value,
            turn.elapsed_ms(),
        )
    }
}

fn request_failure(error: reqwest::Error) -> WebFailure {
    if error.is_timeout() {
        WebFailure::TimedOut
    } else {
        WebFailure::Unavailable
    }
}
fn mcp_failure(error: McpHttpError) -> WebFailure {
    match error {
        McpHttpError::AggregateLimit => WebFailure::Budget,
        McpHttpError::RateLimited => WebFailure::RateLimited,
        McpHttpError::Unauthorized(_) | McpHttpError::InsufficientScope => {
            WebFailure::Authentication
        }
        McpHttpError::Timeout => WebFailure::TimedOut,
        McpHttpError::Cancelled => WebFailure::Cancelled,
        McpHttpError::ResponseTooLarge => WebFailure::TooLarge,
        _ => WebFailure::InvalidResponse,
    }
}

fn bounded(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end]
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
}

fn normalize(
    provider: SearchProvider,
    route: &str,
    count: usize,
    value: Value,
    elapsed_ms: u64,
) -> Result<String, WebFailure> {
    let mut sources = Vec::new();
    let mut unstructured = None;
    let mut truncated = false;
    if provider == SearchProvider::ExaMcp {
        if value.get("error").is_some() || value["result"]["isError"] == true {
            return Err(WebFailure::InvalidResponse);
        }
        let content = value["result"]["content"]
            .as_array()
            .ok_or(WebFailure::InvalidResponse)?;
        let mut text = String::new();
        for block in content.iter().take(32) {
            if let Some(part) = block.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    text.push('\n');
                }
                let remaining = 16000usize.saturating_sub(text.len());
                text.push_str(&bounded(part, remaining));
                truncated |= part.len() > remaining;
            }
        }
        truncated |= content.len() > 32;
        // MCP supplies formatted text, not a promised typed result schema.
        // Preserve it honestly; do not fabricate URLs/dates by parsing prose.
        unstructured = Some(text);
    } else {
        let results = if provider == SearchProvider::Exa {
            value.get("results")
        } else {
            value.pointer("/web/results")
        }
        .and_then(Value::as_array)
        .ok_or(WebFailure::InvalidResponse)?;
        truncated |= results.len() > count;
        for result in results.iter().take(count) {
            let raw_url = result
                .get("url")
                .and_then(Value::as_str)
                .ok_or(WebFailure::InvalidResponse)?;
            let url = Url::parse(raw_url).map_err(|_| WebFailure::InvalidResponse)?;
            if raw_url.len() > 2048
                || !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(WebFailure::InvalidResponse);
            }
            let text = result
                .get("text")
                .or_else(|| result.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("");
            truncated |= text.len() > 1600;
            sources.push(json!({"url":url.as_str(),"title":result.get("title").and_then(Value::as_str).map(|v|bounded(v,256)),
                "published":result.get("publishedDate").and_then(Value::as_str).map(|v|bounded(v,128)),
                "age_label":result.get("age").and_then(Value::as_str).map(|v|bounded(v,128)),
                "text":bounded(text,1600)}));
        }
    }
    let mut result = json!({"route":route,"provider":provider,"sources":sources,"unstructured_evidence":unstructured,
        "empty":sources.is_empty() && unstructured.as_deref().is_none_or(str::is_empty),
        "untrusted":true,"truncated":truncated,"elapsed_ms":elapsed_ms,
        "retrieved_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|WebFailure::Unavailable)?.as_millis(),
        "provider_internal_work":"unknown"});
    loop {
        let encoded = serde_json::to_string(&result).map_err(|_| WebFailure::InvalidResponse)?;
        if encoded.len() <= MAX_OUTPUT_BYTES {
            return Ok(encoded);
        }
        result["truncated"] = json!(true);
        if let Some(text) = result["unstructured_evidence"].as_str() {
            result["unstructured_evidence"] = json!(bounded(text, text.len() / 2));
        } else if let Some(sources) = result["sources"].as_array_mut()
            && !sources.is_empty()
        {
            sources.pop();
        } else {
            return Err(WebFailure::TooLarge);
        }
    }
}

#[cfg(test)]
mod tests;
