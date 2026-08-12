//! OpenRouter's dedicated Image API adapter.

use super::*;
use crate::credential::SecretString;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{StatusCode, header};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ADAPTER_ID: &str = "openrouter.images";
const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const MAX_WIRE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct OpenRouterImageAdapter {
    secret: SecretString,
    client: reqwest::Client,
    descriptors: Vec<FocusedServiceDescriptor>,
    allow_http: bool,
}

impl OpenRouterImageAdapter {
    pub(crate) fn new(secret: SecretString) -> Result<Self, FocusedServiceError> {
        Ok(Self {
            secret,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|_| FocusedServiceError::Transport)?,
            descriptors: vec![descriptor()],
            allow_http: false,
        })
    }

    #[cfg(test)]
    fn allow_http(mut self) -> Self {
        self.allow_http = true;
        self
    }

    fn endpoint(&self, route: &ResolvedServiceRoute) -> Result<reqwest::Url, FocusedServiceError> {
        let mut url = reqwest::Url::parse(route.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL))
            .map_err(|_| FocusedServiceError::Transport)?;
        if (!self.allow_http && url.scheme() != "https")
            || !matches!(url.scheme(), "http" | "https")
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
        {
            return Err(FocusedServiceError::Transport);
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        url.join("images")
            .map_err(|_| FocusedServiceError::Transport)
    }

    fn body(request: &FocusedServiceRequest) -> Result<Value, FocusedServiceError> {
        if !request.input_artifacts.is_empty() || !request.route.model.contains('/') {
            return Err(FocusedServiceError::InvalidRequest(
                "OpenRouter image routes require a namespaced model and currently accept text only",
            ));
        }
        let provider = request
            .route
            .options
            .get("provider_only")
            .filter(|provider| valid_provider(provider))
            .ok_or(FocusedServiceError::InvalidRequest(
                "OpenRouter image routes require provider_only to prevent upstream fallback",
            ))?;
        let mut body = Map::from_iter([
            ("model".into(), Value::String(request.route.model.clone())),
            ("prompt".into(), Value::String(request.prompt.clone())),
            ("n".into(), Value::from(1)),
            (
                "provider".into(),
                json!({"only":[provider],"allow_fallbacks":false}),
            ),
        ]);
        for (key, value) in &request.route.options {
            match key.as_str() {
                "provider_only" => continue,
                "resolution" if matches!(value.as_str(), "512" | "1K" | "2K" | "4K") => {}
                "aspect_ratio" if valid_aspect_ratio(value) => {}
                "size" if valid_size(value) => {}
                "quality" if matches!(value.as_str(), "auto" | "low" | "medium" | "high") => {}
                "output_format" if matches!(value.as_str(), "png" | "jpeg" | "webp") => {}
                "background" if matches!(value.as_str(), "auto" | "transparent" | "opaque") => {}
                "output_compression" if value.parse::<u8>().is_ok_and(|value| value <= 100) => {
                    body.insert(key.clone(), Value::from(value.parse::<u8>().unwrap()));
                    continue;
                }
                "seed" if value.parse::<u64>().is_ok() => {
                    body.insert(key.clone(), Value::from(value.parse::<u64>().unwrap()));
                    continue;
                }
                _ => {
                    return Err(FocusedServiceError::InvalidRequest(
                        "OpenRouter image route contains an unsupported option or value",
                    ));
                }
            }
            body.insert(key.clone(), Value::String(value.clone()));
        }
        Ok(Value::Object(body))
    }
}

impl FocusedServiceAdapter for OpenRouterImageAdapter {
    fn descriptors(&self) -> &[FocusedServiceDescriptor] {
        &self.descriptors
    }

    fn execute<'a>(
        &'a self,
        request: FocusedServiceRequest,
        context: FocusedServiceContext,
    ) -> BoxFuture<'a, Result<FocusedServiceResult, FocusedServiceError>> {
        Box::pin(async move {
            request.validate()?;
            let body = Self::body(&request)?;
            let send = self
                .client
                .post(self.endpoint(&request.route)?)
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", self.secret.expose()),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .json(&body)
                .send();
            let response = tokio::select! {
                biased;
                () = context.cancellation.cancelled() => return Err(FocusedServiceError::Cancelled),
                response = send => response.map_err(classify_transport)?,
            };
            if response.status().is_redirection()
                || response
                    .content_length()
                    .is_some_and(|size| size > MAX_WIRE_BYTES as u64)
            {
                return Err(FocusedServiceError::ResponseTooLarge);
            }
            let status = response.status();
            let json_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.split(';').next() == Some("application/json"));
            let bytes = tokio::select! {
                biased;
                () = context.cancellation.cancelled() => return Err(FocusedServiceError::Cancelled),
                bytes = response.bytes() => bytes.map_err(classify_transport)?,
            };
            if bytes.len() > MAX_WIRE_BYTES {
                return Err(FocusedServiceError::ResponseTooLarge);
            }
            if !json_type {
                return Err(FocusedServiceError::MalformedResponse);
            }
            if !status.is_success() {
                return Err(classify_error(status, &bytes));
            }
            let response: OpenRouterResponse = serde_json::from_slice(&bytes)
                .map_err(|_| FocusedServiceError::MalformedResponse)?;
            if response.data.len() != 1 {
                return Err(FocusedServiceError::MalformedResponse);
            }
            let entry = &response.data[0];
            let image = STANDARD
                .decode(&entry.b64_json)
                .map_err(|_| FocusedServiceError::MalformedResponse)?;
            let media_type = validate_media(&entry.media_type, &image)?;
            if image.len() > request.route.descriptor.max_artifact_bytes {
                return Err(FocusedServiceError::ResponseTooLarge);
            }
            let (artifact, _) = context
                .artifacts
                .put(&image, media_type, context.owner)
                .map_err(|error| FocusedServiceError::Artifact(error.to_string()))?;
            let usage = response.usage.unwrap_or_default();
            let cost_microusd = usage.cost.and_then(cost_microusd);
            Ok(FocusedServiceResult {
                provenance: FocusedServiceProvenance {
                    operation_id: request.operation_id,
                    route: request.route.name,
                    connection: request.route.connection,
                    adapter: request.route.adapter,
                    model: request.route.model,
                    operation: request.route.operation,
                    options: request.route.options,
                    created_unix_ms: now_unix_ms()?,
                },
                artifacts: vec![artifact],
                derived_text: None,
                usage: FocusedServiceUsage {
                    input_units: usage.prompt_tokens,
                    output_units: usage.completion_tokens,
                    cost_microusd,
                },
                usage_available: usage.prompt_tokens.is_some() || usage.completion_tokens.is_some(),
                cost_available: cost_microusd.is_some(),
                untrusted: true,
            })
        })
    }
}

#[derive(Deserialize)]
struct OpenRouterResponse {
    data: Vec<OpenRouterImage>,
    usage: Option<OpenRouterUsage>,
}

#[derive(Deserialize)]
struct OpenRouterImage {
    b64_json: String,
    media_type: String,
}

#[derive(Default, Deserialize)]
struct OpenRouterUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    cost: Option<f64>,
}

pub(crate) fn descriptor() -> FocusedServiceDescriptor {
    FocusedServiceDescriptor {
        adapter: ADAPTER_ID.into(),
        operation: ServiceOperation::ImageGenerate,
        inputs: [MediaModality::Text].into_iter().collect(),
        outputs: [MediaModality::Image].into_iter().collect(),
        supports_editing: false,
        output_formats: ["image/png".into(), "image/jpeg".into(), "image/webp".into()]
            .into_iter()
            .collect(),
        max_prompt_bytes: MAX_FOCUSED_PROMPT_BYTES,
        max_input_artifacts: 0,
        max_output_artifacts: 1,
        max_artifact_bytes: crate::artifact::MAX_ARTIFACT_BYTES,
        requires_credential: true,
        supports_cancellation: true,
        reports_usage: true,
        reports_cost: true,
    }
}

fn valid_provider(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_aspect_ratio(value: &str) -> bool {
    let Some((width, height)) = value.split_once(':') else {
        return false;
    };
    width
        .parse::<u8>()
        .is_ok_and(|value| (1..=21).contains(&value))
        && height
            .parse::<u8>()
            .is_ok_and(|value| (1..=21).contains(&value))
}

fn valid_size(value: &str) -> bool {
    matches!(value, "512" | "1K" | "2K" | "4K")
        || value.split_once('x').is_some_and(|(width, height)| {
            width.parse::<u16>().is_ok_and(|value| value >= 256)
                && height.parse::<u16>().is_ok_and(|value| value >= 256)
        })
}

fn validate_media<'a>(media_type: &'a str, bytes: &[u8]) -> Result<&'a str, FocusedServiceError> {
    match media_type {
        "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Ok(media_type),
        "image/jpeg" if bytes.starts_with(&[0xff, 0xd8, 0xff]) => Ok(media_type),
        "image/webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
            Ok(media_type)
        }
        _ => Err(FocusedServiceError::MalformedResponse),
    }
}

fn cost_microusd(cost: f64) -> Option<u64> {
    (cost.is_finite() && (0.0..=10_000.0).contains(&cost))
        .then(|| (cost * 1_000_000.0).round() as u64)
}

fn classify_transport(error: reqwest::Error) -> FocusedServiceError {
    if error.is_timeout() {
        FocusedServiceError::Timeout
    } else {
        FocusedServiceError::Transport
    }
}

fn classify_error(status: StatusCode, body: &[u8]) -> FocusedServiceError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return FocusedServiceError::Authentication;
    }
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    match code.as_deref() {
        Some("insufficient_credits") => FocusedServiceError::Quota,
        Some("content_policy_violation") => FocusedServiceError::ContentPolicy,
        _ if status == StatusCode::TOO_MANY_REQUESTS => FocusedServiceError::RateLimited,
        _ => FocusedServiceError::Transport,
    }
}

fn now_unix_ms() -> Result<u64, FocusedServiceError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| FocusedServiceError::Transport)?
            .as_millis(),
    )
    .map_err(|_| FocusedServiceError::Transport)
}

#[cfg(test)]
mod tests;
