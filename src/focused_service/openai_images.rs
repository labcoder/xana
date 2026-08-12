//! Official OpenAI Images API adapter for `image.generate`.

use super::*;
use crate::credential::SecretString;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{StatusCode, header};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ADAPTER_ID: &str = "openai.images";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const MAX_WIRE_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct OpenAiImageAdapter {
    secret: SecretString,
    client: reqwest::Client,
    descriptors: Vec<FocusedServiceDescriptor>,
    allow_http: bool,
}

impl OpenAiImageAdapter {
    pub(crate) fn new(secret: SecretString) -> Result<Self, FocusedServiceError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| FocusedServiceError::Transport)?;
        Ok(Self {
            secret,
            client,
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
        let base = route.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
        let mut base = reqwest::Url::parse(base).map_err(|_| FocusedServiceError::Transport)?;
        if (!self.allow_http && base.scheme() != "https")
            || !matches!(base.scheme(), "https" | "http")
            || base.username() != ""
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.host_str().is_none()
        {
            return Err(FocusedServiceError::Transport);
        }
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        base.join("images/generations")
            .map_err(|_| FocusedServiceError::Transport)
    }

    fn body(request: &FocusedServiceRequest) -> Result<Value, FocusedServiceError> {
        if !request.input_artifacts.is_empty() {
            return Err(FocusedServiceError::InvalidRequest(
                "OpenAI image generation editing is not enabled by this adapter",
            ));
        }
        if request.route.model != "gpt-image-2" && !request.route.model.starts_with("gpt-image-2-")
        {
            return Err(FocusedServiceError::InvalidRequest(
                "OpenAI image generation requires gpt-image-2 or a pinned gpt-image-2 snapshot",
            ));
        }
        let mut body = Map::from_iter([
            ("model".into(), Value::String(request.route.model.clone())),
            ("prompt".into(), Value::String(request.prompt.clone())),
            ("n".into(), Value::from(1)),
        ]);
        for (key, value) in &request.route.options {
            match key.as_str() {
                "size" if valid_size(value) => {}
                "quality" if matches!(value.as_str(), "auto" | "low" | "medium" | "high") => {}
                "output_format" if matches!(value.as_str(), "png" | "jpeg" | "webp") => {}
                "background" if matches!(value.as_str(), "auto" | "opaque" | "transparent") => {}
                "moderation" if matches!(value.as_str(), "auto" | "low") => {}
                "output_compression"
                    if value
                        .parse::<u8>()
                        .is_ok_and(|compression| compression <= 100) =>
                {
                    body.insert(key.clone(), Value::from(value.parse::<u8>().unwrap()));
                    continue;
                }
                _ => {
                    return Err(FocusedServiceError::InvalidRequest(
                        "OpenAI image route contains an unsupported option or value",
                    ));
                }
            }
            body.insert(key.clone(), Value::String(value.clone()));
        }
        if body.get("background").and_then(Value::as_str) == Some("transparent")
            && body
                .get("output_format")
                .and_then(Value::as_str)
                .is_some_and(|format| !matches!(format, "png" | "webp"))
        {
            return Err(FocusedServiceError::InvalidRequest(
                "transparent output requires png or webp",
            ));
        }
        Ok(Value::Object(body))
    }
}

impl FocusedServiceAdapter for OpenAiImageAdapter {
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
            let endpoint = self.endpoint(&request.route)?;
            let send = self
                .client
                .post(endpoint)
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
            if response.status().is_redirection() {
                return Err(FocusedServiceError::Transport);
            }
            if response
                .content_length()
                .is_some_and(|length| length > MAX_WIRE_BYTES as u64)
            {
                return Err(FocusedServiceError::ResponseTooLarge);
            }
            let status = response.status();
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let bytes = tokio::select! {
                biased;
                () = context.cancellation.cancelled() => return Err(FocusedServiceError::Cancelled),
                bytes = response.bytes() => bytes.map_err(classify_transport)?,
            };
            if bytes.len() > MAX_WIRE_BYTES {
                return Err(FocusedServiceError::ResponseTooLarge);
            }
            if !content_type
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
            {
                return Err(FocusedServiceError::MalformedResponse);
            }
            if !status.is_success() {
                return Err(classify_error(status, &bytes));
            }
            let response: OpenAiImagesResponse = serde_json::from_slice(&bytes)
                .map_err(|_| FocusedServiceError::MalformedResponse)?;
            let entries = response
                .data
                .ok_or(FocusedServiceError::MalformedResponse)?;
            if entries.len() != 1 {
                return Err(FocusedServiceError::MalformedResponse);
            }
            let image = STANDARD
                .decode(
                    entries[0]
                        .b64_json
                        .as_deref()
                        .ok_or(FocusedServiceError::MalformedResponse)?,
                )
                .map_err(|_| FocusedServiceError::MalformedResponse)?;
            let output_format = response.output_format.as_deref().unwrap_or("png");
            let media_type = match output_format {
                "png" if image.starts_with(b"\x89PNG\r\n\x1a\n") => "image/png",
                "jpeg" if image.starts_with(&[0xff, 0xd8, 0xff]) => "image/jpeg",
                "webp" if image.starts_with(b"RIFF") && image.get(8..12) == Some(b"WEBP") => {
                    "image/webp"
                }
                _ => return Err(FocusedServiceError::MalformedResponse),
            };
            if image.len() > request.route.descriptor.max_artifact_bytes {
                return Err(FocusedServiceError::ResponseTooLarge);
            }
            let (artifact, _) = context
                .artifacts
                .put(&image, media_type, context.owner)
                .map_err(|error| FocusedServiceError::Artifact(error.to_string()))?;
            let usage = response.usage.unwrap_or_default();
            let usage_available = usage.input_tokens.is_some() || usage.output_tokens.is_some();
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
                    input_units: usage.input_tokens,
                    output_units: usage.output_tokens,
                    cost_microusd: None,
                },
                usage_available,
                cost_available: false,
                untrusted: true,
            })
        })
    }
}

#[derive(Deserialize)]
struct OpenAiImagesResponse {
    data: Option<Vec<OpenAiImageEntry>>,
    output_format: Option<String>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiImageEntry {
    b64_json: Option<String>,
}

#[derive(Default, Deserialize)]
struct OpenAiUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
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
        max_prompt_bytes: 32_000,
        max_input_artifacts: 0,
        max_output_artifacts: 1,
        max_artifact_bytes: crate::artifact::MAX_ARTIFACT_BYTES,
        requires_credential: true,
        supports_cancellation: true,
        reports_usage: true,
        reports_cost: false,
    }
}

fn valid_size(value: &str) -> bool {
    if value == "auto" {
        return true;
    }
    let Some((width, height)) = value.split_once('x') else {
        return false;
    };
    let Ok(width) = width.parse::<u16>() else {
        return false;
    };
    let Ok(height) = height.parse::<u16>() else {
        return false;
    };
    width >= 256
        && height >= 256
        && width <= 3840
        && height <= 2160
        && width % 16 == 0
        && height % 16 == 0
        && width <= height.saturating_mul(3)
        && height <= width.saturating_mul(3)
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
        Some("insufficient_quota") => FocusedServiceError::Quota,
        Some("content_policy_violation") | Some("safety_violations") => {
            FocusedServiceError::ContentPolicy
        }
        _ if status == StatusCode::TOO_MANY_REQUESTS => FocusedServiceError::RateLimited,
        _ => FocusedServiceError::Transport,
    }
}

fn now_unix_ms() -> Result<u64, FocusedServiceError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FocusedServiceError::Transport)?
        .as_millis();
    u64::try_from(millis).map_err(|_| FocusedServiceError::Transport)
}

#[cfg(test)]
mod tests;
