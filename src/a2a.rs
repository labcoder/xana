//! Explicit A2A Agent Card discovery and local trust state.
//!
//! Discovery is an application action, never a profile-resolution side effect.
//! Cached cards are normalized untrusted metadata; trust binds one exact
//! declaration and semantic card identity.

mod delegation;

pub(crate) use delegation::{
    A2aDelegationActivation, ExternalAgentActivity, ExternalAgentActivityKind,
    activate_profile_delegation_tools, cancel_tracked_task,
};

use crate::{
    config::{ConnectionRegistry, ExternalAgentDeclaration},
    credential::{CredentialResolver, SecretString},
    mcp::{McpHttpEndpoint, McpHttpSecurity, pinned_client},
    outbound::{RecipientIdentity, RecipientKind},
    paths::XanaPaths,
    private_state::{
        ExternalAgentSkillRecord, ExternalAgentStateDocument, ExternalAgentStateRecord,
        ExternalAgentTaskRecord, PrivateStateError, UpdateDocumentError,
        ensure_interoperable_records, read_document, update_document,
    },
};
use futures::StreamExt;
use reqwest::{StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

pub(crate) const A2A_PROTOCOL_VERSION: &str = "1.0";
const MAX_CARD_BYTES: usize = 512 * 1024;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_COLLECTION_ITEMS: usize = 128;
const CARD_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RETAINED_TASKS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalAgentView {
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) enabled: bool,
    pub(crate) readiness: ExternalAgentReadiness,
    pub(crate) cached: Option<ExternalAgentStateRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalAgentReadiness {
    NotDiscovered,
    Disabled,
    ReviewRequired,
    Ready,
}

impl ExternalAgentReadiness {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotDiscovered => "not_discovered",
            Self::Disabled => "disabled",
            Self::ReviewRequired => "review_required",
            Self::Ready => "ready",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExternalAgentManager {
    paths: XanaPaths,
}

impl ExternalAgentManager {
    pub(crate) fn open(paths: &XanaPaths) -> Self {
        Self {
            paths: paths.clone(),
        }
    }

    pub(crate) fn list(
        &self,
        registry: &ConnectionRegistry,
    ) -> Result<Vec<ExternalAgentView>, A2aError> {
        let state = self.state()?;
        Ok(registry
            .external_agents
            .iter()
            .map(|(name, declaration)| view(name, declaration, state.agents.get(name).cloned()))
            .collect())
    }

    pub(crate) fn show(
        &self,
        name: &str,
        declaration: &ExternalAgentDeclaration,
    ) -> Result<ExternalAgentView, A2aError> {
        let state = self.state()?;
        Ok(view(name, declaration, state.agents.get(name).cloned()))
    }

    pub(crate) async fn refresh(
        &self,
        name: &str,
        declaration: &ExternalAgentDeclaration,
        cancellation: &CancellationToken,
    ) -> Result<ExternalAgentStateRecord, A2aError> {
        self.refresh_with_security(name, declaration, McpHttpSecurity::default(), cancellation)
            .await
    }

    async fn refresh_with_security(
        &self,
        name: &str,
        declaration: &ExternalAgentDeclaration,
        security: McpHttpSecurity,
        cancellation: &CancellationToken,
    ) -> Result<ExternalAgentStateRecord, A2aError> {
        let endpoint = McpHttpEndpoint::parse(&declaration.endpoint, security)
            .map_err(|_| A2aError::InvalidEndpoint)?;
        let client = pinned_client(endpoint.url(), security, CARD_TIMEOUT)
            .await
            .map_err(|_| A2aError::Network)?;
        let credential = declaration
            .credential
            .as_ref()
            .map(|reference| CredentialResolver::default().resolve(reference))
            .transpose()
            .map_err(|_| A2aError::Credential)?;
        let bytes = fetch_card(
            &client,
            endpoint.url().clone(),
            credential.as_ref(),
            cancellation,
        )
        .await?;
        let mut record = normalize_card(name, endpoint.canonical(), &bytes, security)?;
        let prior = self.state()?.agents.remove(name);
        if let Some(prior) = prior {
            record.trusted_identity_digest = prior.trusted_identity_digest;
            record.trusted_unix_ms = prior.trusted_unix_ms;
        }
        self.update(|document| {
            document.agents.insert(name.to_owned(), record.clone());
            Ok(record.clone())
        })
    }

    pub(crate) fn trust(&self, name: &str) -> Result<ExternalAgentStateRecord, A2aError> {
        self.update(|document| {
            let record = document
                .agents
                .get_mut(name)
                .ok_or_else(|| A2aError::NotDiscovered(name.to_owned()))?;
            record.trusted_identity_digest = Some(record.identity_digest.clone());
            record.trusted_unix_ms = Some(now_unix_ms()?);
            Ok(record.clone())
        })
    }

    pub(crate) fn untrust(&self, name: &str) -> Result<(), A2aError> {
        self.update(|document| {
            let record = document
                .agents
                .get_mut(name)
                .ok_or_else(|| A2aError::NotDiscovered(name.to_owned()))?;
            record.trusted_identity_digest = None;
            record.trusted_unix_ms = None;
            Ok(())
        })
    }

    pub(crate) fn remove_cache(&self, name: &str) -> Result<(), A2aError> {
        self.update(|document| {
            document.agents.remove(name);
            document.tasks.retain(|_, task| task.connection != name);
            Ok(())
        })
    }

    #[allow(dead_code)] // Consumed by bounded delegation in M3-17.
    pub(crate) fn trusted_record(
        &self,
        name: &str,
        declaration: &ExternalAgentDeclaration,
    ) -> Result<ExternalAgentStateRecord, A2aError> {
        let view = self.show(name, declaration)?;
        if view.readiness != ExternalAgentReadiness::Ready {
            return Err(A2aError::NotTrusted(name.to_owned()));
        }
        view.cached
            .ok_or_else(|| A2aError::NotTrusted(name.to_owned()))
    }

    pub(crate) fn readiness(
        &self,
        names: &[String],
        registry: &ConnectionRegistry,
    ) -> Result<Vec<String>, A2aError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let state = self.state()?;
        let mut issues = Vec::new();
        for name in names {
            let Some(declaration) = registry.external_agents.get(name) else {
                continue;
            };
            let status = view(name, declaration, state.agents.get(name).cloned()).readiness;
            if status != ExternalAgentReadiness::Ready {
                issues.push(format!(
                    "external agent {name:?} is {}; run `xana external-agent refresh {name}` then `xana external-agent trust {name} --yes`",
                    status.as_str()
                ));
            }
        }
        Ok(issues)
    }

    pub(crate) fn record_task(
        &self,
        connection: &str,
        agent_identity_digest: &str,
        task_id: &str,
        context_id: Option<&str>,
        state: &str,
        remote_cancel: &str,
    ) -> Result<ExternalAgentTaskRecord, A2aError> {
        for value in [
            connection,
            agent_identity_digest,
            task_id,
            state,
            remote_cancel,
        ] {
            if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                return Err(A2aError::InvalidTaskState);
            }
        }
        if context_id.is_some_and(|value| {
            value.is_empty() || value.len() > 512 || value.chars().any(char::is_control)
        }) {
            return Err(A2aError::InvalidTaskState);
        }
        let key = task_key(connection, task_id);
        let mut record = ExternalAgentTaskRecord {
            key: key.clone(),
            connection: connection.to_owned(),
            agent_identity_digest: agent_identity_digest.to_owned(),
            task_id: task_id.to_owned(),
            context_id: context_id.map(str::to_owned),
            state: state.to_owned(),
            remote_cancel: remote_cancel.to_owned(),
            updated_unix_ms: now_unix_ms()?,
        };
        self.update(|document| {
            if record.context_id.is_none()
                && let Some(existing) = document.tasks.get(&key)
            {
                record.context_id = existing.context_id.clone();
            }
            if !document.tasks.contains_key(&key) && document.tasks.len() >= MAX_RETAINED_TASKS {
                let oldest = document
                    .tasks
                    .iter()
                    .min_by_key(|(_, task)| task.updated_unix_ms)
                    .map(|(key, _)| key.clone())
                    .ok_or(A2aError::TaskStateLimit)?;
                document.tasks.remove(&oldest);
            }
            document.tasks.insert(key, record.clone());
            Ok(record.clone())
        })
    }

    pub(crate) fn task(
        &self,
        connection: &str,
        task_id: &str,
    ) -> Result<Option<ExternalAgentTaskRecord>, A2aError> {
        Ok(self
            .state()?
            .tasks
            .get(&task_key(connection, task_id))
            .cloned())
    }

    pub(crate) fn tasks_for(
        &self,
        connection: &str,
    ) -> Result<Vec<ExternalAgentTaskRecord>, A2aError> {
        let mut tasks = self
            .state()?
            .tasks
            .into_values()
            .filter(|task| task.connection == connection)
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_unix_ms));
        Ok(tasks)
    }

    fn state(&self) -> Result<ExternalAgentStateDocument, A2aError> {
        ensure_interoperable_records(&self.paths)?;
        read_document(&self.paths.external_agent_state_file()).map_err(A2aError::State)
    }

    fn update<T>(
        &self,
        operation: impl FnOnce(&mut ExternalAgentStateDocument) -> Result<T, A2aError>,
    ) -> Result<T, A2aError> {
        ensure_interoperable_records(&self.paths)?;
        match update_document(&self.paths.external_agent_state_file(), operation) {
            Ok(value) => Ok(value),
            Err(UpdateDocumentError::State(error)) => Err(A2aError::State(error)),
            Err(UpdateDocumentError::Update(error)) => Err(error),
        }
    }
}

fn task_key(connection: &str, task_id: &str) -> String {
    blake3::hash(format!("{connection}\0{task_id}").as_bytes())
        .to_hex()
        .to_string()
}

fn view(
    name: &str,
    declaration: &ExternalAgentDeclaration,
    cached: Option<ExternalAgentStateRecord>,
) -> ExternalAgentView {
    let readiness = if !declaration.enabled {
        ExternalAgentReadiness::Disabled
    } else if let Some(record) = &cached {
        if record.configured_endpoint == declaration.endpoint
            && record.trusted_identity_digest.as_deref() == Some(&record.identity_digest)
        {
            ExternalAgentReadiness::Ready
        } else {
            ExternalAgentReadiness::ReviewRequired
        }
    } else {
        ExternalAgentReadiness::NotDiscovered
    };
    ExternalAgentView {
        name: name.to_owned(),
        endpoint: declaration.endpoint.clone(),
        enabled: declaration.enabled,
        readiness,
        cached,
    }
}

async fn fetch_card(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    credential: Option<&SecretString>,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, A2aError> {
    let mut request = client
        .get(endpoint)
        .header(header::ACCEPT, "application/json");
    if let Some(credential) = credential {
        request = request.bearer_auth(credential.expose());
    }
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(A2aError::Cancelled),
        response = request.send() => response.map_err(|_| A2aError::Network)?,
    };
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(A2aError::Unauthorized),
        status if status.is_redirection() => return Err(A2aError::RedirectRejected),
        status if !status.is_success() => return Err(A2aError::Http(status.as_u16())),
        _ => {}
    }
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        != Some("application/json")
    {
        return Err(A2aError::UnsupportedMediaType);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CARD_BYTES as u64)
    {
        return Err(A2aError::CardTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(A2aError::Cancelled),
            next = stream.next() => next,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|_| A2aError::Network)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_CARD_BYTES {
            return Err(A2aError::CardTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCard {
    name: String,
    description: String,
    supported_interfaces: Vec<RawInterface>,
    #[serde(default)]
    provider: Option<RawProvider>,
    version: String,
    capabilities: RawCapabilities,
    #[serde(default)]
    security_schemes: BTreeMap<String, Value>,
    #[serde(default)]
    security_requirements: Vec<Value>,
    default_input_modes: Vec<String>,
    default_output_modes: Vec<String>,
    skills: Vec<RawSkill>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInterface {
    url: String,
    protocol_binding: String,
    protocol_version: String,
    #[serde(default)]
    tenant: String,
}

#[derive(Deserialize)]
struct RawProvider {
    url: String,
    organization: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCapabilities {
    #[serde(default)]
    streaming: bool,
    #[serde(default)]
    push_notifications: bool,
    #[serde(default)]
    extended_agent_card: bool,
    #[serde(default)]
    extensions: Vec<RawExtension>,
}

#[derive(Deserialize)]
struct RawExtension {
    uri: String,
    #[serde(default)]
    required: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSkill {
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
    #[serde(default)]
    examples: Vec<String>,
    #[serde(default)]
    input_modes: Vec<String>,
    #[serde(default)]
    output_modes: Vec<String>,
}

fn normalize_card(
    connection: &str,
    configured_endpoint: &str,
    bytes: &[u8],
    security: McpHttpSecurity,
) -> Result<ExternalAgentStateRecord, A2aError> {
    if bytes.len() > MAX_CARD_BYTES {
        return Err(A2aError::CardTooLarge);
    }
    let raw: RawCard = serde_json::from_slice(bytes).map_err(|_| A2aError::MalformedCard)?;
    validate_text(&raw.name, MAX_SHORT_TEXT_BYTES)?;
    validate_text(&raw.description, MAX_TEXT_BYTES)?;
    validate_text(&raw.version, MAX_SHORT_TEXT_BYTES)?;
    validate_count(&raw.supported_interfaces)?;
    validate_count(&raw.default_input_modes)?;
    validate_count(&raw.default_output_modes)?;
    validate_count(&raw.skills)?;
    validate_count(&raw.security_requirements)?;
    if raw.capabilities.push_notifications
        || raw.capabilities.extended_agent_card
        || raw
            .capabilities
            .extensions
            .iter()
            .any(|extension| extension.required)
    {
        return Err(A2aError::UnsupportedCapability);
    }
    for extension in &raw.capabilities.extensions {
        validate_text(&extension.uri, 2048)?;
    }
    let interface = raw
        .supported_interfaces
        .iter()
        .find(|interface| {
            interface.protocol_binding.eq_ignore_ascii_case("JSONRPC")
                && interface.protocol_version == A2A_PROTOCOL_VERSION
                && interface.tenant.is_empty()
        })
        .ok_or(A2aError::IncompatibleProtocol)?;
    let interface_endpoint = McpHttpEndpoint::parse(&interface.url, security)
        .map_err(|_| A2aError::InvalidInterfaceEndpoint)?;
    let input_modes = normalize_modes(raw.default_input_modes)?;
    let output_modes = normalize_modes(raw.default_output_modes)?;
    if !input_modes.iter().any(|mode| mode == "text/plain")
        || !output_modes.iter().any(|mode| mode == "text/plain")
    {
        return Err(A2aError::UnsupportedContentMode);
    }
    let mut skills = Vec::with_capacity(raw.skills.len());
    let mut skill_ids = BTreeSet::new();
    for skill in raw.skills {
        validate_text(&skill.id, MAX_SHORT_TEXT_BYTES)?;
        validate_text(&skill.name, MAX_SHORT_TEXT_BYTES)?;
        validate_text(&skill.description, MAX_TEXT_BYTES)?;
        validate_count(&skill.tags)?;
        validate_count(&skill.examples)?;
        if !skill_ids.insert(skill.id.clone()) {
            return Err(A2aError::MalformedCard);
        }
        skills.push(ExternalAgentSkillRecord {
            id: clean(&skill.id, MAX_SHORT_TEXT_BYTES),
            name: clean(&skill.name, MAX_SHORT_TEXT_BYTES),
            description: clean(&skill.description, MAX_TEXT_BYTES),
            input_modes: if skill.input_modes.is_empty() {
                input_modes.clone()
            } else {
                normalize_modes(skill.input_modes)?
            },
            output_modes: if skill.output_modes.is_empty() {
                output_modes.clone()
            } else {
                normalize_modes(skill.output_modes)?
            },
        });
    }
    let security_schemes = raw.security_schemes.keys().cloned().collect::<Vec<_>>();
    if !raw.security_requirements.is_empty()
        && !raw.security_requirements.iter().any(|requirement| {
            requirement.as_object().is_some_and(|object| {
                object.keys().any(|name| {
                    raw.security_schemes
                        .get(name)
                        .is_some_and(is_supported_bearer_scheme)
                })
            })
        })
    {
        return Err(A2aError::UnsupportedAuthentication);
    }
    let owner = raw.provider.map(|provider| {
        clean(
            &format!("{} ({})", provider.organization, provider.url),
            MAX_SHORT_TEXT_BYTES,
        )
    });
    let identity_material = serde_json::to_vec(&json!({
        "cardVersion":"1.0.0",
        "name":raw.name,
        "owner":owner,
        "agentVersion":raw.version,
        "interface":{
            "url":interface_endpoint.canonical(),
            "binding":"JSONRPC",
            "version":A2A_PROTOCOL_VERSION,
        },
        "streaming":raw.capabilities.streaming,
        "inputModes":input_modes,
        "outputModes":output_modes,
        "securitySchemes":raw.security_schemes,
        "securityRequirements":raw.security_requirements,
        "skills":skills,
    }))
    .map_err(|_| A2aError::MalformedCard)?;
    let recipient = RecipientIdentity::new(
        RecipientKind::ExternalAgent,
        connection,
        configured_endpoint,
        &identity_material,
    )
    .map_err(|_| A2aError::InvalidIdentity)?;
    Ok(ExternalAgentStateRecord {
        connection: connection.to_owned(),
        configured_endpoint: configured_endpoint.to_owned(),
        identity_digest: recipient.identity_digest,
        agent_name: clean(&raw.name, MAX_SHORT_TEXT_BYTES),
        description: clean(&raw.description, MAX_TEXT_BYTES),
        owner,
        agent_version: clean(&raw.version, MAX_SHORT_TEXT_BYTES),
        interface_url: interface_endpoint.canonical().to_owned(),
        protocol_binding: "JSONRPC".to_owned(),
        protocol_version: A2A_PROTOCOL_VERSION.to_owned(),
        streaming: raw.capabilities.streaming,
        input_modes,
        output_modes,
        security_schemes,
        skills,
        refreshed_unix_ms: now_unix_ms()?,
        trusted_identity_digest: None,
        trusted_unix_ms: None,
    })
}

fn is_supported_bearer_scheme(value: &Value) -> bool {
    value
        .get("httpAuthSecurityScheme")
        .and_then(|value| value.get("scheme"))
        .and_then(Value::as_str)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
}

fn normalize_modes(values: Vec<String>) -> Result<Vec<String>, A2aError> {
    validate_count(&values)?;
    let mut normalized = values
        .into_iter()
        .map(|value| {
            validate_text(&value, MAX_SHORT_TEXT_BYTES)?;
            Ok(value.to_ascii_lowercase())
        })
        .collect::<Result<Vec<_>, A2aError>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn validate_count<T>(values: &[T]) -> Result<(), A2aError> {
    if values.len() > MAX_COLLECTION_ITEMS {
        Err(A2aError::CardCollectionTooLarge)
    } else {
        Ok(())
    }
}

fn validate_text(value: &str, limit: usize) -> Result<(), A2aError> {
    if value.is_empty() || value.len() > limit {
        Err(A2aError::InvalidCardField)
    } else {
        Ok(())
    }
}

fn clean(value: &str, limit: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                '�'
            } else {
                character
            }
        })
        .collect::<String>()
        .chars()
        .take(limit)
        .collect()
}

fn now_unix_ms() -> Result<u64, A2aError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| A2aError::Clock)?
        .as_millis();
    u64::try_from(millis).map_err(|_| A2aError::Clock)
}

#[derive(Debug)]
pub(crate) enum A2aError {
    State(PrivateStateError),
    InvalidEndpoint,
    InvalidInterfaceEndpoint,
    Network,
    Credential,
    Unauthorized,
    RedirectRejected,
    Http(u16),
    UnsupportedMediaType,
    CardTooLarge,
    CardCollectionTooLarge,
    MalformedCard,
    InvalidCardField,
    UnsupportedCapability,
    IncompatibleProtocol,
    UnsupportedContentMode,
    UnsupportedAuthentication,
    InvalidIdentity,
    Cancelled,
    Clock,
    NotDiscovered(String),
    NotTrusted(String),
    InvalidTaskState,
    TaskStateLimit,
}

impl fmt::Display for A2aError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => write!(formatter, "external-agent state failed: {error}"),
            Self::InvalidEndpoint => {
                formatter.write_str("external-agent Card endpoint is invalid or unsafe")
            }
            Self::InvalidInterfaceEndpoint => {
                formatter.write_str("Agent Card interface endpoint is invalid or unsafe")
            }
            Self::Network => formatter.write_str("external-agent network request failed"),
            Self::Credential => formatter.write_str("external-agent credential is unavailable"),
            Self::Unauthorized => {
                formatter.write_str("external-agent Card request was not authorized")
            }
            Self::RedirectRejected => {
                formatter.write_str("external-agent Card redirect was rejected")
            }
            Self::Http(status) => write!(
                formatter,
                "external-agent Card request returned HTTP {status}"
            ),
            Self::UnsupportedMediaType => {
                formatter.write_str("external-agent Card must be application/json")
            }
            Self::CardTooLarge => write!(formatter, "Agent Card exceeds {MAX_CARD_BYTES} bytes"),
            Self::CardCollectionTooLarge => {
                formatter.write_str("Agent Card collection exceeds its item bound")
            }
            Self::MalformedCard => formatter.write_str("Agent Card is malformed"),
            Self::InvalidCardField => {
                formatter.write_str("Agent Card contains an invalid or oversized field")
            }
            Self::UnsupportedCapability => {
                formatter.write_str("Agent Card requires an unsupported capability")
            }
            Self::IncompatibleProtocol => {
                formatter.write_str("Agent Card has no compatible JSONRPC A2A 1.0 interface")
            }
            Self::UnsupportedContentMode => {
                formatter.write_str("Agent Card does not support text/plain input and output")
            }
            Self::UnsupportedAuthentication => {
                formatter.write_str("Agent Card requires an unsupported authentication scheme")
            }
            Self::InvalidIdentity => formatter.write_str("Agent Card identity is invalid"),
            Self::Cancelled => formatter.write_str("external-agent request was cancelled"),
            Self::Clock => {
                formatter.write_str("system clock cannot represent external-agent state")
            }
            Self::NotDiscovered(name) => write!(
                formatter,
                "external agent {name:?} has no cached Agent Card"
            ),
            Self::NotTrusted(name) => write!(
                formatter,
                "external agent {name:?} is not trusted for its current identity"
            ),
            Self::InvalidTaskState => {
                formatter.write_str("external-agent task state is invalid or oversized")
            }
            Self::TaskStateLimit => {
                formatter.write_str("external-agent task-state limit has been reached")
            }
        }
    }
}

impl Error for A2aError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            _ => None,
        }
    }
}

impl From<PrivateStateError> for A2aError {
    fn from(value: PrivateStateError) -> Self {
        Self::State(value)
    }
}

#[cfg(test)]
mod tests;
