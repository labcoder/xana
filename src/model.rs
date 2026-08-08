//! Connection-owned model catalogs and explicit selection state.
//!
//! Catalog refresh is a control-plane operation. Startup reads configured and
//! cached non-secret metadata only; it never performs discovery implicitly.

use crate::{
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
};

const CATALOG_VERSION: u32 = 1;
const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;

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
            context_tokens: value.context_tokens,
            max_output_tokens: value.max_output_tokens,
            source: DescriptorSource::Configured,
            is_default: false,
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionDocument {
    version: u32,
    connection: String,
    model: String,
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
    UnknownModel { connection: String, model: String },
    MissingCredential(String),
    InvalidEndpoint(String),
    Transport(String),
    Rejected(String),
    Decode(String),
    Io { path: PathBuf, source: io::Error },
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
        match fs::read_to_string(&self.selection_path) {
            Ok(input) => {
                let document: SelectionDocument = toml::from_str(&input)
                    .map_err(|error| ModelError::Decode(error.to_string()))?;
                if document.version != 1 {
                    return Err(ModelError::Decode(format!(
                        "unsupported selection version {}",
                        document.version
                    )));
                }
                self.validate_selection(&document.connection, &document.model)?;
                Ok(ModelSelection {
                    connection: document.connection,
                    model: document.model,
                })
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let profile = self
                    .registry
                    .profiles
                    .get(&self.registry.default_profile)
                    .expect("configuration validation requires the default profile");
                Ok(ModelSelection {
                    connection: profile.connection.clone(),
                    model: profile.model.clone(),
                })
            }
            Err(source) => Err(ModelError::Io {
                path: self.selection_path.clone(),
                source,
            }),
        }
    }

    pub(crate) fn select(
        &self,
        connection: &str,
        model: &str,
    ) -> Result<ModelSelection, ModelError> {
        self.validate_selection(connection, model)?;
        let document = SelectionDocument {
            version: 1,
            connection: connection.to_owned(),
            model: model.to_owned(),
        };
        let rendered = toml::to_string_pretty(&document)
            .map_err(|error| ModelError::Decode(error.to_string()))?;
        atomic_write(&self.selection_path, rendered.as_bytes())?;
        Ok(ModelSelection {
            connection: connection.to_owned(),
            model: model.to_owned(),
        })
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

    fn cache_path(&self, id: &str) -> PathBuf {
        self.cache_root.join("models").join(format!("{id}.json"))
    }

    fn read_cache(&self, id: &str) -> Result<Vec<ModelDescriptor>, ModelError> {
        let path = self.cache_path(id);
        let metadata = fs::metadata(&path).map_err(|source| ModelError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.len() > MAX_CATALOG_BYTES as u64 {
            return Err(ModelError::Decode(format!(
                "catalog cache exceeds the {MAX_CATALOG_BYTES}-byte limit"
            )));
        }
        let input = fs::read_to_string(&path).map_err(|source| ModelError::Io {
            path: path.clone(),
            source,
        })?;
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
