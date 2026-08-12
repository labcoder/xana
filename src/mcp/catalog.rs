//! Transport-independent MCP identity, exposure, and progressive catalog policy.

use super::protocol::{
    McpNegotiation, McpPromptWire, McpResourceTemplateWire, McpResourceWire, McpToolWire, Page,
    ProtocolError, sanitize_text,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

const MAX_SERVER_NAME_BYTES: usize = 64;
const MAX_PRIMITIVE_NAME_BYTES: usize = 128;
const MAX_DESCRIPTION_PREVIEW_BYTES: usize = 512;
const MAX_TITLE_BYTES: usize = 256;
const MAX_URI_BYTES: usize = 4096;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 32;
const MAX_SCHEMA_NODES: usize = 4096;
const MAX_SCHEMA_STRING_BYTES: usize = 16 * 1024;
const MAX_SEARCH_RESULTS: usize = 64;
const MAX_PAGES_PER_PRIMITIVE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogLimits {
    pub(crate) max_servers: usize,
    pub(crate) max_tools: usize,
    pub(crate) max_resources: usize,
    pub(crate) max_resource_templates: usize,
    pub(crate) max_prompts: usize,
    pub(crate) max_tool_metadata_bytes: usize,
    pub(crate) max_resource_metadata_bytes: usize,
    pub(crate) max_prompt_metadata_bytes: usize,
}

impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            max_servers: 64,
            max_tools: 2048,
            max_resources: 2048,
            max_resource_templates: 1024,
            max_prompts: 1024,
            max_tool_metadata_bytes: 512 * 1024,
            max_resource_metadata_bytes: 512 * 1024,
            max_prompt_metadata_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct McpPrimitiveAllowlist {
    pub(crate) tools: BTreeSet<String>,
    pub(crate) resources: BTreeSet<String>,
    pub(crate) resource_templates: BTreeSet<String>,
    pub(crate) prompts: BTreeSet<String>,
}

impl McpPrimitiveAllowlist {
    pub(crate) fn validate(&self) -> Result<(), McpCatalogError> {
        for name in self.tools.iter().chain(&self.prompts) {
            validate_primitive_name(name)?;
        }
        for uri in &self.resources {
            validate_uri(uri)?;
        }
        for template in &self.resource_templates {
            validate_uri_template(template)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpServerExposure {
    pub(crate) server: String,
    pub(crate) configured_identity_digest: String,
    pub(crate) enabled: bool,
    pub(crate) profile_selected: bool,
    pub(crate) allowlist: McpPrimitiveAllowlist,
}

impl McpServerExposure {
    pub(crate) fn validate(&self) -> Result<(), McpCatalogError> {
        validate_server_name(&self.server)?;
        validate_digest(&self.configured_identity_digest)?;
        self.allowlist.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpServerReadiness {
    Disabled,
    Unavailable,
    Incompatible,
    Unhealthy,
    NotAuthorized,
    Ready,
}

impl McpServerReadiness {
    pub(crate) fn resolve(
        exposure: &McpServerExposure,
        negotiation: Option<&Result<McpNegotiation, ProtocolError>>,
        healthy: bool,
    ) -> Self {
        if !exposure.enabled {
            Self::Disabled
        } else if !exposure.profile_selected {
            Self::NotAuthorized
        } else {
            match negotiation {
                None => Self::Unavailable,
                Some(Err(_)) => Self::Incompatible,
                Some(Ok(_)) if !healthy => Self::Unhealthy,
                Some(Ok(_)) => Self::Ready,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpToolSummary {
    pub(crate) qualified_name: String,
    pub(crate) server: String,
    pub(crate) source_name: String,
    pub(crate) display_alias: String,
    pub(crate) title: Option<String>,
    pub(crate) description_preview: String,
    pub(crate) input_schema_digest: String,
    pub(crate) output_schema_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpResourceSummary {
    pub(crate) server: String,
    pub(crate) uri: String,
    pub(crate) name: String,
    pub(crate) title: Option<String>,
    pub(crate) description_preview: Option<String>,
    pub(crate) mime_type: Option<String>,
    pub(crate) size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpResourceTemplateSummary {
    pub(crate) server: String,
    pub(crate) uri_template: String,
    pub(crate) name: String,
    pub(crate) title: Option<String>,
    pub(crate) description_preview: Option<String>,
    pub(crate) mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpPromptSummary {
    pub(crate) server: String,
    pub(crate) source_name: String,
    pub(crate) title: Option<String>,
    pub(crate) description_preview: Option<String>,
    pub(crate) argument_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct McpToolDefinition {
    pub(crate) summary: McpToolSummary,
    pub(crate) input_schema: Value,
    pub(crate) output_schema: Option<Value>,
}

impl McpToolDefinition {
    pub(super) fn from_wire(server: &str, wire: McpToolWire) -> Result<Self, McpCatalogError> {
        validate_primitive_name(&wire.name)?;
        validate_schema(&wire.input_schema, true)?;
        if let Some(schema) = &wire.output_schema {
            validate_schema(schema, false)?;
        }
        let summary = tool_summary(server, &wire)?;
        Ok(Self {
            summary,
            input_schema: wire.input_schema,
            output_schema: wire.output_schema,
        })
    }
}

pub(crate) trait McpCatalogSource {
    fn load_tool<'a>(
        &'a mut self,
        server: &'a str,
        source_name: &'a str,
    ) -> BoxFuture<'a, Result<McpToolWire, McpCatalogError>>;
}

#[derive(Debug, Clone)]
pub(crate) struct McpCatalog {
    limits: CatalogLimits,
    native_tool_names: BTreeSet<String>,
    servers: BTreeMap<String, McpServerExposure>,
    tools: BTreeMap<String, McpToolSummary>,
    resources: BTreeMap<(String, String), McpResourceSummary>,
    resource_templates: BTreeMap<(String, String), McpResourceTemplateSummary>,
    prompts: BTreeMap<(String, String), McpPromptSummary>,
    tool_metadata_bytes: usize,
    resource_metadata_bytes: usize,
    prompt_metadata_bytes: usize,
    pub(crate) tools_truncated: bool,
    pub(crate) resources_truncated: bool,
    pub(crate) resource_templates_truncated: bool,
    pub(crate) prompts_truncated: bool,
}

impl McpCatalog {
    pub(crate) fn new(
        limits: CatalogLimits,
        native_tool_names: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            limits,
            native_tool_names: native_tool_names.into_iter().collect(),
            servers: BTreeMap::new(),
            tools: BTreeMap::new(),
            resources: BTreeMap::new(),
            resource_templates: BTreeMap::new(),
            prompts: BTreeMap::new(),
            tool_metadata_bytes: 0,
            resource_metadata_bytes: 0,
            prompt_metadata_bytes: 0,
            tools_truncated: false,
            resources_truncated: false,
            resource_templates_truncated: false,
            prompts_truncated: false,
        }
    }

    pub(crate) fn add_server(
        &mut self,
        exposure: McpServerExposure,
    ) -> Result<(), McpCatalogError> {
        exposure.validate()?;
        if !self.servers.contains_key(&exposure.server)
            && self.servers.len() >= self.limits.max_servers
        {
            return Err(McpCatalogError::ServerLimit(self.limits.max_servers));
        }
        if self
            .servers
            .insert(exposure.server.clone(), exposure)
            .is_some()
        {
            return Err(McpCatalogError::DuplicateServer);
        }
        Ok(())
    }

    pub(crate) fn index_tool_page(
        &mut self,
        server: &str,
        page: Page<McpToolWire>,
    ) -> Result<IndexReport, McpCatalogError> {
        let exposure = self.ready_exposure(server)?;
        let allowlist = exposure.allowlist.tools.clone();
        let mut report = IndexReport::default();
        for wire in page.items {
            if !allowlist.contains(&wire.name) {
                report.not_authorized += 1;
                continue;
            }
            let summary = match tool_summary(server, &wire) {
                Ok(summary) => summary,
                Err(_) => {
                    report.invalid += 1;
                    continue;
                }
            };
            if self.native_tool_names.contains(&summary.qualified_name) {
                return Err(McpCatalogError::NativeCollision(summary.qualified_name));
            }
            if self.tools.contains_key(&summary.qualified_name) {
                return Err(McpCatalogError::DuplicateTool(summary.qualified_name));
            }
            self.tool_metadata_bytes = self
                .tool_metadata_bytes
                .saturating_add(summary_metadata_bytes(&summary));
            self.tools.insert(summary.qualified_name.clone(), summary);
            report.indexed += 1;
        }
        self.trim_tools();
        report.truncated = self.tools_truncated;
        Ok(report)
    }

    pub(crate) fn index_resource_page(
        &mut self,
        server: &str,
        page: Page<McpResourceWire>,
    ) -> Result<IndexReport, McpCatalogError> {
        let exposure = self.ready_exposure(server)?;
        let allowlist = exposure.allowlist.resources.clone();
        let mut report = IndexReport::default();
        for wire in page.items {
            if !allowlist.contains(&wire.uri) {
                report.not_authorized += 1;
                continue;
            }
            let summary = match resource_summary(server, wire) {
                Ok(summary) => summary,
                Err(_) => {
                    report.invalid += 1;
                    continue;
                }
            };
            let key = (server.to_owned(), summary.uri.clone());
            if self.resources.contains_key(&key) {
                return Err(McpCatalogError::DuplicateResource(summary.uri));
            }
            self.resource_metadata_bytes = self
                .resource_metadata_bytes
                .saturating_add(summary_metadata_bytes(&summary));
            self.resources.insert(key, summary);
            report.indexed += 1;
        }
        self.trim_resources();
        report.truncated = self.resources_truncated;
        Ok(report)
    }

    pub(crate) fn index_resource_template_page(
        &mut self,
        server: &str,
        page: Page<McpResourceTemplateWire>,
    ) -> Result<IndexReport, McpCatalogError> {
        let exposure = self.ready_exposure(server)?;
        let allowlist = exposure.allowlist.resource_templates.clone();
        let mut report = IndexReport::default();
        for wire in page.items {
            if !allowlist.contains(&wire.uri_template) {
                report.not_authorized += 1;
                continue;
            }
            let summary = match resource_template_summary(server, wire) {
                Ok(summary) => summary,
                Err(_) => {
                    report.invalid += 1;
                    continue;
                }
            };
            let key = (server.to_owned(), summary.uri_template.clone());
            if self.resource_templates.contains_key(&key) {
                return Err(McpCatalogError::DuplicateResourceTemplate(
                    summary.uri_template,
                ));
            }
            self.resource_metadata_bytes = self
                .resource_metadata_bytes
                .saturating_add(summary_metadata_bytes(&summary));
            self.resource_templates.insert(key, summary);
            report.indexed += 1;
        }
        self.trim_resource_templates();
        report.truncated = self.resource_templates_truncated;
        Ok(report)
    }

    pub(crate) fn index_prompt_page(
        &mut self,
        server: &str,
        page: Page<McpPromptWire>,
    ) -> Result<IndexReport, McpCatalogError> {
        let exposure = self.ready_exposure(server)?;
        let allowlist = exposure.allowlist.prompts.clone();
        let mut report = IndexReport::default();
        for wire in page.items {
            if !allowlist.contains(&wire.name) {
                report.not_authorized += 1;
                continue;
            }
            let summary = match prompt_summary(server, wire) {
                Ok(summary) => summary,
                Err(_) => {
                    report.invalid += 1;
                    continue;
                }
            };
            let key = (server.to_owned(), summary.source_name.clone());
            if self.prompts.contains_key(&key) {
                return Err(McpCatalogError::DuplicatePrompt(summary.source_name));
            }
            self.prompt_metadata_bytes = self
                .prompt_metadata_bytes
                .saturating_add(summary_metadata_bytes(&summary));
            self.prompts.insert(key, summary);
            report.indexed += 1;
        }
        self.trim_prompts();
        report.truncated = self.prompts_truncated;
        Ok(report)
    }

    pub(crate) fn search_tools(&self, query: &str, limit: usize) -> Vec<&McpToolSummary> {
        let query = sanitize_text(query, MAX_PRIMITIVE_NAME_BYTES).to_ascii_lowercase();
        self.tools
            .values()
            .filter(|tool| {
                query.is_empty()
                    || tool.qualified_name.to_ascii_lowercase().contains(&query)
                    || tool
                        .description_preview
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .take(limit.min(MAX_SEARCH_RESULTS))
            .collect()
    }

    pub(crate) fn search_resources(&self, query: &str, limit: usize) -> Vec<&McpResourceSummary> {
        let query = sanitize_text(query, MAX_URI_BYTES).to_ascii_lowercase();
        self.resources
            .values()
            .filter(|resource| {
                query.is_empty()
                    || resource.uri.to_ascii_lowercase().contains(&query)
                    || resource.name.to_ascii_lowercase().contains(&query)
            })
            .take(limit.min(MAX_SEARCH_RESULTS))
            .collect()
    }

    pub(crate) fn search_prompts(&self, query: &str, limit: usize) -> Vec<&McpPromptSummary> {
        let query = sanitize_text(query, MAX_PRIMITIVE_NAME_BYTES).to_ascii_lowercase();
        self.prompts
            .values()
            .filter(|prompt| {
                query.is_empty()
                    || prompt.source_name.to_ascii_lowercase().contains(&query)
                    || prompt
                        .description_preview
                        .as_deref()
                        .is_some_and(|value| value.to_ascii_lowercase().contains(&query))
            })
            .take(limit.min(MAX_SEARCH_RESULTS))
            .collect()
    }

    pub(crate) async fn inspect_tool_exact<S: McpCatalogSource>(
        &self,
        server: &str,
        source_name: &str,
        source: &mut S,
    ) -> Result<McpToolDefinition, McpCatalogError> {
        validate_primitive_name(source_name)?;
        let exposure = self.ready_exposure(server)?;
        if !exposure.allowlist.tools.contains(source_name) {
            return Err(McpCatalogError::NotAuthorized);
        }
        let wire = source.load_tool(server, source_name).await?;
        if wire.name != source_name {
            return Err(McpCatalogError::IdentityChanged);
        }
        let definition = McpToolDefinition::from_wire(server, wire)?;
        if let Some(indexed) = self.tools.get(&definition.summary.qualified_name)
            && (indexed.input_schema_digest != definition.summary.input_schema_digest
                || indexed.output_schema_digest != definition.summary.output_schema_digest)
        {
            return Err(McpCatalogError::IdentityChanged);
        }
        Ok(definition)
    }

    pub(crate) fn model_tool_index(&self, max_bytes: usize) -> String {
        let mut output = String::new();
        for tool in self.tools.values() {
            let line = format!("{} - {}\n", tool.qualified_name, tool.description_preview);
            if output.len().saturating_add(line.len()) > max_bytes {
                break;
            }
            output.push_str(&line);
        }
        output
    }

    pub(crate) fn tool_summaries(&self) -> Vec<McpToolSummary> {
        self.tools.values().cloned().collect()
    }

    pub(crate) fn resource_summary(
        &self,
        server: &str,
        uri: &str,
    ) -> Result<McpResourceSummary, McpCatalogError> {
        self.ready_exposure(server)?;
        self.resources
            .get(&(server.to_owned(), uri.to_owned()))
            .cloned()
            .ok_or(McpCatalogError::NotAuthorized)
    }

    pub(crate) fn prompt_summary(
        &self,
        server: &str,
        name: &str,
    ) -> Result<McpPromptSummary, McpCatalogError> {
        self.ready_exposure(server)?;
        self.prompts
            .get(&(server.to_owned(), name.to_owned()))
            .cloned()
            .ok_or(McpCatalogError::NotAuthorized)
    }

    pub(crate) fn tool_count(&self) -> usize {
        self.tools.len()
    }

    pub(crate) fn resource_count(&self) -> usize {
        self.resources.len()
    }

    pub(crate) fn resource_template_count(&self) -> usize {
        self.resource_templates.len()
    }

    pub(crate) fn prompt_count(&self) -> usize {
        self.prompts.len()
    }

    pub(crate) fn metadata_bytes(&self) -> usize {
        self.tool_metadata_bytes
            .saturating_add(self.resource_metadata_bytes)
            .saturating_add(self.prompt_metadata_bytes)
    }

    fn ready_exposure(&self, server: &str) -> Result<&McpServerExposure, McpCatalogError> {
        let exposure = self
            .servers
            .get(server)
            .ok_or(McpCatalogError::UnknownServer)?;
        if !exposure.enabled {
            Err(McpCatalogError::Disabled)
        } else if !exposure.profile_selected {
            Err(McpCatalogError::NotAuthorized)
        } else {
            Ok(exposure)
        }
    }

    fn trim_tools(&mut self) {
        while self.tools.len() > self.limits.max_tools
            || self.tool_metadata_bytes > self.limits.max_tool_metadata_bytes
        {
            let Some(key) = self.tools.keys().next_back().cloned() else {
                break;
            };
            let removed = self.tools.remove(&key).expect("catalog key came from map");
            self.tool_metadata_bytes = self
                .tool_metadata_bytes
                .saturating_sub(summary_metadata_bytes(&removed));
            self.tools_truncated = true;
        }
    }

    fn trim_resources(&mut self) {
        while self.resources.len() > self.limits.max_resources
            || self.resource_metadata_bytes > self.limits.max_resource_metadata_bytes
        {
            let Some(key) = self.resources.keys().next_back().cloned() else {
                break;
            };
            let removed = self
                .resources
                .remove(&key)
                .expect("catalog key came from map");
            self.resource_metadata_bytes = self
                .resource_metadata_bytes
                .saturating_sub(summary_metadata_bytes(&removed));
            self.resources_truncated = true;
        }
    }

    fn trim_resource_templates(&mut self) {
        while self.resource_templates.len() > self.limits.max_resource_templates
            || self.resource_metadata_bytes > self.limits.max_resource_metadata_bytes
        {
            let Some(key) = self.resource_templates.keys().next_back().cloned() else {
                break;
            };
            let removed = self
                .resource_templates
                .remove(&key)
                .expect("catalog key came from map");
            self.resource_metadata_bytes = self
                .resource_metadata_bytes
                .saturating_sub(summary_metadata_bytes(&removed));
            self.resource_templates_truncated = true;
        }
    }

    fn trim_prompts(&mut self) {
        while self.prompts.len() > self.limits.max_prompts
            || self.prompt_metadata_bytes > self.limits.max_prompt_metadata_bytes
        {
            let Some(key) = self.prompts.keys().next_back().cloned() else {
                break;
            };
            let removed = self
                .prompts
                .remove(&key)
                .expect("catalog key came from map");
            self.prompt_metadata_bytes = self
                .prompt_metadata_bytes
                .saturating_sub(summary_metadata_bytes(&removed));
            self.prompts_truncated = true;
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IndexReport {
    pub(crate) indexed: usize,
    pub(crate) invalid: usize,
    pub(crate) not_authorized: usize,
    pub(crate) truncated: bool,
}

#[derive(Debug, Default)]
pub(crate) struct McpPaginationGuard {
    pages: usize,
    cursors: BTreeSet<String>,
}

impl McpPaginationGuard {
    pub(crate) fn accept(&mut self, next_cursor: Option<&str>) -> Result<(), McpCatalogError> {
        self.pages = self.pages.saturating_add(1);
        if self.pages > MAX_PAGES_PER_PRIMITIVE {
            return Err(McpCatalogError::PageLimit(MAX_PAGES_PER_PRIMITIVE));
        }
        if let Some(cursor) = next_cursor
            && !self.cursors.insert(cursor.to_owned())
        {
            return Err(McpCatalogError::PaginationCycle);
        }
        Ok(())
    }
}

fn tool_summary(server: &str, wire: &McpToolWire) -> Result<McpToolSummary, McpCatalogError> {
    validate_server_name(server)?;
    validate_primitive_name(&wire.name)?;
    validate_schema(&wire.input_schema, true)?;
    if let Some(schema) = &wire.output_schema {
        validate_schema(schema, false)?;
    }
    let qualified_name = format!("mcp.{server}.{}", wire.name);
    Ok(McpToolSummary {
        qualified_name,
        server: server.to_owned(),
        source_name: wire.name.clone(),
        display_alias: sanitize_text(wire.title.as_deref().unwrap_or(&wire.name), MAX_TITLE_BYTES),
        title: wire
            .title
            .as_deref()
            .map(|value| sanitize_text(value, MAX_TITLE_BYTES)),
        description_preview: sanitize_text(&wire.description, MAX_DESCRIPTION_PREVIEW_BYTES),
        input_schema_digest: digest_json(&wire.input_schema)?,
        output_schema_digest: wire.output_schema.as_ref().map(digest_json).transpose()?,
    })
}

fn resource_summary(
    server: &str,
    wire: McpResourceWire,
) -> Result<McpResourceSummary, McpCatalogError> {
    validate_uri(&wire.uri)?;
    validate_review_name(&wire.name)?;
    Ok(McpResourceSummary {
        server: server.to_owned(),
        uri: wire.uri,
        name: sanitize_text(&wire.name, MAX_TITLE_BYTES),
        title: wire
            .title
            .as_deref()
            .map(|value| sanitize_text(value, MAX_TITLE_BYTES)),
        description_preview: wire
            .description
            .as_deref()
            .map(|value| sanitize_text(value, MAX_DESCRIPTION_PREVIEW_BYTES)),
        mime_type: wire
            .mime_type
            .as_deref()
            .map(|value| sanitize_text(value, 256)),
        size: wire.size,
    })
}

fn resource_template_summary(
    server: &str,
    wire: McpResourceTemplateWire,
) -> Result<McpResourceTemplateSummary, McpCatalogError> {
    validate_uri_template(&wire.uri_template)?;
    validate_review_name(&wire.name)?;
    Ok(McpResourceTemplateSummary {
        server: server.to_owned(),
        uri_template: wire.uri_template,
        name: sanitize_text(&wire.name, MAX_TITLE_BYTES),
        title: wire
            .title
            .as_deref()
            .map(|value| sanitize_text(value, MAX_TITLE_BYTES)),
        description_preview: wire
            .description
            .as_deref()
            .map(|value| sanitize_text(value, MAX_DESCRIPTION_PREVIEW_BYTES)),
        mime_type: wire
            .mime_type
            .as_deref()
            .map(|value| sanitize_text(value, 256)),
    })
}

fn prompt_summary(server: &str, wire: McpPromptWire) -> Result<McpPromptSummary, McpCatalogError> {
    validate_primitive_name(&wire.name)?;
    if wire.arguments.len() > 128 {
        return Err(McpCatalogError::InvalidPrompt);
    }
    let mut argument_names = Vec::with_capacity(wire.arguments.len());
    let mut unique = BTreeSet::new();
    for argument in wire.arguments {
        validate_primitive_name(&argument.name)?;
        if !unique.insert(argument.name.clone()) {
            return Err(McpCatalogError::InvalidPrompt);
        }
        argument_names.push(argument.name);
    }
    Ok(McpPromptSummary {
        server: server.to_owned(),
        source_name: wire.name,
        title: wire
            .title
            .as_deref()
            .map(|value| sanitize_text(value, MAX_TITLE_BYTES)),
        description_preview: wire
            .description
            .as_deref()
            .map(|value| sanitize_text(value, MAX_DESCRIPTION_PREVIEW_BYTES)),
        argument_names,
    })
}

fn validate_server_name(value: &str) -> Result<(), McpCatalogError> {
    if value.is_empty()
        || value.len() > MAX_SERVER_NAME_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'_' | b'-'))
        })
    {
        return Err(McpCatalogError::InvalidServerName);
    }
    Ok(())
}

fn validate_primitive_name(value: &str) -> Result<(), McpCatalogError> {
    if value.is_empty()
        || value.len() > MAX_PRIMITIVE_NAME_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(McpCatalogError::InvalidPrimitiveName);
    }
    Ok(())
}

fn validate_review_name(value: &str) -> Result<(), McpCatalogError> {
    if value.trim().is_empty() || value.len() > MAX_TITLE_BYTES {
        Err(McpCatalogError::InvalidPrimitiveName)
    } else {
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), McpCatalogError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(McpCatalogError::InvalidIdentityDigest)
    }
}

fn validate_uri(value: &str) -> Result<(), McpCatalogError> {
    if value.is_empty()
        || value.len() > MAX_URI_BYTES
        || value.chars().any(char::is_control)
        || reqwest::Url::parse(value)
            .ok()
            .is_none_or(|uri| uri.scheme().is_empty())
    {
        Err(McpCatalogError::InvalidUri)
    } else {
        Ok(())
    }
}

fn validate_uri_template(value: &str) -> Result<(), McpCatalogError> {
    let scheme_end = value.find(':');
    if value.is_empty()
        || value.len() > MAX_URI_BYTES
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || scheme_end.is_none_or(|position| position == 0)
        || !balanced_braces(value)
    {
        Err(McpCatalogError::InvalidUri)
    } else {
        Ok(())
    }
}

fn balanced_braces(value: &str) -> bool {
    let mut depth = 0_usize;
    for byte in value.bytes() {
        match byte {
            b'{' => depth = depth.saturating_add(1),
            b'}' if depth == 0 => return false,
            b'}' => depth -= 1,
            _ => {}
        }
    }
    depth == 0
}

fn validate_schema(schema: &Value, require_object_root: bool) -> Result<(), McpCatalogError> {
    let encoded = serde_json::to_vec(schema).map_err(|_| McpCatalogError::InvalidSchema)?;
    if encoded.len() > MAX_SCHEMA_BYTES {
        return Err(McpCatalogError::SchemaTooLarge);
    }
    if !schema.is_object()
        || require_object_root && schema.get("type") != Some(&Value::String("object".into()))
    {
        return Err(McpCatalogError::InvalidSchema);
    }
    let mut nodes = 0_usize;
    validate_schema_node(schema, 0, &mut nodes)
}

fn validate_schema_node(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), McpCatalogError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(McpCatalogError::SchemaTooDeep);
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_SCHEMA_NODES {
        return Err(McpCatalogError::SchemaTooComplex);
    }
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                && !reference.starts_with('#')
            {
                return Err(McpCatalogError::ExternalSchemaReference);
            }
            for (key, value) in object {
                if key.len() > MAX_SCHEMA_STRING_BYTES {
                    return Err(McpCatalogError::InvalidSchema);
                }
                validate_schema_node(value, depth + 1, nodes)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_schema_node(value, depth + 1, nodes)?;
            }
        }
        Value::String(value) if value.len() > MAX_SCHEMA_STRING_BYTES => {
            return Err(McpCatalogError::InvalidSchema);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn digest_json(value: &Value) -> Result<String, McpCatalogError> {
    let bytes = serde_json::to_vec(value).map_err(|_| McpCatalogError::InvalidSchema)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn summary_metadata_bytes<T: Serialize>(summary: &T) -> usize {
    serde_json::to_vec(summary).map_or(usize::MAX, |bytes| bytes.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpCatalogError {
    InvalidServerName,
    InvalidPrimitiveName,
    InvalidIdentityDigest,
    InvalidUri,
    InvalidSchema,
    SchemaTooLarge,
    SchemaTooDeep,
    SchemaTooComplex,
    ExternalSchemaReference,
    InvalidPrompt,
    ServerLimit(usize),
    DuplicateServer,
    DuplicateTool(String),
    DuplicateResource(String),
    DuplicateResourceTemplate(String),
    DuplicatePrompt(String),
    NativeCollision(String),
    UnknownServer,
    Disabled,
    NotAuthorized,
    IdentityChanged,
    PageLimit(usize),
    PaginationCycle,
    SourceUnavailable,
}

impl fmt::Display for McpCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidServerName => formatter.write_str("MCP server name is invalid"),
            Self::InvalidPrimitiveName => formatter.write_str("MCP primitive name is invalid"),
            Self::InvalidIdentityDigest => {
                formatter.write_str("MCP configured identity digest is invalid")
            }
            Self::InvalidUri => formatter.write_str("MCP resource URI is invalid"),
            Self::InvalidSchema => formatter.write_str("MCP JSON schema is invalid"),
            Self::SchemaTooLarge => formatter.write_str("MCP JSON schema exceeds its byte limit"),
            Self::SchemaTooDeep => formatter.write_str("MCP JSON schema exceeds its depth limit"),
            Self::SchemaTooComplex => formatter.write_str("MCP JSON schema exceeds its node limit"),
            Self::ExternalSchemaReference => formatter.write_str(
                "MCP JSON schema contains a network reference that Xana will not dereference",
            ),
            Self::InvalidPrompt => formatter.write_str("MCP prompt definition is invalid"),
            Self::ServerLimit(limit) => write!(formatter, "MCP catalog exceeds {limit} servers"),
            Self::DuplicateServer => formatter.write_str("MCP server is duplicated"),
            Self::DuplicateTool(name) => write!(formatter, "MCP tool {name:?} is duplicated"),
            Self::DuplicateResource(uri) => {
                write!(formatter, "MCP resource {uri:?} is duplicated")
            }
            Self::DuplicateResourceTemplate(uri) => {
                write!(formatter, "MCP resource template {uri:?} is duplicated")
            }
            Self::DuplicatePrompt(name) => write!(formatter, "MCP prompt {name:?} is duplicated"),
            Self::NativeCollision(name) => {
                write!(
                    formatter,
                    "MCP tool {name:?} collides with a native capability"
                )
            }
            Self::UnknownServer => formatter.write_str("MCP server is not configured"),
            Self::Disabled => formatter.write_str("MCP server is disabled"),
            Self::NotAuthorized => formatter.write_str("MCP primitive is not profile-authorized"),
            Self::IdentityChanged => {
                formatter.write_str("MCP primitive changed since catalog review")
            }
            Self::PageLimit(limit) => write!(formatter, "MCP pagination exceeds {limit} pages"),
            Self::PaginationCycle => formatter.write_str("MCP pagination cursor repeated"),
            Self::SourceUnavailable => formatter.write_str("MCP catalog source is unavailable"),
        }
    }
}

impl Error for McpCatalogError {}
