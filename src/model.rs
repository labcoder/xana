//! Connection-owned model catalogs and explicit selection state.
//!
//! Catalog refresh is a control-plane operation. Startup reads configured and
//! cached non-secret metadata only; it never performs discovery implicitly.

use crate::{
    bounded_file,
    config::{ConnectionConfig, ConnectionRegistry, ModelOverride, ProviderKind},
    credential::{CredentialResolver, SecretString},
};
use futures::StreamExt;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr,
};

const CATALOG_VERSION: u32 = 1;
const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_SELECTION_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionKind {
    Native,
    Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DescriptorSource {
    Configured,
    Remote,
    ManagedRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelDescriptor {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) input_modalities: BTreeSet<String>,
    pub(crate) tools: Option<bool>,
    pub(crate) reasoning: Option<bool>,
    #[serde(default)]
    pub(crate) reasoning_efforts: Vec<ReasoningEffort>,
    #[serde(default)]
    pub(crate) default_reasoning_effort: Option<String>,
    pub(crate) context_tokens: Option<usize>,
    pub(crate) max_output_tokens: Option<usize>,
    pub(crate) source: DescriptorSource,
    #[serde(default)]
    pub(crate) is_default: bool,
}

impl ModelDescriptor {
    fn configured(id: String, value: &ModelOverride) -> Self {
        Self {
            display_name: id.clone(),
            id,
            input_modalities: if value.input_modalities.is_empty() {
                ["text".to_owned()].into_iter().collect()
            } else {
                value.input_modalities.iter().cloned().collect()
            },
            tools: value.tools,
            reasoning: value.reasoning,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            context_tokens: value.context_tokens,
            max_output_tokens: value.max_output_tokens,
            source: DescriptorSource::Configured,
            is_default: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReasoningEffort {
    pub(crate) id: String,
    pub(crate) description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningSummary {
    Auto,
    Concise,
    Detailed,
    Off,
}

impl ReasoningSummary {
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Concise => "concise",
            Self::Detailed => "detailed",
            Self::Off => "none",
        }
    }
}

impl fmt::Display for ReasoningSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Concise => "concise",
            Self::Detailed => "detailed",
            Self::Off => "off",
        })
    }
}

impl FromStr for ReasoningSummary {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "concise" => Ok(Self::Concise),
            "detailed" => Ok(Self::Detailed),
            "off" | "none" => Ok(Self::Off),
            _ => Err(ModelError::InvalidOption(format!(
                "unknown reasoning summary {value:?}; expected auto, concise, detailed, or off"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionSummary {
    pub(crate) id: String,
    pub(crate) kind: ProviderKind,
    pub(crate) execution: ExecutionKind,
    pub(crate) credential: &'static str,
    pub(crate) models: Vec<ModelDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelSelection {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) reasoning_summary: Option<ReasoningSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionDocument {
    version: u32,
    connection: String,
    model: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    reasoning_summary: Option<ReasoningSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogDocument {
    version: u32,
    connection: String,
    models: Vec<ModelDescriptor>,
}

#[derive(Debug)]
pub(crate) enum ModelError {
    UnknownConnection(String),
    UnknownModel {
        connection: String,
        model: String,
    },
    MissingCredential(String),
    InvalidEndpoint(String),
    Transport(String),
    Rejected(String),
    Decode(String),
    InvalidOption(String),
    StateTooLarge {
        kind: &'static str,
        actual: u64,
        limit: usize,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownConnection(id) => write!(f, "unknown connection {id:?}"),
            Self::UnknownModel { connection, model } => {
                write!(f, "connection {connection:?} has no model {model:?}")
            }
            Self::MissingCredential(reason) => write!(f, "credential unavailable: {reason}"),
            Self::InvalidEndpoint(reason) => write!(f, "invalid model catalog endpoint: {reason}"),
            Self::Transport(reason) => write!(f, "could not reach model catalog: {reason}"),
            Self::Rejected(reason) => write!(f, "model catalog rejected the request: {reason}"),
            Self::Decode(reason) => write!(f, "invalid model catalog response: {reason}"),
            Self::InvalidOption(reason) => write!(f, "invalid model option: {reason}"),
            Self::StateTooLarge {
                kind,
                actual,
                limit,
            } => write!(
                f,
                "{kind} contains {actual} bytes, exceeding the {limit}-byte limit"
            ),
            Self::Io { path, source } => write!(f, "could not access {}: {source}", path.display()),
        }
    }
}

impl Error for ModelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) struct ModelManager {
    registry: ConnectionRegistry,
    cache_root: PathBuf,
    selection_path: PathBuf,
    client: Client,
    credentials: CredentialResolver,
}

impl ModelManager {
    pub(crate) fn new(
        registry: ConnectionRegistry,
        cache_root: PathBuf,
        selection_path: PathBuf,
    ) -> Self {
        Self {
            registry,
            cache_root,
            selection_path,
            client: Client::new(),
            credentials: CredentialResolver::default(),
        }
    }

    pub(crate) fn selected(&self) -> Result<ModelSelection, ModelError> {
        match bounded_file::read_to_string(&self.selection_path, MAX_SELECTION_BYTES) {
            Ok(input) => {
                let document: SelectionDocument = toml::from_str(&input)
                    .map_err(|error| ModelError::Decode(error.to_string()))?;
                if !matches!(document.version, 1 | 2) {
                    return Err(ModelError::Decode(format!(
                        "unsupported selection version {}",
                        document.version
                    )));
                }
                let selection = ModelSelection {
                    connection: document.connection,
                    model: document.model,
                    reasoning_effort: document.reasoning_effort,
                    reasoning_summary: document.reasoning_summary,
                };
                self.normalize_and_validate_selection(selection)
            }
            Err(bounded_file::BoundedReadError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                let profile = self
                    .registry
                    .profiles
                    .get(&self.registry.default_profile)
                    .expect("configuration validation requires the default profile");
                self.normalize_and_validate_selection(ModelSelection {
                    connection: profile.connection.clone(),
                    model: profile.model.clone(),
                    reasoning_effort: None,
                    reasoning_summary: None,
                })
            }
            Err(bounded_file::BoundedReadError::Io { path, source }) => {
                Err(ModelError::Io { path, source })
            }
            Err(bounded_file::BoundedReadError::TooLarge { actual, limit, .. }) => {
                Err(ModelError::StateTooLarge {
                    kind: "model selection",
                    actual,
                    limit,
                })
            }
        }
    }

    pub(crate) fn select(
        &self,
        connection: &str,
        model: &str,
    ) -> Result<ModelSelection, ModelError> {
        self.select_with_options(connection, model, None, None)
    }

    pub(crate) fn select_with_options(
        &self,
        connection: &str,
        model: &str,
        reasoning_effort: Option<String>,
        reasoning_summary: Option<ReasoningSummary>,
    ) -> Result<ModelSelection, ModelError> {
        let selection = self.normalize_and_validate_selection(ModelSelection {
            connection: connection.to_owned(),
            model: model.to_owned(),
            reasoning_effort,
            reasoning_summary,
        })?;
        self.write_selection(&selection)?;
        Ok(selection)
    }

    pub(crate) fn update_reasoning_effort(
        &self,
        effort: Option<String>,
    ) -> Result<ModelSelection, ModelError> {
        let mut selection = self.selected()?;
        selection.reasoning_effort = effort.or_else(|| {
            self.descriptor(&selection.connection, &selection.model)
                .ok()
                .and_then(|descriptor| descriptor.default_reasoning_effort)
        });
        let selection = self.normalize_and_validate_selection(selection)?;
        self.write_selection(&selection)?;
        Ok(selection)
    }

    pub(crate) fn update_reasoning_summary(
        &self,
        summary: ReasoningSummary,
    ) -> Result<ModelSelection, ModelError> {
        let mut selection = self.selected()?;
        selection.reasoning_summary = Some(summary);
        let selection = self.normalize_and_validate_selection(selection)?;
        self.write_selection(&selection)?;
        Ok(selection)
    }

    fn write_selection(&self, selection: &ModelSelection) -> Result<(), ModelError> {
        let document = SelectionDocument {
            version: 2,
            connection: selection.connection.clone(),
            model: selection.model.clone(),
            reasoning_effort: selection.reasoning_effort.clone(),
            reasoning_summary: selection.reasoning_summary,
        };
        let rendered = toml::to_string_pretty(&document)
            .map_err(|error| ModelError::Decode(error.to_string()))?;
        atomic_write(&self.selection_path, rendered.as_bytes())?;
        Ok(())
    }

    pub(crate) fn connection(&self, id: &str) -> Result<&ConnectionConfig, ModelError> {
        self.registry
            .connections
            .get(id)
            .ok_or_else(|| ModelError::UnknownConnection(id.to_owned()))
    }

    pub(crate) fn summaries(&self) -> Vec<ConnectionSummary> {
        self.registry
            .connections
            .values()
            .map(|connection| ConnectionSummary {
                id: connection.id.clone(),
                kind: connection.kind,
                execution: if connection.kind == ProviderKind::Codex {
                    ExecutionKind::Managed
                } else {
                    ExecutionKind::Native
                },
                credential: self.credential_status(connection),
                models: self.models_for(connection),
            })
            .collect()
    }

    pub(crate) fn models_for(&self, connection: &ConnectionConfig) -> Vec<ModelDescriptor> {
        let mut models = BTreeMap::<String, ModelDescriptor>::new();
        for (id, descriptor) in &connection.models {
            models.insert(
                id.clone(),
                ModelDescriptor::configured(id.clone(), descriptor),
            );
        }
        if let Ok(cached) = self.read_cache(&connection.id) {
            for descriptor in cached {
                models
                    .entry(descriptor.id.clone())
                    .and_modify(|configured| merge_remote(configured, &descriptor))
                    .or_insert(descriptor);
            }
        }
        models.into_values().collect()
    }

    pub(crate) fn descriptor(
        &self,
        connection: &str,
        model: &str,
    ) -> Result<ModelDescriptor, ModelError> {
        self.models_for(self.connection(connection)?)
            .into_iter()
            .find(|descriptor| descriptor.id == model)
            .ok_or_else(|| ModelError::UnknownModel {
                connection: connection.to_owned(),
                model: model.to_owned(),
            })
    }

    pub(crate) async fn refresh_native(
        &self,
        id: &str,
    ) -> Result<Vec<ModelDescriptor>, ModelError> {
        let connection = self.connection(id)?;
        if connection.kind == ProviderKind::Codex {
            return Err(ModelError::InvalidEndpoint(
                "managed runtimes refresh their own catalog".into(),
            ));
        }
        let endpoint = catalog_endpoint(connection)?;
        let mut request = self.client.get(endpoint);
        if let Some(reference) = &connection.credential {
            let secret = self
                .credentials
                .resolve(reference)
                .map_err(|error| ModelError::MissingCredential(error.to_string()))?;
            request = apply_catalog_auth(request, connection.kind, &secret);
        }
        if connection.kind == ProviderKind::Anthropic {
            request = request.header("anthropic-version", "2023-06-01");
        }
        let response = request
            .send()
            .await
            .map_err(|error| ModelError::Transport(error.to_string()))?;
        if !response.status().is_success() {
            return Err(ModelError::Rejected(response.status().to_string()));
        }
        let bytes = bounded_response_bytes(response).await?;
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| ModelError::Decode(error.to_string()))?;
        let models = parse_catalog(connection.kind, &value)?;
        self.write_cache(id, &models)?;
        Ok(models)
    }

    pub(crate) fn write_managed_cache(
        &self,
        id: &str,
        models: &[ModelDescriptor],
    ) -> Result<(), ModelError> {
        self.connection(id)?;
        self.write_cache(id, models)
    }

    fn credential_status(&self, connection: &ConnectionConfig) -> &'static str {
        if connection.kind == ProviderKind::Codex {
            return "managed externally";
        }
        match &connection.credential {
            None => "not required",
            Some(reference) => match self.credentials.status(reference) {
                Ok(crate::credential::CredentialAvailability::Available) => "available",
                Ok(crate::credential::CredentialAvailability::Missing) => "missing",
                Err(_) => "unavailable",
            },
        }
    }

    fn validate_selection(&self, connection: &str, model: &str) -> Result<(), ModelError> {
        let connection_config = self.connection(connection)?;
        if self
            .models_for(connection_config)
            .iter()
            .any(|candidate| candidate.id == model)
        {
            Ok(())
        } else {
            Err(ModelError::UnknownModel {
                connection: connection.to_owned(),
                model: model.to_owned(),
            })
        }
    }

    fn normalize_and_validate_selection(
        &self,
        mut selection: ModelSelection,
    ) -> Result<ModelSelection, ModelError> {
        self.validate_selection(&selection.connection, &selection.model)?;
        let connection = self.connection(&selection.connection)?;
        let descriptor = self.descriptor(&selection.connection, &selection.model)?;

        if connection.kind != ProviderKind::Codex
            && (selection.reasoning_effort.is_some() || selection.reasoning_summary.is_some())
        {
            return Err(ModelError::InvalidOption(
                "reasoning options are currently implemented only for managed Codex models".into(),
            ));
        }

        if connection.kind == ProviderKind::Codex {
            if let Some(effort) = &selection.reasoning_effort
                && (effort.is_empty() || effort.len() > 64)
            {
                return Err(ModelError::InvalidOption(
                    "reasoning effort must contain 1 to 64 bytes".into(),
                ));
            }
            if selection.reasoning_effort.is_none() {
                selection.reasoning_effort = descriptor.default_reasoning_effort.clone();
            }
            if selection.reasoning_summary.is_none() && descriptor.reasoning == Some(true) {
                selection.reasoning_summary = Some(ReasoningSummary::Auto);
            }
            if let Some(effort) = &selection.reasoning_effort
                && !descriptor.reasoning_efforts.is_empty()
                && !descriptor
                    .reasoning_efforts
                    .iter()
                    .any(|candidate| candidate.id == *effort)
            {
                let available = descriptor
                    .reasoning_efforts
                    .iter()
                    .map(|candidate| candidate.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ModelError::InvalidOption(format!(
                    "model {:?} does not advertise effort {effort:?}; available: {}",
                    descriptor.id,
                    if available.is_empty() {
                        "none (refresh the Codex catalog)"
                    } else {
                        &available
                    }
                )));
            }
            if selection
                .reasoning_summary
                .is_some_and(|summary| summary != ReasoningSummary::Off)
                && descriptor.reasoning == Some(false)
            {
                return Err(ModelError::InvalidOption(format!(
                    "model {:?} does not advertise reasoning summaries",
                    descriptor.id
                )));
            }
        }
        Ok(selection)
    }

    fn cache_path(&self, id: &str) -> PathBuf {
        self.cache_root.join("models").join(format!("{id}.json"))
    }

    fn read_cache(&self, id: &str) -> Result<Vec<ModelDescriptor>, ModelError> {
        let path = self.cache_path(id);
        let input =
            bounded_file::read_to_string(&path, MAX_CATALOG_BYTES).map_err(
                |error| match error {
                    bounded_file::BoundedReadError::TooLarge { actual, limit, .. } => {
                        ModelError::StateTooLarge {
                            kind: "catalog cache",
                            actual,
                            limit,
                        }
                    }
                    bounded_file::BoundedReadError::Io { path, source } => {
                        ModelError::Io { path, source }
                    }
                },
            )?;
        let document: CatalogDocument =
            serde_json::from_str(&input).map_err(|error| ModelError::Decode(error.to_string()))?;
        if document.version != CATALOG_VERSION || document.connection != id {
            return Err(ModelError::Decode("catalog cache identity mismatch".into()));
        }
        Ok(document.models)
    }

    fn write_cache(&self, id: &str, models: &[ModelDescriptor]) -> Result<(), ModelError> {
        let document = CatalogDocument {
            version: CATALOG_VERSION,
            connection: id.to_owned(),
            models: models.to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&document)
            .map_err(|error| ModelError::Decode(error.to_string()))?;
        atomic_write(&self.cache_path(id), &encoded)
    }
}

async fn bounded_response_bytes(response: reqwest::Response) -> Result<Vec<u8>, ModelError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
    {
        return Err(ModelError::Decode(format!(
            "catalog response exceeds the {MAX_CATALOG_BYTES}-byte limit"
        )));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ModelError::Transport(error.to_string()))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
            return Err(ModelError::Decode(format!(
                "catalog response exceeds the {MAX_CATALOG_BYTES}-byte limit"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ModelError> {
    let parent = path.parent().ok_or_else(|| ModelError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(|source| ModelError::Io {
        path: parent.to_owned(),
        source,
    })?;
    let mut file =
        atomic_write_file::AtomicWriteFile::open(path).map_err(|source| ModelError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| ModelError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.commit().map_err(|source| ModelError::Io {
        path: path.to_owned(),
        source,
    })
}

fn merge_remote(configured: &mut ModelDescriptor, remote: &ModelDescriptor) {
    if configured.input_modalities == ["text".to_owned()].into_iter().collect()
        && !remote.input_modalities.is_empty()
    {
        configured.input_modalities = remote.input_modalities.clone();
    }
    configured.tools = configured.tools.or(remote.tools);
    configured.reasoning = configured.reasoning.or(remote.reasoning);
    if configured.reasoning != Some(false) && configured.reasoning_efforts.is_empty() {
        configured.reasoning_efforts = remote.reasoning_efforts.clone();
    }
    if configured.reasoning != Some(false) {
        configured.default_reasoning_effort = configured
            .default_reasoning_effort
            .clone()
            .or_else(|| remote.default_reasoning_effort.clone());
    }
    configured.context_tokens = configured.context_tokens.or(remote.context_tokens);
    configured.max_output_tokens = configured.max_output_tokens.or(remote.max_output_tokens);
    configured.display_name = remote.display_name.clone();
    configured.is_default = remote.is_default;
}

fn catalog_endpoint(connection: &ConnectionConfig) -> Result<Url, ModelError> {
    let base = connection
        .base_url
        .as_deref()
        .ok_or_else(|| ModelError::InvalidEndpoint("connection has no base URL".into()))?;
    let endpoint = match connection.kind {
        ProviderKind::Ollama => format!(
            "{}/api/tags",
            base.trim_end_matches("/v1").trim_end_matches('/')
        ),
        ProviderKind::OpenRouter => format!("{}/models/user", base.trim_end_matches('/')),
        ProviderKind::OpenAi | ProviderKind::OpenAiCompat => {
            format!("{}/models", base.trim_end_matches('/'))
        }
        ProviderKind::Anthropic => format!("{}/v1/models", base.trim_end_matches('/')),
        ProviderKind::Codex => {
            return Err(ModelError::InvalidEndpoint("Codex uses model/list".into()));
        }
    };
    Url::parse(&endpoint).map_err(|error| ModelError::InvalidEndpoint(error.to_string()))
}

fn apply_catalog_auth(
    request: reqwest::RequestBuilder,
    kind: ProviderKind,
    secret: &SecretString,
) -> reqwest::RequestBuilder {
    match kind {
        ProviderKind::Anthropic => request.header("x-api-key", secret.expose()),
        _ => request.bearer_auth(secret.expose()),
    }
}

fn parse_catalog(kind: ProviderKind, value: &Value) -> Result<Vec<ModelDescriptor>, ModelError> {
    let values = match kind {
        ProviderKind::Ollama => value.get("models"),
        _ => value.get("data"),
    }
    .and_then(Value::as_array)
    .ok_or_else(|| ModelError::Decode("catalog does not contain a model array".into()))?;

    let mut models = Vec::new();
    for value in values {
        let id = match kind {
            ProviderKind::Ollama => value.get("name").or_else(|| value.get("model")),
            _ => value.get("id"),
        }
        .and_then(Value::as_str)
        .ok_or_else(|| ModelError::Decode("catalog model is missing an id".into()))?
        .to_owned();
        let architecture_modalities = value
            .pointer("/architecture/input_modalities")
            .or_else(|| value.get("input_modalities"))
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| matches!(*value, "text" | "image"))
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_else(|| ["text".to_owned()].into_iter().collect());
        let parameters = value
            .get("supported_parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        models.push(ModelDescriptor {
            display_name: value
                .get("display_name")
                .or_else(|| value.get("displayName"))
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_owned(),
            id,
            input_modalities: architecture_modalities,
            tools: parameters.contains("tools").then_some(true),
            reasoning: parameters
                .iter()
                .any(|parameter| parameter.contains("reason"))
                .then_some(true),
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            context_tokens: value
                .get("context_length")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            max_output_tokens: value
                .get("max_output_tokens")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            source: DescriptorSource::Remote,
            is_default: value
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectionConfig, CredentialReference};
    use tempfile::tempdir;

    fn registry() -> ConnectionRegistry {
        ConnectionRegistry {
            default_profile: "default".into(),
            connections: BTreeMap::from([(
                "local".into(),
                ConnectionConfig {
                    id: "local".into(),
                    kind: ProviderKind::Ollama,
                    base_url: Some("http://localhost:11434/v1".into()),
                    credential: None,
                    models: BTreeMap::from([("qwen".into(), ModelOverride::default())]),
                    codex_program: None,
                    codex_home: None,
                },
            )]),
            profiles: BTreeMap::from([(
                "default".into(),
                crate::config::ProfileConfig {
                    id: "default".into(),
                    connection: "local".into(),
                    model: "qwen".into(),
                    max_tool_rounds: 8,
                },
            )]),
        }
    }

    fn codex_registry() -> ConnectionRegistry {
        ConnectionRegistry {
            default_profile: "default".into(),
            connections: BTreeMap::from([(
                "codex".into(),
                ConnectionConfig {
                    id: "codex".into(),
                    kind: ProviderKind::Codex,
                    base_url: None,
                    credential: None,
                    models: BTreeMap::from([(
                        "gpt-5.6-sol".into(),
                        ModelOverride {
                            reasoning: Some(true),
                            ..ModelOverride::default()
                        },
                    )]),
                    codex_program: Some("codex".into()),
                    codex_home: None,
                },
            )]),
            profiles: BTreeMap::from([(
                "default".into(),
                crate::config::ProfileConfig {
                    id: "default".into(),
                    connection: "codex".into(),
                    model: "gpt-5.6-sol".into(),
                    max_tool_rounds: 8,
                },
            )]),
        }
    }

    fn codex_descriptor() -> ModelDescriptor {
        ModelDescriptor {
            id: "gpt-5.6-sol".into(),
            display_name: "GPT-5.6-Sol".into(),
            input_modalities: ["text".into(), "image".into()].into_iter().collect(),
            tools: Some(true),
            reasoning: Some(true),
            reasoning_efforts: vec![
                ReasoningEffort {
                    id: "low".into(),
                    description: "Fast".into(),
                },
                ReasoningEffort {
                    id: "xhigh".into(),
                    description: "Deep".into(),
                },
            ],
            default_reasoning_effort: Some("low".into()),
            context_tokens: None,
            max_output_tokens: None,
            source: DescriptorSource::ManagedRuntime,
            is_default: true,
        }
    }

    #[test]
    fn selection_defaults_to_profile_then_persists_separately() {
        let directory = tempdir().unwrap();
        let manager = ModelManager::new(
            registry(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );
        assert_eq!(manager.selected().unwrap().model, "qwen");
        manager.select("local", "qwen").unwrap();
        assert!(directory.path().join("selection.toml").is_file());
    }

    #[test]
    fn oversized_selection_is_rejected_before_toml_decoding() {
        let directory = tempdir().unwrap();
        let selection_path = directory.path().join("selection.toml");
        fs::write(&selection_path, vec![b'x'; MAX_SELECTION_BYTES + 1]).unwrap();
        let manager = ModelManager::new(registry(), directory.path().join("cache"), selection_path);

        assert!(matches!(
            manager.selected(),
            Err(ModelError::StateTooLarge {
                kind: "model selection",
                ..
            })
        ));
    }

    #[test]
    fn managed_selection_persists_validated_model_options_and_reads_legacy_v1() {
        let directory = tempdir().unwrap();
        let selection_path = directory.path().join("selection.toml");
        let manager = ModelManager::new(
            codex_registry(),
            directory.path().join("cache"),
            selection_path.clone(),
        );
        manager
            .write_managed_cache("codex", &[codex_descriptor()])
            .unwrap();
        let selected = manager
            .select_with_options(
                "codex",
                "gpt-5.6-sol",
                Some("xhigh".into()),
                Some(ReasoningSummary::Detailed),
            )
            .unwrap();
        assert_eq!(selected.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(selected.reasoning_summary, Some(ReasoningSummary::Detailed));
        let persisted = fs::read_to_string(&selection_path).unwrap();
        assert!(persisted.contains("version = 2"));
        assert!(persisted.contains("reasoning_effort = \"xhigh\""));

        fs::write(
            &selection_path,
            "version = 1\nconnection = \"codex\"\nmodel = \"gpt-5.6-sol\"\n",
        )
        .unwrap();
        let legacy = manager.selected().unwrap();
        assert_eq!(legacy.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(legacy.reasoning_summary, Some(ReasoningSummary::Auto));
    }

    #[test]
    fn managed_selection_rejects_an_effort_not_advertised_by_the_model() {
        let directory = tempdir().unwrap();
        let manager = ModelManager::new(
            codex_registry(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );
        manager
            .write_managed_cache("codex", &[codex_descriptor()])
            .unwrap();
        assert!(matches!(
            manager.select_with_options("codex", "gpt-5.6-sol", Some("ultra".into()), None,),
            Err(ModelError::InvalidOption(_))
        ));
    }

    #[test]
    fn openrouter_catalog_preserves_capability_evidence() {
        let value = serde_json::json!({"data":[{
            "id":"openai/example",
            "architecture":{"input_modalities":["text","image"]},
            "supported_parameters":["tools","reasoning"],
            "context_length":1234
        }]});
        let models = parse_catalog(ProviderKind::OpenRouter, &value).unwrap();
        assert!(models[0].input_modalities.contains("image"));
        assert_eq!(models[0].tools, Some(true));
        assert_eq!(models[0].context_tokens, Some(1234));
    }

    #[test]
    fn configuration_types_do_not_serialize_secret_material() {
        let reference = CredentialReference::Stored {
            id: "remote".into(),
        };
        let encoded = toml::to_string(&reference).unwrap();
        assert!(!encoded.contains("token"));
        assert!(encoded.contains("remote"));
    }
}
