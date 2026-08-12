//! Agent Plugins 1.0.0 manifest and component validation.

use super::{
    AGENT_PLUGIN_MANIFEST_SCHEMA, AGENT_PLUGIN_MCP_SCHEMA, McpServerKind, McpServerReview,
    PluginError, PluginManifest, PluginReview,
};
use crate::skill::{SkillCatalog, SkillSource};
use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path},
};

const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_MCP_BYTES: usize = 256 * 1024;
const MAX_MCP_SERVERS: usize = 256;
const MAX_MCP_ARGUMENTS: usize = 256;
const MAX_MCP_ENVIRONMENT: usize = 256;
const MAX_MCP_HEADERS: usize = 128;
const MAX_STRING_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(rename = "$schema")]
    schema: String,
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<Author>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    keywords: Option<Vec<String>>,
    #[serde(default)]
    extensions: Option<Value>,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Author {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

pub(super) fn inspect(
    root: &Path,
    mutable: bool,
    digest: String,
) -> Result<PluginReview, PluginError> {
    let manifest_path = root.join("plugin.json");
    let bytes = read_regular(&manifest_path, MAX_MANIFEST_BYTES)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| PluginError::Json {
        path: manifest_path.clone(),
        reason: source.to_string(),
    })?;
    if !value.is_object() {
        return Err(PluginError::Invalid(
            "plugin.json must contain a top-level JSON object".to_owned(),
        ));
    }
    let raw: RawManifest = serde_json::from_value(value).map_err(|source| PluginError::Json {
        path: manifest_path.clone(),
        reason: source.to_string(),
    })?;
    if raw.schema != AGENT_PLUGIN_MANIFEST_SCHEMA {
        return Err(PluginError::UnsupportedSchema(safe_text(&raw.schema)));
    }
    validate_plugin_name(&raw.name)?;
    validate_manifest_strings(&raw)?;

    let mut diagnostics = raw
        .unknown
        .keys()
        .map(|field| format!("ignored unknown plugin.json field {:?}", safe_text(field)))
        .collect::<Vec<_>>();
    if raw
        .extensions
        .as_ref()
        .is_some_and(|value| !value.is_object())
    {
        diagnostics.push("ignored non-object plugin.json extensions field".to_owned());
    }

    let manifest = PluginManifest {
        name: raw.name,
        version: raw.version,
        description: raw.description,
    };
    let (skills, skill_diagnostics) = inspect_skills(root, &manifest.name, mutable)?;
    diagnostics.extend(skill_diagnostics);
    let (mcp_servers, mcp_diagnostics) = inspect_mcp(root)?;
    diagnostics.extend(mcp_diagnostics);
    diagnostics.sort();

    Ok(PluginReview {
        manifest,
        root: root.to_owned(),
        digest,
        mutable,
        skills,
        mcp_servers,
        diagnostics,
    })
}

fn inspect_skills(
    root: &Path,
    plugin: &str,
    mutable: bool,
) -> Result<(Vec<String>, Vec<String>), PluginError> {
    let skills_root = root.join("skills");
    match fs::symlink_metadata(&skills_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(source) => {
            return Err(PluginError::Io {
                path: skills_root,
                source,
            });
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Ok((
                Vec::new(),
                vec![
                    "disabled plugin skills because skills/ is not a contained directory"
                        .to_owned(),
                ],
            ));
        }
        Ok(_) => {}
    }
    let source = SkillSource::plugin(plugin, skills_root, mutable).map_err(PluginError::Skill)?;
    let catalog = SkillCatalog::discover(vec![source]).map_err(PluginError::Skill)?;
    let mut diagnostics = catalog
        .diagnostics()
        .iter()
        .map(|item| {
            format!(
                "skipped invalid skill {}: {}",
                item.qualified_name, item.reason
            )
        })
        .collect::<Vec<_>>();
    let candidates = catalog
        .entries()
        .map(|entry| entry.qualified_name.clone())
        .collect::<Vec<_>>();
    let mut valid = Vec::new();
    for candidate in candidates {
        match catalog.activate(&candidate) {
            Ok(activated) => valid.push(activated.entry.metadata.name),
            Err(error) => diagnostics.push(format!(
                "skipped invalid skill {}: {}",
                safe_text(&candidate),
                safe_text(&error.to_string())
            )),
        }
    }
    valid.sort();
    diagnostics.sort();
    Ok((valid, diagnostics))
}

fn inspect_mcp(root: &Path) -> Result<(Vec<McpServerReview>, Vec<String>), PluginError> {
    let path = root.join("mcp.json");
    let bytes = match read_regular(&path, MAX_MCP_BYTES) {
        Ok(bytes) => bytes,
        Err(PluginError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(error) => {
            return Ok((
                Vec::new(),
                vec![format!("disabled plugin MCP configuration: {error}")],
            ));
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return Ok((
                Vec::new(),
                vec![
                    "disabled plugin MCP configuration because mcp.json is invalid JSON".to_owned(),
                ],
            ));
        }
    };
    let Some(object) = value.as_object() else {
        return Ok((
            Vec::new(),
            vec!["disabled plugin MCP configuration because mcp.json is not an object".to_owned()],
        ));
    };
    if object
        .keys()
        .any(|key| key != "$schema" && key != "mcpServers")
    {
        return Ok((
            Vec::new(),
            vec!["disabled plugin MCP configuration because mcp.json contains unknown top-level fields".to_owned()],
        ));
    }
    if object.get("$schema").and_then(Value::as_str) != Some(AGENT_PLUGIN_MCP_SCHEMA) {
        return Ok((
            Vec::new(),
            vec![
                "disabled plugin MCP configuration because its schema is unsupported or mismatched"
                    .to_owned(),
            ],
        ));
    }
    let Some(servers) = object.get("mcpServers").and_then(Value::as_object) else {
        return Ok((
            Vec::new(),
            vec![
                "disabled plugin MCP configuration because mcpServers is not an object".to_owned(),
            ],
        ));
    };
    if servers.len() > MAX_MCP_SERVERS {
        return Ok((
            Vec::new(),
            vec![format!(
                "disabled plugin MCP configuration because it exceeds {MAX_MCP_SERVERS} servers"
            )],
        ));
    }
    let mut valid = Vec::new();
    let mut diagnostics = Vec::new();
    for (name, server) in servers {
        match validate_server(root, name, server) {
            Ok(review) => valid.push(review),
            Err(reason) => diagnostics.push(format!(
                "skipped invalid MCP server {:?}: {}",
                safe_text(name),
                safe_text(&reason)
            )),
        }
    }
    valid.sort_by(|left, right| left.name.cmp(&right.name));
    diagnostics.sort();
    Ok((valid, diagnostics))
}

fn validate_server(root: &Path, name: &str, value: &Value) -> Result<McpServerReview, String> {
    validate_label(name, "server name")?;
    let object = value
        .as_object()
        .ok_or_else(|| "configuration is not an object".to_owned())?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "type is missing or not a string".to_owned())?;
    match kind {
        "stdio" => validate_stdio(root, name, object),
        "streamable-http" => validate_http(name, object),
        "sse" => Err("legacy SSE transport is not supported by Xana".to_owned()),
        _ => Err("transport type is unsupported".to_owned()),
    }
}

fn validate_stdio(
    root: &Path,
    name: &str,
    object: &Map<String, Value>,
) -> Result<McpServerReview, String> {
    ensure_fields(object, &["type", "command", "args", "env", "cwd"])?;
    let command = required_string(object, "command")?;
    if command.len() > 4_096 || command.chars().any(char::is_control) {
        return Err("command is empty, contains control characters, or is too large".to_owned());
    }
    if command.starts_with("./") {
        validate_package_relative(root, &command)?;
    } else if command.contains('/')
        || command.contains('\\')
        || command.chars().any(char::is_whitespace)
    {
        return Err("command must be one bare executable token or begin with ./".to_owned());
    }
    let args = string_array(object.get("args"), "args", MAX_MCP_ARGUMENTS)?;
    let env = string_map(object.get("env"), "env", MAX_MCP_ENVIRONMENT)?;
    if env.keys().any(|key| {
        key.eq_ignore_ascii_case("PLUGIN_ROOT") || key.eq_ignore_ascii_case("PLUGIN_DATA")
    }) {
        return Err("env cannot override PLUGIN_ROOT or PLUGIN_DATA".to_owned());
    }
    let cwd = optional_string(object, "cwd")?;
    if let Some(cwd) = cwd.as_deref() {
        validate_cwd(root, cwd)?;
    }
    Ok(McpServerReview {
        name: name.to_owned(),
        kind: McpServerKind::Stdio,
        destination: command,
        argument_count: args.len(),
        environment_names: env.keys().cloned().collect(),
        header_names: Vec::new(),
    })
}

fn validate_http(name: &str, object: &Map<String, Value>) -> Result<McpServerReview, String> {
    ensure_fields(object, &["type", "url", "headers"])?;
    let raw = required_string(object, "url")?;
    let url = Url::parse(&raw).map_err(|_| "url is not an absolute HTTP(S) URL".to_owned())?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("url cannot include user information or a fragment".to_owned());
    }
    let host = url.host_str().ok_or_else(|| "url has no host".to_owned())?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("non-loopback MCP endpoints require HTTPS".to_owned());
    }
    let headers = string_map(object.get("headers"), "headers", MAX_MCP_HEADERS)?;
    let mut normalized = BTreeSet::new();
    for (key, value) in &headers {
        HeaderName::from_bytes(key.as_bytes()).map_err(|_| "header name is invalid".to_owned())?;
        HeaderValue::from_str(value).map_err(|_| "header value is invalid".to_owned())?;
        if !normalized.insert(key.to_ascii_lowercase()) {
            return Err("header names contain a case-insensitive duplicate".to_owned());
        }
    }
    Ok(McpServerReview {
        name: name.to_owned(),
        kind: McpServerKind::StreamableHttp,
        destination: format!(
            "{}://{}{}",
            url.scheme(),
            url.host_str().unwrap_or_default(),
            url.path()
        ),
        argument_count: 0,
        environment_names: Vec::new(),
        header_names: headers.keys().cloned().collect(),
    })
}

fn validate_plugin_name(name: &str) -> Result<(), PluginError> {
    let valid = (1..=64).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
        })
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && !name.contains("--")
        && !name.contains("..");
    if valid {
        Ok(())
    } else {
        Err(PluginError::Invalid(format!(
            "plugin name {:?} does not satisfy Agent Plugins 1.0.0",
            safe_text(name)
        )))
    }
}

fn validate_manifest_strings(raw: &RawManifest) -> Result<(), PluginError> {
    let mut values = Vec::new();
    values.extend(raw.version.iter());
    values.extend(raw.description.iter());
    values.extend(raw.homepage.iter());
    values.extend(raw.repository.iter());
    values.extend(raw.license.iter());
    if let Some(author) = &raw.author {
        values.extend(author.name.iter());
        values.extend(author.email.iter());
        values.extend(author.url.iter());
    }
    if let Some(keywords) = &raw.keywords {
        if keywords.len() > 256 {
            return Err(PluginError::Invalid(
                "plugin keywords exceed the bounded limit".to_owned(),
            ));
        }
        values.extend(keywords.iter());
    }
    if values.iter().any(|value| value.len() > MAX_STRING_BYTES) {
        return Err(PluginError::Invalid(
            "plugin metadata exceeds the bounded string limit".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_fields(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("configuration contains an unknown or cross-variant field".to_owned());
    }
    Ok(())
}

fn required_string(object: &Map<String, Value>, field: &str) -> Result<String, String> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is missing, empty, or not a string"))?;
    if value.len() > MAX_STRING_BYTES {
        return Err(format!("{field} exceeds the bounded string limit"));
    }
    Ok(value.to_owned())
}

fn optional_string(object: &Map<String, Value>, field: &str) -> Result<Option<String>, String> {
    object.get(field).map_or(Ok(None), |value| {
        value
            .as_str()
            .filter(|value| value.len() <= MAX_STRING_BYTES)
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| format!("{field} is not a bounded string"))
    })
}

fn string_array(value: Option<&Value>, field: &str, limit: usize) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{field} is not an array"))?;
    if array.len() > limit {
        return Err(format!("{field} exceeds the bounded entry limit"));
    }
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| value.len() <= MAX_STRING_BYTES)
                .map(str::to_owned)
                .ok_or_else(|| format!("{field} contains a non-string or oversized value"))
        })
        .collect()
}

fn string_map(
    value: Option<&Value>,
    field: &str,
    limit: usize,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| format!("{field} is not an object"))?;
    if object.len() > limit {
        return Err(format!("{field} exceeds the bounded entry limit"));
    }
    object
        .iter()
        .map(|(key, value)| {
            if key.len() > 256 || key.chars().any(char::is_control) {
                return Err(format!("{field} contains an invalid name"));
            }
            value
                .as_str()
                .filter(|value| value.len() <= MAX_STRING_BYTES)
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| format!("{field} contains a non-string or oversized value"))
        })
        .collect()
}

fn validate_cwd(root: &Path, cwd: &str) -> Result<(), String> {
    if let Some(relative) = cwd.strip_prefix("./") {
        validate_relative_text(relative)?;
        let _ = root;
        return Ok(());
    }
    for prefix in ["${PLUGIN_ROOT}", "${PLUGIN_DATA}"] {
        if cwd == prefix {
            return Ok(());
        }
        if let Some(relative) = cwd.strip_prefix(&format!("{prefix}/")) {
            return validate_relative_text(relative);
        }
    }
    Err("cwd must begin with ./, ${PLUGIN_ROOT}, or ${PLUGIN_DATA}".to_owned())
}

fn validate_package_relative(root: &Path, value: &str) -> Result<(), String> {
    let relative = value
        .strip_prefix("./")
        .ok_or_else(|| "path is not plugin-relative".to_owned())?;
    validate_relative_text(relative)?;
    let candidate = root.join(relative);
    if candidate.is_absolute() && !candidate.starts_with(root) {
        return Err("path escapes the plugin root".to_owned());
    }
    Ok(())
}

fn validate_relative_text(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path.components().count() > 32
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("path is not a bounded contained relative path".to_owned());
    }
    Ok(())
}

fn validate_label(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err(format!(
            "{field} is empty, oversized, or contains control characters"
        ))
    } else {
        Ok(())
    }
}

fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>, PluginError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PluginError::Io {
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PluginError::Invalid(format!(
            "{} is not a contained regular file",
            safe_path(path)
        )));
    }
    if metadata.len() > limit as u64 {
        return Err(PluginError::Limit {
            what: "plugin component bytes",
            limit,
        });
    }
    fs::read(path).map_err(|source| PluginError::Io {
        path: path.to_owned(),
        source,
    })
}

pub(super) fn safe_path(path: &Path) -> String {
    safe_text(&path.to_string_lossy())
}

pub(super) fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}
